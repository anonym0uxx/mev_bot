#!/usr/bin/env python
"""Probe why find_divergent_pairs yields 0 on real train panels."""
import sys, json, itertools
import numpy as np
import build_qwen_curriculum_v1 as B

B.build_slinky_lightweight_index()
B.preload_slinky_outcomes()
B.preload_slinky_counterfactuals()
train_lw = B.build_train_filtered_lightweight_index(set())  # exclusion irrelevant for a distribution probe
wins = B.find_dense_time_windows(train_lw, min_candidates=10, window_ms=60000, max_panels=30)
print(f"windows: {len(wins)}")

sims, divs, n_pairs = [], [], 0
for i, (anchor_ms, lo, hi) in enumerate(wins[:12]):
    panel = B.assemble_slinky_panel(anchor_ms, i, lw_idx=train_lw)
    if not panel:
        print(f"panel {i}: None"); continue
    cands = panel.get('candidates', [])
    print(f"panel {i}: {len(cands)} candidates")
    if len(cands) < 5:
        continue
    for c1, c2 in itertools.combinations(cands, 2):
        sim = B.compute_causal_similarity(c1, c2)
        sims.append(sim)
        if sim >= 0.6:
            l1, l2 = c1.get('l2_evidence', {}), c2.get('l2_evidence', {})
            for meas in ('ret_300s_bp', 'mfe_bp'):
                a, b = l1.get(meas), l2.get(meas)
                ok = lambda v: v is not None and isinstance(v, (int, float)) and v == v
                if ok(a) and ok(b):
                    divs.append((meas, abs(a - b)))
                    break
    pairs = B.find_divergent_pairs(panel)
    n_pairs += len(pairs)

sims = np.array(sims)
if len(sims):
    print(f"sim: n={len(sims)} p50={np.percentile(sims,50):.3f} p90={np.percentile(sims,90):.3f} max={sims.max():.3f} frac>=0.6={(sims>=0.6).mean():.3f}")
if divs:
    arr = np.array([d[1] for d in divs])
    from collections import Counter
    print(f"divergence among sim>=0.6: n={len(arr)} measures={Counter(d[0] for d in divs)} p50={np.percentile(arr,50):.0f} p90={np.percentile(arr,90):.0f} max={arr.max():.0f} frac>3000bp={(arr>3000).mean():.3f}")
else:
    print("NO sim>=0.6 pairs had valid outcome measures at all")
print(f"find_divergent_pairs total: {n_pairs}")
