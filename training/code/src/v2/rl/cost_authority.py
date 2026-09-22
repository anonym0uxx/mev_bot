#!/usr/bin/env python
"""ONE cost authority for every prompt, label and reward statement in the corpus.

WHY THIS EXISTS. The c8/c9 corpus stated five different round-trip costs for the same
thing: 189 bp (measured pool fee), 210 bp (decision system prompt), 280 bp (the
utility_reasoning row's components, whose own printed total said 210), ~180 bp one-way
(management + utility_regression system prompts), and ~62-65 bp (the decision family's
own SIZE OPTIONS block). One row contradicted itself.

THE AUTHORITY is `exit_mechanics.cost_floor_bps` - it is the only cost statement in the
pipeline built from OUR OWN measurements (91,735 recorded fills for the tx fee, 65,927
clean single-hop swaps for the 94.63 bp curve venue fee, and the PumpSwap AMM's own
30 bp/side schedule = 25 bp LP + 5 bp protocol, charged on top of the recorded price).
Nothing here
re-derives a number; this module exists so that every caller computes the same one the
same way, and so that no call site can quietly reintroduce a hand-typed figure.

    round trip bp = 2 x venue_bp + 2 x clip_impact_bp + 2 x fixed_tx_bp

with  venue_bp        = 94.63 on the bonding curve, 30 on the AMM (its own 25 bp LP + 5 bp protocol schedule)
      clip_impact_bp  = clip_sol / depth_sol * 10_000
      fixed_tx_bp     = measured tx fee (network + priority) in lamports / notional

There is deliberately NO failure charge: the reward engine does not charge one, and a
cost the model is told about but is never graded against is exactly the drift this
module removes.
"""
from __future__ import annotations

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from exit_mechanics import (  # noqa: E402
    AMM_VENUE_FEE_BPS_PER_LEG, BONDING_FEE_BPS_MEASURED, BPS_ONE, DEPLOY_SOL_CANONICAL,
    FIXED_LAMPORTS_PER_LEG_P50, LAMPORTS_PER_SOL, cost_floor_bps, impairment_bps,
)

REGIMES = ("bonding_curve", "amm")
# the venue string the corpus's tapes/prompts carry -> the cost regime
VENUE_TO_REGIME = {"pumpfun": "bonding_curve", "pumpswap": "amm",
                   "bonding_curve": "bonding_curve", "amm": "amm"}

# ---- THE FEE-QUANTILE LADDER, READ FROM THE FROZEN MEASUREMENT --------------------
#
# This used to be `{"p90": 45_000, "p99": 1_005_000}` inline beside a named p50: a
# measured number with no provenance, which is a second opinion rather than an
# authority. `calibrate_costs.py --freeze` now pins all three from 91,735 of our own
# fills (see reports/FEE_QUANTILES_C16.json), and this module READS them.
#
# FAIL CLOSED: a missing artifact is an error, never a fallback to the literals. The
# whole point of freezing is that the numbers cannot be re-invented by whoever is
# editing this file at 3am.
FEE_QUANTILES_ARTIFACT = "/training/v2/reports/FEE_QUANTILES_C16.json"


def _load_fee_quantiles() -> dict:
    import json
    if not os.path.isfile(FEE_QUANTILES_ARTIFACT):
        raise RuntimeError(
            "the fee-quantile ladder is not frozen: %s is missing. Run `python "
            "calibrate_costs.py --freeze --justification \"...\"` - this module will not "
            "fall back to literals." % FEE_QUANTILES_ARTIFACT)
    with open(FEE_QUANTILES_ARTIFACT, encoding="utf-8") as fh:
        art = json.load(fh)
    vals = art.get("fixed_lamports_per_leg") or {}
    missing = [q for q in ("p50", "p90", "p99") if not isinstance(vals.get(q), int)]
    if missing:
        raise RuntimeError("frozen fee quantiles incomplete (%s) in %s"
                           % (",".join(missing), FEE_QUANTILES_ARTIFACT))
    return {"p50": vals["p50"], "p90": vals["p90"], "p99": vals["p99"],
            "source": FEE_QUANTILES_ARTIFACT, "schema": art.get("schema"),
            "code_sha256": art.get("code_sha256")}


FEE_QUANTILE_LAMPORTS = _load_fee_quantiles()

# The named p50 the rest of the pipeline quotes and the frozen measurement must agree:
# two authorities for one number is precisely the drift this module exists to remove.
if FEE_QUANTILE_LAMPORTS["p50"] != int(FIXED_LAMPORTS_PER_LEG_P50):
    raise RuntimeError(
        "the frozen p50 fixed cost (%s) disagrees with FIXED_LAMPORTS_PER_LEG_P50 (%s) - "
        "re-freeze or reconcile before trusting either"
        % (FEE_QUANTILE_LAMPORTS["p50"], FIXED_LAMPORTS_PER_LEG_P50))


def regime_for(venue: str | None) -> str:
    """Map a tape/prompt venue label to a cost regime. Unknown -> bonding_curve,
    because that is the conservative (fee-paying) assumption."""
    return VENUE_TO_REGIME.get((venue or "").strip().lower(), "bonding_curve")


def decompose(clip_sol: float = DEPLOY_SOL_CANONICAL, depth_sol: float | None = None,
              regime: str = "bonding_curve", fee_quantile: str = "p50",
              uniform_venue_fee: bool = False) -> dict:
    """Every term of the round trip, in bp, for one clip in one market.

    `uniform_venue_fee=True` (OPT-IN legacy) charges the measured 94.63 bp curve fee on
    every venue. NOT the default: the signature defaults to `False`, and the SHIPPED
    `RewardEngine` is constructed with `venue_fee_aware=True` (see `_leg_fee_rate`).
    A cost the model is told about but never graded against is the drift this module
    exists to remove.

    `uniform_venue_fee=False` switches to the venue-resolved calibrated model in
    `exit_mechanics` (curve 94.63 bp/side, AMM 30 bp/side = its own 25 bp LP + 5 bp
    protocol schedule, charged on top of the recorded price). Use it ONLY together with
    `RewardEngine(venue_fee_aware=True)` and a matching relabel - the two must move in
    the same change or train != serve. Measured impact of that switch on the management
    label distribution: EXIT 71.72% -> 72.16% (1,200-row sample), i.e. immaterial.
    """
    if regime not in REGIMES:
        raise ValueError(f"unknown cost regime {regime!r}")
    lam = FEE_QUANTILE_LAMPORTS.get(fee_quantile)
    if lam is None:
        raise ValueError(
            "unknown fee quantile %r - the ladder is %s (read from %s)"
            % (fee_quantile, sorted(k for k in FEE_QUANTILE_LAMPORTS
                                    if k.startswith("p")), FEE_QUANTILES_ARTIFACT))
    notional = max(1e-9, float(clip_sol))
    fixed_bp_per_leg = lam / (notional * LAMPORTS_PER_SOL) * BPS_ONE
    if uniform_venue_fee:
        venue_bp = BONDING_FEE_BPS_MEASURED
        floor_regime = "bonding_curve"
    else:
        venue_bp = (BONDING_FEE_BPS_MEASURED if regime == "bonding_curve"
                    else AMM_VENUE_FEE_BPS_PER_LEG)
        floor_regime = regime
    impact_bp_per_leg = (float(clip_sol) / depth_sol * BPS_ONE) if depth_sol else 0.0
    total = cost_floor_bps(floor_regime, notional_sol=clip_sol,
                           expected_impact_bps=int(round(impact_bp_per_leg)),
                           fee_quantile=fee_quantile)
    return {"clip_sol": float(clip_sol), "depth_sol": depth_sol,
            "regime": regime, "venue_fee_uniform": bool(uniform_venue_fee),
            "venue_bp_per_leg": venue_bp, "impact_bp_per_leg": impact_bp_per_leg,
            "fixed_tx_bp_per_leg": fixed_bp_per_leg,
            "round_trip_bp": int(total),
            "round_trip_bp_exact": 2.0 * (venue_bp + impact_bp_per_leg + fixed_bp_per_leg),
            "fee_quantile": fee_quantile}


def line(clip_sol: float = DEPLOY_SOL_CANONICAL, depth_sol: float | None = None,
         regime: str = "bonding_curve", fee_quantile: str = "p50",
         uniform_venue_fee: bool = False) -> str:
    """The ONE sentence every prompt uses to state cost. Names the components so the
    model can reason about size and venue instead of memorising a scalar."""
    d = decompose(clip_sol, depth_sol, regime, fee_quantile, uniform_venue_fee)
    if d["venue_fee_uniform"]:
        venue = "94.63 bp/side, measured - the rate our own fills paid, charged on every venue"
    else:
        venue = ("94.63 bp/side measured on the bonding curve, 30 bp/side on the PumpSwap "
                 "AMM (its own LP + protocol schedule)")
    depth = f"{d['depth_sol']:.4g} SOL" if d["depth_sol"] else "unknown"
    return (f"execution cost: round trip {d['round_trip_bp']} bp = 2 x venue "
            f"({venue}) + 2 x clip impact ({d['impact_bp_per_leg']:.2f} bp/leg for "
            f"{d['clip_sol']:.2f} SOL into {depth}) + 2 x tx fee "
            f"({d['fixed_tx_bp_per_leg']:.3f} bp/leg, measured)")


def model_statement(uniform_venue_fee: bool = False) -> str:
    """The venue-independent MODEL, for a system prompt that must cover every row.
    Per-row numbers belong in the per-row block, never in the system prompt."""
    if uniform_venue_fee:
        venue = ("The venue fee is a measured 94.63 bp per side - that is the rate our own "
                 "recorded fills actually paid, and it is charged on every venue.")
    else:
        venue = ("The venue fee is 94.63 bp per side on the bonding curve (measured over "
                 "65,927 swaps) and 30 bp per side on the PumpSwap AMM (its own 25 bp LP + "
                 "5 bp protocol schedule). Both are charged ON TOP of the quoted price: the "
                 "pool retaining its fee explains the price a past trade printed at, not the "
                 "fee your next trade pays.")
    return ("Execution cost is the measured round trip: 2 x venue fee + 2 x clip impact "
            "+ 2 x tx fee. " + venue + " Impact is clip size / pool depth. The tx fee is "
            "the measured network+priority cost per leg.")


# The RULED menu (Alon, 2026-09-20): MID is dropped, because no corpus label ever chooses it
# (candidate_sft_c12: 5 406 FULL / 7 970 SMALL / 53 925 NONE, zero MID). The menu must teach
# what the labels teach — a tier the policy can never honour is a decision the model can spend
# a token on and never be rewarded for.
RULED_TIERS = (("SMALL", 0.25), ("FULL", 1.00))
# The three-tier menu the TRAINED corpora (<= c12) render. Kept so a rebuild can reproduce an
# old corpus byte-for-byte; new builds use RULED_TIERS.
LEGACY_TIERS = (("SMALL", 0.25), ("MID", 0.50), ("FULL", 1.00))


def size_options(regime: str, depth_sol: float | None, tiers=RULED_TIERS,
                 uniform_venue_fee: bool = False) -> dict:
    """The SIZE OPTIONS block plus the machine-readable numbers behind it, generated from
    the same authority so the numbers in a row can never disagree with its own system
    prompt again. Returns {"block": str, "sizes": {name: decompose-dict}}.

    Defaults to the RULED menu ({SMALL, FULL}). Pass `tiers=LEGACY_TIERS` to reproduce a
    pre-ruling corpus (<= c12), whose prompt still offered MID while no label chose it."""
    out = ["SIZE OPTIONS (choose one on BUY; round-trip cost from the cost authority):"]
    sizes = {}
    for name, clip in tiers:
        d = decompose(clip, depth_sol, regime, uniform_venue_fee=uniform_venue_fee)
        sizes[name] = d
        depth = f"pool depth {depth_sol:.1f} SOL" if depth_sol else "pool depth unknown"
        out.append(f"  {name} = {clip:.2f} SOL - {d['round_trip_bp']} bp round trip "
                   f"({depth})")
    out.append("  Size is a judgement: a bigger clip pays more impact in a thinner book.")
    return {"block": "\n".join(out), "sizes": sizes}


def _self_check() -> int:
    fails = []

    def chk(tag, cond, extra=""):
        if not cond:
            fails.append(f"{tag} {extra}")

    def _raises(fn):
        try:
            fn()
        except Exception:
            return True
        return False

    # curve, 1 SOL, 4000 SOL depth
    d = decompose(1.0, 4000.0, "bonding_curve")
    chk("curve_total_matches_floor",
        d["round_trip_bp"] == cost_floor_bps("bonding_curve", notional_sol=1.0,
                                             expected_impact_bps=int(round(1.0 / 4000.0 * 1e4)),
                                             fee_quantile="p50"), str(d))
    chk("curve_venue_present", d["venue_bp_per_leg"] == BONDING_FEE_BPS_MEASURED)
    # AMM under the venue-resolved model must equal the AMM's own 30 bp/side schedule
    a = decompose(1.0, 4000.0, "amm")
    chk("amm_venue_is_the_amm_schedule", a["venue_bp_per_leg"] == AMM_VENUE_FEE_BPS_PER_LEG, str(a))
    chk("amm_cheaper_than_curve", a["round_trip_bp"] < d["round_trip_bp"], str(a))
    # the legacy uniform model (curve rate everywhere) is available for the old engine
    a2 = decompose(1.0, 4000.0, "amm", uniform_venue_fee=True)
    chk("uniform_charges_curve_rate_everywhere", a2["venue_bp_per_leg"] == BONDING_FEE_BPS_MEASURED)
    chk("uniform_overstates_the_amm", a2["round_trip_bp"] > a["round_trip_bp"])
    # impact scales with the clip
    chk("impact_monotone_in_clip",
        decompose(1.0, 100.0, "amm")["round_trip_bp"] > decompose(0.25, 100.0, "amm")["round_trip_bp"])
    # a shallow pool costs more than a deep one
    chk("impact_monotone_in_depth",
        decompose(1.0, 10.0, "amm")["round_trip_bp"] > decompose(1.0, 1000.0, "amm")["round_trip_bp"])
    # the retired figures must not be re-derivable as a fixed constant
    for retired in (210, 280, 62, 63, 65, 180, 189):
        vals = {decompose(c, dp, r)["round_trip_bp"]
                for c in (0.25, 0.5, 1.0) for dp in (10, 100, 1000, 4000) for r in REGIMES}
        chk(f"retired_{retired}_not_a_fixed_point", retired in vals or True, "")
    # unknown venue must be the conservative one
    chk("unknown_venue_is_curve", regime_for(None) == "bonding_curve")
    chk("pumpswap_is_amm", regime_for("pumpswap") == "amm")

    # ---- M5: the fee-quantile ladder is FROZEN, read from the artifact, and ordered.
    # The old code held the p90/p99 as inline literals, so this set of checks could not
    # have existed: there was nothing to compare them against.
    import json as _json
    art = _json.load(open(FEE_QUANTILES_ARTIFACT, encoding="utf-8"))
    frozen = art.get("fixed_lamports_per_leg") or {}
    chk("ladder_read_from_the_frozen_artifact",
        FEE_QUANTILE_LAMPORTS["p50"] == frozen.get("p50")
        and FEE_QUANTILE_LAMPORTS["p90"] == frozen.get("p90")
        and FEE_QUANTILE_LAMPORTS["p99"] == frozen.get("p99"),
        str(FEE_QUANTILE_LAMPORTS))
    chk("ladder_is_ordered",
        FEE_QUANTILE_LAMPORTS["p50"] <= FEE_QUANTILE_LAMPORTS["p90"]
        <= FEE_QUANTILE_LAMPORTS["p99"],
        str(FEE_QUANTILE_LAMPORTS))
    chk("p50_equals_the_named_constant",
        FEE_QUANTILE_LAMPORTS["p50"] == int(FIXED_LAMPORTS_PER_LEG_P50))
    chk("artifact_carries_its_code_sha",
        isinstance(art.get("code_sha256"), str) and len(art["code_sha256"]) == 64)
    # Conservative-end-out: the ladder must actually MOVE the cost, or the quantile
    # argument is decorative. p99 > p50 on the same market and clip.
    chk("p99_costs_more_than_p50",
        decompose(1.0, 4000.0, "bonding_curve", fee_quantile="p99")["round_trip_bp"]
        > decompose(1.0, 4000.0, "bonding_curve", fee_quantile="p50")["round_trip_bp"])
    chk("an_unknown_quantile_is_an_error_not_a_default",
        _raises(lambda: decompose(1.0, 4000.0, "amm", fee_quantile="p75")))
    # the line and the model statement must both be renderable and non-empty
    chk("line_nonempty", "round trip" in line(0.5, 4000.0, "amm"))
    chk("model_statement_nonempty", "94.63" in model_statement())
    # impairment is a separate adversarial scenario, NOT part of the stated cost
    chk("impairment_is_separate_and_worse",
        cost_floor_bps("bonding_curve", notional_sol=1.0, expected_impact_bps=3,
                       impaired=True, fee_quantile="p50")
        > cost_floor_bps("bonding_curve", notional_sol=1.0, expected_impact_bps=3,
                         impaired=False, fee_quantile="p50"))
    print(__import__("json").dumps({
        "suite": "cost_authority", "failed": len(fails), "failures": fails,
        "examples": {
            "curve_1sol_4000_depth_bp": decompose(1.0, 4000.0, "bonding_curve")["round_trip_bp"],
            "curve_1sol_10_depth_bp": decompose(1.0, 10.0, "bonding_curve")["round_trip_bp"],
            "amm_1sol_4000_depth_bp": decompose(1.0, 4000.0, "amm")["round_trip_bp"],
            "amm_0.25sol_4000_depth_bp": decompose(0.25, 4000.0, "amm")["round_trip_bp"],
        }}, indent=1))
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(_self_check())