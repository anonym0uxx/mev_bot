import json, collections
import numpy as np

TR = "/training/v2/canonical/renormalized_v7/trades.jsonl"
WORST = "98sMhvDwXj1RQi5c5Mndm3vPe9cBqPrbLaufMXFNMh5g"
MIN_SOL, MIN_TOK = 100_000, 1_000_000

by = collections.defaultdict(list)
for line in open(TR):
    d = json.loads(line)
    by[d["mint"]].append(d)

def px(d):
    s, t = d.get("sol_lamports"), d.get("tokens_raw")
    if not s or not t or abs(s) < MIN_SOL or abs(t) < MIN_TOK:
        return None
    return abs(s) / abs(t)

w = by.get(WORST, [])
w.sort(key=lambda d: d["recv_unix_ms"])
print(f"=== worst mint: {len(w)} trades ===")
prices = []
for d in w[:18]:
    p = px(d)
    prices.append(p)
    print(f"  {d['side']:4s} {d['venue']:8s} {d['resolution']:18s} sol={d['sol_lamports']:>15,} "
          f"tok={d['tokens_raw']:>15,} px={p if p is None else f'{p:.4e}'} trader={d['trader'][:8]}")
good = [p for p in prices if p]
if good:
    print(f"  median={np.median(good):.4e}  ratio max/min={max(good)/min(good):.3e}")

# global: how much does a per-mint robust band remove?
tot = kept = 0
for m, rows in by.items():
    ps = np.array([p for p in (px(d) for d in rows) if p], dtype=float)
    if ps.size < 5:
        tot += len(rows); kept += int(ps.size); continue
    med = float(np.median(ps))
    lo, hi = med / 10.0, med * 10.0
    tot += len(rows)
    kept += int(np.sum((ps >= lo) & (ps <= hi)))
print(f"\nglobal: trades_with_valid_legs={tot:,} within_10x_of_mint_median={kept:,} "
      f"({kept/max(tot,1)*100:.2f}%) -> band removes {100-kept/max(tot,1)*100:.2f}%")
