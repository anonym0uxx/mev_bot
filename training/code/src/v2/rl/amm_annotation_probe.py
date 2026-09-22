"""Probe: how much AMM-pool annotation coverage can the backfill actually give?

Answers three questions before we commit a corpus rebuild:
  1. how many distinct pools does the backfill see, and how many of them can we
     CLASSIFY as WSOL-quoted (only WSOL-quoted pools are unit-safe: quote_reserve
     for a USDC pool is 6dp USDC in the lamports slot -> the 1000x bug);
  2. which corpus mints have any AMM event at all;
  3. per SFT row: is there an AMM state at/before t_dec within the annotation cap.
"""
import collections
import json
import os
import sys

BACKFILL = "/training/v2/reserves/amm_history_v1/amm_reserves.jsonl"
POOLS = "/training/v2/reports/AMM_POOLS_RESOLVED_V1.json"
C5 = "/training/v2/candidate_sft_c5"
CAP_MS = 24 * 3600 * 1000

pools_doc = json.load(open(POOLS, encoding="utf-8"))
per_mint = pools_doc["per_mint_pools"]
pool_quotes = {}
for m, plist in per_mint.items():
    for e in plist:
        pool_quotes[e["pool"]] = tuple(e.get("quotes") or ())

ev = collections.defaultdict(list)   # mint -> list of (recv_ms, pool, base, quote)
seen_pools = collections.Counter()
n = 0
n_null = 0
with open(BACKFILL, encoding="utf-8") as f:
    for line in f:
        n += 1
        try:
            r = json.loads(line)
        except Exception:
            continue
        seen_pools[r["pool"]] += 1
        if r.get("base_reserve") is None or r.get("quote_reserve") is None:
            n_null += 1
            continue
        ev[r["mint"]].append((int(r["recv_unix_ms"]), r["pool"],
                              int(r["base_reserve"]), int(r["quote_reserve"])))

print(json.dumps({
    "backfill_rows": n,
    "rows_with_null_reserves_skipped": n_null,
    "backfill_mints": len(ev),
    "backfill_distinct_pools": len(seen_pools),
    "pools_in_map": sum(1 for p in seen_pools if p in pool_quotes),
    "pools_unmapped": sum(1 for p in seen_pools if p not in pool_quotes),
    "pools_wsol": sum(1 for p in seen_pools if "WSOL" in pool_quotes.get(p, ())),
}, indent=1))

for m in ev:
    ev[m].sort(key=lambda x: x[0])

import bisect
res = {}
for split in ("train", "validation", "examination"):
    path = os.path.join(C5, split + ".jsonl")
    if not os.path.exists(path):
        continue
    c = collections.Counter()
    stale = []
    d = collections.defaultdict(list)
    with open(path, encoding="utf-8") as f:
        for line in f:
            o = json.loads(line)
            mint = o.get("mint")
            m = None
            i = line.find('t_dec_ms=')
            if i >= 0:
                j = i + 9
                k = j
                while k < len(line) and line[k].isdigit():
                    k += 1
                m = int(line[j:k])
            if mint is None or m is None:
                c["no_mint_or_tdec"] += 1
                continue
            lst = ev.get(mint)
            if not lst:
                c["mint_absent_from_backfill"] += 1
                continue
            ts = [x[0] for x in lst]
            idx = bisect.bisect_right(ts, m) - 1
            if idx < 0:
                c["no_amm_event_before_tdec"] += 1
                continue
            age = m - ts[idx]
            if age > CAP_MS:
                c["amm_state_older_than_24h"] += 1
                continue
            pool = lst[idx][1]
            pq = pool_quotes.get(pool)
            if pq is None:
                c["pool_unclassified"] += 1
                continue
            if "WSOL" not in pq:
                c["pool_not_wsol"] += 1
                continue
            c["amm_state_wsol_within_24h"] += 1
            stale.append(age)
            d[mint].append(age)
    tot = sum(c.values())
    stale.sort()
    def pct(q):
        return stale[int(q * (len(stale) - 1))] if stale else None
    print(json.dumps({
        "split": split, "rows": tot, "counts": dict(c),
        "amm_coverage": round(c["amm_state_wsol_within_24h"] / max(tot, 1), 4),
        "mints_with_amm_state": len(d),
        "staleness_ms": {"p10": pct(.10), "p50": pct(.50), "p90": pct(.90),
                         "max": (stale[-1] if stale else None)},
        "share_le_60s": round(sum(1 for a in stale if a <= 60000) / max(len(stale), 1), 4),
    }, indent=1), flush=True)
