#!/usr/bin/env python
"""Family C — FROZEN ECONOMIC EXAM (must exist before training).

Scores policies through the same accounting engine on the realized test-split
paths, net of execution costs, in SOL. Any trained policy must beat `rule_base`
here or it has discovered no edge. This is the anti-imitation test: loss is not
evidence, net SOL is.

Policies: always_hold, always_exit, random, rule_base (deterministic λ-policy),
ridge_ev (learned on train only), oracle_tick (per-tick upper bound).
"""
import argparse, json, os, sys, collections
import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from build_replay_v2 import (load_mint_series, load_seeds, score_actions, HALF_COST_BP,
                             ADD_FRACTION, CAPITAL, ENTRY_TRANCHE_SOL)

# SUPERSEDED by rl/economic_exam_c3.py, whose bar is on the C3 examination split.
# Kept only so the old 0.4725 rule_base number stays reproducible. CAPITAL (the
# episode notional) and ENTRY_TRANCHE_SOL come from the replay engine so there is
# exactly one place that states them; do not re-declare either here.


def schedule(tt, pv, t_dec):
    j0 = int(np.searchsorted(tt, t_dec, side="right"))
    if j0 >= tt.size:
        return None
    entry_px = pv[j0]
    if not np.isfinite(entry_px) or entry_px <= 0:
        return None
    t_entry = int(tt[j0])
    end = min(t_entry + 1800_000, int(tt[-1]))
    if end <= t_entry:
        return None
    ticks = np.arange(t_entry + 30_000, end + 1, 30_000, dtype=np.int64)
    if ticks.size == 0:
        return None
    npri = np.searchsorted(tt, ticks, side="left")
    act = (ticks - tt[npri - 1]) <= 60_000
    ticks, npri = ticks[act], npri[act]
    if ticks.size == 0:
        return None
    sel = (np.arange(min(16, ticks.size)) * (ticks.size / min(16, ticks.size))).astype(int)
    return entry_px, t_entry, ticks[sel], npri[sel], j0


def fwd(tt, pv, tM, i, j0):
    s = []
    for hs in (60, 150, 300):
        jh = int(np.searchsorted(tt, tM + hs * 1000, side="right"))
        seg = pv[max(i, j0):jh]; seg = seg[np.isfinite(seg)]
        if seg.size:
            s.append(float(seg[-1]))
    if not s:
        return None
    return float(np.mean(s)), float(np.std(s))


def run_policy(pol, tt, pv, j0, entry_px, t_entry, ticks, npri, feats=None, model=None):
    ow = HALF_COST_BP / 1e4
    qty = ENTRY_TRANCHE_SOL / entry_px
    cash = CAPITAL - ENTRY_TRANCHE_SOL
    for k in range(ticks.size):
        tM, i = int(ticks[k]), int(npri[k])
        if i <= j0 or (tM - t_entry) < 60_000:
            continue
        cur = pv[i - 1]
        if not np.isfinite(cur) or cur <= 0:
            continue
        f = fwd(tt, pv, tM, i, j0)
        if f is None:
            continue
        fm, fs = f
        if pol == "rule_base":
            act = score_actions(qty, float(cur), fm, fs, cash)[0]
        elif pol == "always_hold":
            act = "HOLD"
        elif pol == "always_exit":
            act = "EXIT"
        elif pol == "random":
            act = ["HOLD", "ADD", "REDUCE", "EXIT"][int(np.random.randint(0, 4))]
        elif pol == "ridge_ev":
            x = np.array([1.0, cur / entry_px - 1.0, np.log1p(tM - t_entry), fm / cur - 1.0, fs / cur])
            sc = float(x @ model)
            act = "EXIT" if sc < 0 else "HOLD"
        elif pol == "oracle_tick":
            u = {"HOLD": qty * fm * (1 - ow), "EXIT": qty * cur * (1 - ow),
                 "REDUCE": 0.5 * qty * cur * (1 - ow) + 0.5 * qty * fm * (1 - ow)}
            spend = qty * ADD_FRACTION * cur * (1 + ow)
            u["ADD"] = (qty * (1 + ADD_FRACTION) * fm * (1 - ow) - spend) if spend <= cash + 1e-9 else -np.inf
            act = max(u, key=lambda kk: u[kk])
        else:
            act = "HOLD"
        if act == "ADD":
            aq = qty * ADD_FRACTION
            sp = aq * cur * (1 + ow)
            if sp <= cash + 1e-12:
                cash -= sp; qty += aq
            else:
                act = "HOLD"
        elif act == "REDUCE":
            q = 0.5 * qty
            cash += q * cur * (1 - ow); qty -= q
        elif act == "EXIT":
            cash += qty * cur * (1 - ow); qty = 0.0
        if qty <= 1e-12:
            break
    mark = pv[npri[-1] - 1] if npri[-1] > 0 else entry_px
    if not np.isfinite(mark):
        mark = entry_px
    return cash + qty * float(mark)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--split", default="test")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--max-move", type=float, default=100.0,
                    help="exclude episodes whose realized 30m move exceeds this (implausible)")
    a = ap.parse_args()
    np.random.seed(7)
    n_excl = 0
    n_used = 0
    names, bounds, allt, allpx = load_mint_series()
    idx = {n: i for i, n in enumerate(names)}
    seeds = load_seeds({"test" if a.split == "test" else "val"})
    if a.limit:
        seeds = seeds[:a.limit]
    print(f"[EXAM] split={a.split} seeds={len(seeds):,} used={n_used:,} excluded_implausible={n_excl:,} "
          f"(max_move={a.max_move:g}x)")

    pols = ["always_hold", "always_exit", "random", "rule_base", "oracle_tick"]
    wealth = {p: [] for p in pols}
    for mint, sp, t_dec, eid in seeds:
        gi = idx.get(mint)
        if gi is None:
            continue
        lo, hi = bounds[gi], bounds[gi + 1]
        tt, pv = allt[lo:hi], allpx[lo:hi]
        s = schedule(tt, pv, t_dec)
        if s is None:
            continue
        entry_px, t_entry, ticks, npri, j0 = s
        jh = int(np.searchsorted(tt, min(t_entry + 1800_000, int(tt[-1])), side="right"))
        seg = pv[j0:jh]; seg = seg[np.isfinite(seg) & (seg > 0)]
        if seg.size == 0:
            continue
        if float(seg.max() / entry_px) > a.max_move:
            n_excl += 1
            continue
        n_used += 1
        for p in pols:
            w = run_policy(p, tt, pv, j0, entry_px, t_entry, ticks, npri)
            if np.isfinite(w):
                wealth[p].append(w - CAPITAL)
    out = {}
    for p, w in wealth.items():
        w = np.array(w)
        if w.size == 0:
            continue
        out[p] = {"episodes": int(w.size),
                  "mean_net_sol": round(float(w.mean()), 5),
                  "median_net_sol": round(float(np.median(w)), 5),
                  "total_net_sol": round(float(w.sum()), 3),
                  "hit_rate": round(float((w > 0).mean()), 4),
                  "p90_net_sol": round(float(np.percentile(w, 90)), 5),
                  "p10_net_sol": round(float(np.percentile(w, 10)), 5)}
    json.dump(out, open(f"/training/v2/reports/ECONOMIC_EXAM_{a.split}.json", "w"), indent=1)
    print(json.dumps(out, indent=1))
    if "rule_base" in out:
        print(f"\n[EXAM] used={n_used:,} excluded_implausible={n_excl:,} (max_move={a.max_move:g}x)")
        print(f"[EXAM] BAR TO BEAT: rule_base mean_net_sol={out['rule_base']['mean_net_sol']:.6f} "
              f"median={out['rule_base']['median_net_sol']:.6f} total={out['rule_base']['total_net_sol']:.3f}")


if __name__ == "__main__":
    main()
