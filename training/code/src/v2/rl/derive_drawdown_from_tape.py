"""Derive the max-drawdown target from the tape's REAL adverse excursions.

Operator ruling: pick the drawdown most likely to be mirrored in production pump.fun trading,
derived from factual on-chain data.

Method: a position's drawdown IS its adverse excursion. So, for real corpus decisions:
  entry price = last trade price at/before t_dec  (lamports per raw token)
  forward path = that mint's trades after t_dec
  MAE = (min forward price - entry)/entry, measured over the holding horizon
Then the constraint is set at a level the data actually produces.

SOL/WSOL only: pumpswap+curve rows are already SOL-quoted; any non-SOL venue is skipped.
"""
import json
import os
import collections
import datetime

CORPUS = '/training/v2/candidate_sft_c8/train.jsonl'
TAPE = '/training/v2/canonical/renormalized_v7/trades.jsonl'
OUT = '/training/v2/reports/DRAWDOWN_DERIVATION.json'
N_MINTS = 700
HORIZONS_MS = (300_000, 1_800_000)

# ---- 1. pick decisions (spread across the corpus, capped by mint count)
dec = collections.defaultdict(list)          # mint -> [t_dec]
with open(CORPUS, encoding='utf-8') as fh:
    for line in fh:
        r = json.loads(line)
        m = r.get('meta') or {}
        if m.get('task') != 'decision_action':
            continue
        mint = m.get('mint')
        sa = (m.get('source_associations') or [])
        if not mint or not sa:
            continue
        row = ((sa[0] or {}).get('source') or {}).get('row', '')
        parts = row.split(':')
        if len(parts) != 3 or not parts[2].isdigit():
            continue
        dec[mint].append(int(parts[2]))

mints = sorted(dec)[:N_MINTS]
mset = set(mints)
n_dec = sum(len(dec[m]) for m in mints)
print(f"mints={len(mints)} decisions={n_dec}", flush=True)

# ---- 2. one pass over the tape for those mints
series = collections.defaultdict(list)
rows = 0
with open(TAPE, encoding='utf-8') as fh:
    for line in fh:
        rows += 1
        if rows % 2_000_000 == 0:
            print(f"  scanned {rows:,}", flush=True)
        if '"mint":"' not in line and '"mint": "' not in line:
            continue
        try:
            r = json.loads(line)
        except Exception:
            continue
        mint = r.get('mint')
        if mint not in mset:
            continue
        tok = abs(r.get('tokens_raw') or 0)
        sol = abs(r.get('sol_lamports') or 0)
        t = r.get('recv_unix_ms')
        if not t or tok <= 0 or sol <= 0:
            continue
        series[mint].append((int(t), sol / tok))       # lamports per raw token
print(f"tape rows scanned: {rows:,}; mints with a series: {len(series)}", flush=True)

res = {}
for H in HORIZONS_MS:
    mae = []
    for mint, tds in dec.items():
        s = series.get(mint)
        if not s or len(s) < 3:
            continue
        s.sort()
        ts = [x[0] for x in s]
        for t_dec in tds:
            lo, hi = 0, len(ts)
            while lo < hi:
                mid = (lo + hi) // 2
                if ts[mid] <= t_dec:
                    lo = mid + 1
                else:
                    hi = mid
            if lo == 0:
                continue
            entry = s[lo - 1][1]
            if entry <= 0:
                continue
            fwd = [p for (t, p) in s[lo:] if t <= t_dec + H and p > 0]
            if len(fwd) < 2:
                continue
            # a price cannot fall more than 100%: floor the excursion, and reject any
            # path whose minimum is non-positive as a data artefact rather than a price
            mn = min(fwd)
            if mn <= 0:
                continue
            mae.append(max(-1.0, (mn - entry) / entry))
    mae.sort()
    if not mae:
        res[str(H)] = {"n": 0}
        continue
    q = lambda f: mae[min(len(mae) - 1, int(f * len(mae)))]
    res[str(H)] = {
        "n": len(mae),
        "p10": round(q(.10), 4), "p25": round(q(.25), 4), "p50": round(q(.50), 4),
        "p75": round(q(.75), 4), "p90": round(q(.90), 4), "p95": round(q(.95), 4),
        "p99": round(q(.99), 4), "worst": round(mae[0], 4), "mean": round(sum(mae) / len(mae), 4),
        "frac_worse_than_-25pct": round(sum(1 for x in mae if x < -0.25) / len(mae), 4),
        "frac_worse_than_-50pct": round(sum(1 for x in mae if x < -0.50) / len(mae), 4),
        "frac_worse_than_-75pct": round(sum(1 for x in mae if x < -0.75) / len(mae), 4),
    }
    print(f"horizon {H//1000}s -> {json.dumps(res[str(H)])}", flush=True)

json.dump({
    "source_tape": TAPE, "corpus": CORPUS, "mints": len(mints), "decisions": n_dec,
    "method": "MAE = (min forward price - entry)/entry, entry = last trade price <= t_dec",
    "quote_filter": "SOL-quoted venues only (curve + pumpswap)",
    "by_horizon_ms": res,
}, open(OUT, 'w', encoding='utf-8'), indent=1)
print("wrote", OUT)
