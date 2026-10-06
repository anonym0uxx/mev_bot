#!/usr/bin/env python
"""Stratified re-run of the reward engine's predictive reconciliation.

NEW module (v2/reserves).  Mirrors `reward_engine.reconcile_predictive` exactly
(same episode definition, same fee, same lamport-checked accounting) and adds the
split the aggregate number hides:

  ALL          - identical to reward_engine --reconcile --reserves ... (cross-check)
  CURVE_STRATUM- only fills whose (signature, mint, side) key exists in the
                 reconstructed reserves table, i.e. priced off the real curve
  FULLY_CURVE  - only episodes in which EVERY fill was curve-priced (no tape
                 fallback anywhere in the episode)

Usage:
  python reconcile_curve.py --reserves <parquet...> --out <report.json>
"""
from __future__ import annotations

import argparse, json, sys
import numpy as np

sys.path.insert(0, "/training/v2/code/src/v2/rl")
sys.path.insert(0, "/training/v2/code/src/v2/reserves")
from reward_engine import (RewardEngine, load_canonical_tapes,  # noqa: E402
                           to_sol_checked, to_lamports_checked,
                           LAMPORTS_PER_SOL)
from reserves_loader import ReserveTable  # noqa: E402


def replay(path, reserves=None, stratum="ALL"):
    eng = RewardEngine()
    trades = []
    with open(path) as fh:
        for line in fh:
            trades.append(json.loads(line))
    if not trades:
        raise SystemExit(f"FATAL: zero rows read from {path}")
    bymint = {}
    for r in trades:
        bymint.setdefault(r["mint"], []).append(r)
    rec_net, sim_net, per_fill = [], [], []
    fills = curve_fills = 0
    episodes = 0
    refused = 0
    for m, rs in sorted(bymint.items()):
        if len({r["venue"] for r in rs}) > 1:
            refused += 1
            continue
        rs = sorted(rs, key=lambda r: (r["recv_unix_ms"], r["signature"]))
        px, tt = [], []
        for r in rs:
            if r["tokens_raw"] != 0:
                px.append((r["recv_unix_ms"],
                           abs(r["sol_lamports"]) / abs(r["tokens_raw"])))
        # identical tie-break to the engine: sort by (ms, price), not by signature
        px.sort()
        tt = [x[0] for x in px]
        if not px:
            continue
        bytr = {}
        for r in rs:
            bytr.setdefault(r["trader"], []).append(r)
        for trader, ep in bytr.items():
            if len(ep) < 2:
                continue
            ep = sorted(ep, key=lambda r: r["recv_unix_ms"])
            # NOTE: mirror reward_engine.reconcile_predictive exactly - it sums
            # recorded sol_lamports over ALL episode fills while `simulated` only
            # accumulates fills that could be priced.  Reproducing that quirk is
            # what makes the ALL stratum a genuine cross-check of the engine.
            rec_all = to_sol_checked(sum(int(x["sol_lamports"]) for x in ep), "rec")
            rec = 0
            sim = 0.0
            le = []
            ok = True
            for r in ep:
                pc = None
                if reserves is not None:
                    pc = reserves.price_pre_lamports_per_raw_token(
                        r.get("signature"), r["mint"], r["side"] == "buy")
                if stratum != "ALL" and pc is None:
                    if stratum == "FULLY_CURVE":
                        ok = False
                        break
                    continue
                p = float(pc) if (pc is not None and np.isfinite(pc) and pc > 0) else None
                if p is None:
                    j = int(np.searchsorted(np.array(tt), r["recv_unix_ms"], side="left")) - 1
                    if j < 0:
                        continue
                    p = px[j][1]
                else:
                    curve_fills += 1
                tok = abs(r["tokens_raw"])
                notional = abs(r["sol_lamports"]) / LAMPORTS_PER_SOL
                pred = to_sol_checked(int(round(tok * p * (1.0 - eng.fee_rate))), "pred")
                sim += (-1.0 if r["side"] == "buy" else 1.0) * pred
                rec += int(r["sol_lamports"])
                le.append(abs(pred - notional))
                fills += 1
            if not ok or not le:
                continue
            rec_net.append(rec_all if stratum == "ALL" else to_sol_checked(rec, "rec"))
            sim_net.append(sim)
            per_fill.append(float(np.mean(le)))
            episodes += 1
    rec_net = np.array(rec_net)
    sim_net = np.array(sim_net)
    err = np.abs(sim_net - rec_net)
    corr = float(np.corrcoef(rec_net, sim_net)[0, 1]) if rec_net.size > 1 else float("nan")
    return {
        "stratum": stratum,
        "episodes_replayed": int(rec_net.size),
        "fills_replayed": fills,
        "fills_priced_from_curve_reserves": curve_fills,
        "mints_multi_venue_refused": refused,
        "corr_recorded_vs_sim": round(corr, 6),
        "mean_abs_err_sol": round(float(err.mean()), 6),
        "max_abs_err_sol": round(float(err.max()), 4),
        "p90_abs_err_sol": round(float(np.percentile(err, 90)), 6),
        "mean_abs_err_sol_per_fill": round(float(np.mean(per_fill)), 6),
        "fee_rate_used": eng.fee_rate,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--trades",
                    default="/training/v2/canonical/renorm_pooltest/trades.jsonl")
    ap.add_argument("--reserves", nargs="*", required=True)
    ap.add_argument("--out", default="/training/v2/reports/RECONCILE_CURVE_STRATA.json")
    a = ap.parse_args()
    t = ReserveTable(a.reserves)
    print("reserves_rows", len(t))
    out = {"reserves_rows": len(t), "trades": a.trades}
    for s in ("ALL", "CURVE_STRATUM", "FULLY_CURVE"):
        out[s] = replay(a.trades, t, s)
        print(s, json.dumps(out[s]))
    json.dump(out, open(a.out, "w"), indent=1)
    print("wrote", a.out)


if __name__ == "__main__":
    main()