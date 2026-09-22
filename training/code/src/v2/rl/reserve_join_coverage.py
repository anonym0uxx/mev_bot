#!/usr/bin/env python
"""Reserve-join coverage diagnostic: how much of each SFT partition can get a
decision-time curve state, and WHY it fails (mint never captured vs quiet mint).

Motivation: the previous corpus build used a 60 s wall-clock staleness cap for the
reserve join. For pump.fun that cap is the wrong instrument - the curve state only
changes when the mint TRADES and it is emitted on every trade, so "the newest state
at or before t_dec" is the EXACT state at t_dec no matter how long the mint has been
quiet. Wall-clock age only matters as a proxy for a CAPTURE GAP. This script
separates the two failure modes:

  * mint_never_captured - no reserve event for this mint at all (a true coverage gap)
  * quiet_then_stale     - events exist for the mint, but the newest one is old
                           (NOT a coverage gap: the state is still exact)
  * matched              - newest event at or before t_dec within the budget

Usage:
  python reserve_join_coverage.py [--budgets 60000,300000,3600000] [--json out.json]
"""
from __future__ import annotations

import argparse
import collections
import glob
import json
import os
import re
import sys

import numpy as np
import pyarrow.parquet as pq

RESERVE_GLOBS = [
    "/training/v2/reserves/s0823r/s0823r_all.parquet",
    "/training/v2/reserves/s0824/s0824_all.parquet",
    "/training/v2/reserves/s0909a/s0909a_all.parquet",
    "/training/v2/reserves/s0909b/s0909b_all.parquet",
]
SPLITS = ("train", "validation", "examination")
EP = re.compile(rb'"episode_id":\s*"v2:[^"]*?:(\d{13})"')
DT = re.compile(rb'DECISION TIME \(unix ms\):\s*(\d{13})')
MI = re.compile(rb'"mint":\s*"([^"]+)"')


def load_reserve_series(paths):
    acc = collections.defaultdict(list)
    rows = 0
    for p in paths:
        t = pq.read_table(p, columns=["mint_b58", "recv_unix_ms",
                                      "virtual_sol_reserves_lamports",
                                      "virtual_token_reserves_raw"])
        mins = t["mint_b58"].to_pylist()
        ts = t["recv_unix_ms"].to_numpy(zero_copy_only=False).astype("int64")
        vs = t["virtual_sol_reserves_lamports"].to_numpy(zero_copy_only=False).astype("int64")
        vt = t["virtual_token_reserves_raw"].to_numpy(zero_copy_only=False).astype("int64")
        for i, m in enumerate(mins):
            if int(vs[i]) > 0 and int(vt[i]) > 0:
                acc[m].append((int(ts[i]), int(vs[i]), int(vt[i])))
        rows += t.num_rows
    series = {}
    for m, lst in acc.items():
        lst.sort()
        arr = np.array(lst, dtype=np.int64)
        series[m] = (arr[:, 0], arr[:, 1], arr[:, 2])
    return series, rows, len(series)


def read_episodes(path):
    out = []
    with open(path, "rb") as f:
        buf = b""
        while True:
            c = f.read(1 << 24)
            if not c:
                break
            buf += c
            cut = buf.rfind(b"\n")
            if cut < 0:
                continue
            body, buf = buf[:cut], buf[cut:]
            for line in body.split(b"\n"):
                if not line:
                    continue
                m = EP.search(line) or DT.search(line)
                if not m:
                    continue
                mm = MI.search(line)
                out.append((mm.group(1).decode() if mm else None, int(m.group(1))))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--src", default="/training/v2/candidate_sft_c4")
    ap.add_argument("--budgets", default="60000,300000,3600000,-1")
    ap.add_argument("--json", default="/training/v2/reports/RESERVE_JOIN_COVERAGE.json")
    a = ap.parse_args()
    budgets = [int(x) for x in a.budgets.split(",")]
    series, rows, mints = load_reserve_series(RESERVE_GLOBS)
    print("reserve rows=%d  mints=%d  ts range=[%d, %d]"
          % (rows, mints,
             min(int(v[0][0]) for v in series.values()),
             max(int(v[0][-1]) for v in series.values())))

    rep = {"reserve_rows": rows, "reserve_mints": mints,
           "reserve_ts_min": min(int(v[0][0]) for v in series.values()),
           "reserve_ts_max": max(int(v[0][-1]) for v in series.values()),
           "budgets_ms": budgets, "partitions": {}}
    for split in SPLITS:
        p = os.path.join(a.src, split + ".jsonl")
        if not os.path.exists(p):
            continue
        eps = read_episodes(p)
        stats = {b: collections.Counter() for b in budgets}
        staleness = []
        tmin = min(t for _, t in eps)
        tmax = max(t for _, t in eps)
        for mint, t in eps:
            arr = series.get(mint)
            if arr is None:
                for b in budgets:
                    stats[b]["mint_never_captured"] += 1
                continue
            ts = arr[0]
            j = int(np.searchsorted(ts, t, side="right")) - 1
            if j < 0:
                for b in budgets:
                    stats[b]["no_event_at_or_before_t_dec"] += 1
                continue
            age = t - int(ts[j])
            staleness.append(age)
            for b in budgets:
                if b < 0 or age <= b:
                    stats[b]["matched"] += 1
                    stats[b]["matched_quiet_over_60s"] += (1 if age > 60_000 else 0)
                else:
                    stats[b]["quiet_then_stale"] += 1
        st = np.array(staleness, dtype=np.int64) if staleness else np.array([], dtype=np.int64)
        rep["partitions"][split] = {
            "episodes": len(eps),
            "episode_t_dec_range": [tmin, tmax],
            "episodes_with_any_reserve_state": int(st.size),
            "staleness_ms_p50": int(np.median(st)) if st.size else None,
            "staleness_ms_p90": int(np.percentile(st, 90)) if st.size else None,
            "staleness_ms_max": int(st.max()) if st.size else None,
            "coverage": {("any" if b < 0 else str(b)):
                         {"matched": s["matched"],
                          "mint_never_captured": s["mint_never_captured"],
                          "no_event_at_or_before_t_dec": s["no_event_at_or_before_t_dec"],
                          "quiet_then_stale": s["quiet_then_stale"],
                          "matched_quiet_over_60s": s["matched_quiet_over_60s"],
                          "coverage_frac": round(s["matched"] / max(len(eps), 1), 6)}
                         for b, s in stats.items()},
        }
        print("%-12s episodes=%6d  any_state=%6d  p50=%sms p90=%sms max=%sms"
              % (split, len(eps), st.size,
                 rep["partitions"][split]["staleness_ms_p50"],
                 rep["partitions"][split]["staleness_ms_p90"],
                 rep["partitions"][split]["staleness_ms_max"]))
        for b in budgets:
            c = rep["partitions"][split]["coverage"]["any" if b < 0 else str(b)]
            print("   budget %-7s coverage=%6.3f  matched=%6d  quiet_then_stale=%6d  "
                  "mint_never_captured=%6d"
                  % ("inf" if b < 0 else b, c["coverage_frac"], c["matched"],
                     c["quiet_then_stale"], c["mint_never_captured"]))
    os.makedirs(os.path.dirname(a.json), exist_ok=True)
    json.dump(rep, open(a.json, "w"), indent=1)
    print("wrote", a.json)
    return 0


if __name__ == "__main__":
    sys.exit(main())
