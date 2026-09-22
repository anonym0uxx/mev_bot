#!/usr/bin/env python
"""SETTLEMENT TEST: is the corpus `price_sol_per_raw` a UNIT mislabel, or a different
quantity altogether?

The first pass joined the last backfill event by MINT alone over 150k rows and got a
ratio spread of 1.3e3 .. 6.9e13 (median 1.02e9) - ten orders of magnitude, which means
that join was too noisy to pin anything (mint-level joins mix pools and pick up stale
events).

This pass is deliberately restrictive so that what is left is the unit relationship:
  * mints whose SOLE pump-swap pool is WSOL (463 of 765) - no pool ambiguity at all;
  * the backfill event must be from THAT pool and within TIGHT_MS of t_dec;
  * report the ratio distribution and, if it collapses, the single factor k.

ratio = prompt_price / (quote_reserve / base_reserve / 1e9)   # SOL per RAW token
  ratio ~ 1     -> prompt price is SOL per RAW token (no fault)
  ratio ~ 1e6   -> prompt price is SOL per WHOLE token, field name is wrong
  ratio ~ 1e9   -> prompt price is lamports per WHOLE token
"""
import collections
import json
import math
import os
import re
import statistics

C3 = "/training/v2/candidate_sft_c3"
V2 = "/training/v2/reports/AMM_POOLS_RESOLVED_V2.jsonl"
BACKFILL = "/training/v2/reserves/amm_history_v1/amm_reserves.jsonl"
OUT = "/training/v2/reports/FAULT_price_settlement.json"
TIGHT_MS = 30_000
SOL = 1_000_000_000.0

MINT = re.compile(rb'"mint":\s*"([^"]+)"')
TMS = re.compile(rb"t_dec_ms=(\d{13})")
PRX = re.compile(rb"price_sol_per_raw=([0-9eE.+\-]+)")

# ---- 1. unambiguous mints: exactly one pool and it is WSOL
sole = {}
for l in open(V2, encoding="utf-8"):
    r = json.loads(l)
    if r["n_pools"] == 1 and len(r["wsol_pools"]) == 1:
        sole[r["mint"]] = r["wsol_pools"][0]
print(json.dumps({"sole_wsol_pool_mints": len(sole)}))

# ---- 2. corpus rows for those mints (union of splits)
rows = []
for split in ("train", "validation", "examination"):
    p = os.path.join(C3, f"{split}.jsonl")
    with open(p, "rb") as f:
        for line in f:
            m = MINT.search(line)
            if not m:
                continue
            mint = m.group(1).decode()
            if mint not in sole:
                continue
            t, px = TMS.search(line), PRX.search(line)
            if not (t and px):
                continue
            try:
                fpx = float(px.group(1))
            except ValueError:
                continue
            rows.append((mint, int(t.group(1)), fpx))
print(json.dumps({"corpus_rows_sole_pool": len(rows),
                  "mints": len({r[0] for r in rows})}))

# ---- 3. backfill events restricted to those mints AND their own pool
want = {r[0] for r in rows}
ev = collections.defaultdict(list)
kept = dropped_pool = 0
for l in open(BACKFILL, encoding="utf-8"):
    if '"mint"' not in l:
        continue
    try:
        r = json.loads(l)
    except Exception:
        continue
    m = r.get("mint")
    if m not in want:
        continue
    if r.get("pool") != sole[m]:
        dropped_pool += 1
        continue
    kept += 1
    ev[m].append((int(r["recv_unix_ms"]), r.get("base_reserve"), r.get("quote_reserve")))
print(json.dumps({"events_kept": kept, "events_wrong_pool_dropped": dropped_pool}))

for m in ev:
    ev[m].sort(key=lambda x: x[0])

# ---- 4. tight join
ratios, deltas, skipped = [], [], collections.Counter()
for mint, t, px in rows:
    cands = ev.get(mint)
    if not cands:
        skipped["no_event"] += 1
        continue
    best = None
    for e in cands:
        if e[0] <= t and (best is None or e[0] > best[0]):
            best = e
    if best is None:
        skipped["no_event_before_t_dec"] += 1
        continue
    if t - best[0] > TIGHT_MS:
        skipped["stale_over_%dms" % TIGHT_MS] += 1
        continue
    b, q = best[1], best[2]
    if not b or not q or b <= 0 or q <= 0:
        skipped["bad_reserve"] += 1
        continue
    amm_raw = (q / b) / SOL
    ratio = px / amm_raw
    ratios.append(ratio)
    deltas.append(t - best[0])

res = {"schema": "fault_price_settlement_v1", "tight_ms": TIGHT_MS,
       "sole_pool_mints": len(sole), "corpus_rows": len(rows),
       "events_kept": kept, "events_wrong_pool_dropped": dropped_pool,
       "joined": len(ratios), "skipped": dict(skipped)}
if ratios:
    ratios.sort()
    logs = sorted(math.log10(r) for r in ratios if r > 0)
    q = lambda f: ratios[min(len(ratios) - 1, int(f * len(ratios)))]
    med = statistics.median(ratios)
    vs_med = sorted(abs(math.log10(r / med)) for r in ratios if r > 0)
    res["ratio"] = {
        "p01": q(0.01), "p05": q(0.05), "p50": q(0.50), "p95": q(0.95),
        "p99": q(0.99), "min": ratios[0], "max": ratios[-1],
        "log10_p05": logs[int(0.05 * len(logs))], "log10_p50": logs[len(logs) // 2],
        "log10_p95": logs[int(0.95 * len(logs))],
        "median_factor_k": med,
        "log10_spread_around_median_p05_p95": [vs_med[int(0.05 * len(vs_med))],
                                               vs_med[int(0.95 * len(vs_med))]],
        "frac_within_2x_of_median": round(
            sum(1 for r in ratios if 0.5 <= r / med <= 2.0) / len(ratios), 4),
        "frac_near_1": round(sum(1 for r in ratios if 0.5 < r < 2) / len(ratios), 5),
        "frac_near_1e3": round(sum(1 for r in ratios if 5e2 < r < 2e3) / len(ratios), 5),
        "frac_near_1e6": round(sum(1 for r in ratios if 5e5 < r < 2e6) / len(ratios), 5),
        "frac_near_1e9": round(sum(1 for r in ratios if 5e8 < r < 2e9) / len(ratios), 5),
    }
    res["join_latency_ms"] = {"p50": statistics.median(deltas),
                              "p90": sorted(deltas)[int(0.9 * len(deltas))],
                              "max": max(deltas)}
    k = res["ratio"]["median_factor_k"]
    verdicts = {1.0: "prompt price IS SOL per RAW token - no unit fault",
                1e6: "prompt price is SOL per WHOLE token - FIELD NAME/UNIT FAULT",
                1e9: "prompt price is LAMPORTS per WHOLE token - UNIT FAULT"}
    res["verdict"] = next((v for kk, v in verdicts.items()
                           if abs(math.log10(k / kk)) < 0.15), "NO clean factor: different quantity")
    res["verdict_detail"] = ("median factor k=%.4g (log10=%.3f)" % (k, math.log10(k)))

os.makedirs(os.path.dirname(OUT), exist_ok=True)
json.dump(res, open(OUT, "w"), indent=1)
print(json.dumps(res, indent=1))
print("WROTE", OUT)