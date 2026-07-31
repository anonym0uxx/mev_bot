"""
Gate runner — composes individual checks into task-level and milestone-level gates,
records every result to the evidence store, and produces the pass/fail verdict that is
the ONLY thing allowed to advance loop state.

The model's self_check claims are compared against verified reality; mismatches are
recorded as trust signals (used by the orchestrator to shrink task size for over-claiming).
"""
from __future__ import annotations

import hashlib
import os
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


# --------------------------------------------------------------------- criterion bindings
# A criterion is satisfied ONLY by a typed binding of exactly one kind:
#   MECHANICAL — a named check in the battery that causally verifies the property.
#   ARTIFACT   — a specific study or doc, pinned by content hash, that argues the property.
#   OPERATOR   — a human attestation recorded under the b5a3afc TTY guard.
# A criterion with no binding of any type is UNVERIFIED and BLOCKS certification.
# An ARTIFACT binding whose hash no longer matches is UNVERIFIED, not satisfied.
#
# The constitution declares 18 acceptance criteria. Most are NOT code properties
# (capital allocation, key custody, signal-horizon matching) and have no mechanical
# check. They require ARTIFACT or OPERATOR bindings. This mapping is the ONLY
# authority for criterion satisfaction — the blanket `gate_passed` loop is dead.
@dataclass
class CriterionBinding:
    criterion: int
    binding_type: str       # 'MECHANICAL' | 'ARTIFACT' | 'OPERATOR' | 'UNVERIFIED'
    check_name: str = ""    # for MECHANICAL: the CheckResult.name that verifies it
    artifact_path: str = "" # for ARTIFACT: repo-relative path to the study/doc
    artifact_sha256: str = ""  # for ARTIFACT: pinned content hash at binding time
    operator_note: str = "" # for OPERATOR: what the operator attested
    note: str = ""          # free-form explanation

    def __repr__(self) -> str:
        if self.binding_type == "MECHANICAL":
            return f"[{self.criterion}] MECHANICAL -> {self.check_name}"
        elif self.binding_type == "ARTIFACT":
            return f"[{self.criterion}] ARTIFACT -> {self.artifact_path}@{self.artifact_sha256[:12]}"
        elif self.binding_type == "OPERATOR":
            return f"[{self.criterion}] OPERATOR -> {self.operator_note}"
        return f"[{self.criterion}] UNVERIFIED"


def _file_sha256(path: str, repo: str = "") -> str:
    """Content hash of a repo-relative file. Returns '' if the file is missing."""
    full = os.path.join(repo, path) if repo else path
    try:
        h = hashlib.sha256()
        with open(full, "rb") as f:
            for chunk in iter(lambda: f.read(8192), b""):
                h.update(chunk)
        return h.hexdigest()
    except (OSError, FileNotFoundError):
        return ""


# The authoritative criterion binding table. Every criterion the constitution
# declares MUST appear here. UNVERIFIED means: no binding exists, and the
# criterion BLOCKS certification until one is established.
CRITERION_BINDINGS: dict[int, CriterionBinding] = {
    52:  CriterionBinding(52,  "UNVERIFIED", note="key-custody election — operator process, not code"),
    69:  CriterionBinding(69,  "UNVERIFIED", note="native Windows / no WSL dependency — requires artifact study"),
    81:  CriterionBinding(81,  "UNVERIFIED", note="taxonomy forward-only — requires artifact study"),
    85:  CriterionBinding(85,  "UNVERIFIED", note="capital allocation — policy, not code"),
    96:  CriterionBinding(96,  "UNVERIFIED", note="signal-horizon matching law — requires artifact study"),
    97:  CriterionBinding(97,  "UNVERIFIED", note="scalp-readiness — requires artifact study"),
    98:  CriterionBinding(98,  "UNVERIFIED", note="no-edge rescoping — requires artifact study"),
    99:  CriterionBinding(99,  "UNVERIFIED", note="memory soak / no unbounded growth — check_memory_soak exists but soak_bin not built"),
    102: CriterionBinding(102, "UNVERIFIED", note="safety constants static — requires artifact study"),
    103: CriterionBinding(103, "UNVERIFIED", note="latency budgets — check_bench exists but bench_name empty (Shape 3)"),
    109: CriterionBinding(109, "MECHANICAL", check_name="hotpath_lint",
                          note="§24 Rust perf law — Phase-A clauses enforced by lint; deploy clauses require Phase-B"),
    110: CriterionBinding(110, "UNVERIFIED", note="attention spend source — policy, not code"),
    111: CriterionBinding(111, "UNVERIFIED", note="amendment subsystem — requires artifact study"),
    112: CriterionBinding(112, "UNVERIFIED", note="size-viability band — requires artifact study"),
    113: CriterionBinding(113, "UNVERIFIED",
                          note="two-phase build boundary — check_phase_provenance exists but is inside the bench block (bench_name='' → skipped)"),
    114: CriterionBinding(114, "MECHANICAL", check_name="build",
                          note="build execution surfaces — cargo build compiles all 26 workspace members"),
    115: CriterionBinding(115, "MECHANICAL", check_name="test",
                          note="pq-narrative property-tested — required_tests now lists dossier_narrative_nv_* tests; check_tests verifies they ran"),
    116: CriterionBinding(116, "MECHANICAL", check_name="test",
                          note="pq-watchlist never-idle — required_tests now lists dossier_rank_wr_* tests; check_tests verifies they ran"),
}


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
        elif cfg.run_determinism:
            # Shape 3 fail-closed: declared run_determinism: true but replay_bin is empty.
            # The guard would silently no-op; instead emit a failing result so the gate
            # cannot pass while a declared check is absent.
            results.append(checks.CheckResult(
                "determinism", False,
                {"declared": True, "replay_bin": ""},
                "declared run_determinism: true but replay_bin is empty — check is a silent no-op"))

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
        elif cfg.run_bench:
            # Shape 3 fail-closed: declared run_bench: true but bench_name is empty.
            results.append(checks.CheckResult(
                "bench", False,
                {"declared": True, "bench_name": ""},
                "declared run_bench: true but bench_name is empty — check is a silent no-op"))
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

        # --- Typed criterion satisfaction (replaces the blanket loop) ---
        # OLD (dead): for crit in scoped_criteria: set_criterion(satisfied=gate_passed)
        #             unmet = unsatisfied_criteria(...); passed = gate_passed and not unmet
        # The `not unmet` conjunct was dead code: if gate_passed=True, every criterion was
        # set satisfied=1, so unmet was empty BY CONSTRUCTION. The conjunct could never
        # change the verdict. It reduced exactly to `gate_passed`.
        #
        # NEW: each criterion is evaluated against its typed binding in CRITERION_BINDINGS.
        # MECHANICAL: satisfied only if the named check exists in results AND passed.
        # ARTIFACT:   satisfied only if the pinned sha256 still matches the file on disk.
        # OPERATOR:   satisfied (operator has attested; recorded separately).
        # UNVERIFIED: blocks certification — no binding of any type exists.
        gate_passed = all(r.passed for r in results)
        result_by_name = {r.name: r for r in results}
        unmet: list[str] = []
        for crit_id in scoped_criteria:
            binding = CRITERION_BINDINGS.get(crit_id)
            if binding is None:
                unmet.append(f"criterion {crit_id}: not in CRITERION_BINDINGS — UNVERIFIED")
                self.store.set_criterion(str(crit_id), milestone,
                                        evidence=f"UNVERIFIED: no binding defined",
                                        satisfied=False, run_id=self.run_id)
                continue
            if binding.binding_type == "MECHANICAL":
                check = result_by_name.get(binding.check_name)
                if check is None:
                    unmet.append(f"criterion {crit_id}: MECHANICAL binding -> {binding.check_name} "
                                f"not in results (check did not run)")
                    self.store.set_criterion(str(crit_id), milestone,
                                            evidence=f"MECHANICAL:{binding.check_name} absent",
                                            satisfied=False, run_id=self.run_id)
                elif not check.passed:
                    unmet.append(f"criterion {crit_id}: MECHANICAL binding -> {binding.check_name} FAILED")
                    self.store.set_criterion(str(crit_id), milestone,
                                            evidence=f"MECHANICAL:{binding.check_name} failed",
                                            satisfied=False, run_id=self.run_id)
                else:
                    self.store.set_criterion(str(crit_id), milestone,
                                            evidence=f"MECHANICAL:{binding.check_name} passed",
                                            satisfied=True, run_id=self.run_id)
            elif binding.binding_type == "ARTIFACT":
                current_hash = _file_sha256(binding.artifact_path, self.repo)
                if not current_hash or current_hash != binding.artifact_sha256:
                    unmet.append(f"criterion {crit_id}: ARTIFACT binding -> {binding.artifact_path} "
                                f"hash mismatch or file missing")
                    self.store.set_criterion(str(crit_id), milestone,
                                            evidence=f"ARTIFACT:{binding.artifact_path} hash-stale",
                                            satisfied=False, run_id=self.run_id)
                else:
                    self.store.set_criterion(str(crit_id), milestone,
                                            evidence=f"ARTIFACT:{binding.artifact_path} hash-verified",
                                            satisfied=True, run_id=self.run_id)
            elif binding.binding_type == "OPERATOR":
                # Operator attestations are recorded separately; if present, the criterion
                # is satisfied. This binding type marks the criterion as operator-verified.
                self.store.set_criterion(str(crit_id), milestone,
                                        evidence=f"OPERATOR:{binding.operator_note}",
                                        satisfied=True, run_id=self.run_id)
            else:  # UNVERIFIED
                unmet.append(f"criterion {crit_id}: UNVERIFIED — {binding.note}")
                self.store.set_criterion(str(crit_id), milestone,
                                        evidence=f"UNVERIFIED:{binding.note}",
                                        satisfied=False, run_id=self.run_id)

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
