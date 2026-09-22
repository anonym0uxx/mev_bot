#!/usr/bin/env python
"""Smoke: score ONE real episode with the mechanics regime and print the reserve
provenance (which reserves priced it, and their staleness in ms).

Run: python mechanics_smoke.py [--trade-path ...] [--reserves ...] [--mint M]
"""
from __future__ import annotations

import argparse
import collections
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import reward_engine as RE
from regime_pricing import Regime, build_curve_oracle

SWEEP = ["ADD", "HOLD", "ADD", "REDUCE", "HOLD", "EXIT"]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--trade-path", default="/training/v2/canonical/renorm_pooltest/trades.jsonl")
    ap.add_argument("--reserves", nargs="*",
                    default=["/training/v2/reserves/s0824/s0824_all.parquet",
                             "/training/v2/reserves/s0909a/s0909a_all.parquet",
                             "/training/v2/reserves/s0909b/s0909b_all.parquet",
                             "/training/v2/reserves/s0823r/s0823r_all.parquet"])
    ap.add_argument("--mint", default=None)
    ap.add_argument("--max-stale-ms", type=int, default=60_000)
    a = ap.parse_args()

    rows = [json.loads(l) for l in open(a.trade_path)]
    bymint = collections.defaultdict(list)
    for r in rows:
        bymint[r["mint"]].append(r)
    oracle = build_curve_oracle(list(a.reserves))
    held = {m for (m, reg) in oracle._series if reg is Regime.BONDING_CURVE}

    cands = [m for m in bymint if m in held and len(bymint[m]) >= 6]
    cands.sort(key=lambda m: -len(bymint[m]))
    if a.mint:
        cands = [a.mint]
    out = {"mints_with_both_tape_and_reserves": len(cands), "attempts": []}
    for m in cands[:12]:
        tapes = RE.load_canonical_tapes(path=a.trade_path, mints={m})
        tp = tapes.get(m)
        if tp is None or tp.tt.size < 6:
            continue
        eng = RE.RewardEngine(regime=Regime.BONDING_CURVE, oracle=oracle,
                              max_reserve_stale_ms=a.max_stale_ms)
        # decide early enough that a full management horizon remains
        t_dec = int(tp.tt[max(1, tp.tt.size // 5)])
        r = RE.score_action_sequence(tp, t_dec, SWEEP, engine=eng)
        rec = {"mint": m, "fills": int(tp.tt.size), "t_dec_ms": t_dec,
               "status": r.get("status"), "net_sol_returned": r.get("net_sol_returned"),
               "regime": r.get("regime"), "reserves_used_count": r.get("reserves_used_count"),
               "max_staleness_ms": r.get("max_staleness_ms"),
               "lookahead_ms": r.get("lookahead_ms"),
               "reserve_joins_first": (r.get("reserve_joins") or [None])[0],
               "reason": r.get("reason")}
        out["attempts"].append(rec)
        if r.get("status") == "ok":
            out["scored_episode"] = rec
            break
    print(json.dumps(out, indent=1, default=str))
    return 0


if __name__ == "__main__":
    sys.exit(main())
