#!/usr/bin/env python
"""Which exit family is right - trailing stops or TP ladders?

The exit derivation reduces everything to the sign of dG/dln x, where
G(x) = E[V_exit / x_t | x_t, alive]: if the expected exit value RISES with the
current price level, price moves are regime-revealing (momentum continues), the
optimal stopping set is a down-crossing one, and a TRAILING STOP dominates;
if it falls, the move is a fade and fixed TP LADDERS return.

We cannot observe G directly, so this measures a defensible PROXY from our own
recorded tape (NOT a theoretical object): at each decision time t, with entry at
the mint's first fill,

    M_t   = px_t / px_entry                    (current multiple)
    R     = M_max(t, +horizon) / M_t           (remaining favourable excursion)
    T     = M_end(t, +horizon) / M_t           (remaining terminal move)

dG/dln x > 0  <=>  E[log R | log M_t] is INCREASING in log M_t.

Also estimates the Pareto tail index alpha of M_max (Hill), because alpha sets how
much expectancy a capped ladder writes away.

Run: /home/alon/qwen27b-venv/bin/python estimate_exit_family.py
"""
import json
import os
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from reward_engine import load_canonical_tapes  # noqa: E402

P = "/training/v2/canonical/renorm_pooltest/trades.jsonl"
HORIZON_MS = 1_800_000
STEP_MS = 30_000
MAX_DECISIONS_PER_MINT = 40


def hill_alpha(x, frac=0.15):
    """Hill estimator of the tail index on the top `frac` of positive x."""
    x = np.sort(np.asarray([v for v in x if v > 0], dtype=float))
    if x.size < 50:
        return None, 0
    k = max(5, int(x.size * frac))
    top = x[-k:]
    xk = top[0]
    if xk <= 0:
        return None, k
    alpha = 1.0 / np.mean(np.log(top / xk))
    return float(alpha), int(k)


def spearman(a, b):
    ra = np.argsort(np.argsort(a))
    rb = np.argsort(np.argsort(b))
    ra = ra - ra.mean(); rb = rb - rb.mean()
    den = np.sqrt((ra ** 2).sum() * (rb ** 2).sum())
    return float((ra * rb).sum() / den) if den > 0 else 0.0


def main():
    tapes = load_canonical_tapes(P)
    rows = []
    for t in tapes.values():
        if t.tt.size < 20:
            continue
        px = np.asarray(t.px, dtype=float)
        tt = np.asarray(t.tt, dtype=np.int64)
        ok = np.isfinite(px) & (px > 0)
        if ok.sum() < 20:
            continue
        j0 = int(np.argmax(ok))
        px0 = px[j0]
        t0 = int(tt[j0])
        end = int(tt[-1])
        decisions = 0
        # Walk FILL indices, not a time grid: most mints have short tapes, and a
        # time grid yields ~1.9 decisions/mint which starves the estimator.
        # DROP degenerate states: a decision taken after price collapsed to near
        # zero has a meaningless log-multiple (any bounce explodes in log space)
        # and previously dominated the whole regression.
        M_MIN, M_MAX, FWD_CAP = 0.25, 20.0, 50.0
        for i in range(j0 + 1, tt.size, 3):
            if decisions >= MAX_DECISIONS_PER_MINT:
                break
            if not ok[i] or (tt[i] - t0) < STEP_MS:
                continue
            m_t = px[i] / px0
            if not np.isfinite(m_t) or m_t < M_MIN or m_t > M_MAX:
                continue
            jf = int(np.searchsorted(tt, min(int(tt[i]) + HORIZON_MS, end),
                                     side="right")) - 1
            if jf <= i:
                continue
            fut = px[i:jf + 1][ok[i:jf + 1]]
            if fut.size < 2:
                continue
            fwd = float(fut.max() / px[i])
            if not np.isfinite(fwd) or fwd > FWD_CAP:
                continue
            rows.append((float(np.log(m_t)),
                         float(np.log(fwd)),
                         float(np.log(fut[-1] / px[i])),
                         float(fut.max() / px0)))
            decisions += 1

    if len(rows) < 200:
        print(json.dumps({"verdict": "INSUFFICIENT_DATA", "n": len(rows)}, indent=1))
        return 1

    a = np.asarray(rows, dtype=float)
    lx, lr, lt, mmax = a[:, 0], a[:, 1], a[:, 2], a[:, 3]

    # binned conditional expectation (MEDIAN - the mean-of-logs is outlier-driven)
    bins = np.quantile(lx, [0, .2, .4, .6, .8, 1.0])
    bins[-1] += 1e-9
    cond = []
    for lo, hi in zip(bins[:-1], bins[1:]):
        m = (lx >= lo) & (lx < hi)
        if m.sum() >= 20:
            cond.append({"logM_lo": round(float(lo), 4), "n": int(m.sum()),
                         "median_logR": round(float(np.median(lr[m])), 4),
                         "median_logT": round(float(np.median(lt[m])), 4)})
    # Theil-Sen slope: median of pairwise slopes, robust to the fat tail
    slope = None
    if lx.size >= 50:
        idx = np.random.RandomState(0).choice(lx.size, size=min(4000, lx.size),
                                              replace=False)
        xs, ys = lx[idx], lr[idx]
        sl = []
        for k in range(0, xs.size - 1, max(1, xs.size // 200)):
            for j in range(k + 1, xs.size, max(1, xs.size // 200)):
                dx = xs[j] - xs[k]
                if dx != 0:
                    sl.append((ys[j] - ys[k]) / dx)
        slope = float(np.median(sl)) if sl else None
    rho = spearman(lx, lr) if lx.size >= 20 else 0.0
    # Hill alpha stability across tail fractions; report the spread honestly
    alphas = {}
    for frac in (0.05, 0.10, 0.15, 0.20):
        a_, k_ = hill_alpha(mmax, frac)
        if a_ is not None:
            alphas[f"{frac:.2f}"] = round(a_, 4)
    alpha = None
    alpha_stable = False
    if len(alphas) >= 3:
        vals = list(alphas.values())
        alpha = float(np.median(vals))
        alpha_stable = (max(vals) - min(vals)) <= 0.35 * max(1e-9, alpha)

    mom = (slope is not None and slope > 0 and rho > 0)
    out = {"n_decisions": int(a.shape[0]), "n_mints": len(tapes),
           "decisions_per_mint": round(float(a.shape[0]) / max(1, len(tapes)), 2),
           "conditional": cond,
           "slope_d_logR_per_logM_theilsen": slope,
           "spearman_logM_logR": rho,
           "pareto_alpha_hill_by_frac": alphas,
           "pareto_alpha_median": alpha,
           "alpha_stable_across_k": alpha_stable,
           "frac_mmax_above_2x": float((mmax > 2).mean()),
           "frac_mmax_above_5x": float((mmax > 5).mean()),
           "median_mmax": float(np.median(mmax)),
           "verdict": ("MOMENTUM / regime-revealing (dG/dlnx > 0): trailing stop is the "
                       "correct family; a capped ladder writes away the tail"
                       if mom else
                       "FADE / mean-reverting (dG/dlnx <= 0): fixed TP ladders return"),
           "estimator_caveats": [
               "proxy for dG/dlnx, not the object itself",
               f"thin density: {round(float(a.shape[0]) / max(1, len(tapes)), 2)} decisions/mint",
               "tail index unstable across k" if not alpha_stable else "tail index stable",
           ],
           }
    print(json.dumps(out, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())