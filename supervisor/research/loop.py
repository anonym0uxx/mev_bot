"""
Standing research loop (post-build) — drives the constitution's reflection -> hypothesis ->
experiment -> admission cycle (§56) and the Continuous-Improvement Mandate (§62).

STATUS: scaffold. The control flow, safety gates, and model interaction are real; the points
that must bind to the *actual bot processes* (the research runner, the frozen evaluator binary)
are marked TODO(live) because those artifacts don't exist until the build produces them.
Two bindings are now REAL and laptop-operational (durable memory, §43/§56.10): reconciled-
outcome ingestion from a trade-JSONL artifact (`ingest_reconciled_outcomes_from_jsonl`) and
hypothesis persistence into the evidence store. Nothing here can promote to live capital —
that is always a human gate.
"""
from __future__ import annotations

import hashlib
import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Optional

from ..core.model_client import ModelClient, SchemaViolation
from ..core.schemas import get_schema
from ..store.evidence import EvidenceStore
from ..console.escalate import EscalationChannel, Escalation


@dataclass
class Hypothesis:
    hypothesis_id: str
    statement: str
    # §56.10: causal mechanism, competing explanations, and the disconfirming evidence
    # sought must survive into the knowledge base — full HYPOTHESIS_SCHEMA carriage.
    causal_mechanism: str
    expected_net_sol_impact: float
    prior_probability: float
    cost_to_test: str
    edge_half_life: str
    competing_explanations: list = field(default_factory=list)
    disconfirming_evidence_sought: str = ""

    def voi_score(self) -> float:
        """Value-of-information rank: expected impact * probability / cost proxy."""
        cost = {"none": 0.1, "low": 1.0, "medium": 3.0, "high": 8.0, "unknown": 5.0}.get(self.cost_to_test, 5.0)
        return (self.expected_net_sol_impact * self.prior_probability) / cost

    def to_record(self) -> dict:
        """HYPOTHESIS_SCHEMA-shaped dict for EvidenceStore.record_hypothesis."""
        return {
            "hypothesis_id": self.hypothesis_id,
            "statement": self.statement,
            "causal_mechanism": self.causal_mechanism,
            "competing_explanations": self.competing_explanations,
            "disconfirming_evidence_sought": self.disconfirming_evidence_sought,
            "expected_net_sol_impact": self.expected_net_sol_impact,
            "prior_probability": self.prior_probability,
            "cost_to_test": self.cost_to_test,
            "edge_half_life": self.edge_half_life,
        }


def ingest_reconciled_outcomes_from_jsonl(store: EvidenceStore, run_id: str,
                                          jsonl_path: str | Path,
                                          report_summary: dict) -> dict:
    """REAL default binding for ResearchAdapters.ingest_reconciled_outcomes (§43).

    Reads a reconciled trade-JSONL artifact plus its report summary dict, persists a
    `reconciled_outcomes` row in the evidence store, and returns the summary the
    hypothesis generator consumes. Laptop-operational: needs only the artifact file,
    no live bot process. Fails with clear errors — never silently.
    """
    p = Path(jsonl_path)
    if not p.is_file():
        raise FileNotFoundError(
            f"reconciled trade JSONL not found: {p} — pass the replay/reconcile output path")
    if not isinstance(report_summary, dict):
        raise ValueError("report_summary must be a dict (the reconcile report's summary block)")
    raw = p.read_bytes()
    digest = hashlib.sha256(raw).hexdigest()
    trades = 0
    net_lamports = 0
    for lineno, line in enumerate(raw.decode("utf-8", errors="replace").splitlines(), 1):
        line = line.strip()
        if not line:
            continue
        try:
            rec = json.loads(line)
        except json.JSONDecodeError as e:
            raise ValueError(f"{p}:{lineno}: invalid trade-JSONL line: {e}") from e
        trades += 1
        v = rec.get("net_lamports", rec.get("pnl_lamports", 0))
        if isinstance(v, (int, float)):
            net_lamports += int(v)
    # The report summary, when present, is authoritative over the line-scan tally.
    net_lamports = int(report_summary.get("net_lamports", net_lamports))
    trades = int(report_summary.get("trades", trades))
    fill_mode = str(report_summary.get("fill_mode", "unknown"))
    evidence_status = str(report_summary.get("evidence_status", "research-artifact"))
    store.record_reconciled_outcome(run_id=run_id, source_path=str(p), digest=digest,
                                    net_lamports=net_lamports, trades=trades,
                                    fill_mode=fill_mode, evidence_status=evidence_status)
    return {"source_path": str(p), "digest": digest, "net_lamports": net_lamports,
            "trades": trades, "fill_mode": fill_mode, "evidence_status": evidence_status,
            "summary": report_summary}


# Injected adapters to the real bot. Remaining TODO(live): the sealed-experiment runner and
# the frozen-evaluator hash reader (those binaries don't exist until the build produces them).
@dataclass
class ResearchAdapters:
    # REAL default available: bind ingest_reconciled_outcomes_from_jsonl via functools.partial
    # (store, run_id, jsonl_path, report_summary) — persists rows in the evidence store.
    ingest_reconciled_outcomes: Callable[[], dict]
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
        # 1) ingest reconciled outcomes (REAL default: ingest_reconciled_outcomes_from_jsonl,
        #    bound by the caller; persists reconciled_outcomes rows in the evidence store)
        outcomes = self.adapters.ingest_reconciled_outcomes()

        # 2) model generates hypotheses (constrained); persisted to the evidence store
        #    (§56.10 — competing explanations and disconfirming evidence survive into the KB)
        hyps = self._generate_hypotheses(outcomes)
        if not hyps:
            return None
        for h in hyps:
            self.store.record_hypothesis(h.to_record(), created_run=self.run_id)

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
        # REAL: the disconfirming result is durable — mark the hypothesis rejected (§56.10).
        self.store.set_inference_state(top.hypothesis_id, "RejectedInference")
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
        # HYPOTHESIS_SCHEMA emits disconfirming_evidence_sought as an array of strings;
        # carry it as a single joined string (the store column is TEXT).
        des = obj.get("disconfirming_evidence_sought", "")
        if isinstance(des, list):
            des = "; ".join(str(x) for x in des)
        comp = obj.get("competing_explanations", [])
        if not isinstance(comp, list):
            comp = [str(comp)]
        return [Hypothesis(
            hypothesis_id=obj["hypothesis_id"],
            statement=obj["statement"],
            causal_mechanism=obj["causal_mechanism"],
            expected_net_sol_impact=obj["expected_net_sol_impact"],
            prior_probability=obj["prior_probability"],
            cost_to_test=obj["cost_to_test"],
            edge_half_life=obj["edge_half_life"],
            competing_explanations=comp,
            disconfirming_evidence_sought=des,
        )]

    def _design_experiment(self, h: Hypothesis) -> dict:
        # TODO(live): expand into a full sealed experiment spec the runner understands
        return {"hypothesis_id": h.hypothesis_id, "statement": h.statement, "sealed": True}
