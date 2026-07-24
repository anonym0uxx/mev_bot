"""
Brain review — the weekly-governance view of the engine's episodic-recall evidence.

Turns one `brain_analysis_v1` artifact into the strategy-analysis report a human reads before
the periodic review: what is nominated for retirement, which style lens is paying per venue
phase, which metas are decaying, which callers earned follow or unfollow, and — the section
that usually matters most — WHAT THE BRAIN REFUSED TO ANSWER.

A NOMINATION IS NOT A RETIREMENT.
--------------------------------
Every "retirement" row below is an engine NOMINATION: a lane, archetype, setup class or source
whose realised contribution has been negative over a sample the engine considers large enough
to raise its hand. Nothing here retires anything. §56 sequential retirement requires the §51
FDR/PBO verdict (does the finding survive multiple-comparison correction?) and the §52 baseline
verdict (does removing it actually beat the unmodified baseline?). This report is an INPUT to
that review and never a substitute for it. Acting on a nomination without those verdicts is
exactly the overfitting-to-noise failure §51 exists to prevent.

A REFUSAL IS NOT A ZERO.
------------------------
Cells the engine declined to estimate appear in the REFUSED section with their reason and no
numbers at all. They are not "flat", not "no effect", and not "fine". They are the thin-evidence
frontier — the research agenda — and they are reported as such.

Deterministic and model-free: same artifact in, byte-identical report out. Nothing in this
module calls a model, touches the network, or reads a clock.
"""
from __future__ import annotations

from typing import Any, Optional

from ..store.brain_analysis import BrainAnalysis, LAMPORTS_PER_SOL


# The sentence that must accompany every nomination, wherever it is surfaced.
NOMINATION_DISCLAIMER: str = (
    "A NOMINATION IS NOT A RETIREMENT. §56 sequential retirement requires the §51 FDR/PBO and "
    "§52 baseline verdicts; this report is an input to that review, never a substitute for it."
)

REFUSAL_DISCLAIMER: str = (
    "A REFUSAL IS NOT A ZERO. A refused cell has no estimate at all — it is neither evidence "
    "of decay nor evidence against it. Refused cells are the research agenda, not a finding."
)


def _sol(lamports: int) -> float:
    return lamports / LAMPORTS_PER_SOL


# Sort keys that order None LAST without ever substituting a value for it. Deliberately not
# `x or 0` / `x or ""`: those coerce a refusal into a datum (and would also mangle a real 0).
def _opt_int_key(v: Optional[int]) -> tuple[int, int]:
    return (1, 0) if v is None else (0, v)


def _opt_str_key(v: Optional[str]) -> tuple[int, str]:
    return (1, "") if v is None else (0, v)


def review_brain_analysis(analysis: Optional[BrainAnalysis]) -> dict[str, Any]:
    """Build the governance review as a plain dict. Deterministic ordering throughout.

    With `analysis is None` the review is not empty — it says, in the `status` field, that no
    artifact was available, so a reader can never mistake "we did not look" for "we looked and
    found nothing".
    """
    if analysis is None:
        return {
            "status": "no_artifact",
            "note": ("No brain_analysis_v1 artifact was loadable. This is an ABSENCE OF "
                     "EVIDENCE, not evidence of health: no lane, class, meta or source below "
                     "has been reviewed this period."),
            "nomination_disclaimer": NOMINATION_DISCLAIMER,
            "refusal_disclaimer": REFUSAL_DISCLAIMER,
            "retirement_nominations": [],
            "lens_paying": [],
            "best_paying_lens": None,
            "decaying_metas": [],
            "callers_follow": [],
            "callers_unfollow": [],
            "refused": [],
            "support_inputs_needed": [],
            "counts": {},
        }

    a = analysis

    # --- retirement NOMINATIONS: worst realised net first, then subject/key -------------
    noms = sorted(a.retirement_flags,
                  key=lambda f: (f.realized_net_lamports, f.subject, f.key))
    retirement_nominations = [{
        "subject": f.subject,
        "key": f.key,
        "reason": f.reason,
        "n": f.n,
        "realized_net_lamports": f.realized_net_lamports,
        "realized_net_sol": _sol(f.realized_net_lamports),
        "evidence_ref": f"brain_retire:{a.tick}/{f.subject}/{f.key}",
        "status": "NOMINATED_FOR_REVIEW",
        "disclaimer": NOMINATION_DISCLAIMER,
    } for f in noms]

    # --- which lens is paying, per venue phase: only CONDITIONED cells -------------------
    # Best-paying first within each venue phase. `known_lenses()` guarantees a non-None
    # median, and the key still routes a hypothetical None to the end rather than to 0.
    lenses = sorted(a.known_lenses(), key=lambda l: (
        l.venue_phase,
        (1, 0) if l.median_net_lamports is None else (0, -l.median_net_lamports),
        l.lens))
    lens_paying = [{
        "lens": l.lens,
        "venue_phase": l.venue_phase,
        "n": l.n,
        "median_net_lamports": l.median_net_lamports,
        "median_net_sol": _sol(l.median_net_lamports) if l.median_net_lamports is not None
                          else None,
        "win_rate_bp": l.win_rate_bp,
    } for l in lenses]
    best = a.best_paying_lens
    best_paying_lens = None if best is None else {
        "lens": best.lens, "venue_phase": best.venue_phase,
        "median_net_lamports": best.median_net_lamports,
        "median_net_sol": _sol(best.median_net_lamports), "n": best.n,
    }

    # --- decaying metas: steepest outcome decline first ----------------------------------
    metas = sorted(a.decaying_metas(),
                   key=lambda m: (m.outcome_decline_bp, -m.participation_decline_bp,
                                  m.meta_category))
    decaying_metas = [{
        "meta_category": m.meta_category,
        "phase": m.phase,
        "n": m.n,
        "participation_decline_bp": m.participation_decline_bp,
        "outcome_decline_bp": m.outcome_decline_bp,
        "evidence_ref": f"brain_meta:{a.tick}/{m.meta_category}",
    } for m in metas]

    # --- sources ---------------------------------------------------------------------------
    follows = sorted(a.follow_recommendations,
                     key=lambda f: (-f.realized_net_attributed, f.author_id))
    callers_follow = [{
        "author_id": f.author_id, "platform": f.platform, "n_calls": f.n_calls,
        "realized_net_attributed": f.realized_net_attributed,
        "realized_net_sol": _sol(f.realized_net_attributed),
        "median_lead_ns": f.median_lead_ns, "trust_tier": f.trust_tier,
        "evidence_ref": f"brain_caller:{a.tick}/{f.author_id}",
    } for f in follows]
    unfollows = sorted(a.unfollow_candidates,
                       key=lambda u: (u.realized_net_attributed, u.author_id))
    callers_unfollow = [{
        "author_id": u.author_id, "platform": u.platform, "n_calls": u.n_calls,
        "realized_net_attributed": u.realized_net_attributed,
        "realized_net_sol": _sol(u.realized_net_attributed),
        "evidence_ref": f"brain_caller:{a.tick}/{u.author_id}",
    } for u in unfollows]

    # --- the thin-evidence frontier: what the brain REFUSED to answer ---------------------
    refused = [{
        "subject": r.subject, "key": r.key, "reason": r.reason,
        "estimate": None,   # explicit: there is no number here, and none may be invented
    } for r in sorted(a.refusals(), key=lambda r: (r.subject, r.key))]

    support = [{
        "kind": s.kind, "platform": s.platform, "author_id": s.author_id, "mint_id": s.mint_id,
    } for s in sorted(a.support_inputs_needed,
                      key=lambda s: (s.kind, _opt_str_key(s.platform),
                                     _opt_int_key(s.author_id), _opt_int_key(s.mint_id)))]

    return {
        "status": "ok",
        "tick": a.tick,
        "info_time_ns": a.info_time_ns,
        "schema_version": a.schema_version,
        "episodes_total": a.episodes_total,
        "episodes_admitted": a.episodes_admitted,
        "nomination_disclaimer": NOMINATION_DISCLAIMER,
        "refusal_disclaimer": REFUSAL_DISCLAIMER,
        "retirement_nominations": retirement_nominations,
        "lens_paying": lens_paying,
        "best_paying_lens": best_paying_lens,
        "decaying_metas": decaying_metas,
        "callers_follow": callers_follow,
        "callers_unfollow": callers_unfollow,
        "refused": refused,
        "support_inputs_needed": support,
        "counts": {
            "setup_classes_total": len(a.setup_classes),
            "setup_classes_known": len(a.known_setup_classes()),
            "setup_classes_refused": len(a.unknown_setup_classes()),
            "lenses_total": len(a.lens_scoreboard),
            "lenses_known": len(a.known_lenses()),
            "lenses_refused": len(a.unknown_lenses()),
            "metas_total": len(a.meta_state),
            "metas_decaying": len(a.decaying_metas()),
            "retirement_nominations": len(a.retirement_flags),
            "refusals": len(refused),
        },
    }


def render_brain_review(review: dict[str, Any]) -> str:
    """Render `review_brain_analysis` output as plain text. Deterministic; no clock, no model."""
    L: list[str] = []
    if review.get("status") == "no_artifact":
        L.append("BRAIN REVIEW — NO ARTIFACT")
        L.append(str(review.get("note", "")))
        L.append(NOMINATION_DISCLAIMER)
        L.append(REFUSAL_DISCLAIMER)
        return "\n".join(L)

    c = review["counts"]
    L.append(f"BRAIN REVIEW — tick {review['tick']} (schema {review['schema_version']}, "
             f"info_time_ns {review['info_time_ns']})")
    L.append(f"Episodes: {review['episodes_total']} total, {review['episodes_admitted']} "
             "admitted")
    L.append(f"Coverage: setup classes {c['setup_classes_known']}/{c['setup_classes_total']} "
             f"conditioned ({c['setup_classes_refused']} REFUSED); lenses "
             f"{c['lenses_known']}/{c['lenses_total']} conditioned "
             f"({c['lenses_refused']} REFUSED)")

    L.append("")
    L.append(f"== RETIREMENT NOMINATIONS ({len(review['retirement_nominations'])}) ==")
    L.append(NOMINATION_DISCLAIMER)
    if not review["retirement_nominations"]:
        L.append("  (none)")
    for r in review["retirement_nominations"]:
        L.append(f"  [{r['status']}] {r['subject']}={r['key']} reason={r['reason']} "
                 f"n={r['n']} realized_net={r['realized_net_lamports']} lamports "
                 f"({r['realized_net_sol']:.4f} SOL)  ref={r['evidence_ref']}")

    L.append("")
    L.append("== STYLE LENS, BY VENUE PHASE (conditioned cells only) ==")
    if not review["lens_paying"]:
        L.append("  (no lens cell is conditioned — every cell is a REFUSAL)")
    for l in review["lens_paying"]:
        L.append(f"  {l['venue_phase']}: lens={l['lens']} n={l['n']} "
                 f"median_net={l['median_net_lamports']} lamports "
                 f"({l['median_net_sol']:.4f} SOL) win_rate_bp={l['win_rate_bp']}")
    b = review["best_paying_lens"]
    if b is None:
        L.append("  BEST PAYING: REFUSED — no cell is conditioned enough to name one.")
    else:
        L.append(f"  BEST PAYING: {b['lens']} on {b['venue_phase']} "
                 f"median_net={b['median_net_lamports']} lamports "
                 f"({b['median_net_sol']:.4f} SOL) n={b['n']}")

    L.append("")
    L.append(f"== DECAYING METAS ({len(review['decaying_metas'])}) ==")
    if not review["decaying_metas"]:
        L.append("  (none flagged decaying)")
    for m in review["decaying_metas"]:
        L.append(f"  meta={m['meta_category']} n={m['n']} "
                 f"participation_decline_bp={m['participation_decline_bp']} "
                 f"outcome_decline_bp={m['outcome_decline_bp']}  ref={m['evidence_ref']}")

    L.append("")
    L.append(f"== SOURCES: FOLLOW ({len(review['callers_follow'])}) ==")
    if not review["callers_follow"]:
        L.append("  (none)")
    for f in review["callers_follow"]:
        L.append(f"  author={f['author_id']} platform={f['platform']} n_calls={f['n_calls']} "
                 f"attributed_net={f['realized_net_attributed']} lamports "
                 f"({f['realized_net_sol']:.4f} SOL) median_lead_ns={f['median_lead_ns']} "
                 f"tier={f['trust_tier']}  ref={f['evidence_ref']}")
    L.append(f"== SOURCES: UNFOLLOW ({len(review['callers_unfollow'])}) ==")
    if not review["callers_unfollow"]:
        L.append("  (none)")
    for u in review["callers_unfollow"]:
        L.append(f"  author={u['author_id']} platform={u['platform']} n_calls={u['n_calls']} "
                 f"attributed_net={u['realized_net_attributed']} lamports "
                 f"({u['realized_net_sol']:.4f} SOL)  ref={u['evidence_ref']}")

    L.append("")
    L.append(f"== WHAT THE BRAIN REFUSED TO ANSWER ({len(review['refused'])}) — "
             "the research agenda ==")
    L.append(REFUSAL_DISCLAIMER)
    if not review["refused"]:
        L.append("  (nothing refused: every cell in scope is conditioned)")
    for r in review["refused"]:
        L.append(f"  {r['subject']}={r['key']} reason={r['reason']} estimate=NONE")

    L.append("")
    L.append(f"== SUPPORT INPUTS THE ENGINE LACKS ({len(review['support_inputs_needed'])}) ==")
    if not review["support_inputs_needed"]:
        L.append("  (none)")
    for s in review["support_inputs_needed"]:
        L.append(f"  kind={s['kind']} platform={s['platform']} author_id={s['author_id']} "
                 f"mint_id={s['mint_id']}")
    return "\n".join(L)
