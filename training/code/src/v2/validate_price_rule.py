import json, collections
import numpy as np

TR = "/training/v2/canonical/renorm_pooltest/trades.jsonl"
rows = [json.loads(l) for l in open(TR)]
FLOOR = 10_000  # 1e-5 SOL: below this no real swap value leg exists

def pick(d):
    ps, pt = d.get("pool_sol_lamports"), d.get("pool_tokens_raw")
    s, t = d.get("sol_lamports"), d.get("tokens_raw")
    if ps is not None and pt not in (None, 0) and abs(ps) >= FLOOR:
        return abs(ps) / abs(pt), "pool"
    if s is not None and t not in (None, 0) and abs(s) >= FLOOR:
        return abs(s) / abs(t), "trader"
    return None, "drop"

kept = collections.defaultdict(list)
n_drop = 0
for d in rows:
    p, base = pick(d)
    if p is None:
        n_drop += 1
        continue
    kept[d["mint"]].append((d["recv_unix_ms"], p))

j = []
for m, v in kept.items():
    v.sort()
    p = np.array([x[1] for x in v], float)
    p = p[np.isfinite(p) & (p > 0)]
    if p.size < 10:
        continue
    j.append(np.abs(np.log(p[1:] / p[:-1])))
j = np.concatenate(j) if j else np.array([])
print(f"trades={len(rows):,} dropped_dust={n_drop:,} ({n_drop/len(rows)*100:.2f}%)")
print(f"FLOOR+pool-pref  pairs={j.size:,} median={np.median(j):.4f} p99={np.percentile(j,99):.3f} "
      f"max={j.max():.2f} frac>0.69={float((j>0.69).mean()):.4f} frac>2.3={float((j>2.3).mean()):.4f}")
