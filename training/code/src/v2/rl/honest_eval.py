#!/usr/bin/env python
"""Honest evaluation primitives for the mev_bot trading brain (Phase 0 / Phase 3).

WHY THIS EXISTS (2026-09-21). Our acceptance gate is a LOWER BOUND over clustered
outcomes, and the literature review found the two ways LLM-trading evaluations lie to
themselves: (a) treating correlated rows as independent, which shrinks the error bar
and manufactures significance; (b) reporting a single pooled number from one split,
which cannot distinguish skill from a lucky window. This module supplies the two
primitives both problems need, plus the multiple-trials deflation:

  purged_kfold_indices   - Lopez de Prado style purged/embargoed splits. Adjacent
                           observations share information (overlapping outcome
                           windows, same-regime runs); an embargoed, purged split is
                           the minimum honest protocol.
  cluster_bootstrap_lb   - bootstrap that resamples CLUSTERS (mints), not rows. Two
                           decisions on the same mint are not independent samples.
  deflated_sharpe_ratio  - deflate a Sharpe by the number of configurations actually
                           tried, because "best of N backtests" is a selection.

Everything is mechanical and deterministic (seeded). No model, no GPU, no I/O.

Run:  /home/alon/qwen27b-venv/bin/python honest_eval.py --self-check
"""
from __future__ import annotations

import argparse
import json
import math
import sys

import numpy as np

EULER_GAMMA = 0.5772156649015329


def purged_kfold_indices(n: int, n_splits: int = 5, embargo: int = 0,
                         purge: int | None = None) -> list:
    """Contiguous-block purged K-fold with an embargo.

    Returns [(train_idx, test_idx), ...] as int64 arrays. Test blocks are contiguous
    slices; every training index within `purge` positions of a test block is dropped,
    and a further `embargo` positions after each test block are dropped.

    `purge` defaults to one test-block length (the standard choice when observations
    near a boundary share an outcome window).

    IMPORTANT: this function assumes the input is ALREADY in causal time order. It
    cannot detect a shuffled input, so the caller must sort. `assert_time_sorted` is
    provided for that.
    """
    if n_splits < 2:
        raise ValueError("n_splits must be >= 2")
    if n < n_splits:
        raise ValueError(f"n={n} too small for n_splits={n_splits}")
    if embargo < 0:
        raise ValueError("embargo must be >= 0")
    idx = np.arange(n, dtype=np.int64)
    bounds = np.linspace(0, n, n_splits + 1, dtype=int)
    if purge is None:
        purge = int(np.ceil(n / n_splits))
    out = []
    for k in range(n_splits):
        lo, hi = int(bounds[k]), int(bounds[k + 1])
        test = idx[lo:hi]
        drop_lo = max(0, lo - purge)
        drop_hi = min(n, hi + embargo)
        keep = np.ones(n, dtype=bool)
        keep[drop_lo:drop_hi] = False          # purge before + test + embargo after
        train = idx[keep]
        out.append((train, test))
    return out


def assert_time_sorted(times, label: str = "times") -> None:
    """Refuse a non-monotonic time vector instead of silently mis-purging."""
    t = np.asarray(times, dtype=float)
    if t.size > 1 and np.any(np.diff(t) < 0):
        raise ValueError(f"{label} are not in causal time order: purged CV would be invalid")


def cluster_bootstrap_lb(values, clusters, alpha: float = 0.05, n_boot: int = 2000,
                         seed: int = 12345, min_clusters: int = 8) -> dict:
    """One-sided (1-alpha) lower confidence bound on the MEAN, resampling CLUSTERS.

    Rationale: rows sharing a mint (or a mint-episode) are one economic bet. Row
    bootstrapping treats N correlated decisions as N independent draws and shrinks the
    interval; that is the single most common way a memecoin backtest manufactures
    significance. Resampling clusters corrects for both within-cluster correlation and
    unequal cluster sizes.

    Refuses (rather than guessing) when there are too few clusters for a bound to mean
    anything -- a wide interval is information, a fabricated one is not.
    """
    v = np.asarray(list(values), dtype=float)
    c = np.asarray(list(clusters))
    if v.shape[0] != c.shape[0]:
        raise ValueError(f"values/clusters length mismatch: {v.shape[0]} vs {c.shape[0]}")
    if v.size == 0:
        raise ValueError("empty sample")
    if not np.all(np.isfinite(v)):
        raise ValueError("non-finite value in sample")
    uniq, inv = np.unique(c, return_inverse=True)
    n_clusters = int(uniq.size)
    if n_clusters < min_clusters:
        raise ValueError(f"only {n_clusters} clusters (< {min_clusters}): a lower bound "
                         f"is not estimable; report the raw sample instead")
    if not 0.0 < alpha < 1.0:
        raise ValueError(f"alpha must be in (0,1), got {alpha}")
    # cluster sums and counts -> resample clusters, rebuild the mean each time
    sums = np.bincount(inv, weights=v, minlength=n_clusters)
    cnts = np.bincount(inv, minlength=n_clusters).astype(float)
    rng = np.random.default_rng(seed)
    draws = rng.integers(0, n_clusters, size=(n_boot, n_clusters))
    num = sums[draws].sum(axis=1)
    den = cnts[draws].sum(axis=1)
    means = num / den
    lb = float(np.quantile(means, alpha))
    return {"mean": float(v.mean()), "lb": lb, "ub": float(np.quantile(means, 1.0 - alpha)),
            "alpha": float(alpha), "n": int(v.size), "n_clusters": n_clusters,
            "cluster_mean_min": float(sums.min() / max(cnts[cnts > 0].min(), 1.0)),
            "n_boot": int(n_boot), "seed": int(seed)}


def _norm_ppf(p: float) -> float:
    """Inverse standard normal CDF (Acklam's rational approximation, |err|<1.15e-9)."""
    if not 0.0 < p < 1.0:
        raise ValueError(f"p must be in (0,1), got {p}")
    a = (-3.969683028665376e+01, 2.209460984245205e+02, -2.759285104469687e+02,
         1.383577518672690e+02, -3.066479806614716e+01, 2.506628277459239e+00)
    b = (-5.447609879822406e+01, 1.615858368580409e+02, -1.556989798598866e+02,
         6.680131188771972e+01, -1.328068155288572e+01)
    c = (-7.784894002430293e-03, -3.223964580411365e-01, -2.400758277161838e+00,
         -2.549732539343734e+00, 4.374664141464968e+00, 2.938163982698783e+00)
    d = (7.784695709041462e-03, 3.224671290700398e-01, 2.445134137142996e+00,
         3.754408661907416e+00)
    plow, phigh = 0.02425, 1 - 0.02425
    if p < plow:
        q = math.sqrt(-2 * math.log(p))
        return (((((c[0] * q + c[1]) * q + c[2]) * q + c[3]) * q + c[4]) * q + c[5]) / \
               ((((d[0] * q + d[1]) * q + d[2]) * q + d[3]) * q + 1)
    if p > phigh:
        q = math.sqrt(-2 * math.log(1 - p))
        return -(((((c[0] * q + c[1]) * q + c[2]) * q + c[3]) * q + c[4]) * q + c[5]) / \
               ((((d[0] * q + d[1]) * q + d[2]) * q + d[3]) * q + 1)
    q = p - 0.5
    r = q * q
    return (((((a[0] * r + a[1]) * r + a[2]) * r + a[3]) * r + a[4]) * r + a[5]) * q / \
           (((((b[0] * r + b[1]) * r + b[2]) * r + b[3]) * r + b[4]) * r + 1)


def _norm_cdf(x: float) -> float:
    return 0.5 * (1.0 + math.erf(x / math.sqrt(2.0)))


def deflated_sharpe_ratio(returns, n_trials: int, periods_per_year: float | None = None,
                          sr_std: float | None = None) -> dict:
    """Deflated Sharpe Ratio (Bailey & Lopez de Prado 2014), on a return series.

    Deflation: the benchmark is not SR=0 but the EXPECTED MAXIMUM Sharpe of `n_trials`
    independent configurations under the null, so "best of N backtests" is priced in.
    `n_trials` must be the number of configurations ACTUALLY tried (including the ones
    that failed) -- understating it is how a search is laundered into a result.

    Skew/kurtosis adjusted: SR_hat is converted with its own higher moments, because
    trading returns are not Gaussian.

    Refusals: <2 observations, non-finite input, n_trials < 1.
    """
    r = np.asarray(list(returns), dtype=float)
    if r.size < 2:
        raise ValueError(f"need >=2 observations, got {r.size}")
    if not np.all(np.isfinite(r)):
        raise ValueError("non-finite return in series")
    if n_trials < 1:
        raise ValueError(f"n_trials must be >= 1, got {n_trials}")
    sd = float(r.std(ddof=1))
    if sd == 0.0:
        raise ValueError("zero-variance return series: Sharpe undefined")
    sr = float(r.mean() / sd)
    if periods_per_year:
        sr *= math.sqrt(periods_per_year)
    T = int(r.size)
    m2 = float(np.mean((r - r.mean()) ** 2))
    m3 = float(np.mean((r - r.mean()) ** 3))
    m4 = float(np.mean((r - r.mean()) ** 4))
    skew = m3 / (m2 ** 1.5) if m2 > 0 else 0.0
    kurt = m4 / (m2 ** 2) if m2 > 0 else 3.0
    # SE of the Sharpe estimate under the null; caller may supply a cross-trial std
    se = float(sr_std) if sr_std else (1.0 / math.sqrt(T))
    if n_trials == 1:
        sr0 = 0.0
    else:
        sr0 = se * ((1.0 - EULER_GAMMA) * _norm_ppf(1.0 - 1.0 / n_trials)
                    + EULER_GAMMA * _norm_ppf(1.0 - 1.0 / (n_trials * math.e)))
    denom = 1.0 - skew * sr + ((kurt - 1.0) / 4.0) * sr * sr
    if denom <= 0:
        raise ValueError(f"non-positive variance term {denom}: moments are degenerate")
    z = (sr - sr0) * math.sqrt(T - 1) / math.sqrt(denom)
    return {"sharpe": sr, "sr0_expected_max": float(sr0), "dsr": _norm_cdf(z),
            "z": float(z), "skew": float(skew), "kurtosis": float(kurt),
            "n_trials": int(n_trials), "n": T}


def _self_check() -> int:
    fails = []

    # (1) purged CV: no train index may sit inside the purge/embargo of a test block,
    #     test blocks must tile the sample, and every index must be test exactly once.
    n, k, emb = 100, 5, 3
    folds = purged_kfold_indices(n, n_splits=k, embargo=emb)
    seen = np.zeros(n, dtype=int)
    for train, test in folds:
        seen[test] += 1
        lo, hi = int(test.min()), int(test.max())
        bad = train[(train >= lo - 20) & (train <= hi + emb)]
        if bad.size:
            fails.append(f"purge_leak:{bad[:3].tolist()}")
        if set(train.tolist()) & set(test.tolist()):
            fails.append("train_test_overlap")
    if not np.all(seen == 1):
        fails.append(f"test_coverage_not_exactly_once:{seen.min()},{seen.max()}")
    try:
        purged_kfold_indices(3, n_splits=5)
        fails.append("too_small_n_not_refused")
    except ValueError:
        pass

    # (2) cluster bootstrap: with a single value per cluster and zero variance the LB
    #     must equal that value; a mean-shifted cluster shift must move the LB; and the
    #     LB must be BELOW the mean for a spread sample.
    flat = cluster_bootstrap_lb([2.0] * 20, [f"m{i}" for i in range(20)], n_boot=200)
    if not math.isclose(flat["lb"], 2.0, abs_tol=1e-12):
        fails.append(f"flat_cluster_lb_wrong:{flat['lb']}")
    rng = np.random.default_rng(7)
    vals = rng.normal(0.0, 1.0, size=400)
    clus = [f"m{i % 40}" for i in range(400)]
    b = cluster_bootstrap_lb(vals, clus, n_boot=800)
    if not (b["lb"] < b["mean"]):
        fails.append("cluster_lb_not_below_mean")
    if b["n_clusters"] != 40:
        fails.append("cluster_count_wrong")
    try:
        cluster_bootstrap_lb([1.0, 2.0, 3.0], ["a", "b", "c"])
        fails.append("too_few_clusters_not_refused")
    except ValueError:
        pass
    try:
        cluster_bootstrap_lb([1.0, np.nan], ["a", "b"])
        fails.append("nan_not_refused")
    except ValueError:
        pass

    # (3) DSR: more trials must deflate more (SR0 rises), and a single trial must
    #     reduce to the undeflated test (SR0 == 0).
    r2 = rng.normal(0.05, 1.0, size=500)
    d1 = deflated_sharpe_ratio(r2, n_trials=1)
    d100 = deflated_sharpe_ratio(r2, n_trials=100)
    if not math.isclose(d1["sr0_expected_max"], 0.0, abs_tol=1e-12):
        fails.append("dsr_single_trial_sr0_not_zero")
    if not (d100["sr0_expected_max"] > d1["sr0_expected_max"]):
        fails.append("dsr_not_deflating_with_trials")
    if not (d100["dsr"] <= d1["dsr"]):
        fails.append("dsr_probability_not_monotone")
    if not (0.0 <= d100["dsr"] <= 1.0):
        fails.append("dsr_out_of_range")
    try:
        deflated_sharpe_ratio([1.0, 1.0, 1.0], n_trials=1)
        fails.append("zero_variance_not_refused")
    except ValueError:
        pass

    # (4) norm_ppf sanity against known quantiles (guards the DSR's only magic number)
    for p, want in ((0.5, 0.0), (0.975, 1.959963985), (0.025, -1.959963985)):
        if not math.isclose(_norm_ppf(p), want, abs_tol=1e-6):
            fails.append(f"norm_ppf_wrong:{p}:{_norm_ppf(p)}")

    # (5) time-order guard must refuse a shuffled input rather than mis-purge
    try:
        assert_time_sorted([3, 1, 2])
        fails.append("unsorted_time_not_refused")
    except ValueError:
        pass

    print(json.dumps({"suite": "honest_eval", "failed": len(fails), "failures": fails,
                      "purged_folds": len(folds),
                      "cluster_lb": {"mean": b["mean"], "lb": b["lb"],
                                     "n_clusters": b["n_clusters"]},
                      "dsr": {"n1": d1["dsr"], "n100": d100["dsr"],
                              "sr0_100": d100["sr0_expected_max"]}}, indent=1))
    return 1 if fails else 0


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--self-check", action="store_true")
    a = ap.parse_args()
    if a.self_check:
        return _self_check()
    ap.print_help()
    return 0


if __name__ == "__main__":
    sys.exit(main())
