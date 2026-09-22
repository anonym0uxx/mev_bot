#!/usr/bin/env python
"""ENTRY-SIDE base rate, decomposed: is "no entry edge" real, or is it the penalty?

entry_edge.py scores entries with RewardConfig(lambda_drawdown=1, lambda_downside=1,
lambda_time=0.05) - risk weights calibrated for the EXIT problem. A fresh ENTRY has
maximum time-in-market and maximum drawdown exposure by construction, so the penalty
terms can dominate the PnL term and manufacture a negative verdict.

This script reports, per baseline exit policy:
  * pure   : return_on_capital (net_sol / implied capital) - no penalty
  * penal  : reward_scalar() with the default config (cross-check vs entry_edge.py)
  * capture: captured multiple / forward max multiple over the episode horizon
             (forward max uses tape rows with t >= t_dec only - LABEL-side, never feature-side)
  * mint-level bootstrap CI on the mean pure return (resample MINTS, not rows)
Every number is reported with n and n_mints.
"""
from __future__ import annotations

import json
import os
import re
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from reward_engine import load_canonical_tapes  # noqa: E402
from reward_terms import RewardConfig, reward_scalar  # noqa: E402
from exit_policy import TEMPLATES, BASELINE_POLICIES, simulate_policy  # noqa: E402

TAPES = "/training/v2/canonical/renorm_corpus_mints/trades.jsonl"
CORPUS = "/training/v2/candidate_sft_c3/train.jsonl"
MAX_RECORDS = 40000
HORIZON_MS = 1_800_000
PURE = RewardConfig(lambda_drawdown=0.0, lambda_downside=0.0, lambda_time=0.0)
EP_RE = re.compile(r"v2:([A-Za-z0-9]+):(\d+)$")


def fwd_max(tape, t_dec_ms, px0, horizon_ms=HORIZON_MS):
    """Max price multiple over [t_dec, t_dec+horizon] using tape rows only.

    Uses tape.px (the simulator's own unit-consistent price series) - NOT
    sol/tok, which is lamports-per-raw-token and off by 1e9.
    """
    tt = tape.tt
    px = tape.px
    i = int(np.searchsorted(tt, t_dec_ms, side="left"))
    j = int(np.searchsorted(tt, t_dec_ms + horizon_ms, side="right"))
    if j <= i or px0 <= 0:
        return None
    seg = px[i:j]
    seg = seg[np.isfinite(seg)]
    if seg.size == 0:
        return None
    return float(seg.max() / px0)


def dec_px(tape, t_dec_ms):
    """Price at the decision instant (for the H3 late-entry test)."""
    i = int(np.searchsorted(tape.tt, t_dec_ms, side="right")) - 1
    if i < 0 or i >= tape.px.size:
        return None
    v = float(tape.px[i])
    return v if np.isfinite(v) and v > 0 else None


def peak_dwell(tape, t_dec_ms, horizon_ms=HORIZON_MS, tau=0.25):
    """H2 test: seconds spent within (1-tau) of the forward peak (label-side only)."""
    tt = tape.tt
    px = tape.px
    i = int(np.searchsorted(tt, t_dec_ms, side="left"))
    j = int(np.searchsorted(tt, t_dec_ms + horizon_ms, side="right"))
    if j - i < 2:
        return None
    seg = px[i:j]
    ok = np.isfinite(seg)
    if not ok.any():
        return None
    m = float(seg[ok].max())
    if m <= 0:
        return None
    tt_seg = tt[i:j][ok]
    seg = seg[ok]
    inzone = seg >= (1.0 - tau) * m
    if not inzone.any():
        return None
    return float((tt_seg[inzone].max() - tt_seg[inzone].min()) / 1000.0)


def boot_ci(per_mint_means, n_boot=2000, seed=7):
    a = np.asarray(per_mint_means, dtype=float)
    if a.size < 2:
        return (float("nan"), float("nan"))
    rng = np.random.default_rng(seed)
    idx = rng.integers(0, a.size, size=(n_boot, a.size))
    ms = a[idx].mean(axis=1)
    return float(np.percentile(ms, 2.5)), float(np.percentile(ms, 97.5))


def main():
    tapes = load_canonical_tapes(TAPES, min_notional_lamports=100_000, max_px_ratio=50)
    per_mint = {}
    scanned = 0
    for line in open(CORPUS, encoding="utf-8"):
        if scanned >= MAX_RECORDS:
            break
        try:
            r = json.loads(line)
        except Exception:
            continue
        if r.get("family") != "decision":
            continue
        scanned += 1
        parts = (r.get("episode_id") or "").split(":")
        if len(parts) < 3:
            continue
        try:
            per_mint.setdefault(parts[1], []).append(int(parts[2]))
        except ValueError:
            continue

    pols = [p for p in BASELINE_POLICIES if p != "EXIT_NOW"]
    rec = {p: {"pure": [], "penal": [], "cap": [], "mint": [], "h3": [], "fm": [], "dwell": []}
           for p in pols}
    used = 0
    for mint, tdecs in per_mint.items():
        tape = tapes.get(mint)
        if tape is None:
            continue
        t0, tN = int(tape.tt[0]), int(tape.tt[-1])
        inwin = [t for t in tdecs if t0 <= t <= tN]
        if not inwin:
            continue
        step = max(1, len(inwin) // 8)
        for t_dec in inwin[::step][:8]:
            used += 1
            dp = dec_px(tape, t_dec)
            for p in pols:
                ep = simulate_policy(tape, t_dec, TEMPLATES[p], refuse_cross_graduation=False)
                if ep.get("status") != "ok":
                    continue
                cap = float(ep["final_equity_sol"]) - float(ep["net_sol_returned"])
                if cap <= 0:
                    continue
                pure = float(ep["net_sol_returned"]) / cap
                pen = reward_scalar(ep)["reward"]
                rec[p]["pure"].append(pure)
                rec[p]["penal"].append(pen)
                rec[p]["mint"].append(mint)
                px0 = ep.get("entry_px")
                fm = fwd_max(tape, t_dec, float(px0)) if px0 else None
                if fm is not None:
                    # H1: is the >2x forward-max prior present on the SCORED decisions?
                    rec[p]["fm"].append(float(fm))
                    if fm > 1.0:
                        rec[p]["cap"].append((1.0 + pure) / fm)
                if dp and px0:
                    # H3: entry fill price vs the price the model actually saw
                    rec[p]["h3"].append(float(px0) / dp)
                # H2: peak dwell - time inside (1-tau) of the forward peak
                d = peak_dwell(tape, t_dec)
                if d is not None:
                    rec[p]["dwell"].append(d)

    out = {"decisions_scored": used, "policy": {}, "notes": {
        "pure": "net_sol_returned / implied capital, NO risk penalty",
        "penal": "reward_scalar() default RewardConfig (drawdown+downside+time)",
        "capture_p95": "p95 of captured_gross_multiple / forward_max_multiple over 30min"}}
    for p in pols:
        d = rec[p]
        pure = np.asarray(d["pure"], float)
        pen = np.asarray(d["penal"], float)
        cap = np.asarray(d["cap"], float)
        fmv = np.asarray(d["fm"], float)
        h1 = (fmv > 2.0).astype(float)
        h5 = (fmv > 5.0).astype(float)
        dw = np.asarray(d["dwell"], float)
        h3 = np.asarray(d["h3"], float)
        # conditional capture inside the money buckets (value-weighted, not count-weighted)
        b2 = pure[fmv > 2.0] if fmv.size == pure.size else np.zeros(0)
        b5 = pure[fmv > 5.0] if fmv.size == pure.size else np.zeros(0)
        c2 = cap[fmv[fmv > 1.0] > 2.0] if cap.size else np.zeros(0)
        c5 = cap[fmv[fmv > 1.0] > 5.0] if cap.size else np.zeros(0)
        if pure.size == 0:
            continue
        mi = np.asarray(d["mint"])
        pm = {m: pure[mi == m].mean() for m in set(mi.tolist())}
        lo, hi = boot_ci(list(pm.values()))
        out["policy"][p] = {
            "n": int(pure.size), "n_mints": int(len(pm)),
            "pure_mean": round(float(pure.mean()), 5),
            "pure_median": round(float(np.median(pure)), 5),
            "pure_p05": round(float(np.percentile(pure, 5)), 5),
            "pure_p95": round(float(np.percentile(pure, 95)), 5),
            "pure_p_positive": round(float((pure > 0).mean()), 4),
            "pure_mean_mint_boot_ci95": [round(lo, 5), round(hi, 5)],
            "penal_mean": round(float(pen.mean()), 5),
            "penal_median": round(float(np.median(pen)), 5),
            "penalty_drag": round(float(pen.mean() - pure.mean()), 5),
            "capture_p50": round(float(np.median(cap)), 5) if cap.size else None,
            "capture_p95": round(float(np.percentile(cap, 95)), 5) if cap.size else None,
            "capture_frac_ge_0_5": round(float((cap >= 0.5).mean()), 4) if cap.size else None,
            # H1: does the >2x forward-max prior exist on the SCORED decisions?
            "H1_p_fwdmax_gt_2x": round(float(np.mean(h1)), 4) if h1.size else None,
            "H1_p_fwdmax_gt_5x": round(float(np.mean(h5)), 4) if h5.size else None,
            # H2: peak dwell inside (1-tau) of the forward peak, seconds
            "H2_dwell_p50_s": round(float(np.median(dw)), 2) if dw.size else None,
            "H2_dwell_p90_s": round(float(np.percentile(dw, 90)), 2) if dw.size else None,
            "H2_frac_dwell_lt_5s": round(float((dw < 5.0).mean()), 4) if dw.size else None,
            # H3: entry fill price / price at the decision instant (1.0 = no late entry)
            "H3_entry_vs_dec_px_p50": round(float(np.median(h3)), 5) if h3.size else None,
            "H3_entry_vs_dec_px_p95": round(float(np.percentile(h3, 95)), 5) if h3.size else None,
            "H3_frac_entry_gt_1_02": round(float((h3 > 1.02).mean()), 4) if h3.size else None,
            # MONEY BUCKETS: what happens on the decisions that matter
            "B_pure_mean_given_fm_gt_2x": round(float(b2.mean()), 5) if b2.size else None,
            "B_pure_mean_given_fm_gt_5x": round(float(b5.mean()), 5) if b5.size else None,
            "B_pure_p_positive_given_fm_gt_2x": round(float((b2 > 0).mean()), 4) if b2.size else None,
            "B_capture_p50_given_fm_gt_2x": round(float(np.median(c2)), 5) if c2.size else None,
            "B_capture_p50_given_fm_gt_5x": round(float(np.median(c5)), 5) if c5.size else None,
        }
    best = max(out["policy"], key=lambda k: out["policy"][k]["pure_mean"])
    out["best_pure_policy"] = best
    out["entry_edge_present_pure"] = bool(
        out["policy"][best]["pure_mean"] > 0 and
        out["policy"][best]["pure_mean_mint_boot_ci95"][0] > 0)
    print(json.dumps(out, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())