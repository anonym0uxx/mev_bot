"""Venue-separated size re-derivation (correct (split,line) join).

Corrects the pooled-lambda false positivity: the pooled size label pooled AMM and
curve returns inside each causal stratum, so curve rows inherited AMM's positive
edge. Re-pools by (stratum, REGIME), shrinks each venue toward its own grand mean,
and sizes AMM at lambda_amm while fixing curve to a SMALL satellite (Kelly = 0).

Uses the SAME (split,line) keying as relabel_size_kelly.py (barrier + size_labels
are both keyed that way), NOT (mint,t_dec), which is lossy (duplicate keys).
"""
import collections
import json
import math
import statistics as st
import sys

sys.path.insert(0, "/training/v2/code/src/v2/rl")
from exit_mechanics import cost_floor_bps, DEPLOY_SOL_CANONICAL  # noqa: E402

BAR = "/training/v2/reports/barrier_labels_c9.jsonl"
SIZE_LABELS = "/training/v2/reports/size_labels_c9.jsonl"
ACC = 1.01
LAM_AMM = 0.13681  # E[r^2]/(2A^2) AMM BUY-only (censored-included), venue_lambda_buy.py
TIERS = (0.0, 0.25, 0.5, 1.0)
FLOOR = {r: int(cost_floor_bps(r, DEPLOY_SOL_CANONICAL, 0, False, "p50"))
         for r in ("amm", "bonding_curve")}


def gross_bp(b):
    o = b.get("outcome")
    if o == "tp":
        return 10000.0
    if o == "sl":
        return -5000.0
    return float(b.get("last_bp", 0.0))


def net_ret(b):
    reg = b.get("regime") or "amm"
    return (gross_bp(b) - FLOOR[reg]) / 1e4


# barrier by (split, line): regime + outcome + last_bp
obs = {}
with open(BAR, encoding="utf-8") as fh:
    for ln in fh:
        b = json.loads(ln)
        obs[(b["split"], b["line"])] = b

# size_labels by (split, line): stratum + action + barrier_outcome
stratum_of = {}
with open(SIZE_LABELS, encoding="utf-8") as fh:
    for ln in fh:
        d = json.loads(ln)
        stratum_of[(d["split"], d["line"])] = (d.get("action"), d.get("stratum"))

# join -> (stratum, regime) -> net returns for BUY rows
pool = collections.defaultdict(list)
for k in stratum_of:
    action, stratum = stratum_of[k]
    if action != "BUY":
        continue
    b = obs.get(k)
    if not b:
        continue
    v = net_ret(b) if b.get("outcome") in ("tp", "sl", "timeout", "censored") else None
    if v is None:
        continue
    reg = b.get("regime") or "amm"
    pool[(stratum, reg)].append(v)

# venue-separated empirical-Bayes shrinkage
grand = {}
shrink = {}
means = {}
for reg in ("amm", "bonding_curve"):
    xs = [v for (s, g), vs in pool.items() if g == reg for v in vs]
    if not xs:
        grand[reg] = 0.0
        shrink[reg] = 0.0
        continue
    grand[reg] = st.fmean(xs)
    var_within = st.pvariance(xs)
    strat_means = [st.fmean(vs) for (s, g), vs in pool.items() if g == reg]
    var_between = st.pvariance(strat_means) if len(strat_means) > 1 else 0.0
    k = (var_within / var_between) if var_between > 1e-12 else 0.0
    shrink[reg] = k
    for (s, g), vs in pool.items():
        if g != reg:
            continue
        n = len(vs)
        w = n / (n + k) if (n + k) > 0 else 0.0
        means[(s, g)] = w * st.fmean(vs) + (1.0 - w) * grand[reg]


def tier_amm(mu):
    best, bestv = 0.0, 0.0
    for w in TIERS:
        val = w * mu - LAM_AMM * w * w
        if val > bestv + 1e-12:
            best, bestv = w, val
    return best


def size_for(stratum, reg):
    if reg == "amm":
        mu = means.get((stratum, "amm"))
        if mu is None:
            return None, None
        t = tier_amm(mu)
        return ("FULL" if t == 1.0 else "MID" if t == 0.5 else
                "SMALL" if t == 0.25 else None), mu
    # curve: fixed satellite (Kelly = 0)
    return "SMALL", means.get((stratum, "bonding_curve"))


# emit the (stratum, regime) -> size map for the apply step
tier_map = {}
for (s, g) in sorted(means):
    sz, mu = size_for(s, g)
    tier_map[f"{s}|{g}"] = {"stratum": s, "regime": g, "mu_shrunk": round(mu, 6) if mu else None,
                            "size": sz}

json.dump({
    "schema": "venue_size_map_v1", "lambda_amm": LAM_AMM,
    "floor": FLOOR, "grand_means": {k: round(v, 6) for k, v in grand.items()},
    "shrink_k": {k: round(v, 4) for k, v in shrink.items()},
    "n_strata_venue": len(means), "tier_map": tier_map,
}, open("/training/v2/reports/VENUE_SIZE_MAP.json", "w"), indent=1)

print("=== venue-separated grand means ===", {k: round(v, 6) for k, v in grand.items()})
print("=== shrink k ===", {k: round(v, 4) for k, v in shrink.items()})
print("=== n (stratum, regime) cells ===", len(means))
print("=== tier census by venue ===")
census = collections.Counter()
for (s, g), mu in means.items():
    sz, _ = size_for(s, g)
    census[(g, sz)] += 1
for k in sorted(census, key=lambda x: (x[0], str(x[1]))):
    print(f"  {k[0]:15s} {str(k[1]):6s} strata={census[k]}")
print("-> /training/v2/reports/VENUE_SIZE_MAP.json")
