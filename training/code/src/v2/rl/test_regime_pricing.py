#!/usr/bin/env python
"""UNIT TESTS for the two-regime mechanics engine (regime_pricing.py).

Run:  /home/alon/qwen27b-venv/bin/python test_regime_pricing.py
Exit: 0 = all pass, 1 = a failure (every failing tag is printed).

Covers, for BOTH regimes:
  * constant-product invariants on the reserves (k never falls),
  * price impact / slippage computed from OUR OWN order size,
  * the fee policy of each venue (bonding: fee on the SOL leg both ways;
    pumpswap: LP fee retained in the pool + protocol/creator extraction),
  * LOUD refusal when reserves are absent, mismatched, stale, future-dated,
    non-positive, or smaller than our order.

The AMM-path test loads a SYNTHETIC pool-reserve parquet written in the exact
schema the forward LaserStream capture will produce (see
FORWARD_RESERVE_CAPTURE_PLAN.md). It is a fixture, not market history: it exists
to prove the loader -> oracle -> price path works so the FIRST forward pumpswap
fill after capture is priced correctly.
"""
from __future__ import annotations

import json
import os
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import numpy as np  # noqa: E402

from regime_pricing import (  # noqa: E402
    Regime, ReserveState, ReserveUnavailable, ReserveStale, ReserveLookahead,
    RegimeMismatch, OrderTooLarge, MechanicsEngine, DictReserveOracle,
    pool_reserve_oracle, price_fill, price_fill_lamports_per_raw_token,
    fill_from_token_leg,
    bonding_params, amm_params, cp_buy_out, cp_sell_out,
    LAMPORTS_PER_SOL, V0_LAMPORTS, T0_RAW, V_GRAD_LAMPORTS,
    AMM_LP_FEE_RATE, AMM_TRADER_EXTRACTION_RATE,
)

T0 = 1_800_000_000_000
FAILS = []
CHECKS = [0]


def chk(tag, cond, extra=""):
    ok = bool(cond)
    CHECKS[0] += 1
    print(("[PASS] " if ok else "[FAIL] ") + tag + ((" -- " + extra) if extra and not ok else ""))
    if not ok:
        FAILS.append(tag)


def curve_state(vs=V0_LAMPORTS, vt=T0_RAW, ts=T0):
    return ReserveState(regime=Regime.BONDING_CURVE, mint="CURVEpump", sol_lamports=vs,
                        token_raw=vt, ts_unix_ms=ts, venue="pumpfun_bonding",
                        account="CurveAcct111", source="unit_test")


def pool_state(ps=500 * LAMPORTS_PER_SOL, pt=400_000_000 * 10 ** 6, ts=T0):
    return ReserveState(regime=Regime.AMM, mint="POOLpump", sol_lamports=ps, token_raw=pt,
                        ts_unix_ms=ts, venue="pumpswap", account="PoolAcct222",
                        source="unit_test")


# ---------------------------------------------------------------- bonding ----
def test_bonding_curve_regime():
    r = curve_state()
    chk("bonding: k0 identity", int(r.sol_lamports) * int(r.token_raw) == V0_LAMPORTS * T0_RAW)
    t = price_fill(regime=Regime.BONDING_CURVE, side="buy", reserves=r,
                   decision_ts_ms=T0, sol_in_lamports=2 * LAMPORTS_PER_SOL)
    chk("bonding: buy k_post >= k_pre", t.k_post >= t.k_pre)
    chk("bonding: buy impact from OUR size, positive", t.impact_only_bp > 0)
    chk("bonding: buy tokens positive", t.token_delta_raw > 0)
    chk("bonding: fee on the SOL leg",
        t.fee_lamports == int(round(2 * LAMPORTS_PER_SOL * bonding_params().fee_rate)))
    chk("bonding: px_exec == sol_in/tokens_out",
        abs(t.px_exec - 2 * LAMPORTS_PER_SOL / t.token_delta_raw) < 1e-9)
    # our own size drives the impact: 4x the order, strictly more impact
    t4 = price_fill(regime=Regime.BONDING_CURVE, side="buy", reserves=r,
                    decision_ts_ms=T0, sol_in_lamports=8 * LAMPORTS_PER_SOL)
    chk("bonding: 4x order -> strictly larger impact",
        t4.impact_only_bp > t.impact_only_bp)
    chk("bonding: impact is dx/x on the curve",
        abs(t.impact_only_bp - 1e4 * (t.order_lamports_in - t.fee_lamports) / V0_LAMPORTS) < 1e-6)
    # sell side
    s = price_fill(regime=Regime.BONDING_CURVE, side="sell", reserves=r,
                   decision_ts_ms=T0, token_in_raw=int(0.01 * T0_RAW))
    chk("bonding: sell k_post >= k_pre", s.k_post >= s.k_pre)
    chk("bonding: sell returns SOL", s.sol_delta_lamports > 0)
    chk("bonding: sell fee on the SOL leg", s.fee_lamports > 0)
    # round-trip must lose money to the fee + impact
    buy = price_fill(regime=Regime.BONDING_CURVE, side="buy", reserves=r,
                     decision_ts_ms=T0, sol_in_lamports=LAMPORTS_PER_SOL)
    r2 = ReserveState(regime=Regime.BONDING_CURVE, mint="CURVEpump",
                      sol_lamports=buy.reserves_used["sol_lamports"] + buy.order_lamports_in
                      - buy.fee_lamports,
                      token_raw=buy.reserves_used["token_raw"] - buy.token_delta_raw,
                      ts_unix_ms=T0 + 1, venue="pumpfun_bonding")
    back = price_fill(regime=Regime.BONDING_CURVE, side="sell", reserves=r2,
                      decision_ts_ms=T0 + 1, token_in_raw=buy.token_delta_raw)
    chk("bonding: round trip < 1 SOL back (fee+impact)",
        back.sol_delta_lamports < LAMPORTS_PER_SOL)
    # graduation
    near = curve_state(vs=V_GRAD_LAMPORTS - 1_000_000)
    tg = price_fill(regime=Regime.BONDING_CURVE, side="buy", reserves=near,
                    decision_ts_ms=T0, sol_in_lamports=LAMPORTS_PER_SOL)
    chk("bonding: crossing graduation flagged", tg.crosses_graduation)
    try:
        price_fill(regime=Regime.BONDING_CURVE, side="buy", reserves=near,
                   decision_ts_ms=T0, sol_in_lamports=LAMPORTS_PER_SOL,
                   refuse_cross_graduation=True)
        chk("bonding: graduation refusal", False)
    except OrderTooLarge:
        chk("bonding: graduation refusal", True)


# -------------------------------------------------------------------- amm ----
def test_amm_regime():
    p = pool_state()
    k0 = p.sol_lamports * p.token_raw
    t = price_fill(regime=Regime.AMM, side="buy", reserves=p,
                   decision_ts_ms=T0, sol_in_lamports=5 * LAMPORTS_PER_SOL)
    chk("amm: k_post >= k_pre", t.k_post >= t.k_pre)
    chk("amm: k taken from the pool's own reserves",
        abs(t.k_pre - k0) / k0 < 1e-12)
    chk("amm: buy impact from OUR size, positive", t.impact_only_bp > 0)
    chk("amm: fee = LP + protocol + creator on the trader leg",
        abs(t.fee_lamports / t.order_lamports_in - (AMM_LP_FEE_RATE + AMM_TRADER_EXTRACTION_RATE)) < 0.002,
        "fee %.6f of order" % (t.fee_lamports / t.order_lamports_in))
    chk("amm: px_exec == sol_in/tokens_out",
        abs(t.px_exec - 5 * LAMPORTS_PER_SOL / t.token_delta_raw) < 1e-9)
    t4 = price_fill(regime=Regime.AMM, side="buy", reserves=p,
                    decision_ts_ms=T0, sol_in_lamports=20 * LAMPORTS_PER_SOL)
    chk("amm: 4x order -> strictly larger impact", t4.impact_only_bp > t.impact_only_bp)
    chk("amm: slippage from our own size", t.slippage_lamports > 0)
    s = price_fill(regime=Regime.AMM, side="sell", reserves=p,
                   decision_ts_ms=T0, token_in_raw=int(0.01 * p.token_raw))
    chk("amm: sell k_post >= k_pre", s.k_post >= s.k_pre)
    chk("amm: sell returns SOL", s.sol_delta_lamports > 0)
    chk("amm: sell fee > 0", s.fee_lamports > 0)
    chk("amm: sell sol_out < gross (fee extracted)",
        s.sol_delta_lamports < s.sol_delta_lamports + s.fee_lamports)
    # deeper pool -> smaller impact for the SAME order (impact is reserve-relative)
    deep = pool_state(ps=50_000 * LAMPORTS_PER_SOL, pt=40_000_000_000 * 10 ** 6)
    td = price_fill(regime=Regime.AMM, side="buy", reserves=deep,
                    decision_ts_ms=T0, sol_in_lamports=5 * LAMPORTS_PER_SOL)
    chk("amm: same order on deep pool -> smaller impact",
        td.impact_only_bp < t.impact_only_bp)
    # integer constant-product helpers
    chk("amm: cp_buy_out matches the closed form",
        cp_buy_out(1000, 1000, 100) == 1000 * 100 // (1000 + 100))
    chk("amm: cp_sell_out matches the closed form",
        cp_sell_out(1000, 1000, 100) == 1000 * 100 // (1000 + 100))


# -------------------------------------------------------------- refusals ----
def test_refusals():
    for reg in (Regime.BONDING_CURVE, Regime.AMM):
        try:
            price_fill(regime=reg, side="buy", reserves=None,
                       decision_ts_ms=T0, sol_in_lamports=LAMPORTS_PER_SOL)
            chk("%s: absent reserves refused" % reg.value, False)
        except ReserveUnavailable:
            chk("%s: absent reserves refused" % reg.value, True)
    try:
        price_fill(regime=Regime.AMM, side="buy", reserves=curve_state(),
                   decision_ts_ms=T0, sol_in_lamports=LAMPORTS_PER_SOL)
        chk("amm: curve reserves refused (RegimeMismatch)", False)
    except RegimeMismatch:
        chk("amm: curve reserves refused (RegimeMismatch)", True)
    try:
        price_fill(regime=Regime.BONDING_CURVE, side="buy", reserves=pool_state(),
                   decision_ts_ms=T0, sol_in_lamports=LAMPORTS_PER_SOL)
        chk("bonding: pool reserves refused (RegimeMismatch)", False)
    except RegimeMismatch:
        chk("bonding: pool reserves refused (RegimeMismatch)", True)
    try:
        price_fill(regime=Regime.BONDING_CURVE, side="buy", reserves=curve_state(ts=T0 + 1),
                   decision_ts_ms=T0, sol_in_lamports=LAMPORTS_PER_SOL)
        chk("future-dated reserves refused loudly", False)
    except ReserveLookahead:
        chk("future-dated reserves refused loudly", True)
    try:
        price_fill(regime=Regime.BONDING_CURVE, side="buy",
                   reserves=curve_state(ts=T0 - 5_000), decision_ts_ms=T0,
                   sol_in_lamports=LAMPORTS_PER_SOL, max_stale_ms=1_000)
        chk("stale reserves refused loudly", False)
    except ReserveStale:
        chk("stale reserves refused loudly", True)
    for bad in ({"sol_lamports": 0, "token_raw": T0_RAW},
                {"sol_lamports": V0_LAMPORTS, "token_raw": 0},
                {"sol_lamports": -5, "token_raw": T0_RAW}):
        try:
            st = ReserveState(regime=Regime.BONDING_CURVE, mint="m", ts_unix_ms=T0, **bad)
            price_fill(regime=Regime.BONDING_CURVE, side="buy", reserves=st,
                       decision_ts_ms=T0, sol_in_lamports=LAMPORTS_PER_SOL)
            chk("non-positive reserves refused %s" % bad, False)
        except ReserveUnavailable:
            chk("non-positive reserves refused %s" % bad, True)
    st = curve_state(vs=1 * LAMPORTS_PER_SOL, vt=T0_RAW)
    try:
        price_fill(regime=Regime.BONDING_CURVE, side="buy", reserves=st,
                   decision_ts_ms=T0, sol_in_lamports=5 * LAMPORTS_PER_SOL)
        chk("order larger than reserves refused", False)
    except OrderTooLarge:
        chk("order larger than reserves refused", True)


# ------------------------------------------- forward AMM first-fill path -----
def test_forward_amm_first_fill_path():
    """Loader -> oracle -> engine for the FIRST forward pumpswap fill after capture.

    The fixture is written in the forward-capture schema and is explicitly
    SYNTHETIC (labelled in the file name and in the source field).
    """
    import pyarrow as pa
    import pyarrow.parquet as pq
    with tempfile.TemporaryDirectory() as d:
        p = os.path.join(d, "SYNTHETIC_pumpswap_pool_reserves_part0000.parquet")
        t = pa.table({
            "mint": pa.array(["POOLpump", "POOLpump"]),
            "pool": pa.array(["PoolAcct222", "PoolAcct222"]),
            "event_time_unix_ms": pa.array([T0, T0 + 60_000], pa.int64()),
            "pool_sol_lamports": pa.array([500 * LAMPORTS_PER_SOL,
                                           505 * LAMPORTS_PER_SOL], pa.int64()),
            "pool_token_reserves_raw": pa.array([400_000_000 * 10 ** 6,
                                                 399_000_000 * 10 ** 6], pa.int64()),
            "venue": pa.array(["pumpswap", "pumpswap"]),
            "slot": pa.array([1, 2], pa.int64()),
        })
        pq.write_table(t, p)
        o = pool_reserve_oracle(os.path.join(d, "SYNTHETIC_*.parquet"))
        eng = MechanicsEngine(regime=Regime.AMM, oracle=o, max_reserve_stale_ms=120_000)
        ft = eng.quote_buy("POOLpump", T0 + 30_000, 1.0)      # after snapshot 1
        chk("forward amm: first fill priced from captured pool reserves",
            ft.token_delta_raw > 0 and ft.staleness_ms == 30_000)
        chk("forward amm: reserves used are the captured ones",
            ft.reserves_used["sol_lamports"] == 500 * LAMPORTS_PER_SOL)
        ft2 = eng.quote_buy("POOLpump", T0 + 90_000, 1.0)     # after snapshot 2
        chk("forward amm: price moves when the pool state changes",
            ft2.px_pre != ft.px_pre and ft2.reserve_ts_ms == T0 + 60_000)
        try:
            eng.quote_buy("POOLpump", T0 - 1, 1.0)            # before capture
            chk("forward amm: refuses before the first captured state", False)
        except ReserveUnavailable:
            chk("forward amm: refuses before the first captured state", True)
        try:
            eng.quote_buy("POOLpump", T0 + 200_000, 1.0)      # beyond stale budget
            chk("forward amm: refuses past the stale budget", False)
        except ReserveStale:
            chk("forward amm: refuses past the stale budget", True)
        rep = eng.episode_reserve_report()
        chk("forward amm: episode exposes reserves used + staleness",
            rep["reserve_joins_count"] == 2 and rep["max_staleness_ms"] == 30_000
            and rep["lookahead_joins"] == 0)


def test_amm_token_leg_roundtrip():
    """REGRESSION: token-leg -> SOL-leg must invert price_fill EXACTLY on AMM.

    The AMM branch of fill_from_token_leg used dx_pool*(1+extraction)/(1+lp),
    dropping the LP fee from the numerator, so every AMM replay under-priced the
    SOL leg by the LP fee. Measured on real captured pool reserves
    (validate_forward_amm.py): a 0.5 SOL buy round-tripped to 0.498754 SOL.
    """
    st = pool_state()
    sol_in = 500_000_000
    f = price_fill(regime=Regime.AMM, side="buy", reserves=st, decision_ts_ms=T0,
                   sol_in_lamports=sol_in)
    inv = fill_from_token_leg(regime=Regime.AMM, side="buy", reserves=st,
                              token_leg_raw=int(f.token_delta_raw), decision_ts_ms=T0)
    got = int(inv.order_lamports_in)
    chk("amm inverse: token-leg round trip recovers the SOL leg",
        abs(got - sol_in) <= 1, f"recovered={got} vs {sol_in}")
    chk("amm inverse: not off by exactly the LP fee",
        abs(got - int(round(sol_in * (1.0 - AMM_LP_FEE_RATE)))) > 1,
        f"got={got} fee-only-price={int(round(sol_in * (1.0 - AMM_LP_FEE_RATE)))}")


def main():
    test_bonding_curve_regime()
    test_amm_regime()
    test_refusals()
    test_forward_amm_first_fill_path()
    test_amm_token_leg_roundtrip()
    out = {"suite": "test_regime_pricing", "checks": CHECKS[0], "failed": len(FAILS),
           "failures": FAILS}
    print(json.dumps(out, indent=1))
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
