import sys, numpy as np
sys.path.insert(0, "/training/v2/code/src/v2")
from build_replay_v2 import load_mint_series
names, bounds, allt, allpx = load_mint_series()
jumps = []
sample = []
for gi in range(0, min(4000, len(names))):
    lo, hi = bounds[gi], bounds[gi + 1]
    if hi - lo < 10:
        continue
    p = allpx[lo:hi]
    p = p[np.isfinite(p) & (p > 0)]
    if p.size < 10:
        continue
    r = np.abs(np.log(p[1:] / p[:-1]))
    jumps.append(r)
    if len(sample) < 5:
        sample.append(np.round(p[:12], 8))
j = np.concatenate(jumps)
print(f"consecutive-trade price jumps: n={j.size:,}")
print("|log ratio| quantiles:", np.round(np.percentile(j, [50, 75, 90, 99, 100]), 3))
print("frac |log|>0.69 (>2x):", round(float((j > 0.69).mean()), 4))
print("frac |log|>2.3 (>10x):", round(float((j > 2.3).mean()), 4))
print("\nsample price series (first 12 trades of 5 mints):")
for s in sample:
    print(" ", s)
