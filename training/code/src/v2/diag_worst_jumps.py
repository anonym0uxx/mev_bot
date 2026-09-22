import json, collections
import numpy as np

TR = "/training/v2/canonical/renorm_pooltest/trades.jsonl"
by = collections.defaultdict(list)
for line in open(TR):
    d = json.loads(line)
    by[d["mint"]].append(d)

worst = []
for m, v in by.items():
    v.sort(key=lambda d: d["recv_unix_ms"])
    prev = None
    for d in v:
        s, t = d.get("sol_lamports"), d.get("tokens_raw")
        if not s or not t:
            continue
        p = abs(s) / abs(t)
        if prev is not None:
            r = abs(np.log(p / prev[0])) if p > 0 and prev[0] > 0 else 0
            if r > 6:
                worst.append((r, m, prev[1], d))
        prev = (p, d)
worst.sort(key=lambda x: -x[0])
print(f"consecutive pairs with |log|>6 (>{np.exp(6):.0f}x): {len(worst)}")
for r, m, a, b in worst[:6]:
    print(f"\nratio={np.exp(r):.2e} mint={m[:12]}..")
    for tag, d in (("A", a), ("B", b)):
        s, t = d.get("sol_lamports"), d.get("tokens_raw")
        ps, pt = d.get("pool_sol_lamports"), d.get("pool_tokens_raw")
        print(f"  {tag} {d['side']:4s} {d['venue']:8s} res={d['resolution']:18s} "
              f"sol={s} tok={t} px={abs(s)/abs(t) if t else 0:.3e} "
              f"pool_sol={ps} pool_tok={pt} basis={d.get('price_basis')}")
