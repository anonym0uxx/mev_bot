"""Relabel the entry SIZE dimension on the derived Kelly rule (R1 + R2).

WHY NOT THE OLD LABEL. The old size label was `argmax over tiers of a SINGLE-PATH
simulated value` at a pinned lambda = 0.20. Two problems, both named in the plan:
the per-row value is a sample of size one (its variance swamps the tier gap), and
the lambda was taste.

WHAT THIS DOES
  1. mu for a decision is estimated CROSS-SECTIONALLY, not from its own path: rows
     are pooled into causal strata (features available at t_dec only) and the
     stratum's mean NET barrier return is the estimate. That is what "label the
     distribution instead of one path" means operationally, and it is what removes
     the single-path variance.
  2. mu is shrunk toward the BUY grand mean by an empirical-Bayes weight so a thin
     stratum cannot manufacture a tier out of noise.
  3. The tier is the Kelly discretization on the derived lambda:
        tier = argmax_{w in {0,.25,.5,1}}  w*mu - lambda*w^2
     with lambda measured from the outcome distribution (derive_lambda_kelly.py),
     not chosen.

Only BUY rows get a size: WATCH and SKIP are no-position by construction, and
inventing a size for them would be supervision for an action the corpus does not
take. Unpriced rows get no tier and an explicit reason.

The strata are built from the ENRICHED causal fields, so the label is a function of
the fields the prompt carries - which is what makes them citable in EVIDENCE.
"""
import collections
import json
import math
import statistics as st
import sys

sys.path.insert(0, "/training/v2/code/src/v2/rl")
from exit_mechanics import cost_floor_bps, DEPLOY_SOL_CANONICAL  # noqa: E402

BAR = "/training/v2/reports/barrier_labels_c9.jsonl"
LAMBDA_REPORT = "/training/v2/reports/LAMBDA_KELLY_DERIVATION.json"
CORPUS = "/training/v2/candidate_sft_c9"
OUT = "/training/v2/reports/size_labels_c9.jsonl"
SPLITS = {"train": "train.jsonl", "validation": "validation.jsonl",
          "examination": "examination.jsonl"}
TIERS = (0.0, 0.25, 0.5, 1.0)
ACC = 1.01
FLOOR = {r: int(cost_floor_bps(r, DEPLOY_SOL_CANONICAL, 0, False, "p50"))
         for r in ("amm", "bonding_curve")}

lam_report = json.load(open(LAMBDA_REPORT, encoding="utf-8"))
LAM = float(lam_report["lambda_derived_uncensored"])
lam_used_from = "lambda_derived_uncensored"

# ---- per-row causal descriptors, straight from the enriched fields -------------
def tercile(x, cuts):
    if x is None:
        return "na"
    for i, c in enumerate(cuts):
        if x <= c:
            return "t%d" % (i + 1)
    return "t%d" % (len(cuts) + 1)


def holders_bucket(h):
    if h is None:
        return "na"
    if h < 50:
        return "thin"
    if h < 200:
        return "mid"
    return "thick"


def creator_bucket(c):
    if c is None:
        return "unknown"
    if c <= 0:
        return "fresh"
    if c <= 2:
        return "few"
    return "serial"


rows = {}
for sp, fn in SPLITS.items():
    with open("%s/%s" % (CORPUS, fn), encoding="utf-8") as fh:
        for i, ln in enumerate(fh):
            try:
                m = json.loads(ln)["meta"]
            except Exception:                                    # noqa: BLE001
                continue
            e = m.get("c9_enrichment") or {}
            rows[(sp, i)] = {
                "action": m.get("action"),
                "holders_at_t": e.get("holders_at_t"),
                "top1_float_share": e.get("top1_float_share"),
                "wash_ratio": e.get("wash_ratio"),
                "creator_past_launches": e.get("creator_past_launches"),
                "creator_known": e.get("creator_known"),
                "mcap_sol_at_t": e.get("mcap_sol_at_t"),
                "bundle_wallets": e.get("bundle_wallets"),
                "holder_hhi": e.get("holder_hhi"),
            }

# tercile cuts for top1_float_share, taken on the JOINED population (causal field,
# so using the whole population to place the cut is a data-description, not leakage)
t1 = [r["top1_float_share"] for r in rows.values() if r["top1_float_share"] is not None]
t1.sort()
CUT1 = [t1[int(0.33 * len(t1))], t1[int(0.66 * len(t1))]] if t1 else [0.0, 0.0]

for r in rows.values():
    r["stratum"] = "|".join((tercile(r["top1_float_share"], CUT1),
                             holders_bucket(r["holders_at_t"]),
                             creator_bucket(r["creator_past_launches"])))

# ---- per-row net barrier return ----------------------------------------------
def net_r(b):
    o = b.get("outcome")
    if o == "tp":
        g = 10_000.0
    elif o == "sl":
        g = -5_000.0
    elif o in ("timeout", "censored"):
        g = float(b["last_bp"])
    else:
        return None
    return (g - FLOOR.get(b.get("regime") or "amm", 0)) / 1e4


obs = {}
with open(BAR, encoding="utf-8") as fh:
    for ln in fh:
        try:
            b = json.loads(ln)
        except Exception:                                        # noqa: BLE001
            continue
        obs[(b["split"], b["line"])] = b

# ---- stratum means -----------------------------------------------------------
by_strat = collections.defaultdict(list)
buy_r_all = []
for k, r in rows.items():
    if r["action"] != "BUY":
        continue
    b = obs.get(k)
    if not b:
        continue
    v = net_r(b)
    if v is None:
        continue
    by_strat[r["stratum"]].append(v)
    buy_r_all.append(v)

grand = st.fmean(buy_r_all)
n_tot = len(buy_r_all)
var_within = st.pvariance(buy_r_all)
means = {s: st.fmean(v) for s, v in by_strat.items()}
var_between = st.pvariance(list(means.values())) if len(means) > 1 else 0.0
k_shrink = (var_within / var_between) if var_between > 1e-12 else 0.0

strat_stats = {}
for s, v in by_strat.items():
    n = len(v)
    w = n / (n + k_shrink) if (n + k_shrink) > 0 else 0.0
    mu = w * st.fmean(v) + (1.0 - w) * grand
    strat_stats[s] = {"n": n, "mu_raw": round(st.fmean(v), 6),
                      "mu_shrunk": round(mu, 6),
                      "se_raw": round(math.sqrt(st.pvariance(v) / max(1, n)), 6),
                      "shrink_weight_own": round(w, 4),
                      "tp_rate": round(sum(1 for x in v if x >= 1.0) / n, 4)}


def tier_for_mu(mu):
    best, bestv = 0.0, 0.0
    for w in TIERS:
        val = w * mu - LAM * w * w
        if val > bestv + 1e-12:
            best, bestv = w, val
    return best


THRESH = {"small": 0.25 * LAM, "mid": 0.75 * LAM, "full": 1.50 * LAM}

n_written = 0
tier_census = collections.Counter()
stratum_tier = collections.Counter()
with open(OUT, "w", encoding="utf-8") as fo:
    for (sp, i), r in sorted(rows.items()):
        rec = {"key": None, "split": sp, "line": i, "action": r["action"],
               "stratum": r["stratum"], "lambda_exposure": LAM,
               "lambda_source": lam_used_from, "tier_thresholds_on_mu": THRESH}
        b = obs.get((sp, i))
        rec["barrier_outcome"] = (b.get("outcome") if b else None)
        rec["censored"] = bool(b.get("censored")) if b else None
        if r["action"] == "BUY":
            ss = strat_stats.get(r["stratum"])
            if ss is None:
                rec.update({"status": "no_priced_buy_in_stratum", "size": None})
            else:
                t = tier_for_mu(ss["mu_shrunk"])
                rec.update({"status": "labeled", "size": ("FULL" if t == 1.0 else
                                                          "MID" if t == 0.5 else
                                                          "SMALL" if t == 0.25 else None),
                            "tier_fraction": t, "stratum_mu": ss["mu_shrunk"],
                            "stratum_mu_raw": ss["mu_raw"], "stratum_n": ss["n"],
                            "expectancy_at_tier": round(ss["mu_shrunk"] * t - LAM * t * t, 6)})
                tier_census[rec["size"] or "NONE"] += 1
                stratum_tier[(r["stratum"], rec["size"])] += 1
        else:
            rec.update({"status": "not_buy", "size": None})
        fo.write(json.dumps(rec) + "\n")
        n_written += 1

report = {"schema": "size_labels_kelly_v1", "lambda": LAM,
          "lambda_source": lam_used_from, "tier_thresholds_on_mu": THRESH,
          "grand_mu_buy": round(grand, 6), "n_buy_priced": n_tot,
          "var_within": round(var_within, 6), "var_between_strata": round(var_between, 6),
          "shrink_k": round(k_shrink, 4), "n_strata": len(strat_stats),
          "top1_tercile_cuts": CUT1, "tier_census_buy_rows": dict(tier_census),
          "rows_written": n_written,
          "strata": strat_stats,
          "strata_with_negative_mu": [s for s, v in strat_stats.items()
                                      if v["mu_shrunk"] <= 0]}
json.dump(report, open("/training/v2/reports/SIZE_LABELS_C9_REPORT.json", "w"), indent=1)

print(json.dumps({k: report[k] for k in (
    "lambda", "grand_mu_buy", "n_buy_priced", "var_within", "var_between_strata",
    "shrink_k", "n_strata", "tier_census_buy_rows", "rows_written",
    "strata_with_negative_mu")}, indent=1))
print("\nstrata (sorted by mu_shrunk):")
for s, v in sorted(strat_stats.items(), key=lambda kv: kv[1]["mu_shrunk"]):
    print("  %-28s n=%5d mu_raw=%+.4f mu_shrunk=%+.4f se=%.4f tier=%s" % (
        s, v["n"], v["mu_raw"], v["mu_shrunk"], v["se_raw"],
        tier_for_mu(v["mu_shrunk"])))
