"""
RootCauseEngine — §56.5 deterministic root-cause classification and distribution aggregation.

Constitution §56.5 (verbatim taxonomy): "RootCauseEngine — classifications include: ENTRY_LATE,
EXIT_LATE, LIQUIDITY_COLLAPSE, CREATOR_RUG, CREATOR_DISTRIBUTION, CLUSTER_DISTRIBUTION,
MIGRATION_TIMING, PRIORITY_FEE, JITO_MISS, NOZOMI_MISS, HELIUS_SENDER_MISS, LEADER_TIMING,
RPC_DELAY, SOURCE_LATENCY, SOURCE_GAP, SOURCE_SUNSET_TRANSITION, FILTER_COVERAGE_MISS,
PROVIDER_QUOTA, DECODE_LATENCY, DECISION_LATENCY, TRANSACTION_BUILD_LATENCY, SIGNING_LATENCY,
ROUTE_FAILURE, SLIPPAGE, PRICE_IMPACT, BAD_FEATURE, BAD_THRESHOLD, BAD_ENTRY_MODE,
BAD_SETUP_CLASSIFICATION, BAD_RISK_CLASSIFICATION, BAD_CREATOR_CLASSIFICATION,
BAD_CLUSTER_CLASSIFICATION, SOCIAL_FALSE_POSITIVE, ATTENTION_EXHAUSTION,
THESIS_INVALIDATION_TOO_LATE, THESIS_INVALIDATION_TOO_EARLY, MARKET_REGIME, META_ROTATION_LAG,
CAPITAL_MISALLOCATION, SCALP_HORIZON_MISS, SCALP_COST_FLOOR_BREACH, COPY_BAIT_LOSS,
SELF_DEALING_SIGNAL_FOLLOWED, GUARD_ABORT, ACCOUNT_CONSTRUCTION_ERROR, PROGRAM_VERSION_DRIFT,
UNKNOWN_PROGRAM_ERROR, UNSELLABLE, TERMINAL_LOSS, UNKNOWN. Produce distributions, not anecdotes;
Hermes receives aggregate evidence and linked records."

This module is the supervisor-side, deterministic realisation of that taxonomy: a pure classifier
that maps a single evidence row (REJECT code, ExitReason, gate failure, program error, or
submission miss) to exactly one root-cause class, plus an aggregator that rolls a batch of rows
into a distribution for the reflection report. It is deterministic and wall-clock-free — the same
evidence rows always yield the same distribution — so it is safe to test and to replay.
"""
from __future__ import annotations

from collections import Counter
from dataclasses import dataclass, field
from typing import Any, Optional


# §56.5 taxonomy — the closed set of root-cause classes, in constitution order.
ROOT_CAUSE_CLASSES: tuple[str, ...] = (
    "ENTRY_LATE", "EXIT_LATE", "LIQUIDITY_COLLAPSE", "CREATOR_RUG", "CREATOR_DISTRIBUTION",
    "CLUSTER_DISTRIBUTION", "MIGRATION_TIMING", "PRIORITY_FEE", "JITO_MISS", "NOZOMI_MISS",
    "HELIUS_SENDER_MISS", "LEADER_TIMING", "RPC_DELAY", "SOURCE_LATENCY", "SOURCE_GAP",
    "SOURCE_SUNSET_TRANSITION", "FILTER_COVERAGE_MISS", "PROVIDER_QUOTA", "DECODE_LATENCY",
    "DECISION_LATENCY", "TRANSACTION_BUILD_LATENCY", "SIGNING_LATENCY", "ROUTE_FAILURE",
    "SLIPPAGE", "PRICE_IMPACT", "BAD_FEATURE", "BAD_THRESHOLD", "BAD_ENTRY_MODE",
    "BAD_SETUP_CLASSIFICATION", "BAD_RISK_CLASSIFICATION", "BAD_CREATOR_CLASSIFICATION",
    "BAD_CLUSTER_CLASSIFICATION", "SOCIAL_FALSE_POSITIVE", "ATTENTION_EXHAUSTION",
    "THESIS_INVALIDATION_TOO_LATE", "THESIS_INVALIDATION_TOO_EARLY", "MARKET_REGIME",
    "META_ROTATION_LAG", "CAPITAL_MISALLOCATION", "SCALP_HORIZON_MISS", "SCALP_COST_FLOOR_BREACH",
    "COPY_BAIT_LOSS", "SELF_DEALING_SIGNAL_FOLLOWED", "GUARD_ABORT", "ACCOUNT_CONSTRUCTION_ERROR",
    "PROGRAM_VERSION_DRIFT", "UNKNOWN_PROGRAM_ERROR", "UNSELLABLE", "TERMINAL_LOSS", "UNKNOWN",
)
_ROOT_CAUSE_SET = frozenset(ROOT_CAUSE_CLASSES)

# ---- deterministic evidence → root-cause maps -----------------------------------------------
# Each map keys off one canonical evidence field; the classifier applies them in a fixed priority
# order (below) so a row carrying several fields resolves deterministically to one class.

# ExitReason (rust `enum ExitReason`) → §56.5 class. Profitable, non-defect exits (TakeProfit*)
# do not carry a loss root cause and resolve to UNKNOWN unless the row says the exit was late.
_EXIT_REASON_MAP: dict[str, str] = {
    "RugPrecursor": "CREATOR_RUG",
    "LiquidityAbort": "LIQUIDITY_COLLAPSE",
    "HardStop": "EXIT_LATE",
    "StopLoss": "EXIT_LATE",
    "TrailingStop": "EXIT_LATE",
    "TimeStop": "SCALP_HORIZON_MISS",
    "ForceClose": "GUARD_ABORT",
    "ThesisInvalidation": "THESIS_INVALIDATION_TOO_LATE",
    "TakeProfit": "UNKNOWN",
    "TakeProfitLadder": "UNKNOWN",
    "Manual": "UNKNOWN",
}

# Reject (rust `enum Reject`) → §56.5 class. The pure-governance admission rejections
# (MissingCausalHypothesis, MissingExperiment, BaselineNotDefeated, NoPromotedPolicy) are
# working-as-designed gate refusals, not trade-outcome root causes, and map to GUARD_ABORT.
_REJECT_MAP: dict[str, str] = {
    "EconomicallyUnviable": "SCALP_COST_FLOOR_BREACH",
    "BelowMinOut": "SLIPPAGE",
    "Unsellable": "UNSELLABLE",
    "SigningDenied": "SIGNING_LATENCY",
    "ExceedsDaily": "CAPITAL_MISALLOCATION",
    "ExceedsLifetime": "CAPITAL_MISALLOCATION",
    "ExceedsPerTrade": "CAPITAL_MISALLOCATION",
    "LossTriggered": "MARKET_REGIME",
    "SocialLed": "SOCIAL_FALSE_POSITIVE",
    "NeedsOnchainConfirmation": "SOURCE_GAP",
    "NoMeasurement": "GUARD_ABORT",
    "NoNumericConfirmation": "GUARD_ABORT",
    "MissingCausalHypothesis": "GUARD_ABORT",
    "MissingExperiment": "GUARD_ABORT",
    "BaselineNotDefeated": "GUARD_ABORT",
    "NoPromotedPolicy": "GUARD_ABORT",
}

# Decoded program-error class (§36 failure taxonomy) → §56.5 class.
_PROGRAM_ERROR_MAP: dict[str, str] = {
    "account_construction": "ACCOUNT_CONSTRUCTION_ERROR",
    "version_drift": "PROGRAM_VERSION_DRIFT",
    "slippage_exceeded": "SLIPPAGE",
    "route_failure": "ROUTE_FAILURE",
    "unknown": "UNKNOWN_PROGRAM_ERROR",
}

# Submission-surface miss → §56.5 class.
_SUBMISSION_MISS_MAP: dict[str, str] = {
    "jito": "JITO_MISS",
    "nozomi": "NOZOMI_MISS",
    "helius_sender": "HELIUS_SENDER_MISS",
    "leader": "LEADER_TIMING",
    "rpc": "RPC_DELAY",
}

# Hot-path stage whose measured latency blew the budget → §56.5 latency class.
_LATENCY_STAGE_MAP: dict[str, str] = {
    "decode": "DECODE_LATENCY",
    "decision": "DECISION_LATENCY",
    "build": "TRANSACTION_BUILD_LATENCY",
    "signing": "SIGNING_LATENCY",
    "source": "SOURCE_LATENCY",
}

# CI/build gate that failed → nearest §56.5 class (build/governance evidence, not trade outcome).
_GATE_MAP: dict[str, str] = {
    "no-stubs": "BAD_FEATURE",
    "hot-path-lint": "DECISION_LATENCY",
    "determinism": "BAD_FEATURE",
    "missing-dossiers": "BAD_FEATURE",
    "criteria": "BAD_THRESHOLD",
}


@dataclass
class Classification:
    """One evidence row's deterministic classification and why it resolved that way."""
    root_cause: str
    matched_field: str          # which evidence field decided it (audit trail)
    evidence_ref: str = ""      # linked source record id, when supplied


def classify(evidence: dict[str, Any]) -> Classification:
    """Map ONE journal/exit/gate evidence row to exactly one §56.5 root-cause class.

    Deterministic and priority-ordered — a row carrying several signals always resolves the same
    way. The priority reflects specificity (an explicit pre-classification first, then the most
    proximate mechanical cause, down to the coarse fallbacks):

      1. explicit `root_cause` (already classified upstream; validated against the taxonomy)
      2. decoded `program_error` (§36 failure taxonomy)
      3. `submission_miss` (which landing surface missed)
      4. `latency_stage` (which hot-path stage blew the criterion-103 budget)
      5. `reject_code`  (rust `enum Reject`)
      6. `exit_reason`  (rust `enum ExitReason`), refined by outcome sign / lane
      7. `gate`         (CI/build/governance gate failure)
      8. `terminal` / `unsellable` outcome flags
      9. UNKNOWN (honest fallback — never invented)

    Recognised extra fields that refine an exit: `net_lamports` (sign), `lane` ('scalp' routes an
    ambiguous TimeStop to SCALP_HORIZON_MISS), and `forfeited_upside` (a ThesisInvalidation that
    cut a still-running winner is TOO_EARLY rather than TOO_LATE).
    """
    # 1) explicit pre-classification
    rc = evidence.get("root_cause")
    if isinstance(rc, str) and rc in _ROOT_CAUSE_SET:
        return Classification(rc, "root_cause", str(evidence.get("evidence_ref", "")))

    ref = str(evidence.get("evidence_ref", ""))

    # 2) decoded program error
    pe = evidence.get("program_error")
    if pe:
        return Classification(_PROGRAM_ERROR_MAP.get(str(pe), "UNKNOWN_PROGRAM_ERROR"),
                              "program_error", ref)

    # 3) submission miss
    sm = evidence.get("submission_miss")
    if sm:
        return Classification(_SUBMISSION_MISS_MAP.get(str(sm), "LEADER_TIMING"),
                              "submission_miss", ref)

    # 4) hot-path latency stage
    ls = evidence.get("latency_stage")
    if ls:
        return Classification(_LATENCY_STAGE_MAP.get(str(ls), "DECISION_LATENCY"),
                              "latency_stage", ref)

    # 5) reject code
    rj = evidence.get("reject_code")
    if rj:
        return Classification(_REJECT_MAP.get(str(rj), "GUARD_ABORT"), "reject_code", ref)

    # 6) exit reason, with outcome/lane refinement
    er = evidence.get("exit_reason")
    if er:
        er = str(er)
        cls = _EXIT_REASON_MAP.get(er, "UNKNOWN")
        if er == "TimeStop" and str(evidence.get("lane", "")).lower() != "scalp":
            cls = "EXIT_LATE"
        if er == "ThesisInvalidation" and evidence.get("forfeited_upside"):
            cls = "THESIS_INVALIDATION_TOO_EARLY"
        # A "TakeProfit" that the row still flags as a loss is a late/floor-breaching exit.
        if cls == "UNKNOWN":
            nl = evidence.get("net_lamports")
            if isinstance(nl, (int, float)) and nl < 0:
                cls = "SCALP_COST_FLOOR_BREACH" if str(evidence.get("lane", "")).lower() == "scalp" \
                    else "EXIT_LATE"
        return Classification(cls, "exit_reason", ref)

    # 7) gate failure
    g = evidence.get("gate")
    if g:
        return Classification(_GATE_MAP.get(str(g), "UNKNOWN"), "gate", ref)

    # 8) terminal-state flags
    if evidence.get("unsellable"):
        return Classification("UNSELLABLE", "unsellable", ref)
    if evidence.get("terminal"):
        return Classification("TERMINAL_LOSS", "terminal", ref)

    # 9) honest fallback
    return Classification("UNKNOWN", "fallback", ref)


@dataclass
class DistributionReport:
    """Aggregate §56.5 evidence — distributions, not anecdotes."""
    total: int
    counts: dict[str, int]                       # root_cause -> count (desc by count, then name)
    fractions: dict[str, float]                  # root_cause -> share of total
    linked: dict[str, list[str]] = field(default_factory=dict)  # root_cause -> evidence refs

    def top(self, k: int = 5) -> list[tuple[str, int]]:
        return list(self.counts.items())[:k]

    def as_dict(self) -> dict[str, Any]:
        return {"total": self.total, "counts": self.counts, "fractions": self.fractions,
                "linked": self.linked}


class RootCauseEngine:
    """Classify a batch of evidence rows and aggregate the §56.5 distribution.

    Optionally persists each classification to the evidence store's `root_cause_classifications`
    table (so the reflection report and any later query see the same distribution). The store is
    optional to keep the classifier pure and unit-testable without a database.
    """

    def __init__(self, store: Optional[Any] = None, run_id: str = "") -> None:
        self.store = store
        self.run_id = run_id

    def classify_row(self, evidence: dict[str, Any], persist: bool = False) -> Classification:
        c = classify(evidence)
        if persist and self.store is not None:
            self.store.record_root_cause(
                self.run_id, c.evidence_ref, c.root_cause,
                {"matched_field": c.matched_field,
                 "evidence": {k: evidence[k] for k in evidence if k != "evidence_ref"}})
        return c

    def aggregate(self, rows: list[dict[str, Any]], persist: bool = False) -> DistributionReport:
        """Roll a batch of evidence rows into a §56.5 root-cause distribution.

        Deterministic ordering: descending by count, then alphabetical by class name, so the
        report is byte-stable for identical input (safe to diff and to replay).
        """
        counter: Counter[str] = Counter()
        linked: dict[str, list[str]] = {}
        for row in rows:
            c = self.classify_row(row, persist=persist)
            counter[c.root_cause] += 1
            if c.evidence_ref:
                linked.setdefault(c.root_cause, []).append(c.evidence_ref)
        total = sum(counter.values())
        ordered = sorted(counter.items(), key=lambda kv: (-kv[1], kv[0]))
        counts = {k: v for k, v in ordered}
        fractions = {k: (v / total if total else 0.0) for k, v in ordered}
        return DistributionReport(total=total, counts=counts, fractions=fractions, linked=linked)

    def reflection_block(self, rows: list[dict[str, Any]], persist: bool = False) -> dict:
        """The §56.4/§56.5 reflection-report block: aggregate evidence + linked records only.

        A reflection may summarise evidence and classify root cause (§56.4) — never mutate
        production. This returns exactly that: the distribution plus its linked source records,
        with no recommendation and no action.
        """
        rep = self.aggregate(rows, persist=persist)
        return {
            "kind": "root_cause_distribution",
            "constitution": "§56.5",
            "total_classified": rep.total,
            "distribution": rep.counts,
            "fractions": rep.fractions,
            "top": rep.top(),
            "linked_records": rep.linked,
        }
