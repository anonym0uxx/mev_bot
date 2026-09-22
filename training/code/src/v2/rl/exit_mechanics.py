#!/usr/bin/env python
"""Exit-policy MECHANICS: the market physics our reward must obey.

Ported from the Rust repo (/mnt/data/repos/mev_bot/rust), which is the
authoritative implementation for live execution:

  * pump-quant-strategy/src/exit_ladder.rs
      derive_target_bps(floor_bps, margin_bps, mfe_p25_bps) -> Option<u32>
      protection_level_fp(peak_price_fp, entry_price_fp, trail_bps, hard_sl_bps)
  * pump-quant-simulator/src/terminal_loss.rs
      TerminalLossPolicy::{WriteToZero, ResidualBps, FixedResidualLamports}
      ("An unexitable position may never be valued at displayed price")
  * pump-quant-simulator/src/fill.rs (Mode C calibrated adversarial execution)
      ExitImpairment{first_sell_penalty_bps, retry_slippage_bps, fee_escalation_bps}
      test magnitudes: 200 / 300 / 50 bp; Pessimistic doubles them
  * pump-quant-protocol/src/curve.rs
      FEE_NUMERATOR=100, FEE_DENOMINATOR=10_000  -> 100 bp bonding-curve fee

WHY THESE ARE BASELINE MATH, NOT RULES FOR THE MODEL: the trained policy must be
free to invent its own ladder/trail parameters. These functions are (a) the
mechanical TRUTH the simulator applies to ANY policy - so the model cannot wish
away costs or sell an unexitable position at the mark - and (b) the reference set
its advantage is measured against. They are never a fixed answer menu.

All bps math is integer to mirror the Rust fixed-point discipline.
"""
from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass, asdict

BPS_ONE = 10_000

# --- fixed per-leg cost: CALIBRATED FROM OUR OWN FILLS (authoritative) -------
# Measured 2026-09-13 over 91,735 recorded fills (calibrate_costs.py):
#   tx fee p50 = 10_000 lamports  (p%, pumpswap 5_500 | pumpfun 25_000)
#   p90 = 45_000 | p99 = 1_005_000 | mean 66_521 | max 400_005_000
# The Rust reference books 150_000/leg (incl. a 100k Jito tip our fills do not
# pay) - NOT used: the operator ruling is that our own measured numbers win, and
# Rust is a reference implementation, not the authority.
# --- THE deploy notional: ONE source of truth -------------------------------
# The cost floor is in bps, but its dominant term is a FIXED lamport tx fee, so
# its bp value scales as 1/notional. Two components quietly disagreeing about the
# deploy size therefore mis-state cost by a large factor (p99 congestion is
# 10.05 bp/leg at 1.0 SOL but 100.5 bp/leg at 0.1 SOL). 1.0 SOL is the notional
# the v3 RL path actually trades - the whole scoring chain (grpo_dataset,
# score_wall_eval, v3_bridge, rl_reward_v3) - so it is canonical. Import THIS
# rather than hardcoding a size anywhere.
DEPLOY_SOL_CANONICAL = 1.0

FIXED_LAMPORTS_PER_LEG_P50 = 10_000
FIXED_LAMPORTS_PER_LEG_P90 = 45_000
FIXED_LAMPORTS_PER_LEG_P99 = 1_005_000
FIXED_LAMPORTS_BY_VENUE = {"pumpswap": 5_500, "pumpfun": 25_000}
# Reference-only value from the Rust cost model (kept for cross-checking only):
RUST_FIXED_LAMPORTS_PER_LEG_REFERENCE = 150_000

# Venue fee: the rate OURS trade pays, per side, on top of the recorded price.
# * bonding curve - 94.63 bp is the direct measurement of what our fills paid
#   (trader leg minus pool leg over 65,927 clean single-hop swaps); the pool does not
#   retain it, so pricing the NEXT trade at the observed price would let us trade free.
# * PumpSwap AMM - the pool DOES retain its fee (measured gross/net pool-vs-trader ratio
#   p50 = 1.0), so the fee is already reflected in the price a trade prints at. A new
#   trade still pays the venue's own schedule, which is 0.25% to LPs + 5 bp protocol
#   = 30 bp per side. Charging 0 understates our cost; charging the 94.63 bp curve rate
#   overstates it by 64.63 bp/side, which is what the uniform legacy model did and it
#   biased the whole corpus against graduated mints.
AMM_VENUE_FEE_BPS_PER_LEG = 30     # charged per side on a PumpSwap fill
BONDING_FEE_BPS_MEASURED = 94.63   # our notional-weighted measurement, 65,927 swaps
BONDING_FEE_BPS_PROTOCOL = 100     # protocol nominal
BONDING_FEE_BPS_RUST_APP = 125     # Rust app booking (reference only)

# --- Exit impairment: MEASURED, frozen, never cheaper by accident -----------------
#
# The Rust Mode-C magnitudes (200 / 300 / 50 bp) are TEST FIXTURES for a different cost
# model whose fixed leg was already measured ~15x our real cost. They are kept as a
# recorded REFERENCE, not as a floor on our measurement: a floor would re-introduce
# another codebase's booking rates into our reward, which is the exact substitution the
# operator ruling forbids. What replaces the floor is the LEVEL - training charges the
# measured adverse quantile, the gate and evaluation charge 'pessimistic' (x2) - plus the
# requirement that any measured value BELOW the reference carries a recorded
# justification in the freeze file (`calibrate_impairment.py --freeze --justification`).
IMPAIRMENT_FREEZE = "/training/v2/reports/EXIT_IMPAIRMENT_C14.json"
RUST_REFERENCE_IMPAIRMENT = {"first_sell_penalty_bps": 200, "retry_slippage_bps": 300,
                             "fee_escalation_lamports": 50_000}
# Used ONLY when the freeze is absent; `source` then reads `reference_unfrozen`, which is
# what the readiness gate refuses on.
UNFROZEN_FALLBACK = dict(RUST_REFERENCE_IMPAIRMENT)
DEFAULT_IMPAIRMENT = UNFROZEN_FALLBACK  # back-compat alias
PESSIMISTIC_FACTOR = 2
_IMPAIRMENT_CACHE = None


def impairment_source() -> dict:
    """The frozen, MEASURED impairment (`calibrate_impairment.py` over our own fills)."""
    global _IMPAIRMENT_CACHE
    if _IMPAIRMENT_CACHE is None:
        try:
            with open(IMPAIRMENT_FREEZE, encoding="utf-8") as fh:
                obj = json.load(fh)
            acc = obj.get("accounting") or {}
            _IMPAIRMENT_CACHE = {
                "source": "frozen",
                "values": {k: int(v) for k, v in obj["values"].items()},
                "quantile": acc.get("quantile"),
                "population": acc.get("population"),
                "reference_floor": obj.get("reference_floor"),
                "justification": bool(obj.get("justification")),
            }
        except Exception:
            _IMPAIRMENT_CACHE = {"source": "reference_unfrozen",
                                 "values": dict(UNFROZEN_FALLBACK),
                                 "quantile": None, "population": None}
    return _IMPAIRMENT_CACHE

# --- predeclared terminal-loss policies (Rust terminal_loss.rs) --------------
TERMINAL_WRITE_TO_ZERO = "write_to_zero"
TERMINAL_RESIDUAL_BPS = "residual_bps"
TERMINAL_FIXED_LAMPORTS = "fixed_residual_lamports"


def derive_target_bps(floor_bps: int, margin_bps: int, mfe_p25_bps=None):
    """Per-market profit target. Returns None when the market is INADMISSIBLE.

    There is no global-constant TP. The target is always floor + margin, and if
    the market's own conditional upside evidence (25th percentile favourable
    excursion) cannot even cover that, the market must be declared inadmissible
    rather than handed a guaranteed-net-loss target.
    """
    target = int(floor_bps) + int(margin_bps)
    if mfe_p25_bps is not None and int(mfe_p25_bps) < target:
        return None
    return target


def protection_level_fp(peak_price_fp: float, entry_price_fp: float,
                        trail_bps: int, hard_sl_bps: int) -> float:
    """Protection (stop) level = max(trail-from-peak, hard-stop-from-entry).

    Armed from entry, so there is no TP-gated dead zone, and monotone in the peak:
    a higher peak can never lower the level.
    """
    trail_factor = max(0, BPS_ONE - int(trail_bps)) / BPS_ONE
    sl_factor = max(0, BPS_ONE - int(hard_sl_bps)) / BPS_ONE
    return max(float(peak_price_fp) * trail_factor, float(entry_price_fp) * sl_factor)


def terminal_value_lamports(basis_lamports: int, policy: str, residual_bps: int = 0,
                            fixed_lamports: int = 0) -> int:
    """Recovered value of a TERMINALLY UNEXITABLE position.

    Invariant (Rust §38): the result is always in [0, basis]. Never valued at the
    displayed/appreciated mark.
    """
    b = int(basis_lamports)
    if policy == TERMINAL_WRITE_TO_ZERO:
        v = 0
    elif policy == TERMINAL_RESIDUAL_BPS:
        v = b * min(int(residual_bps), BPS_ONE) // BPS_ONE
    elif policy == TERMINAL_FIXED_LAMPORTS:
        v = min(int(fixed_lamports), b)
    else:
        raise ValueError(f"unknown terminal-loss policy {policy!r}")
    return max(0, min(v, b))


def impairment_bps(level: str = "realistic") -> dict:
    """Exit impairment magnitudes; 'pessimistic' doubles them (the gate's stress level).

    TRAIN ON THE MEASURED ADVERSE QUANTILE, GATE ON THE STRESS LEVEL - the same shape the
    cost floor uses (charge p50, evaluate at p99). 'realistic' is our own fills'
    p25-adverse shortfall; 'pessimistic' doubles every magnitude.
    """
    f = PESSIMISTIC_FACTOR if level == "pessimistic" else 1
    src = impairment_source()
    out: dict = {k: int(v) * f for k, v in src["values"].items()}
    out["source"] = src["source"]
    out["level"] = level
    return out


def exit_proceeds_lamports(gross_lamports: int, first_sell: bool, retries: int = 0,
                           level: str = "realistic") -> dict:
    """Mode-C-style net proceeds for a sell, including impairment.

    Charges, in this order: the first-sell penalty (once per episode), retry slippage per
    retry, and the congestion adder on the fixed leg IN LAMPORTS - because the fixed leg
    is size-dependent in bps and a bp figure would be a remembered number. Never returns
    more than the gross, never less than zero, and is monotone in `retries`.
    """
    imp = impairment_bps(level)
    gross = int(gross_lamports)
    p_bps = imp["first_sell_penalty_bps"] if first_sell else 0
    p_bps += imp["retry_slippage_bps"] * max(0, int(retries))
    total = min(p_bps, BPS_ONE)
    esc = int(imp["fee_escalation_lamports"])
    net = gross * (BPS_ONE - total) // BPS_ONE - esc
    return {"gross_lamports": gross, "impairment_bps": total,
            "escalation_lamports": esc,
            "net_lamports": max(0, min(net, gross)),
            "level": level, "source": imp["source"]}


LAMPORTS_PER_SOL = 1_000_000_000


def cost_floor_bps(regime: str = "amm", notional_sol: float = DEPLOY_SOL_CANONICAL,
                   expected_impact_bps: int = 0, impaired: bool = False,
                   fee_quantile: str = "p50") -> int:
    """Measured round-trip cost floor in bps: the number a TP must clear.

    Built from OUR calibrated numbers:
    * the AMM charges its own schedule on top of the recorded price: 25 bp LP + 5 bp
      protocol = 30 bp per side (gross/net p50 = 1.0 is the price a PAST trade printed
      at, not the fee OUR next trade pays).
    * the bonding curve DOES deduct its fee from the SOL leg (measured 94.63 bp).
    * the dominant nibble is the FIXED per-leg tx fee, in lamports, so it is
      SIZE-DEPENDENT in bps: 10_000 lamports is 0.1 bp/leg at 1 SOL but 1 bp/leg at
      0.1 SOL, and 1_005_000 lamports (p99 congestion) is 100 bp/leg at 0.1 SOL.
      Use fee_quantile='p90'/'p99' for adversarial scenarios.
    """
    lam = {"p50": FIXED_LAMPORTS_PER_LEG_P50, "p90": FIXED_LAMPORTS_PER_LEG_P90,
           "p99": FIXED_LAMPORTS_PER_LEG_P99}[fee_quantile]
    notional_lamports = max(1.0, float(notional_sol) * LAMPORTS_PER_SOL)
    fixed_bps_per_leg = lam / notional_lamports * BPS_ONE
    venue_bps = (BONDING_FEE_BPS_MEASURED if regime == "bonding_curve"
                 else AMM_VENUE_FEE_BPS_PER_LEG)
    floor = 2.0 * (venue_bps + fixed_bps_per_leg) + 2.0 * int(expected_impact_bps)
    if impaired:
        imp = impairment_bps("realistic")
        # bps part + the congestion adder expressed at THIS notional (the fixed leg is
        # size-dependent, so the lamport adder has to be converted, not added as bps).
        floor += imp["first_sell_penalty_bps"] + (
            imp["fee_escalation_lamports"] / notional_lamports * BPS_ONE)
    return int(round(floor))


def is_admissible(floor_bps: int, margin_bps: int, mfe_p25_bps=None) -> dict:
    """Gate: a market whose own evidence cannot clear the floor is not tradeable."""
    t = derive_target_bps(floor_bps, margin_bps, mfe_p25_bps)
    return {"admissible": t is not None, "target_bps": t,
            "floor_bps": int(floor_bps), "margin_bps": int(margin_bps),
            "mfe_p25_bps": mfe_p25_bps}


def _self_check() -> int:
    fails = []

    def chk(tag, cond, extra=""):
        if not cond:
            fails.append(f"{tag} {extra}")

    # 1. TP is never a global constant and refuses inadmissible markets
    chk("target_is_floor_plus_margin", derive_target_bps(220, 300, None) == 520)
    chk("inadmissible_when_upside_below_target", derive_target_bps(220, 300, 400) is None)
    chk("admissible_when_upside_covers", derive_target_bps(220, 300, 520) == 520)
    # 2. protection: armed from entry, monotone in peak, no dead zone
    e = 1.0
    chk("protection_equals_sl_at_entry",
        abs(protection_level_fp(e, e, 500, 400) - 0.96) < 1e-12)
    p1 = protection_level_fp(2.0, e, 500, 400)
    p2 = protection_level_fp(3.0, e, 500, 400)
    chk("protection_monotone_in_peak", p2 > p1 > 0.96)
    chk("sl_saturates_at_zero", protection_level_fp(1.0, 1.0, 20_000, 20_000) == 0.0)
    # 3. terminal loss may never exceed basis (Rust §38)
    chk("write_to_zero", terminal_value_lamports(10**9, TERMINAL_WRITE_TO_ZERO) == 0)
    chk("residual_capped_at_basis",
        terminal_value_lamports(10**9, TERMINAL_RESIDUAL_BPS, residual_bps=50_000) == 10**9)
    chk("fixed_residual_capped",
        terminal_value_lamports(10**8, TERMINAL_FIXED_LAMPORTS,
                                fixed_lamports=10**12) == 10**8)
    # 4. impairment reduces proceeds, never increases them
    gross = 10**9
    r = exit_proceeds_lamports(gross, first_sell=True, retries=1)
    p = exit_proceeds_lamports(gross, first_sell=True, retries=1, level="pessimistic")
    chk("impairment_reduces_proceeds", r["net_lamports"] < gross)
    chk("pessimistic_stricter_than_realistic", p["net_lamports"] < r["net_lamports"])
    chk("impairment_never_exceeds_gross",
        exit_proceeds_lamports(gross, True, retries=10**6)["net_lamports"] >= 0)
    # 4b. the impairment is the MEASURED one, and the invariants that keep it honest:
    #     strictly positive magnitudes, monotone in retries, congestion charged in
    #     lamports, and a `source` the readiness gate can key on.
    src = impairment_source()
    imp = impairment_bps("realistic")
    chk("impairment_values_positive",
        all(imp[k] > 0 for k in ("first_sell_penalty_bps", "retry_slippage_bps",
                                 "fee_escalation_lamports")), str(imp))
    chk("impairment_source_labelled", src["source"] in ("frozen", "reference_unfrozen"),
        src["source"])
    r0 = exit_proceeds_lamports(gross, True, retries=0)["net_lamports"]
    r1 = exit_proceeds_lamports(gross, True, retries=1)["net_lamports"]
    r2 = exit_proceeds_lamports(gross, True, retries=2)["net_lamports"]
    chk("impairment_monotone_in_retries", r0 > r1 > r2, f"{r0} {r1} {r2}")
    chk("impairment_charged_once_for_first_sell",
        exit_proceeds_lamports(gross, False, retries=0)["net_lamports"] > r0)
    chk("escalation_is_lamports_not_bps",
        exit_proceeds_lamports(gross, True)["escalation_lamports"] ==
        imp["fee_escalation_lamports"])
    # 5. cost floor: shipped venue fees + size-dependent fixed cost
    f_amm = cost_floor_bps("amm", notional_sol=1.0, expected_impact_bps=50)
    f_curve = cost_floor_bps("bonding_curve", notional_sol=1.0, expected_impact_bps=50)
    f_small = cost_floor_bps("amm", notional_sol=0.1, expected_impact_bps=50)
    chk("curve_floor_above_amm_floor", f_curve > f_amm, f"{f_curve} vs {f_amm}")
    chk("impairment_raises_floor",
        cost_floor_bps("amm", notional_sol=1.0, expected_impact_bps=50,
                       impaired=True) > f_amm,
        "the impaired floor must exceed the SAME floor with the same impact")
    # RETIRED: 1 SOL at 0 AMM venue = 100.2 -> 100 (superseded by 30 bp/side below)
    # AMM round trip at 1 SOL with a 50 bp/leg impact allowance:
    #   2 x (30 venue + 0.1 tx) + 2 x 50 = 160.2 -> 160
    # The old expectation was 100, from a model that charged the AMM ZERO venue fee on
    # the argument that the pool retains its fee (measured gross/net ratio p50 = 1.0) and
    # it is therefore "inside the price". That is true of the price a PAST trade printed
    # at, not of the fee OUR next trade pays: PumpSwap charges 0.25% to LPs + 5 bp to the
    # protocol = 30 bp per side. Charging 0 understated our cost; charging the curve's
    # 94.63 bp overstated it by 64.63 bp/side and biased the corpus against graduated
    # mints, which are the majority of the tape.
    chk("amm_floor_1sol_value", f_amm == 160, f_amm)
    # fixed cost in lamports must make SMALL size strictly more expensive in bps
    chk("fixed_cost_is_size_dependent", f_small > f_amm, f"{f_small} vs {f_amm}")
    # congestion (p99) at our size must be able to erase a scalp rung
    f_p99 = cost_floor_bps("amm", notional_sol=0.1, expected_impact_bps=50,
                           fee_quantile="p99")
    chk("p99_congestion_dominates_floor", f_p99 > 250, f_p99)
    # 6. the admissibility gate is the composition of both
    g = is_admissible(f_amm, 300, mfe_p25_bps=100)
    chk("gate_rejects_thin_upside", not g["admissible"])
    chk("gate_accepts_adequate_upside",
        is_admissible(f_amm, 300, mfe_p25_bps=f_amm + 300)["admissible"])
    print(json.dumps({"suite": "exit_mechanics", "failed": len(fails),
                      "failures": fails, "amm_floor_1sol_bps": f_amm,
                      "curve_floor_1sol_bps": f_curve,
                      "amm_floor_0p1sol_bps": f_small}, indent=1))
    return 1 if fails else 0


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--self-check", action="store_true")
    a = ap.parse_args()
    if a.self_check:
        return _self_check()
    ap.print_help()
    return 0


if __name__ == "__main__":
    sys.exit(main())