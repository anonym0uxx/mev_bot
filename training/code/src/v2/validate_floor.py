import json, collections
import numpy as np

TR = "/training/v2/canonical/renorm_pooltest/trades.jsonl"
rows = [json.loads(l) for l in open(TR)]

def series(floor, use_pool_if_available=False):
    kept = collections.defaultdict(list); n_drop = 0
    for d in rows:
        ps, pt = d.get("pool_sol_lamports"), d.get("pool_tokens_raw")
        s, t = d.get("sol_lamports"), d.get("tokens_raw")
        p = None
        if use_pool_if_available and ps is not None and pt not in (None, 0) and abs(ps) >= floor:
            p = abs(ps) / abs(pt)
        elif s is not None and t not in (None, 0) and abs(s) >= floor:
            p = abs(s) / abs(t)
        if p is None:
            n_drop += 1; continue
        kept[d["mint"]].append((d["recv_unix_ms"], p))
    j = []
    for m, v in kept.items():
        v.sort()
        a = np.array([x[1] for x in v], float)
        a = a[np.isfinite(a) & (a > 0)]
        if a.size < 10:
            continue
        j.append(np.abs(np.log(a[1:] / a[:-1])))
    j = np.concatenate(j) if j else np.array([])
    return j, n_drop

for floor in (0, 10_000, 100_000):
    j, nd = series(floor, use_pool_if_available=False)
    print(f"trader floor={floor:>7,} drop={nd:6,} pairs={j.size:8,} median={np.median(j):.4f} "
          f"p99={np.percentile(j,99):.3f} max={j.max():7.2f} "
          f"frac>0.69={float((j>0.69).mean()):.4f} frac>2.3={float((j>2.3).mean()):.4f}")
