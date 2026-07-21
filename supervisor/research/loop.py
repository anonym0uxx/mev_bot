"""
Standing research loop (post-build) — drives the constitution's reflection -> hypothesis ->
experiment -> admission cycle (§56) and the Continuous-Improvement Mandate (§62).

STATUS: scaffold. The control flow, safety gates, and model interaction are real; the points
that must bind to the *actual bot processes* (the research runner, the frozen evaluator binary,
the QuantMemoryStore) are marked TODO(live) because those artifacts don't exist until the build
produces them. Nothing here can promote to live capital — that is always a human gate.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Callable, Optional

from ..core.model_client import ModelClient, SchemaViolation
from ..core.schemas import get_schema
from ..store.evidence import EvidenceStore
from ..console.escalate import EscalationChannel, Escalation


@dataclass
class Hypothesis:
    hypothesis_id: str
    statement: str
    expected_net_sol_impact: float
    prior_probability: float
    cost_to_test: str
    edge_half_life: str

    def voi_score(self) -> float:
        """Value-of-information rank: expected impact * probability / cost proxy."""
        cost = {"none": 0.1, "low": 1.0, "medium": 3.0, "high": 8.0, "unknown": 5.0}.get(self.cost_to_test, 5.0)
        return (self.expected_net_sol_impact * self.prior_probability) / cost


# Injected adapters to the real bot (all TODO(live) until the build produces them)
@dataclass
class ResearchAdapters:
    ingest_reconciled_outcomes: Callable[[], dict]        # TODO(live): read QuantMemoryStore
    seal_and_run_experiment: Callable[[dict], dict]       # TODO(live): pq-research-runner
    evaluator_hash: Callable[[], str]                     # TODO(live): read frozen evaluator hash
    expected_evaluator_hash: str                          # pinned; mismatch => refuse results


class ResearchLoop:
    def __init__(self, model: ModelClient, store: EvidenceStore,
                 escalate: EscalationChannel, adapters: ResearchAdapters, run_id: str):
        self.model = model
        self.store = store
        self.escalate = escalate
        self.adapters = adapters
        self.run_id = run_id

    def cycle(self) -> Optional[dict]:
        """One research cycle. Returns a shadow-candidate proposal or None."""
        # 1) ingest reconciled outcomes (TODO(live))
        outcomes = self.adapters.ingest_reconciled_outcomes()

        # 2) model generates hypotheses (constrained)
        hyps = self._generate_hypotheses(outcomes)
        if not hyps:
            return None

        # 3) VOI rank (deterministic, supervisor-owned)
        hyps.sort(key=lambda h: h.voi_score(), reverse=True)
        top = hyps[0]

        # 4) design + seal + run experiment (TODO(live) runner)
        experiment = self._design_experiment(top)
        results = self.adapters.seal_and_run_experiment(experiment)

        # 5) verify frozen-evaluator integrity BEFORE trusting any grade (§44)
        if self.adapters.evaluator_hash() != self.adapters.expected_evaluator_hash:
            self.escalate.raise_escalation(Escalation(
                "RESEARCH", top.hypothesis_id, "evaluator_release",
                "Frozen evaluator hash mismatch — refusing to accept experiment grades."))
            return None

        # 6) pass -> propose SHADOW candidate; live promotion is ALWAYS a human gate
        if results.get("passed"):
            proposal = {"hypothesis": top.hypothesis_id, "stage": "SHADOW_CANDIDATE",
                        "results": results}
            self.escalate.raise_escalation(Escalation(
                "RESEARCH", top.hypothesis_id, "promotion_to_live",
                f"Shadow-validated candidate ready. Human gate required before ANY live capital.\n{proposal}"))
            return proposal

        # 7) fail -> knowledge base, redirect (never terminal; §62 obligates continued search)
        # TODO(live): persist disconfirming result to QuantMemoryStore
        return None

    def _generate_hypotheses(self, outcomes: dict) -> list[Hypothesis]:
        try:
            obj = self.model.constrained(
                "You are the research reflection engine. From reconciled trade outcomes, propose ONE "
                "falsifiable hypothesis to improve net-SOL edge. You never authorize trades; you only "
                "propose. Return the hypothesis schema object.",
                f"Reconciled outcomes summary: {outcomes}",
                get_schema("hypothesis"),
            )
        except SchemaViolation:
            return []
        return [Hypothesis(
            hypothesis_id=obj["hypothesis_id"],
            statement=obj["statement"],
            expected_net_sol_impact=obj["expected_net_sol_impact"],
            prior_probability=obj["prior_probability"],
            cost_to_test=obj["cost_to_test"],
            edge_half_life=obj["edge_half_life"],
        )]

    def _design_experiment(self, h: Hypothesis) -> dict:
        # TODO(live): expand into a full sealed experiment spec the runner understands
        return {"hypothesis_id": h.hypothesis_id, "statement": h.statement, "sealed": True}
