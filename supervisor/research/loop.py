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

BRAIN GROUNDING (real, laptop-operational): the loop now reads the engine's `brain_analysis.json`
episodic-recall artifact. It feeds the model a refusal-aware digest of that evidence AND emits
deterministic, model-free hypotheses straight off it, each carrying an `evidence_ref` that
resolves against the rows the evidence store persisted for that tick (§68/§111). A row the
engine refused to estimate is carried as a REFUSAL end to end — never as a zero. If the
artifact is absent or unparseable the loop behaves exactly as it did before the brain existed.
"""
from __future__ import annotations

import hashlib
import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Optional

from ..core.model_client import ModelClient, SchemaViolation
from ..core.schemas import get_schema
from ..store.evidence import EvidenceStore, BIAS_AUDIT_LABEL
from ..store.brain_analysis import (
    BrainAnalysis, LAMPORTS_PER_SOL, MetaStateRow, RetirementFlagRow, SetupClassRow,
    UnfollowRow, load_brain_analysis,
)
from ..console.escalate import EscalationChannel, Escalation


# Default seed source shipped alongside the store (§45.1 documented finding inventory).
_DEFAULT_KB_SEED = Path(__file__).resolve().parent.parent / "store" / "kb_seed.json"

# §45.2 — the FIRST registered research experiment. Stable id so re-seeding is idempotent and the
# knowledge-base query (§56.10) can find it before proposing anything new.
BIAS_AUDIT_EXPERIMENT_ID = "EXP-45.2-ENRICHMENT-BIAS-AUDIT"


def _bias_audit_hypothesis() -> dict:
    """The §45.2 enrichment-selection bias audit, shaped for EvidenceStore.record_hypothesis.

    §45.2: "The first registered research experiment must audit the enrichment-selection bias in
    the historical 856-trade enriched subset (enrichment success plausibly correlates with token
    liveliness, biasing all conclusions conditioned on it) and determine whether the April
    conclusions survive full-population, missingness-aware analysis. Until then, every
    graduation-cohort claim carries BIAS_AUDIT_REQUIRED."
    """
    return {
        "hypothesis_id": BIAS_AUDIT_EXPERIMENT_ID,
        "statement": (
            f"[{BIAS_AUDIT_LABEL}] The April graduation-cohort conclusions drawn from the "
            "enriched 856-trade subset survive a full-population, missingness-aware re-analysis; "
            "i.e. enrichment-selection (enrichment success correlating with token liveliness) "
            "does not materially bias the estimated edge."
        ),
        "causal_mechanism": (
            "Enrichment succeeds more often on livelier tokens, so any statistic conditioned on "
            "the enriched subset over-represents survivors and inflates apparent edge."
        ),
        "competing_explanations": [
            "Enrichment success is independent of outcome (no selection effect).",
            "Selection effect exists but is dominated by the fixed-cost sizing defect, not signal.",
        ],
        "disconfirming_evidence_sought": (
            "Full-population re-run (including enrichment-failed trades) in which the graduation "
            "edge collapses or reverses relative to the enriched-subset estimate."
        ),
        "expected_net_sol_impact": 0.0,
        "prior_probability": 0.5,
        "cost_to_test": "low",
        "edge_half_life": "durable",
        "inference_state": "Hypothesis",
        "labels": BIAS_AUDIT_LABEL,
    }


def seed_knowledge_base(store: EvidenceStore, run_id: str,
                        seed_source: str | Path | None = None) -> dict:
    """§45.1 KB seeding + §45.2 first-experiment registration.

    Reads the documented seed-finding inventory (a JSON list of prior repository findings, each
    with full §45.1 provenance and an evidence-status label), records every finding into the
    ResearchKnowledgeBase, and registers the §45.2 enrichment-bias audit as the FIRST KB
    experiment — a BIAS_AUDIT_REQUIRED-labeled hypothesis row at the head of the VOI queue.

    Laptop-operational and idempotent: re-running replaces the same finding/experiment rows by
    stable id. Imported markdown conclusions are stored as claims, never verified facts (§45.1).
    Returns a summary; raises with a clear message on a malformed seed source (never silently).
    """
    src = Path(seed_source) if seed_source is not None else _DEFAULT_KB_SEED
    if not src.is_file():
        raise FileNotFoundError(
            f"KB seed source not found: {src} — pass the documented seed-finding inventory "
            "(§45.1). A missing seed source is a seeding failure, not an empty knowledge base.")
    try:
        doc = json.loads(src.read_text(encoding="utf-8"))
    except json.JSONDecodeError as e:
        raise ValueError(f"{src}: invalid KB seed JSON: {e}") from e
    findings = doc.get("findings", doc) if isinstance(doc, dict) else doc
    if not isinstance(findings, list):
        raise ValueError(f"{src}: seed source must contain a 'findings' list (§45.1)")

    seeded_ids: list[str] = []
    for i, finding in enumerate(findings):
        if not isinstance(finding, dict):
            raise ValueError(f"{src}: findings[{i}] is not an object")
        res = store.record_seeded_finding(finding, created_run=run_id)
        seeded_ids.append(res["id"])

    # Register §45.2 as the first KB experiment (BIAS_AUDIT_REQUIRED-labeled hypothesis row).
    audit = _bias_audit_hypothesis()
    store.record_hypothesis(audit, created_run=run_id)

    return {
        "seed_source": str(src),
        "findings_seeded": len(seeded_ids),
        "finding_ids": seeded_ids,
        "bias_audit_experiment_id": BIAS_AUDIT_EXPERIMENT_ID,
        "bias_audit_label": BIAS_AUDIT_LABEL,
    }


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
    # §68/§111: the store record this hypothesis was derived FROM. Empty for a model-proposed
    # hypothesis; a resolvable 'brain*:<tick>/<row key>' ref for a brain-grounded one.
    evidence_ref: str = ""
    # Provenance of `expected_net_sol_impact` — how the number was obtained, in words. Never
    # empty on a brain-grounded hypothesis, and literally "none" when NO impact was estimated
    # (an unresolved refusal), so a reader can tell a measured number from a declined one.
    impact_basis: str = ""
    labels: str = ""

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
            "evidence_ref": self.evidence_ref,
            "labels": self.labels,
        }


# ======================================================================================
# BRAIN GROUNDING — the engine's episodic-recall evidence, fed to the research loop.
#
# Two independent paths, deliberately:
#   1. `brain_evidence_digest` shapes the artifact into a refusal-aware prompt block for the
#      model. It states what the brain KNOWS *and* what it REFUSED to answer and why.
#   2. `_brain_grounded_hypotheses` emits hypotheses with NO model in the loop at all —
#      deterministic, auditable, and available even when the model endpoint is down.
#
# The single rule both obey: a `confidence == "unknown"` row is a REFUSAL, not a zero. It is
# neither evidence of decay nor evidence against it. No estimate is ever imputed from one.
# ======================================================================================

# Minimum observed sample before an observation may drive a class/meta/source-level proposal.
# Below these, the row is treated as thin evidence (a value-of-information target), not as a
# finding. These are policy constants, not measurements — they are the ONLY numbers in the
# grounded path that do not come out of the artifact, and they gate proposals rather than
# quantify them.
BRAIN_MIN_N_SETUP_CLASS: int = 8
BRAIN_MIN_N_META: int = 8
BRAIN_MIN_N_SOURCE: int = 5
# Bound on hypotheses emitted per deterministic rule, so one noisy artifact cannot flood the
# VOI queue. Rows are ranked by OBSERVED magnitude before the cut, so the bound keeps the
# largest observed effects.
BRAIN_MAX_PER_RULE: int = 5

# Documented prior mapping: P(the proposal is right) as a function of the OBSERVED sample the
# engine conditioned on. More episodes behind a median => more belief that acting on it helps.
# Monotone, coarse, and fixed — it is a stated policy, never a model output or a fitted value.
_PRIOR_BY_N: tuple[tuple[int, float], ...] = (
    (64, 0.80), (32, 0.70), (16, 0.60), (8, 0.50),
)
# A hypothesis derived from a REFUSAL gets exactly maximum entropy. The brain said it does not
# know; claiming any other prior would be inventing a belief it did not express.
_PRIOR_ON_REFUSAL: float = 0.50

BRAIN_GROUNDED_LABEL: str = "BRAIN_GROUNDED"
# Stamped when the brain refused and no observed peer exists to scale the information value.
# Such a hypothesis carries expected_net_sol_impact 0.0 meaning "NO ESTIMATE WAS MADE" — it is
# a declined estimate, not a measured zero, and this label is what says so in the store.
NO_IMPACT_ESTIMATE_LABEL: str = "NO_OBSERVED_IMPACT_ESTIMATE"


def _prior_from_n(n: int) -> float:
    """Map an OBSERVED sample size onto the documented prior table above."""
    for threshold, prior in _PRIOR_BY_N:
        if n >= threshold:
            return prior
    return 0.40


def _sol(lamports: int) -> float:
    """Lamports -> SOL. The single documented divisor; no other conversion exists."""
    return lamports / LAMPORTS_PER_SOL


def _median_int(values: list[int]) -> Optional[int]:
    """Deterministic integer median (lower-middle on even counts; no float averaging).

    Returns None for an empty input — there is no median of nothing, and 0 would be a lie.
    """
    if not values:
        return None
    s = sorted(values)
    return s[(len(s) - 1) // 2]


def _peer_magnitude_lamports(analysis: BrainAnalysis) -> Optional[int]:
    """The observed per-episode outcome magnitude across CONDITIONED setup classes.

    Used only to scale the value of information of a REFUSAL: "if this refused cell were
    conditioned, the outcome it would reveal is, on the evidence of its peer cells, of about
    this magnitude". It is a median of numbers the engine actually measured — it makes no claim
    whatsoever about the refused cell's own value.

    Returns None when there are NO conditioned peers. In that case no impact is estimated at
    all (0.0 + NO_OBSERVED_IMPACT_ESTIMATE), because nothing has been observed to scale by.
    """
    mags = [abs(c.median_net_lamports) for c in analysis.known_setup_classes()
            if c.median_net_lamports is not None]
    return _median_int(mags)


def _id_part(raw: str) -> str:
    """Stable, filesystem/id-safe slug of an arbitrary artifact key.

    Deterministic: the readable prefix plus a hash of the FULL key, so two keys that share a
    prefix (or differ only in stripped characters) never collide on one hypothesis id.
    """
    safe = "".join(ch if ch.isalnum() else "_" for ch in raw)[:32].strip("_")
    return f"{safe}-{hashlib.sha256(raw.encode('utf-8')).hexdigest()[:8]}"


def _fmt_opt(v: Optional[int]) -> str:
    """Render an optional estimate for the digest. A refusal prints REFUSED, never 0."""
    return "REFUSED" if v is None else str(v)


def brain_evidence_digest(analysis: Optional[BrainAnalysis], max_rows: int = 12) -> str:
    """Shape the brain artifact into a compact, refusal-aware evidence block for the model.

    The digest is as explicit about the REFUSALS as about the findings. A refusal is not noise
    to be filtered out before prompting: it is the map of where the engine's evidence is thin,
    and therefore where a cheap experiment buys the most information. Suppressing it would let
    the model read "absence of a decay finding" as "no decay", which is exactly the error this
    whole chain exists to prevent.

    Deterministic: same artifact in, byte-identical digest out. Bounded by `max_rows` per
    section so a large artifact cannot blow the context window.
    """
    if analysis is None:
        return ("BRAIN EVIDENCE: UNAVAILABLE. No brain_analysis_v1 artifact was loadable for "
                "this cycle. You have NO episodic-recall evidence — do not speculate about "
                "setup-class or meta performance; reason only from the reconciled outcomes "
                "below.")

    out: list[str] = []
    a = analysis
    out.append(
        f"BRAIN EVIDENCE DIGEST (brain_analysis_v1 schema={a.schema_version} tick={a.tick} "
        f"info_time_ns={a.info_time_ns} episodes_total={a.episodes_total} "
        f"episodes_admitted={a.episodes_admitted})")
    out.append("All numbers below are OBSERVED integers from the engine's episodic recall. "
               "Lamports; 1 SOL = 1e9 lamports.")

    known_c = a.known_setup_classes()
    out.append("")
    out.append(f"[CONDITIONED SETUP CLASSES] {len(known_c)} of {len(a.setup_classes)} classes "
               "carry an estimate:")
    if not known_c:
        out.append("  (none — every setup class is a refusal; see REFUSALS below)")
    for c in known_c[:max_rows]:
        out.append(
            f"  sig={c.signature} phase={c.venue_phase} lane={c.discovery_lane} "
            f"meta={c.meta_category} n={c.n} median_net={c.median_net_lamports} "
            f"mean_net={c.mean_net_lamports} win_rate_bp={c.win_rate_bp} "
            f"p25={c.p25_net_lamports} p75={c.p75_net_lamports} "
            f"median_hold_ns={c.median_hold_ns} nearest_distance={c.nearest_distance}")

    known_l = a.known_lenses()
    out.append("")
    out.append(f"[LENS SCOREBOARD] {len(known_l)} of {len(a.lens_scoreboard)} lens/venue cells "
               "carry an estimate:")
    for l in known_l[:max_rows]:
        out.append(f"  lens={l.lens} phase={l.venue_phase} n={l.n} "
                   f"median_net={l.median_net_lamports} win_rate_bp={l.win_rate_bp}")
    if a.best_paying_lens is not None:
        b = a.best_paying_lens
        out.append(f"  BEST PAYING: lens={b.lens} phase={b.venue_phase} "
                   f"median_net={b.median_net_lamports} n={b.n}")
    else:
        out.append("  BEST PAYING: REFUSED (no lens cell is conditioned enough to name one)")

    decaying = a.decaying_metas()
    out.append("")
    out.append(f"[META STATE] {len(decaying)} of {len(a.meta_state)} categories are decaying:")
    for m in decaying[:max_rows]:
        out.append(f"  meta={m.meta_category} phase={m.phase} n={m.n} "
                   f"participation_decline_bp={m.participation_decline_bp} "
                   f"outcome_decline_bp={m.outcome_decline_bp}")

    out.append("")
    out.append(f"[RETIREMENT NOMINATIONS] {len(a.retirement_flags)} (an engine NOMINATION is "
               "NOT a retirement — §56 retirement needs §51 FDR/PBO and §52 baseline verdicts):")
    for f in a.retirement_flags[:max_rows]:
        out.append(f"  subject={f.subject} key={f.key} reason={f.reason} n={f.n} "
                   f"realized_net={f.realized_net_lamports}")

    out.append("")
    out.append(f"[SOURCES] follow={len(a.follow_recommendations)} "
               f"unfollow={len(a.unfollow_candidates)}:")
    for fr in a.follow_recommendations[:max_rows]:
        out.append(f"  FOLLOW author={fr.author_id} platform={fr.platform} n_calls={fr.n_calls} "
                   f"realized_net_attributed={fr.realized_net_attributed} "
                   f"median_lead_ns={fr.median_lead_ns} trust_tier={fr.trust_tier}")
    for u in a.unfollow_candidates[:max_rows]:
        out.append(f"  UNFOLLOW author={u.author_id} platform={u.platform} n_calls={u.n_calls} "
                   f"realized_net_attributed={u.realized_net_attributed}")

    refusals = a.refusals()
    out.append("")
    out.append(f"[WHAT THE BRAIN REFUSED TO ANSWER] {len(refusals)} refusals. Each one is a "
               "question with NO answer — not an answer of zero:")
    for r in refusals[:max_rows]:
        out.append(f"  {r.subject}={r.key} reason={r.reason}")
    if len(refusals) > max_rows:
        out.append(f"  ... and {len(refusals) - max_rows} more")
    for c in a.unknown_setup_classes()[:max_rows]:
        out.append(f"  (refused class detail) sig={c.signature} phase={c.venue_phase} "
                   f"lane={c.discovery_lane} meta={c.meta_category} "
                   f"n={_fmt_opt(c.n)} median_net={_fmt_opt(c.median_net_lamports)}")

    out.append("")
    out.append("[SUPPORT INPUTS THE ENGINE SAYS IT LACKS]")
    for s in a.support_inputs_needed[:max_rows]:
        out.append(f"  kind={s.kind} platform={s.platform} author_id={s.author_id} "
                   f"mint_id={s.mint_id}")

    out.append("")
    out.append("BINDING RULES FOR READING THE ABOVE:")
    out.append("  1. A REFUSED value is a refusal. It is NOT zero, NOT 'no effect', and NOT "
               "evidence that the cell is fine. Never average it in, never impute it.")
    out.append("  2. Every number above was measured. Do not invent, extrapolate or round one "
               "into a claim the artifact does not make.")
    out.append("  3. Refusals are where evidence is thinnest and a cheap experiment therefore "
               "buys the most information. Proposing to MEASURE a refused cell is a valid, "
               "often the highest-value, hypothesis.")
    out.append("  4. A retirement nomination is an input to human review, never a decision.")
    return "\n".join(out)


def _brain_grounded_hypotheses(analysis: Optional[BrainAnalysis]) -> list[Hypothesis]:
    """Deterministic, model-free hypotheses read straight off the brain artifact.

    Five rules, each emitting a hypothesis whose `evidence_ref` names the exact artifact row it
    came from (`brain*:<tick>/<row key>`) so §68/§111 holds: the reference resolves against the
    rows `EvidenceStore.ingest_brain_analysis` persisted for that tick.

      R1  conditioned-negative setup class  -> propose excluding/downweighting it.
      R2  decaying meta                      -> propose reducing exposure to the category.
      R3  unfollow candidate                 -> propose demoting the source.
      R4  engine retirement nomination       -> propose the §51/§52 retirement review.
      R5  refused setup class (thin evidence)-> propose MEASURING it (value of information).

    Impact is always an OBSERVED quantity: median net x observed n for R1/R2, realized net
    attributed for R3/R4. R5 estimates no effect for the refused cell at all — it scales the
    value of the *information* by the median magnitude its conditioned peers exhibit, and when
    there are no conditioned peers it estimates nothing (0.0 + NO_OBSERVED_IMPACT_ESTIMATE).

    The lens scoreboard deliberately drives no deterministic hypothesis: lens cells are not
    persisted as their own table, so a lens-keyed evidence_ref could not resolve, and a
    hypothesis whose evidence does not resolve is exactly what §68/§111 forbids. Lens evidence
    reaches the model through the digest and the human through `analysis/brain_review.py`.

    Deterministic: same artifact in, identical list (contents AND order) out.
    """
    if analysis is None:
        return []
    a = analysis
    tick = a.tick
    out: list[Hypothesis] = []

    # ---- R1: a conditioned setup class whose observed median is negative ---------------
    negatives: list[SetupClassRow] = [
        c for c in a.known_setup_classes()
        if c.median_net_lamports is not None and c.n is not None
        and c.median_net_lamports < 0 and c.n >= BRAIN_MIN_N_SETUP_CLASS
    ]
    # Rank by observed drag (|median| * n), largest first; ties broken by the stable row key.
    negatives.sort(key=lambda c: (-(abs(c.median_net_lamports) * c.n), c.store_key))
    for c in negatives[:BRAIN_MAX_PER_RULE]:
        assert c.median_net_lamports is not None and c.n is not None  # narrowed above
        drag_lamports = -c.median_net_lamports * c.n
        out.append(Hypothesis(
            hypothesis_id=f"BRAIN-{tick}-SETUPCLASS-{_id_part(c.store_key)}",
            statement=(
                f"Excluding (or downweighting) setup class {c.signature} on {c.venue_phase} "
                f"(lane={c.discovery_lane}, meta={c.meta_category}) raises net SOL. Over the "
                f"n={c.n} episodes the engine conditioned on, its median net was "
                f"{c.median_net_lamports} lamports (win rate {c.win_rate_bp} bp, "
                f"p25={c.p25_net_lamports}, p75={c.p75_net_lamports}) — an observed drag of "
                f"{_sol(drag_lamports):.4f} SOL."),
            causal_mechanism=(
                "The class's entry conditions select episodes whose realised distribution is "
                "centred below the cost floor, so every fill in it pays the spread and fees "
                "without an offsetting move."),
            expected_net_sol_impact=_sol(drag_lamports),
            prior_probability=_prior_from_n(c.n),
            cost_to_test="low",
            edge_half_life="weeks",
            competing_explanations=[
                "The negative median is a regime artefact and the class pays again outside it.",
                "The drag is sizing/cost, not selection: the same class at smaller size is "
                "positive.",
                f"The n={c.n} sample is unrepresentative despite clearing the n>="
                f"{BRAIN_MIN_N_SETUP_CLASS} floor.",
            ],
            disconfirming_evidence_sought=(
                "A replay excluding this class in which net SOL does NOT improve, or a "
                "regime-split in which the class's median is positive in the current regime."),
            evidence_ref=f"brain_setup:{tick}/{c.store_key}",
            impact_basis=(f"observed: median_net_lamports({c.median_net_lamports}) x "
                          f"n({c.n}) from brain_setup:{tick}/{c.store_key}"),
            labels=BRAIN_GROUNDED_LABEL,
        ))

    # ---- R2: a decaying meta category ---------------------------------------------------
    # Impact is attributed ONLY from conditioned classes inside the meta. A decline in bp is
    # not a lamport quantity, so it never becomes one here.
    decaying: list[MetaStateRow] = [m for m in a.decaying_metas() if m.n >= BRAIN_MIN_N_META]
    meta_drag: dict[int, int] = {}
    for c in a.known_setup_classes():
        if c.median_net_lamports is not None and c.n is not None and c.median_net_lamports < 0:
            meta_drag[c.meta_category] = (meta_drag.get(c.meta_category, 0)
                                          + (-c.median_net_lamports * c.n))
    decaying.sort(key=lambda m: (-meta_drag.get(m.meta_category, 0), m.meta_category))
    for m in decaying[:BRAIN_MAX_PER_RULE]:
        drag = meta_drag.get(m.meta_category)
        if drag is not None:
            impact = _sol(drag)
            basis = (f"observed: sum over conditioned setup classes in meta {m.meta_category} "
                     f"of median_net x n = {drag} lamports")
            labels = BRAIN_GROUNDED_LABEL
            impact_sentence = f" Attributed observed drag: {impact:.4f} SOL."
        else:
            # The meta is decaying by participation/outcome bp, but NO conditioned class inside
            # it has a lamport estimate. There is nothing observed to size the impact with, so
            # none is claimed. 0.0 here means "not estimated", stamped by the label.
            impact = 0.0
            basis = ("none: meta is flagged decaying but no conditioned setup class inside it "
                     "carries a lamport estimate — no impact was estimated")
            labels = f"{BRAIN_GROUNDED_LABEL},{NO_IMPACT_ESTIMATE_LABEL}"
            impact_sentence = (" No net-SOL impact is estimated: no conditioned class inside "
                               "this meta carries a lamport estimate.")
        out.append(Hypothesis(
            hypothesis_id=f"BRAIN-{tick}-META-{m.meta_category}",
            statement=(
                f"Reducing exposure to meta category {m.meta_category} raises net SOL. The "
                f"engine classes it as decaying over n={m.n} episodes: participation is down "
                f"{m.participation_decline_bp} bp and outcomes are down {m.outcome_decline_bp} "
                f"bp.{impact_sentence}"),
            causal_mechanism=(
                "Attention rotates out of a saturated meta faster than the strategy's entry "
                "criteria adapt, so later entries buy exit liquidity from earlier participants "
                "(§56.5 META_ROTATION_LAG)."),
            expected_net_sol_impact=impact,
            prior_probability=_prior_from_n(m.n),
            cost_to_test="low",
            edge_half_life="days",
            competing_explanations=[
                "The category is not decaying; participation fell for an unrelated supply "
                "reason while per-episode economics held.",
                "Decay is real but already priced by existing exposure caps, so cutting "
                "further only forgoes upside.",
            ],
            disconfirming_evidence_sought=(
                "A forward window in which entries in this category, taken after the decay "
                "flag, realise a positive median net."),
            evidence_ref=f"brain_meta:{tick}/{m.meta_category}",
            impact_basis=basis,
            labels=labels,
        ))

    # ---- R3: an unfollow candidate (source quality) --------------------------------------
    unfollow: list[UnfollowRow] = [
        u for u in a.unfollow_candidates
        if u.realized_net_attributed < 0 and u.n_calls >= BRAIN_MIN_N_SOURCE
    ]
    unfollow.sort(key=lambda u: (u.realized_net_attributed, u.author_id))
    for u in unfollow[:BRAIN_MAX_PER_RULE]:
        out.append(Hypothesis(
            hypothesis_id=f"BRAIN-{tick}-SOURCE-{u.author_id}",
            statement=(
                f"Demoting caller {u.author_id} on {u.platform} raises net SOL. Across "
                f"n_calls={u.n_calls} attributed calls the realised attributed net was "
                f"{u.realized_net_attributed} lamports "
                f"({_sol(u.realized_net_attributed):.4f} SOL)."),
            causal_mechanism=(
                "The caller's posts lead price by too little (or follow it), so acting on them "
                "buys the move already made — copy-bait rather than lead (§56.5 COPY_BAIT_LOSS "
                "/ SOCIAL_FALSE_POSITIVE)."),
            expected_net_sol_impact=_sol(-u.realized_net_attributed),
            prior_probability=_prior_from_n(u.n_calls),
            cost_to_test="none",
            edge_half_life="weeks",
            competing_explanations=[
                "Attribution is mis-assigned: the loss belongs to a co-firing source.",
                "The caller leads genuinely but the execution lane is too slow to capture it, "
                "so the defect is latency, not source quality.",
            ],
            disconfirming_evidence_sought=(
                "A markout re-run, attributed at the same horizon, in which this caller's "
                "attributed net is positive."),
            evidence_ref=f"brain_caller:{tick}/{u.author_id}",
            impact_basis=(f"observed: realized_net_attributed({u.realized_net_attributed}) "
                          f"over n_calls({u.n_calls}) from brain_caller:{tick}/{u.author_id}"),
            labels=BRAIN_GROUNDED_LABEL,
        ))

    # ---- R4: an engine retirement NOMINATION ---------------------------------------------
    flags: list[RetirementFlagRow] = [
        f for f in a.retirement_flags if f.realized_net_lamports < 0]
    flags.sort(key=lambda f: (f.realized_net_lamports, f.subject, f.key))
    for f in flags[:BRAIN_MAX_PER_RULE]:
        out.append(Hypothesis(
            hypothesis_id=f"BRAIN-{tick}-RETIRE-{f.subject}-{_id_part(f.key)}",
            statement=(
                f"Retiring {f.subject} '{f.key}' raises net SOL. The engine nominates it "
                f"(reason={f.reason}) on n={f.n} episodes realising "
                f"{f.realized_net_lamports} lamports "
                f"({_sol(f.realized_net_lamports):.4f} SOL). A NOMINATION IS NOT A RETIREMENT: "
                "this hypothesis exists to be tested through §51 FDR/PBO and §52 baseline "
                "verdicts before §56 sequential retirement may act."),
            causal_mechanism=(
                "The subject's realised contribution has been negative over a sample the "
                "engine considers sufficient to nominate, so capital routed through it is "
                "capital not routed through a paying alternative."),
            expected_net_sol_impact=_sol(-f.realized_net_lamports),
            prior_probability=_prior_from_n(f.n),
            cost_to_test="low",
            edge_half_life="weeks",
            competing_explanations=[
                "The loss is regime-specific and the subject returns to profit outside it.",
                "The subject is a loss-leader whose removal degrades a downstream lane that "
                "depends on its coverage.",
                "Multiple-comparison artefact: the nomination survives no FDR correction (§51).",
            ],
            disconfirming_evidence_sought=(
                "A §52 baseline comparison in which removing the subject fails to beat the "
                "unmodified baseline, or a §51 FDR/PBO run in which the nomination does not "
                "survive correction."),
            evidence_ref=f"brain_retire:{tick}/{f.subject}/{f.key}",
            impact_basis=(f"observed: realized_net_lamports({f.realized_net_lamports}) over "
                          f"n({f.n}) from brain_retire:{tick}/{f.subject}/{f.key}"),
            labels=BRAIN_GROUNDED_LABEL,
        ))

    # ---- R5: a REFUSAL — the value-of-information branch ----------------------------------
    # The engine declined to estimate this cell. That is not a finding about the cell; it is a
    # finding about our evidence. The experiment is cheap (observe, do not trade differently)
    # and the uncertainty is genuine and maximal, which is where the VOI comes from — NOT from
    # an invented effect size.
    peer = _peer_magnitude_lamports(a)
    unknowns = sorted(a.unknown_setup_classes(), key=lambda c: c.store_key)
    for c in unknowns[:BRAIN_MAX_PER_RULE]:
        if peer is None:
            impact = 0.0
            basis = ("none: the brain refused this cell AND no conditioned peer class exists "
                     "to scale the value of information — no impact was estimated")
            labels = f"{BRAIN_GROUNDED_LABEL},{NO_IMPACT_ESTIMATE_LABEL}"
            voi_sentence = ("No impact is estimated: nothing has been observed anywhere in the "
                            "index to scale it by.")
        else:
            impact = _sol(peer * BRAIN_MIN_N_SETUP_CLASS)
            basis = (f"information value: median |median_net| across conditioned peer classes "
                     f"({peer} lamports) x the n={BRAIN_MIN_N_SETUP_CLASS} sample the "
                     f"experiment would gather. Makes NO claim about this cell's own value.")
            labels = BRAIN_GROUNDED_LABEL
            voi_sentence = (
                f"Value of information, scaled by conditioned peers (median |median_net| "
                f"{peer} lamports x n={BRAIN_MIN_N_SETUP_CLASS}): {impact:.4f} SOL. This is "
                "the worth of the ANSWER, not a claim about this cell's outcome.")
        out.append(Hypothesis(
            hypothesis_id=f"BRAIN-{tick}-VOI-SETUPCLASS-{_id_part(c.store_key)}",
            statement=(
                f"Setup class {c.signature} on {c.venue_phase} (lane={c.discovery_lane}, "
                f"meta={c.meta_category}) is UNMEASURED: the engine refused an estimate "
                f"(reason={c.unknown_reason}). Gathering enough episodes to condition it "
                f"changes a decision we are currently making blind. {voi_sentence}"),
            causal_mechanism=(
                "The cell has too few in-radius episodes for the recall index to produce an "
                "estimate, so every decision touching it is taken without conditioning — the "
                "outcome could be strongly positive or strongly negative and we cannot tell."),
            expected_net_sol_impact=impact,
            prior_probability=_PRIOR_ON_REFUSAL,
            # 'none' is honest: filling a refused cell needs observation, not capital at risk.
            # It is also what gives a refusal its high VOI rank — cheap answer to a real
            # unknown — rather than any inflated impact number.
            cost_to_test="none",
            edge_half_life="unknown",
            competing_explanations=[
                "The cell is rare enough that it will never accumulate a usable sample and the "
                "right answer is to route around it permanently.",
                "The refusal is an index-radius artefact and a nearer neighbour would answer "
                "it without new episodes.",
            ],
            disconfirming_evidence_sought=(
                "Episodes accumulating to the conditioning threshold WITHOUT the resulting "
                "estimate changing any admission or sizing decision — i.e. the answer turns "
                "out not to matter."),
            evidence_ref=f"brain_setup:{tick}/{c.store_key}",
            impact_basis=basis,
            labels=labels,
        ))

    # Deterministic output order: highest VOI first, hypothesis id as the tie-break.
    out.sort(key=lambda h: (-h.voi_score(), h.hypothesis_id))
    return out


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
    # REAL, optional: path to the engine's `brain_analysis.json` (written beside
    # live_status.json). Absent or unparseable => the loop runs exactly as it did before the
    # brain existed. The brain is an ENHANCEMENT, never a dependency (pinned by test).
    brain_analysis_path: Optional[str | Path] = None


class ResearchLoop:
    def __init__(self, model: ModelClient, store: EvidenceStore,
                 escalate: EscalationChannel, adapters: ResearchAdapters, run_id: str):
        self.model = model
        self.store = store
        self.escalate = escalate
        self.adapters = adapters
        self.run_id = run_id

    def load_brain_analysis(self) -> Optional[BrainAnalysis]:
        """Load and persist this cycle's brain evidence. None whenever it is unavailable.

        Ingesting BEFORE any hypothesis is generated is what makes the grounded hypotheses'
        `evidence_ref`s resolve (§68/§111): the rows they cite are already in the store by the
        time they are recorded.
        """
        path = self.adapters.brain_analysis_path
        if path is None:
            return None
        analysis = load_brain_analysis(path)
        if analysis is None:
            return None
        self.store.ingest_brain_analysis(self.run_id, analysis)
        return analysis

    def cycle(self) -> Optional[dict]:
        """One research cycle. Returns a shadow-candidate proposal or None."""
        # 1) ingest reconciled outcomes (REAL default: ingest_reconciled_outcomes_from_jsonl,
        #    bound by the caller; persists reconciled_outcomes rows in the evidence store)
        outcomes = self.adapters.ingest_reconciled_outcomes()

        # 1b) ingest the engine's episodic-recall evidence, if any. Every failure mode of this
        #     step yields None and the cycle proceeds ungrounded — never degraded silently:
        #     the loader logs loudly on anything worse than an absent file.
        analysis = self.load_brain_analysis()

        # 2) hypotheses, from two independent sources:
        #    (a) the model, now fed a refusal-aware digest of the brain evidence, and
        #    (b) deterministic rules read straight off the artifact — cheap, auditable, and
        #        available even when the model endpoint is not.
        #    Both are persisted (§56.10 — competing explanations and disconfirming evidence
        #    survive into the KB).
        hyps = self._generate_hypotheses(outcomes, analysis)
        seen = {h.hypothesis_id for h in hyps}
        for h in _brain_grounded_hypotheses(analysis):
            if h.hypothesis_id not in seen:
                seen.add(h.hypothesis_id)
                hyps.append(h)
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

    def _generate_hypotheses(self, outcomes: dict,
                             analysis: Optional[BrainAnalysis] = None) -> list[Hypothesis]:
        """Model-proposed hypotheses, grounded in the brain digest when one is available.

        `analysis` defaults to None so every pre-brain caller keeps working unchanged; with
        None the digest degrades to an explicit "no episodic evidence, do not speculate"
        instruction rather than to silence.
        """
        try:
            obj = self.model.constrained(
                "You are the research reflection engine. From reconciled trade outcomes and the "
                "engine's episodic-recall evidence, propose ONE falsifiable hypothesis to "
                "improve net-SOL edge. You never authorize trades; you only propose. "
                "A value marked REFUSED is a refusal, not a zero: it means the engine declined "
                "to estimate on thin evidence, and it is neither evidence of decay nor evidence "
                "against it — never impute, average, or reason as if it were 0. Proposing to "
                "MEASURE a refused cell is a legitimate hypothesis. Return the hypothesis "
                "schema object.",
                f"Reconciled outcomes summary: {outcomes}\n\n"
                f"{brain_evidence_digest(analysis)}",
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
