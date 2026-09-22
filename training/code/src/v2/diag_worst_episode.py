import sys, numpy as np
sys.path.insert(0, "/training/v2/code/src/v2")
from build_replay_v2 import load_mint_series, load_seeds

names, bounds, allt, allpx = load_mint_series()
idx = {n: i for i, n in enumerate(names)}
seeds = load_seeds({"test"})
best = (0, None)
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
        continue
    jh = int(np.searchsorted(tt, min(int(tt[j0]) + 1800_000, int(tt[-1])), side="right"))
    seg = pv[j0:jh]; seg = seg[np.isfinite(seg) & (seg > 0)]
    if seg.size == 0:
        continue
    r = float(seg.max() / e)
    if r > best[0]:
        best = (r, (mint, tt[j0], e, seg))

r, (mint, t0, e, seg) = best
print(f"worst episode ratio={r:.3e} mint={mint}")
print(f"entry_px={e:.6e}  n_forward_trades={seg.size}")
print("forward price series (first 30):")
print(np.array2string(seg[:30], precision=6, max_line_width=120))
print("max px:", seg.max(), " at index", int(seg.argmax()))
print("last px:", seg[-1])
