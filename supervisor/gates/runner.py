"""
Gate runner — composes individual checks into task-level and milestone-level gates,
records every result to the evidence store, and produces the pass/fail verdict that is
the ONLY thing allowed to advance loop state.

The model's self_check claims are compared against verified reality; mismatches are
recorded as trust signals (used by the orchestrator to shrink task size for over-claiming).
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Callable, Optional

from . import checks
from .hotpath_lint import check_hotpath_lint
from .build_phase import check_phase_provenance
from ..reinforcement.dossier import missing_dossiers
from ..store.evidence import EvidenceStore, GateRecord


@dataclass
class GateConfig:
    target_triple: Optional[str] = None            # e.g. x86_64-pc-windows-msvc
    production_globs: list[str] = field(default_factory=lambda: [
        "rust/**/src/**/*.rs",
    ])
    run_bench: bool = False
    bench_name: str = ""
    bench_budgets_ns: dict[str, float] = field(default_factory=dict)
    run_determinism: bool = False
    replay_bin: str = ""
    replay_fixture: str = ""
    required_tests: list[str] = field(default_factory=list)
    run_hotpath_lint: bool = True
    lint_hot_globs: list[str] = field(default_factory=list)    # [] -> module defaults
    lint_money_globs: list[str] = field(default_factory=list)  # [] -> module defaults
    require_dossiers: list[str] = field(default_factory=list)  # hard components this milestone needs
    infra_manifest: str = ""                       # infrastructure manifest (machine provenance, §9.5)
    criteria_touched: list[int] = field(default_factory=list)  # criteria this milestone certifies


@dataclass
class GateVerdict:
    passed: bool
    results: list[checks.CheckResult]
    trust_mismatches: list[str] = field(default_factory=list)

    def summary(self) -> str:
        line = "PASS" if self.passed else "FAIL"
        parts = [f"{r.name}:{'ok' if r.passed else 'X'}" for r in self.results]
        return f"[{line}] " + " ".join(parts)


class GateRunner:
    def __init__(self, repo: str, store: EvidenceStore, run_id: str):
        self.repo = repo
        self.store = store
        self.run_id = run_id

    def _record(self, task_id: str, r: checks.CheckResult) -> None:
        self.store.record_gate(self.run_id, GateRecord(task_id, r.name, r.passed, r.detail))

    # ------------------------------------------------------------- task gate
    def task_gate(self, task_id: str, cfg: GateConfig,
                  model_self_check: Optional[dict] = None) -> GateVerdict:
        """Fast per-task gate: build + hygiene + tests. (Bench/determinism run at milestone.)"""
        results = [
            checks.check_build(self.repo, cfg.target_triple),
            checks.check_fmt(self.repo),
            checks.check_clippy(self.repo),
            checks.check_no_stubs(self.repo, cfg.production_globs),
            checks.check_tests(self.repo, cfg.required_tests or None),
            checks.check_dossier_test_integrity(self.repo),
        ]
        if cfg.run_hotpath_lint:
            results.append(check_hotpath_lint(self.repo,
                                              cfg.lint_hot_globs or None,
                                              cfg.lint_money_globs or None))
        for r in results:
            self._record(task_id, r)

        trust = self._trust_check(model_self_check, results)
        passed = all(r.passed for r in results)
        return GateVerdict(passed, results, trust)

    # -------------------------------------------------------- milestone gate
    def milestone_gate(self, milestone: str, cfg: GateConfig,
                       scoped_criteria: list[str]) -> GateVerdict:
        """
        Full milestone gate: everything in task_gate PLUS secrets, benchmarks, determinism,
        and the criteria->evidence mapping. A milestone cannot close with unmapped criteria.
        """
        results = [
            checks.check_build(self.repo, cfg.target_triple),
            checks.check_fmt(self.repo),
            checks.check_clippy(self.repo),
            checks.check_no_stubs(self.repo, cfg.production_globs),
            checks.check_tests(self.repo, cfg.required_tests or None),
            checks.check_secrets(self.repo),
            checks.check_dossier_test_integrity(self.repo),
        ]
        if cfg.run_hotpath_lint:
            results.append(check_hotpath_lint(self.repo,
                                              cfg.lint_hot_globs or None,
                                              cfg.lint_money_globs or None))
        if cfg.run_determinism and cfg.replay_bin:
            results.append(checks.check_determinism(self.repo, cfg.replay_bin, cfg.replay_fixture))
        if cfg.run_bench and cfg.bench_name:
            # §9.5 / criterion 113: a benchmark (hardware-measured latency) is Phase-B-exclusive.
            # It may be certified only on the deployment host. Fail closed otherwise — author
            # the code anywhere, but the microsecond budget is proven on the server.
            pinned = ""
            try:
                pinned = self.store.get_pinned_manifest()
            except Exception:  # noqa: BLE001
                pinned = ""
            phase = check_phase_provenance("bench", cfg.criteria_touched or [103, 109],
                                           cfg.infra_manifest, pinned_sha=pinned)
            results.append(checks.CheckResult(
                "phase_provenance_bench", phase.ok, phase.reason, phase.detail))
            if phase.ok:
                results.append(checks.check_bench(self.repo, cfg.bench_name,
                                                  cfg.bench_budgets_ns))
            # if not on deployment hardware, the bench itself is NOT run and NOT recorded as
            # passing — the phase gate is the failing signal, and it explains why.
        elif cfg.criteria_touched:
            # Non-bench milestones still record their phase for any Phase-B criteria they claim.
            pinned = ""
            try:
                pinned = self.store.get_pinned_manifest()
            except Exception:  # noqa: BLE001
                pinned = ""
            phase = check_phase_provenance("", cfg.criteria_touched, cfg.infra_manifest,
                                           pinned_sha=pinned)
            if not phase.ok:
                results.append(checks.CheckResult(
                    "phase_provenance", False, phase.reason, phase.detail))
        # Dossier-presence gate: a milestone needing a hard component cannot close while
        # that dossier is absent — a loud escalating failure, never a silent skip.
        if cfg.require_dossiers:
            absent = [c for c in cfg.require_dossiers if c in missing_dossiers()]
            results.append(checks.CheckResult(
                "dossiers_present", not absent,
                {"required": cfg.require_dossiers, "absent": absent},
                summary=("all required dossiers present" if not absent
                         else f"MISSING dossiers (author independently, never autogenerate): {absent}")))

        for r in results:
            self._record(milestone, r)

        # criteria mapping: each scoped criterion must map to a passing gate/artifact.
        gate_passed = all(r.passed for r in results)
        for crit in scoped_criteria:
            # a criterion is satisfied only if the gate battery passed; the orchestrator may
            # additionally attach artifact evidence for criteria not covered by a mechanical gate.
            self.store.set_criterion(crit, milestone, evidence=self._summary(results),
                                     satisfied=gate_passed, run_id=self.run_id)
        unmet = self.store.unsatisfied_criteria(milestone, self.run_id)
        passed = gate_passed and not unmet
        return GateVerdict(passed, results,
                           [] if passed else [f"unmet criteria: {unmet}"])

    # --------------------------------------------------------------- helpers
    @staticmethod
    def _summary(results: list[checks.CheckResult]) -> str:
        return "; ".join(f"{r.name}={'ok' if r.passed else 'fail'}" for r in results)

    @staticmethod
    def _trust_check(self_check: Optional[dict], results: list[checks.CheckResult]) -> list[str]:
        """Compare model claims to verified reality -> trust-mismatch signals."""
        if not self_check:
            return []
        mismatches: list[str] = []
        build = next((r for r in results if r.name == "build"), None)
        if self_check.get("compiles_locally") is True and build and not build.passed:
            mismatches.append("claimed compiles_locally=true but build failed")
        test = next((r for r in results if r.name == "test"), None)
        if self_check.get("tests_added") and test and not test.passed:
            mismatches.append("claimed tests_added but test gate failed")
        if self_check.get("determinism_impact") == "none":
            det = next((r for r in results if r.name == "determinism"), None)
            if det and not det.passed:
                mismatches.append("claimed no determinism impact but determinism gate failed")
        return mismatches
