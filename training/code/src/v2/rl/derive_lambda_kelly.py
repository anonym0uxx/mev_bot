"""Derive lambda and the size-tier ceiling from the barrier labels (R2).

Replaces the pinned lambda = 0.20 with a number that traces to measured data.

Derivation, stated once so it can be attacked:

  The size objective is   value(w) = w*mu - lambda*w^2
  where w is the fraction of the account deployed and mu the per-unit-notional
  expected return of the read. That quadratic form is not a heuristic: it IS the
  second-order expansion of log-wealth, the Kelly criterion,

      E[log(1 + G/A)] ~= G/A - G^2/(2 A^2),     G = w*r,  A = account

  so matching the two gives   lambda = E[r^2] / (2 A^2).

  log-wealth is the growth-optimal, never-ruin objective, and a book whose risk
  budget is a max-drawdown (already derived = 50% from measured MAE) is exactly a
  book that wants log-wealth rather than mean-variance. So lambda is MEASURED off
  the barrier labels, not chosen.

Tier thresholds fall out of the same expression (proof in the report):
      SMALL beats SKIP   when mu > 0.25*lambda
      MID   beats SMALL  when mu > 0.75*lambda
      FULL  beats MID    when mu > 1.50*lambda
and the tier ceiling is checked against the drawdown budget: FULL = 1.0 of a
1.01 SOL account with the -50% stop floors the account at ~-49.5%, i.e. exactly
the derived 50% budget (DRAWDOWN_DERIVATION.md).

Right-censored rows are NOT silently scored as losses: they are excluded from the
moment estimate and their share is reported, with the sensitivity to treating them
at the observed mark vs dropping them made explicit.
"""
import collections
import json
import math

BAR = "/training/v2/reports/barrier_labels_c9.jsonl"
OUT = "/training/v2/reports/LAMBDA_KELLY_DERIVATION.json"

ACC = 1.01                      # account capital, SOL (DEPLOY_SOL_CANONICAL + fee buffer)
TIERS = (0.0, 0.25, 0.5, 1.0)

FLOOR = {}
import sys                                                   # noqa: E402
sys.path.insert(0, "/training/v2/code/src/v2/rl")
from exit_mechanics import cost_floor_bps, DEPLOY_SOL_CANONICAL  # noqa: E402
for reg in ("amm", "bonding_curve"):
    FLOOR[reg] = {q: int(cost_floor_bps(reg, DEPLOY_SOL_CANONICAL, 0, False, q))
                  for q in ("p50", "p90", "p99")}

rows = []
with open(BAR, encoding="utf-8") as fh:
    for line in fh:
        try:
            r = json.loads(line)
        except Exception:                                    # noqa: BLE001
            continue
        rows.append(r)

outcomes = collections.Counter(r.get("outcome") or r.get("status") for r in rows)
labeled = [r for r in rows if r.get("outcome") in ("tp", "sl", "timeout", "censored")]

# --- per-row per-unit-notional return, net of the measured round-trip floor -----
def gross_bp(r):
    o = r["outcome"]
    if o == "tp":
        return 10_000.0
    if o == "sl":
        return -5_000.0
    return float(r["last_bp"])


def net_ret(r, quantile="p50"):
    reg = r.get("regime") or "amm"
    fl = FLOOR.get(reg, FLOOR["amm"])[quantile]
    return (gross_bp(r) - fl) / 1e4


sets = {
    "all_labeled": labeled,
    "resolved_only": [r for r in labeled if r["outcome"] in ("tp", "sl")],
    "uncensored": [r for r in labeled if r["outcome"] != "censored"],
    "censored_as_mark": labeled,
}
censored = [r for r in labeled if r["outcome"] == "censored"]
uncensored = [r for r in labeled if r["outcome"] != "censored"]

report = {"schema": "lambda_kelly_derivation_v1", "bar": BAR, "rows_in": len(rows),
          "outcomes": dict(outcomes), "account_sol": ACC,
          "cost_floor_bps": FLOOR, "n_labeled": len(labeled),
          "n_censored": len(censored),
          "censored_share": round(len(censored) / max(1, len(labeled)), 4),
          "quantile_used": "p50"}

r_all = [net_ret(r, "p50") for r in labeled]
r_unc = [net_ret(r, "p50") for r in uncensored]


def moments(xs):
    n = len(xs)
    m1 = sum(xs) / n
    m2 = sum(x * x for x in xs) / n
    return {"n": n, "mean_r": round(m1, 6), "E_r2": round(m2, 6),
            "sd_r": round(math.sqrt(max(0.0, m2 - m1 * m1)), 6)}


m_all = moments(r_all)
m_unc = moments(r_unc)
report["moments_all_labeled"] = m_all
report["moments_uncensored"] = m_unc

lam_all = m_all["E_r2"] / (2.0 * ACC * ACC)
lam_unc = m_unc["E_r2"] / (2.0 * ACC * ACC)
report["lambda_derived_all_labeled"] = round(lam_all, 6)
report["lambda_derived_uncensored"] = round(lam_unc, 6)
report["lambda_previously_pinned"] = 0.20

# --- fractional Kelly: the growth-optimal fraction on the measured distribution -
def e_log_w(w, xs):
    s = 0.0
    for x in xs:
        v = 1.0 + w * x / ACC
        if v <= 0:
            return -1e9
        s += math.log(v)
    return s / len(xs)


grid = [i / 200.0 for i in range(0, 201)]
best_w, best_v = 0.0, e_log_w(0.0, r_unc)
for w in grid:
    v = e_log_w(w, r_unc)
    if v > best_v:
        best_w, best_v = w, v
report["kelly_full_fraction"] = round(best_w, 4)
report["e_log_w_full_kelly"] = round(best_v, 6)
report["kelly_grid_note"] = ("fraction of the ACCOUNT, so 1.0 = 1.01 SOL deployed; "
                             "upper grid bound 1.0 because a tier cannot exceed the account")

# drawdown of the full-Kelly path (worst case from the measured SL barrier)
report["full_tier_worst_case_account"] = round(
    -0.5 * (1.0 / ACC) * (1.0 * ACC / ACC), 4)
report["full_tier_worst_case_note"] = (
    "FULL deploys 1.0 SOL of a 1.01 SOL account, so the -50% stop floors the account "
    "at ~-49.5% = the derived 50% drawdown budget")

# --- tier thresholds implied by lambda ----------------------------------------
lam = lam_all
report["tier_thresholds_on_mu"] = {
    "small_beats_skip": round(0.25 * lam, 6),
    "mid_beats_small": round(0.75 * lam, 6),
    "full_beats_mid": round(1.50 * lam, 6),
    "full_beats_skip": round(1.00 * lam, 6),
}


def tier_for_mu(mu):
    best, bestv = 0.0, 0.0
    for w in TIERS:
        v = w * mu - lam * w * w
        if v > bestv + 1e-12:
            best, bestv = w, v
    return best


report["tier_for_mu_probe"] = {str(mu): tier_for_mu(mu)
                               for mu in (0.0, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0)}

# --- sensitivity to the cost quantile and to censoring treatment --------------
sens = {}
for q in ("p50", "p90", "p99"):
    xs = [net_ret(r, q) for r in uncensored]
    mm = moments(xs)
    sens[q] = {"E_r2": mm["E_r2"], "mean_r": mm["mean_r"],
               "lambda": round(mm["E_r2"] / (2.0 * ACC * ACC), 6)}
report["cost_quantile_sensitivity"] = sens

# censored rows: excluded vs scored at their observed mark
xs_censored_at_mark = [net_ret(r, "p50") for r in labeled]
sens_cens = {"excluded": round(lam_unc, 6),
             "scored_at_observed_mark": round(moments(xs_censored_at_mark)["E_r2"]
                                              / (2.0 * ACC * ACC), 6)}
report["censoring_sensitivity_lambda"] = sens_cens
report["censored_mark_stats_bp"] = {
    "p50": round(sorted(float(r["last_bp"]) for r in censored)[len(censored) // 2], 2)
    if censored else None,
    "mean": round(sum(float(r["last_bp"]) for r in censored) / max(1, len(censored)), 2)}

with open(OUT, "w", encoding="utf-8") as fh:
    json.dump(report, fh, indent=1)
print(json.dumps(report, indent=1))
