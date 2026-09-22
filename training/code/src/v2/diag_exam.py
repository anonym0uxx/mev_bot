import sys, os, numpy as np
sys.path.insert(0, "/training/v2/code/src/v2")
from build_replay_v2 import load_mint_series, load_seeds

names, bounds, allt, allpx = load_mint_series()
idx = {n: i for i, n in enumerate(names)}
seeds = load_seeds({"test"})
ratios = []
bad = 0
for mint, sp, t_dec, eid in seeds:
    gi = idx.get(mint)
    if gi is None:
        continue
    lo, hi = bounds[gi], bounds[gi + 1]
    tt, pv = allt[lo:hi], allpx[lo:hi]
    j0 = int(np.searchsorted(tt, t_dec, side="right"))
    if j0 >= tt.size:
        continue
    e = pv[j0]
    if not np.isfinite(e) or e <= 0:
        bad += 1
        continue
    end = min(int(tt[j0]) + 1800_000, int(tt[-1]))
    jh = int(np.searchsorted(tt, end, side="right"))
    seg = pv[j0:jh]; seg = seg[np.isfinite(seg)]
    if seg.size == 0:
        continue
    r = seg.max() / e
    ratios.append(r)
    if r > 100:
        bad += 1
r = np.array(ratios)
print(f"episodes={r.size}  max_price_ratio>100x: {bad}")
print("price ratio quantiles:", np.round(np.percentile(r, [50, 90, 99, 99.9, 100]), 2))
print("frac >10x:", float((r > 10).mean()), " frac >1000x:", float((r > 1000).mean()))
