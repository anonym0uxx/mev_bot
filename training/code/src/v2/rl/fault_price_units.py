#!/usr/bin/env python
"""FAULT HUNT (1) price-unit agreement, (2) backfill pool attribution.

Report mode showed 0 rows agreeing within 1% between the prompt's
price_sol_per_raw and the reserve-implied price, ~80% disagreeing by >50%.
This measures the actual ratio and the pool-attribution mismatch rate.
"""
import collections
import json
import re
import statistics

C3 = "/training/v2/candidate_sft_c3/train.jsonl"
BACKFILL = "/training/v2/reserves/amm_history_v1/amm_reserves.jsonl"
V2 = "/training/v2/reports/AMM_POOLS_RESOLVED_V2.jsonl"
SOL = 1_000_000_000.0

MINT = re.compile(r'"mint":\s*"([^"]+)"')
TMS = re.compile(r"t_dec_ms=(\d{13})")
PRX = re.compile(r"price_sol_per_raw=([0-9eE.+-]+)")

v2 = {}
for l in open(V2, encoding="utf-8"):
    r = json.loads(l)
    v2[r["mint"]] = list(r.get("wsol_pools") or [])

# --- sample corpus rows (mint, t_dec, tape price)
sample = []
wanted = set()
with open(C3, encoding="utf-8") as f:
    for i, line in enumerate(f):
        if i >= 150000:
            break
        mm, tt, pp = MINT.search(line), TMS.search(line), PRX.search(line)
        if not (mm and tt and pp):
            continue
        try:
            px = float(pp.group(1))
        except ValueError:
            continue
        sample.append((mm.group(1), int(tt.group(1)), px))
        wanted.add(mm.group(1))
print(json.dumps({"corpus_sample": len(sample), "sample_mints": len(wanted)}))

# --- stream the backfill: per-mint last event <= t_dec, and attribution stats
events = collections.defaultdict(list)
n_rows = n_null = n_pool_in_auth = n_pool_not_auth = n_no_pool_field = 0
pool_per_mint = collections.defaultdict(set)
for l in open(BACKFILL, encoding="utf-8"):
    n_rows += 1
    try:
        r = json.loads(l)
    except Exception:
        n_null += 1
        continue
    m = r.get("mint")
    if m not in wanted:
        continue
    p = r.get("pool")
    b, q = r.get("base_reserve"), r.get("quote_reserve")
    if not p:
        n_no_pool_field += 1
    else:
        pool_per_mint[m].add(p)
        if p in v2.get(m, []):
            n_pool_in_auth += 1
        else:
            n_pool_not_auth += 1
    events[m].append((int(r["recv_unix_ms"]), p, b, q))

print(json.dumps({"backfill_rows": n_rows, "decode_fail": n_null,
                  "pool_in_authoritative_wsol": n_pool_in_auth,
                  "pool_NOT_authoritative": n_pool_not_auth,
                  "no_pool_field": n_no_pool_field,
                  "auth_match_frac": round(n_pool_in_auth /
                                           max(1, n_pool_in_auth + n_pool_not_auth), 4)}))

# --- ratio: tape price vs (quote/base)/1e9
ratios = []
tokens = []
for m, t, px in sample:
    ev = events.get(m)
    if not ev:
        continue
    ev.sort(key=lambda x: x[0])
    cand = [e for e in ev if e[0] <= t]
    if not cand:
        continue
    ts, pool, b, q = cand[-1]
    if not b or not q or b <= 0 or q <= 0:
        continue
    amm_raw = (q / b) / SOL          # SOL per RAW token
    if px <= 0 or amm_raw <= 0:
        continue
    ratios.append(px / amm_raw)      # 1.0 => same unit
    tokens.append(amm_raw * 1e6)     # per whole token

if ratios:
    ratios.sort()
    qs = lambda f: ratios[min(len(ratios) - 1, int(f * len(ratios)))]
    print(json.dumps({
        "n_compared": len(ratios),
        "ratio_tape_over_reserve": {
            "p05": round(qs(0.05), 4), "p50": round(qs(0.50), 4),
            "p95": round(qs(0.95), 4), "min": round(ratios[0], 4),
            "max": round(ratios[-1], 4),
            "median_log10": round(statistics.median([__import__("math").log10(r)
                                                     for r in ratios if r > 0]), 4)},
        "frac_within_1pct": round(sum(1 for r in ratios if abs(r - 1) <= 0.01) / len(ratios), 5),
        "frac_within_10pct": round(sum(1 for r in ratios if abs(r - 1) <= 0.10) / len(ratios), 5),
        "frac_near_1e6": round(sum(1 for r in ratios if 5e5 < r < 2e6) / len(ratios), 5),
        "frac_near_1e3": round(sum(1 for r in ratios if 5e2 < r < 2e3) / len(ratios), 5),
    }, indent=1))

multi = {m: sorted(p) for m, p in pool_per_mint.items() if len(p) > 1}
print(json.dumps({"mints_with_multiple_backfill_pools": len(multi),
                  "examples": list(multi.items())[:3]}, indent=1))