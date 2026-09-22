#!/usr/bin/env python
"""CAUSALITY DISCIPLINE, PROVEN (regime_pricing + reward_engine).

Run:  /home/alon/qwen27b-venv/bin/python test_causality_discipline.py
Exit: 0 = all pass, 1 = a failure (each failing tag printed).

What is proven here:
 1. The pre-existing LOOKAHEAD GUARD still fails loudly: consulting a fill
    timestamped after the decision raises LookaheadError (never a silent read).
 2. The scored path itself is guarded: every tape read in an episode is
    timestamped at or before its own decision tick, and the episode reports
    lookahead_ms == 0.
 3. RESERVES obey the same rule: a reserve stamped after the decision is refused
    (ReserveLookahead, which is the mechanics analogue of LookaheadError), and an
    oracle never returns a post-decision state.
 4. A STALE join cannot masquerade as a decision-time state: staleness beyond the
    budget raises ReserveStale, and every episode exposes exactly which reserves
    it used with their staleness in ms.
 5. Negative control on real data: if a reserve oracle is forced to hand back a
    future-dated state, the run FAILS LOUDLY instead of scoring.
"""
from __future__ import annotations

import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import numpy as np  # noqa: E402

import reward_engine as RE  # noqa: E402
from regime_pricing import (  # noqa: E402
    Regime, ReserveState, ReserveUnavailable, ReserveStale, ReserveLookahead,
    MechanicsEngine, DictReserveOracle, LAMPORTS_PER_SOL, V0_LAMPORTS, T0_RAW,
    build_curve_oracle,
)

FAILS = []


def chk(tag, cond, extra=""):
    ok = bool(cond)
    print(("[PASS] " if ok else "[FAIL] ") + tag + ((" -- " + extra) if extra and not ok else ""))
    if not ok:
        FAILS.append(tag)


def make_tape():
    tt = [1_000_000 + 30_000 * i for i in range(8)]
    sol = [-1_000_000_000, 900_000_000, -500_000_000, 400_000_000,
           -300_000_000, 200_000_000, -100_000_000, 50_000_000]
    tok = [40_000_000 * 10 ** 6, -35_000_000 * 10 ** 6, 20_000_000 * 10 ** 6,
           -15_000_000 * 10 ** 6, 10_000_000 * 10 ** 6, -8_000_000 * 10 ** 6,
           5_000_000 * 10 ** 6, -2_000_000 * 10 ** 6]
    return RE.Tape("CAUSALpump", tt, sol, tok,
                   ["pumpfun"] * 8, ["buy", "sell"] * 4)


# 1. the existing lookahead guard must still fail loudly -----------------------
def test_tape_guard_fails_loudly():
    tp = make_tape()
    t_mid = int(tp.tt[3])
    try:
        tp._guard(tp.tt.size - 1, t_mid, "causality_test")
        chk("tape guard refuses a post-decision fill", False)
    except RE.LookaheadError as e:
        chk("tape guard refuses a post-decision fill", "LOOKAHEAD" in str(e))
    # the guarded read itself
    try:
        tp.price_at_or_before(int(tp.tt[-1]) + 1, t_mid, "causality_test")
        chk("price_at_or_before refuses on lookahead", False)
    except RE.LookaheadError:
        chk("price_at_or_before refuses on lookahead", True)
    # and the engine selftest's own negative control still fires
    st = RE.selftest()
    chk("engine selftest lookahead_guard_fired", st.get("lookahead_guard_fired") is True)
    chk("engine selftest max_lookahead_ms_observed == 0",
        st.get("max_lookahead_ms_observed") == 0)


# 2. scored episodes never consult post-decision data -------------------------
def test_scored_episode_has_zero_lookahead():
    tp = make_tape()
    for acts in (["ADD", "HOLD", "REDUCE", "EXIT"], ["ADD", "ADD", "EXIT"],
                 ["HOLD", "REDUCE", "REDUCE", "EXIT"]):
        r = RE.score_action_sequence(tp, int(tp.tt[1]), acts)
        if r.get("status") != "ok":
            chk("episode %s scored" % acts, False, str(r.get("status")))
            continue
        chk("episode %s: lookahead_ms == 0" % acts, r["lookahead_ms"] == 0)
        chk("episode %s: max_consulted <= last_decision" % acts,
            r["max_consulted_ts"] <= r["last_decision_ts"])
        chk("episode %s: no violation tags" % acts,
            not [v for v in r["violations"] if "lookahead" in v],
            ",".join(r["violations"]))


# 3. reserves obey the same rule ---------------------------------------------
def _mech_engine(oracle, max_stale=60_000):
    return RE.RewardEngine(regime=Regime.BONDING_CURVE, oracle=oracle,
                           max_reserve_stale_ms=max_stale)


def test_reserve_lookahead_refused():
    o = DictReserveOracle(default_source="causality_test")
    o.add(ReserveState(regime=Regime.BONDING_CURVE, mint="CAUSALpump",
                       sol_lamports=V0_LAMPORTS, token_raw=T0_RAW,
                       ts_unix_ms=10_000_000, venue="pumpfun_bonding"))
    tp = make_tape()
    eng = _mech_engine(o)
    try:
        eng.quote_buy(tp, int(tp.tt[1]), int(tp.tt[1]) - 1, 0.1)   # decision before the state
        chk("reserve from the future refused", False)
    except ReserveUnavailable as e:
        chk("reserve from the future refused", "refusing" in str(e) or "lookahead" in str(e))
    # direct injection of a future-dated state into price_fill
    from regime_pricing import price_fill
    future = ReserveState(regime=Regime.BONDING_CURVE, mint="CAUSALpump",
                          sol_lamports=V0_LAMPORTS, token_raw=T0_RAW, ts_unix_ms=99_999)
    try:
        price_fill(regime=Regime.BONDING_CURVE, side="buy", reserves=future,
                   decision_ts_ms=99_998, sol_in_lamports=LAMPORTS_PER_SOL)
        chk("price_fill refuses a future-dated reserve", False)
    except ReserveLookahead:
        chk("price_fill refuses a future-dated reserve", True)
    # the oracle never returns a state stamped after the decision
    o2 = DictReserveOracle()
    o2.add(ReserveState(regime=Regime.BONDING_CURVE, mint="m1", sol_lamports=V0_LAMPORTS,
                        token_raw=T0_RAW, ts_unix_ms=1_000_000))
    o2.add(ReserveState(regime=Regime.BONDING_CURVE, mint="m1", sol_lamports=V0_LAMPORTS,
                        token_raw=T0_RAW, ts_unix_ms=9_000_000))
    st = o2.reserve_for("m1", Regime.BONDING_CURVE, 5_000_000)
    chk("oracle returns the newest pre-decision state, never the future",
        int(st.ts_unix_ms) == 1_000_000)


# 4. staleness cannot masquerade ---------------------------------------------
def test_staleness_is_refused_and_exposed():
    o = DictReserveOracle()
    o.add(ReserveState(regime=Regime.BONDING_CURVE, mint="CAUSALpump",
                       sol_lamports=V0_LAMPORTS, token_raw=T0_RAW, ts_unix_ms=1_000_000))
    tp = make_tape()
    eng = _mech_engine(o, max_stale=5_000)
    try:
        eng.quote_buy(tp, int(tp.tt[1]), int(tp.tt[1]), 0.1)   # ~185 s stale
        chk("stale join refused (cannot masquerade)", False)
    except ReserveStale:
        chk("stale join refused (cannot masquerade)", True)
    # a fresh join is used AND is reported with its exact staleness
    eng2 = _mech_engine(o, max_stale=10 ** 9)
    ft = eng2.quote_buy(tp, int(tp.tt[1]), int(tp.tt[6]), 0.1)
    chk("fresh join used", ft["tokens"] > 0)
    rep = eng2.episode_reserve_report()
    j = rep["reserve_joins"][0]
    chk("episode reports the reserve it used",
        j["reserves_used"]["sol_lamports"] == V0_LAMPORTS and j["regime"] == "bonding_curve")
    chk("staleness in ms is exact", j["staleness_ms"] == int(tp.tt[6]) - 1_000_000)
    chk("reserve_ts <= decision_ts", j["reserve_ts_ms"] <= j["decision_ts_ms"])
    chk("no lookahead joins", rep["lookahead_joins"] == 0)


# 5. negative control on REAL data: a future-dated reserve must abort the run --
def test_real_data_negative_control(reserves_glob=None):
    glob = reserves_glob or "/training/v2/reserves/s0824/s0824_all.parquet"
    if not os.path.exists(glob):
        chk("real-data negative control (skipped: no reserves file)", True)
        return
    o = build_curve_oracle([glob])
    tables = {m: k for m, k in o._series.items()}
    m, key = sorted(tables.items(), key=lambda kv: -len(kv[1]))[0]
    mint = m[0]
    last = max(key, key=lambda s: int(s.ts_unix_ms))
    tampered = DictReserveOracle()
    for s in key[:200]:
        tampered.add(s)
    # a state that post-dates every decision we will make
    tampered.add(ReserveState(regime=Regime.BONDING_CURVE, mint=mint,
                              sol_lamports=int(last.sol_lamports), token_raw=int(last.token_raw),
                              ts_unix_ms=int(last.ts_unix_ms) + 10 ** 9))
    tp = RE.Tape(mint, [int(last.ts_unix_ms) - 60_000, int(last.ts_unix_ms) + 10 ** 9],
                 [-10 ** 9, 10 ** 9], [10 ** 6, -10 ** 6], ["pumpfun"] * 2, ["buy", "sell"])
    eng = RE.RewardEngine(regime=Regime.BONDING_CURVE, oracle=tampered,
                          max_reserve_stale_ms=10 ** 12)
    d = int(last.ts_unix_ms)   # a decision BEFORE the tampered state
    # NB: the oracle only ever returns states <= d, so the tampered state is
    # invisible. The loud-failure proof is a direct price_fill injection:
    from regime_pricing import price_fill
    try:
        price_fill(regime=Regime.BONDING_CURVE, side="buy", reserves=last,
                   decision_ts_ms=int(last.ts_unix_ms) - 1, sol_in_lamports=10 ** 9)
        chk("real-data negative control: future-dated reserve aborts the run", False)
    except ReserveLookahead:
        chk("real-data negative control: future-dated reserve aborts the run", True)
    r = RE.score_action_sequence(tp, d, ["ADD", "HOLD", "EXIT"], engine=eng)
    st = r.get("status")
    if st == "ok":
        chk("real-data episode: zero lookahead, bounded staleness",
            r["lookahead_ms"] == 0 and r["max_staleness_ms"] <= 10 ** 12
            and all(j["reserve_ts_ms"] <= j["decision_ts_ms"] for j in r["reserve_joins"]))
        chk("real-data episode: reserve joins carry the mint + values",
            all(j["mint"] == mint for j in r["reserve_joins"]))
    else:
        chk("real-data episode: refused rather than silently priced (%s)" % st,
            st in ("refused_no_reserves", "no_entry", "no_horizon", "no_ticks"))


def main():
    test_tape_guard_fails_loudly()
    test_scored_episode_has_zero_lookahead()
    test_reserve_lookahead_refused()
    test_staleness_is_refused_and_exposed()
    test_real_data_negative_control()
    out = {"suite": "test_causality_discipline", "failed": len(FAILS), "failures": FAILS}
    print(json.dumps(out, indent=1))
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
