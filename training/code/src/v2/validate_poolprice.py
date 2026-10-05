import json, collections
import numpy as np

TR = "/training/v2/canonical/renorm_pooltest/trades.jsonl"
rows = []
for line in open(TR):
    d = json.loads(line)
    rows.append((d["mint"], d["recv_unix_ms"], d.get("sol_lamports"), d.get("tokens_raw"),
                 d.get("pool_sol_lamports"), d.get("pool_tokens_raw"), d.get("price_basis")))
print(f"trades={len(rows):,}  price_basis pool={sum(1 for r in rows if r[6]=='pool'):,}")

def series(rows, sol_i, tok_i):
    by = collections.defaultdict(list)
    for r in rows:
        s, t = r[sol_i], r[tok_i]
        if s is None or t is None or t == 0:
            continue
        by[r[0]].append((r[1], abs(s) / abs(t)))
    return by

def jumps(by):
    out = []
    for m, v in by.items():
        v.sort()
        p = np.array([x[1] for x in v], dtype=float)
        p = p[np.isfinite(p) & (p > 0)]
        if p.size < 10:
            continue
        out.append(np.abs(np.log(p[1:] / p[:-1])))
    return np.concatenate(out) if out else np.array([])

for label, si, ti in (("trader", 2, 3), ("pool", 4, 5)):
    j = jumps(series(rows, si, ti))
    if j.size == 0:
        print(f"{label}: no data"); continue
    print(f"{label:7s} n={j.size:9,} median={np.median(j):.4f} "
          f"p99={np.percentile(j,99):.3f} max={j.max():.2f} "
          f"frac>0.69={float((j>0.69).mean()):.4f} frac>2.3={float((j>2.3).mean()):.4f}")
