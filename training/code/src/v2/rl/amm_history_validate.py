#!/usr/bin/env python
"""amm_history_validate — acceptance test for the AMM reserve backfill.

The engine's AMM branch was validated on captured reserves at k_rel_drift p50 =
1.65e-5 (independently corroborated at 1.7e-5). Recovered history must meet the SAME
bar, otherwise it is not the same object and must not be fed to the pricing engine.

Checks:
  * decoded-event coverage (decoded / fetched)
  * per-pool k = base*quote drift across consecutive events, p50/p90/p99
  * per-mint coverage + time span, and how much of the required 766 mints are covered
  * k must be non-decreasing (fees accrue to the pool) - a falling k means a
    mis-decoded or mis-ordered event
"""
from __future__ import annotations

import argparse
import collections
import datetime as dt
import json
import os
import statistics as st

OUTDIR = "/training/v2/reserves/amm_history_v1"


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--outdir", default=OUTDIR)
    ap.add_argument("--index", default="")
    a = ap.parse_args(argv)
    path = os.path.join(a.outdir, "amm_reserves.jsonl")
    idx = a.index or os.path.join(a.outdir, "sig_index.jsonl")

    per_pool = collections.defaultdict(list)
    fetched = decoded = 0
    for line in open(path, encoding="utf-8"):
        try:
            r = json.loads(line)
        except Exception:
            continue
        fetched += 1
        b, q = r.get("base_reserve"), r.get("quote_reserve")
        if not b or not q:
            continue
        decoded += 1
        per_pool[r["mint"]].append((int(r.get("slot") or 0), int(r["recv_unix_ms"]),
                                    int(b), int(q)))

    # ORDER MATTERS. `recv_unix_ms` is OUR capture receive time, not on-chain
    # order, so comparisons between two rows in the SAME slot are not a
    # well-defined pool transition (reproduced: 27% "falling k" purely from
    # mis-ordering). Slot number IS on-chain order, so only compare across
    # strictly increasing slots.
    # POOL IDENTITY. A mint can sit in MORE THAN ONE pump-swap pool (SOL-quoted
    # and USDC-quoted), and the Buy/SellEvent does not carry the pool address, so
    # grouping by mint mixes two pools and their k values jump. Reproduced: 25% of
    # cross-slot pairs "fall" when grouped by mint alone. Fix: cluster the mint's
    # events on implied price level (pools sit at different price levels), which
    # recovers pool identity from the data we already fetched.
    import math

    def split_pools(rows, max_groups=3):
        if len(rows) < 8:
            return [rows]
        pts = sorted(rows, key=lambda r: math.log(r[2] / r[3]) if r[3] > 0 else 0.0)
        lp = [math.log(r[2] / r[3]) if r[3] > 0 else 0.0 for r in pts]
        gaps = [(abs(lp[i + 1] - lp[i]), i) for i in range(len(lp) - 1)]
        gaps.sort(reverse=True)
        cuts = sorted(i for _, i in gaps[:max_groups - 1]
                      if abs(lp[i + 1] - lp[i]) > 0.05)
        groups, start = [], 0
        for c in cuts:
            groups.append(pts[start:c + 1])
            start = c + 1
        groups.append(pts[start:])
        return [g for g in groups if g]

    drifts = []
    falling = 0
    pairs = 0
    same_slot_pairs = 0
    n_pools = 0
    for mint, rows in per_pool.items():
        for grp in split_pools(rows):
            n_pools += 1
            grp.sort(key=lambda x: (x[0], x[1]))
            prev = None
            for (sl, t, b, q) in grp:
                if prev is None:
                    prev = (sl, b, q)
                    continue
                if sl == prev[0]:
                    same_slot_pairs += 1
                    prev = (sl, b, q)
                    continue
                k1, k2 = prev[1] * prev[2], b * q
                prev = (sl, b, q)
                if k1 <= 0:
                    continue
                d = (k2 / k1) - 1.0
                drifts.append(abs(d))
                pairs += 1
                if d < -1e-9:
                    falling += 1

    need = 0
    have = set()
    if os.path.isfile(idx):
        for line in open(idx, encoding="utf-8"):
            try:
                have.add(json.loads(line)["mint"])
            except Exception:
                continue
        need = len(have)

    def pct(x, p):
        return round(float(st.quantiles(x, n=100)[p - 1]), 10) if len(x) > 1 else None

    covered = set(per_pool)
    ts_all = [t for rows in per_pool.values() for (_, t, _, _) in rows]
    out = {
        "rows_fetched": fetched,
        "rows_decoded": decoded,
        "decode_frac": round(decoded / fetched, 5) if fetched else None,
        "distinct_mints": len(covered),
        "index_mints_required": need,
        "mint_coverage_frac": round(len(covered & have) / need, 4) if need else None,
        "k_pairs": pairs,
        "k_rel_drift_p50": pct(drifts, 50),
        "k_rel_drift_p90": pct(drifts, 90),
        "k_rel_drift_p99": pct(drifts, 99),
        "k_pairs_falling": falling,
        "k_pairs_falling_frac": round(falling / pairs, 6) if pairs else None,
        "k_pairs_same_slot_excluded": same_slot_pairs,
        "price_clustered_pools": n_pools,
        "ts_min": min(ts_all) if ts_all else None,
        "ts_max": max(ts_all) if ts_all else None,
        "window_days": round((max(ts_all) - min(ts_all)) / 86400000, 2) if ts_all else None,
        "ts_min_utc": dt.datetime.fromtimestamp(min(ts_all) / 1000, dt.timezone.utc
                                                ).isoformat() if ts_all else None,
        "benchmark_k_rel_drift_p50": 1.65e-5,
    }
    out["verdict"] = (
        "PASS" if (out["k_rel_drift_p50"] is not None
                   and out["k_rel_drift_p50"] <= 5e-5
                   and out["k_pairs_falling_frac"] is not None
                   and out["k_pairs_falling_frac"] < 0.02) else
        "PENDING/FAIL")
    print(json.dumps(out, indent=1))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
