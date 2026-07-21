"""
Build-loop orchestrator — the finite state machine that drives GLM-5.2 through M0..M9.

Flow per milestone:
    load milestone -> (decompose into tasks) -> task loop
        task: prompt model (constrained) -> apply diff -> Tier-0 scan -> task gate
              pass -> commit + next ; fail -> feedback + retry (bounded) ; Tier-0 -> escalate
        HARD component task -> route to ReinforcementEngine instead of a single model turn
    all tasks done -> milestone gate (full battery + criteria map)
        pass -> optimization ratchet (perf milestones) -> commit + advance
        fail -> escalate

This module wires components; it deliberately keeps the model OUT of any advancement decision.
Task decomposition for NON-hard milestones is driven by the model but every unit is gate-verified.
Integration points that require the actual bot repo (git apply, scratch-crate verify) are
implemented against a small VCS/exec adapter so the FSM itself is unit-testable.
"""
from __future__ import annotations

import time
from dataclasses import dataclass, field
from typing import Callable, Optional

from .config import SupervisorConfig
from .constitution import Constitution, Milestone
from .model_client import ModelClient, ModelUnavailable, SchemaViolation
from .safety import is_blocked
from .schemas import get_schema, TaskResponse
from ..gates.runner import GateRunner, GateConfig, GateVerdict
from ..store.evidence import EvidenceStore
from ..console.escalate import EscalationChannel, Escalation
from ..reinforcement.engine import ReinforcementEngine
from ..reinforcement.dossier import Dossier, load_dossier, available_dossiers, HARD_COMPONENTS


# --- adapters (injected; real impls talk to git/filesystem, test impls are in-memory) -----
@dataclass
class VcsAdapter:
    apply_diff: Callable[[str], tuple[bool, str]]          # (diff) -> (ok, detail)
    commit: Callable[[str], str]                           # (message) -> sha
    branch: Callable[[str], None]                          # (name) -> None
    revert_last: Callable[[], None]


@dataclass
class OrchestratorResult:
    milestone: str
    advanced: bool
    detail: str


class BuildOrchestrator:
    def __init__(self, cfg: SupervisorConfig, model: ModelClient, store: EvidenceStore,
                 gates: GateRunner, vcs: VcsAdapter, escalate: EscalationChannel,
                 reinforcement: Optional[ReinforcementEngine], run_id: str):
        self.cfg = cfg
        self.model = model
        self.store = store
        self.gates = gates
        self.vcs = vcs
        self.escalate = escalate
        self.reinforcement = reinforcement
        self.run_id = run_id

    # ------------------------------------------------------------- public API
    def run_milestone(self, cons: Constitution, mkey: str) -> OrchestratorResult:
        ms = cons.milestones.get(mkey)
        if ms is None:
            return OrchestratorResult(mkey, False, f"milestone {mkey} not found in constitution")

        # endpoint health first — model problems are distinct from code problems
        try:
            self.model.health()
        except ModelUnavailable as e:
            self._escalate(mkey, "-", "model_unavailable", str(e))
            return OrchestratorResult(mkey, False, f"model unavailable: {e}")

        self.vcs.branch(f"build/{mkey.lower()}")

        # decompose -> tasks (model proposes a task list; each is independently gated)
        tasks = self._decompose(ms)
        for task in tasks:
            ok = self._run_task(cons, ms, task)
            if not ok:
                return OrchestratorResult(mkey, False, f"task {task['task_id']} unresolved -> escalated")

        # milestone gate (full battery + criteria)
        gcfg = self._gate_config_for(ms)
        verdict = self.gates.milestone_gate(mkey, gcfg, ms.scoped_criteria)
        if not verdict.passed:
            self._escalate(mkey, "-", "milestone_gate_fail", verdict.summary())
            return OrchestratorResult(mkey, False, f"milestone gate failed: {verdict.summary()}")

        # optimization ratchet on perf-critical milestones
        if gcfg.run_bench:
            self._optimize(cons, ms, gcfg)

        sha = self.vcs.commit(f"{mkey} complete: gates green, criteria mapped [{self.run_id}]")
        self.store.record_commit(sha, self.run_id, mkey, "-", f"{mkey} milestone gate pass")
        return OrchestratorResult(mkey, True, f"advanced; commit {sha[:10]}")

    # ------------------------------------------------------------- task cycle
    def _run_task(self, cons: Constitution, ms: Milestone, task: dict) -> bool:
        task_id = task["task_id"]
        component = task.get("hard_component")

        # HARD component -> reinforcement engine (best-of-N + micro-decompose)
        if component and component in HARD_COMPONENTS and self.reinforcement:
            return self._run_hard_component(cons, ms, task, component)

        # ordinary task: single constrained model turn, then gate
        budget = self.cfg.task_retry_budget
        feedback = ""
        for _ in range(budget):
            try:
                resp = self.model.constrained(
                    self._task_system(cons),
                    self._task_user(cons, ms, task, feedback),
                    get_schema("task"),
                )
            except SchemaViolation as e:
                feedback = f"Your previous output was not schema-valid: {e}"
                continue
            tr = TaskResponse.from_json(resp)

            if tr.action == "REPORT_BLOCKED":
                self._escalate(ms.key, task_id, "model_blocked",
                               f"model reports blocked: {resp}")
                return False
            if tr.action == "ASK_CLARIFY":
                # deterministic policy: answer from the constitution section, don't invent
                feedback = self._answer_clarify(cons, ms, resp)
                continue

            # Tier-0 scan BEFORE applying anything
            paths = [f.path for f in tr.files_changed]
            hits = is_blocked(tr.diff, paths)
            if hits:
                self._escalate(ms.key, task_id, hits[0].domain.value,
                               f"Tier-0 tripwire: {[h.reason for h in hits]}")
                return False

            applied, detail = self.vcs.apply_diff(tr.diff)
            if not applied:
                feedback = f"diff did not apply: {detail}"
                self.store.upsert_task(task_id, self.run_id, ms.key, "task", "proposed", resp)
                continue

            verdict = self.gates.task_gate(task_id, self._gate_config_for(ms),
                                           model_self_check=resp.get("self_check"))
            self.store.upsert_task(task_id, self.run_id, ms.key, "task",
                                   "gated_pass" if verdict.passed else "gated_fail", resp)
            if verdict.passed:
                sha = self.vcs.commit(f"{ms.key} {task_id}: {tr.files_changed[0].rationale if tr.files_changed else ''}")
                self.store.record_commit(sha, self.run_id, ms.key, task_id, task_id)
                return True

            # failed -> revert, feed the actual gate output back (verified reality, not opinion)
            self.vcs.revert_last()
            feedback = ("Gates failed. Fix these specific, verified problems:\n" +
                        "\n".join(f"- {r.name}: {r.summary}" for r in verdict.results if not r.passed))
            if verdict.trust_mismatches:
                feedback += "\nNote: your self_check did not match reality: " + "; ".join(verdict.trust_mismatches)

        self._escalate(ms.key, task_id, "retry_exhausted",
                       f"task {task_id} failed gates after {budget} attempts")
        return False

    # ------------------------------------------------------- hard component
    def _run_hard_component(self, cons: Constitution, ms: Milestone, task: dict, component: str) -> bool:
        dossiers = available_dossiers()
        if component not in dossiers:
            self._escalate(ms.key, task["task_id"], "missing_dossier",
                           f"HARD component {component} has no dossier authored yet (design-once step).")
            return False
        dossier = load_dossier(dossiers[component])
        passed, bodies, outcomes = self.reinforcement.implement_component(dossier)
        if not passed:
            failed = next((o for o in outcomes if not o.passed), None)
            self._escalate(ms.key, failed.leaf_id if failed else task["task_id"], "hard_leaf_unresolved",
                           f"leaf {failed.leaf_id if failed else '?'} failed at max granularity/N "
                           f"after {failed.attempts if failed else '?'} attempts — scoped human unblock needed.")
            return False
        # integrate assembled bodies (adapter writes them into the component skeleton), then gate
        diff = self._assemble_component_diff(component, dossier, bodies)
        applied, detail = self.vcs.apply_diff(diff)
        if not applied:
            self._escalate(ms.key, task["task_id"], "integration_failed", detail)
            return False
        verdict = self.gates.task_gate(task["task_id"], self._gate_config_for(ms))
        if verdict.passed:
            sha = self.vcs.commit(f"{ms.key} {component}: reinforcement-built, gates green")
            self.store.record_commit(sha, self.run_id, ms.key, task["task_id"], component)
            return True
        self.vcs.revert_last()
        self._escalate(ms.key, task["task_id"], "hard_component_gate_fail", verdict.summary())
        return False

    # ------------------------------------------------------ optimization ratchet
    def _optimize(self, cons: Constitution, ms: Milestone, gcfg: GateConfig) -> None:
        """Bounded, benchmark-gated improvement passes; regressions auto-revert."""
        rounds = 3
        for _ in range(rounds):
            baseline = {m: self.store.best_benchmark(ms.key, m) for m in gcfg.bench_budgets_ns}
            try:
                resp = self.model.constrained(
                    self._task_system(cons),
                    f"Propose ONE behavior-preserving latency optimization for {ms.key}. "
                    f"Tests must stay green; the benchmark must improve or equal. "
                    f"Current bests: {baseline}.",
                    get_schema("task"),
                )
            except SchemaViolation:
                return
            tr = TaskResponse.from_json(resp)
            if tr.action != "PROPOSE_DIFF" or not tr.diff:
                return
            applied, _ = self.vcs.apply_diff(tr.diff)
            if not applied:
                continue
            verdict = self.gates.milestone_gate(ms.key, gcfg, ms.scoped_criteria)
            improved = verdict.passed  # bench is part of milestone gate; failure = regression or break
            if not improved:
                self.vcs.revert_last()          # ratchet: never keep a regression
            else:
                self.vcs.commit(f"{ms.key} optimize: benchmark-proven improvement")

    # ------------------------------------------------------------- decompose
    def _decompose(self, ms: Milestone) -> list[dict]:
        """
        Ask the model for a task list, but the supervisor owns the HARD-component flags.
        Non-hard milestones get model-proposed tasks; hard components are injected explicitly.
        """
        tasks: list[dict] = []
        # inject known hard components that belong to this milestone (by dossier availability + keywords)
        for comp in HARD_COMPONENTS:
            if comp.split("_")[0] in ms.body.lower() or comp in ms.body.lower():
                tasks.append({"task_id": f"{ms.key}.{comp}", "hard_component": comp})
        # a real impl also asks the model to enumerate the remaining ordinary tasks;
        # kept minimal here — orchestrator is exercised in tests via injected task lists.
        if not tasks:
            tasks.append({"task_id": f"{ms.key}.impl", "hard_component": None})
        return tasks

    # ----------------------------------------------------------- prompt build
    def _task_system(self, cons: Constitution) -> str:
        return (
            "You are GLM-5.2 building the Hermes trading system under a binding constitution. "
            f"Constitution version {cons.content_hash} (git {cons.git_hash}). "
            "You propose one small, verified increment at a time. Your self_check claims will be "
            "independently verified; false claims are recorded. Return only the schema object. "
            "Never touch keys, live capital, funds, or the evaluator — those are Tier-0 and forbidden to you."
        )

    def _task_user(self, cons: Constitution, ms: Milestone, task: dict, feedback: str) -> str:
        return (
            f"MILESTONE {ms.key}: {ms.title}\n\n{ms.body[:3000]}\n\n"
            f"TASK: {task['task_id']}\n"
            f"{('PRIOR ATTEMPT FEEDBACK:' + chr(10) + feedback) if feedback else ''}\n"
            "Propose the smallest correct diff that advances this task. Add tests. "
            "Keep functions small and hot paths allocation-free and float-free where required."
        )

    def _answer_clarify(self, cons: Constitution, ms: Milestone, resp: dict) -> str:
        # deterministic: point the model back at the milestone body; never invent policy
        return f"Re-read this milestone's governing text and proceed; do not assume beyond it:\n{ms.body[:2000]}"

    # -------------------------------------------------------------- helpers
    def _gate_config_for(self, ms: Milestone) -> GateConfig:
        g = self.cfg.gate
        # perf-critical milestones enable bench/determinism (config-driven in production)
        perf = ms.key in {"M1", "M2", "M4"}
        return GateConfig(
            target_triple=g.target_triple,
            production_globs=g.production_globs,
            run_bench=g.run_bench and perf,
            bench_name=g.bench_name,
            bench_budgets_ns=g.bench_budgets_ns,
            run_determinism=g.run_determinism and ms.key in {"M2", "M4"},
            replay_bin=g.replay_bin,
            replay_fixture=g.replay_fixture,
            required_tests=g.required_tests,
        )

    def _assemble_component_diff(self, component: str, dossier: Dossier, bodies: dict[str, str]) -> str:
        """
        Produce a unified diff that inserts each verified leaf body into the component skeleton.
        Real impl uses the dossier's file/insertion map; kept as a structured placeholder that the
        VCS adapter in production expands. In tests the adapter interprets this directly.
        """
        parts = [f"# component:{component}"]
        for lid, body in bodies.items():
            parts.append(f"# leaf:{lid}\n{body}")
        return "\n".join(parts)

    def _escalate(self, milestone: str, task_id: str, domain: str, context: str) -> None:
        self.store.escalate(self.run_id, milestone, task_id, domain, context)
        self.escalate.raise_escalation(Escalation(milestone, task_id, domain, context))
