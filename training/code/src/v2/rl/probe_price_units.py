#!/usr/bin/env python
"""Probe: why did capture_ratio report 0.0 for every episode?

Capture = (1+pure_return)/forward_max. It cannot be exactly 0 for all 9,010 episodes
unless forward_max is astronomically large relative to entry_px. Check the price
units on the canonical tape directly: print raw sol/tokens at sampled rows vs the
entry_px the simulator quotes, for a few mints.
"""
import json, os, re, sys
import numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from reward_engine import load_canonical_tapes
from exit_policy import TEMPLATES, simulate_policy

TAPES = "/training/v2/canonical/renorm_corpus_mints/trades.jsonl"
CORPUS = "/training/v2/candidate_sft_c3/train.jsonl"
tapes = load_canonical_tapes(TAPES, min_notional_lamports=100_000, max_px_ratio=50)
print("tapes loaded:", len(tapes))

per_mint = {}
n = 0
for line in open(CORPUS, encoding="utf-8"):
    if n >= 20000:
        break
    try:
        r = json.loads(line)
    except Exception:
        continue
    if r.get("family") != "decision":
        continue
    n += 1
    parts = (r.get("episode_id") or "").split(":")
    if len(parts) >= 3:
        try:
            per_mint.setdefault(parts[1], []).append(int(parts[2]))
        except ValueError:
            pass

shown = 0
ratios = []
for mint, tdecs in per_mint.items():
    tape = tapes.get(mint)
    if tape is None:
        continue
    t0, tN = int(tape.tt[0]), int(tape.tt[-1])
    inwin = [t for t in tdecs if t0 <= t <= tN]
    if not inwin:
        continue
    t_dec = inwin[0]
    ep = simulate_policy(tape, t_dec, TEMPLATES["TRAIL_ONLY"], refuse_cross_graduation=False)
    if ep.get("status") != "ok":
        continue
    px0 = float(ep["entry_px"])
    i = int(np.searchsorted(tape.tt, t_dec, side="left"))
    j = int(np.searchsorted(tape.tt, t_dec + 1_800_000, side="right"))
    if j <= i:
        continue
    seg_raw = tape.sol[i:j] / np.maximum(tape.tok[i:j], 1)
    fm = float(seg_raw.max() / px0)
    ratios.append(fm)
    if shown < 6:
        print(json.dumps({
            "mint": mint[:12], "n_rows_tape": int(len(tape.tt)),
            "t_dec": t_dec,
            "entry_px_simulator": f"{px0:.6e}",
            "tape_px_at_t_dec": f"{float(tape.sol[i]/max(tape.tok[i],1)):.6e}",
            "tape_px_max_after": f"{float(seg_raw.max()):.6e}",
            "tape_px_min_after": f"{float(seg_raw.min()):.6e}",
            "fwd_max_multiple": f"{fm:.3e}",
            "sol_row_min_max": [float(tape.sol[i:j].min()), float(tape.sol[i:j].max())],
            "tok_row_min_max": [float(tape.tok[i:j].min()), float(tape.tok[i:j].max())],
        }, indent=1))
        shown += 1
    if len(ratios) >= 400:
        break
a = np.asarray(ratios)
print(json.dumps({"n_checked": int(a.size),
                  "fwd_max_median": float(np.median(a)),
                  "fwd_max_p95": float(np.percentile(a, 95)),
                  "fwd_max_max": float(a.max()),
                  "frac_fwd_max_gt_2": float((a > 2).mean()),
                  "frac_fwd_max_gt_1000": float((a > 1000).mean())}, indent=1))