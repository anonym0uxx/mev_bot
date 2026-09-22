#!/usr/bin/env python
"""Stratified + venue-segmented predictive reconciliation.

NEW module (v2/reserves).  Mirrors `reconcile_curve.py` (which itself mirrors
reward_engine.reconcile_predictive) and adds what STEP 4 of the reserve task asks
for and the aggregate hides:

  ALL / CURVE_STRATUM / FULLY_CURVE   (as reconcile_curve.py)
  x  venue segment {pumpfun, pumpswap}
  x  curve-priced vs tape-priced fill counts per venue

A pumpswap fill can NEVER be priced from a bonding-curve reserve table - the
bonding curve account is closed on migration - so every pumpswap fill that is not
also present in an AMM pool-reserve table is structurally unpriceable.  That count
is reported explicitly instead of being buried in the fallback.

Usage:
  python reconcile_curve_v2.py --reserves <ls parquet...> [--reserves-slinky DIR] --out <json>
"""
from __future__ import annotations

import argparse, collections, json, sys
import numpy as np

sys.path.insert(0, "/training/v2/code/src/v2/rl")
sys.path.insert(0, "/training/v2/code/src/v2/reserves")
from reward_engine import RewardEngine, to_sol_checked, LAMPORTS_PER_SOL  # noqa: E402


def replay(path, reserves=None, stratum="ALL", venue=None):
    eng = RewardEngine()
    trades = [json.loads(l) for l in open(path)]
    if not trades:
        raise SystemExit("FATAL: zero rows in %s" % path)
    bymint = collections.defaultdict(list)
    for r in trades:
        bymint[r["mint"]].append(r)
    rec_net, sim_net, per_fill = [], [], []
    fills = curve_fills = tape_fills = 0
    refused = 0
    for m, rs in sorted(bymint.items()):
        if len({r["venue"] for r in rs}) > 1:
            refused += 1
            continue
        if venue and rs[0]["venue"] != venue:
            continue
        rs = sorted(rs, key=lambda r: (r["recv_unix_ms"], r["signature"]))
        px = sorted((r["recv_unix_ms"], abs(r["sol_lamports"]) / abs(r["tokens_raw"]))
                    for r in rs if r["tokens_raw"])
        if not px:
            continue
        tt = np.array([x[0] for x in px])
        bytr = collections.defaultdict(list)
        for r in rs:
            bytr[r["trader"]].append(r)
        for trader, ep in bytr.items():
            if len(ep) < 2:
                continue
            ep = sorted(ep, key=lambda r: r["recv_unix_ms"])
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
                    j = int(np.searchsorted(tt, r["recv_unix_ms"], side="left")) - 1
                    if j < 0:
                        continue
                    p = px[j][1]
                    tape_fills += 1
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
    rec_net = np.array(rec_net)
    sim_net = np.array(sim_net)
    err = np.abs(sim_net - rec_net)
    return {
        "stratum": stratum, "venue": venue or "all",
        "episodes_replayed": int(rec_net.size), "fills_replayed": fills,
        "fills_priced_from_curve_reserves": curve_fills,
        "fills_priced_from_tape": tape_fills,
        "mints_multi_venue_refused": refused,
        "corr_recorded_vs_sim": round(float(np.corrcoef(rec_net, sim_net)[0, 1]), 6) if rec_net.size > 1 else None,
        "mean_abs_err_sol": round(float(err.mean()), 6) if err.size else None,
        "p90_abs_err_sol": round(float(np.percentile(err, 90)), 6) if err.size else None,
        "mean_abs_err_sol_per_fill": round(float(np.mean(per_fill)), 6) if per_fill else None,
        "fee_rate_used": eng.fee_rate,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--trades", default="/training/v2/canonical/renorm_pooltest/trades.jsonl")
    ap.add_argument("--reserves", nargs="*", default=None)
    ap.add_argument("--reserves-slinky", default=None)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    tables = []
    sl_stats = None
    if a.reserves_slinky:
        from slinky_reserves_loader import SlinkyFillReserves
        st = SlinkyFillReserves(a.trades, a.reserves_slinky)
        sl_stats = st.stats(); sl_stats["table_rows"] = len(st)
        tables.append(st)
    n_ls = 0
    if a.reserves:
        from reserves_loader import ReserveTable
        t = ReserveTable(a.reserves); n_ls = len(t); tables.append(t)
    from slinky_reserves_loader import ChainReserveTable
    rtab = ChainReserveTable(tables) if tables else None
    tape = [json.loads(l) for l in open(a.trades)]
    ven_counts = collections.Counter(r["venue"] for r in tape)
    out = {"trades": a.trades, "tape_rows": len(tape), "tape_venue_counts": dict(ven_counts),
           "reserves_rows": sum(len(t) for t in tables), "reserves_laserstream_rows": n_ls,
           "reserves_slinky_rows": (len(tables[0]) if a.reserves_slinky else 0),
           "reserves_slinky_stats": sl_stats,
           "reserves_chain_stats": (rtab.stats() if (rtab is not None and len(tables) > 1) else None)}
    for s in ("ALL", "CURVE_STRATUM", "FULLY_CURVE"):
        out[s] = replay(a.trades, rtab, s)
        print(s, json.dumps(out[s]))
    for v in ("pumpfun", "pumpswap"):
        out["VENUE_" + v] = replay(a.trades, rtab, "ALL", venue=v)
        print("VENUE", v, json.dumps(out["VENUE_" + v]))
        out["VENUE_%s_CURVE" % v] = replay(a.trades, rtab, "CURVE_STRATUM", venue=v)
        print("VENUE_CURVE", v, json.dumps(out["VENUE_%s_CURVE" % v]))
    json.dump(out, open(a.out, "w"), indent=1, default=str)
    print("wrote", a.out)


if __name__ == "__main__":
    main()
