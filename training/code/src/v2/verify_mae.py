import json, collections
import numpy as np

MIN_SOL, MIN_TOK = 100_000, 1_000_000

# episodes from the ledger (mint + decision time + stored mae)
eps = []
for line in open("/training/v2/canonical/ledger_v7/states.jsonl"):
    r = json.loads(line)
    oe = r["outcome_evidence"]
    eps.append((r["identity"]["mint"], int(r["decision_clock"]["t_dec_ms"]),
                oe.get("mae_bp"), oe.get("mfe_bp")))
print(f"episodes={len(eps):,}")

want = {e[0] for e in eps}
by = collections.defaultdict(list)
for line in open("/training/v2/canonical/renormalized_v7/trades.jsonl"):
    d = json.loads(line)
    if d["mint"] in want:
        by[d["mint"]].append(d)
print(f"mints with trades={len(by):,}")

# floors + per-mint robust band (same rule as the ledger build)
ser = {}
for m, rows in by.items():
    rows.sort(key=lambda d: d["recv_unix_ms"])
    t = np.array([d["recv_unix_ms"] for d in rows], dtype=np.int64)
    s = np.array([d.get("sol_lamports") or 0 for d in rows], dtype=np.float64)
    k = np.array([d.get("tokens_raw") or 0 for d in rows], dtype=np.float64)
    ok = (np.abs(s) >= MIN_SOL) & (np.abs(k) >= MIN_TOK)
    if ok.sum() < 2:
        continue
    t, s, k = t[ok], s[ok], k[ok]
    p = np.abs(s) / np.abs(k)
    fin = np.isfinite(p)
    if fin.sum() >= 5:
        med = float(np.median(p[fin]))
        b = fin & (p >= med / 10) & (p <= med * 10)
        t, p = t[b], p[b]
    if t.size >= 2:
        ser[m] = (t, p)

size1 = 0; n = 0
mae_incl, mae_excl, stored = [], [], []
for mint, t_dec, smae, smfe in eps:
    if mint not in ser:
        continue
    t, p = ser[mint]
    j0 = int(np.searchsorted(t, t_dec, side="right"))
    if j0 >= t.size:
        continue
    entry = float(p[j0])
    if not np.isfinite(entry) or entry <= 0:
        continue
    jH = int(np.searchsorted(t, t_dec + 300_000, side="right"))
    win = p[j0:jH]
    win = win[np.isfinite(win)]
    if win.size == 0:
        continue
    n += 1
    if win.size == 1:
        size1 += 1
    mae_incl.append(float((win.min() - entry) / entry * 1e4))
    rest = win[1:]
    mae_excl.append(float((rest.min() - entry) / entry * 1e4) if rest.size else np.nan)
    if smae is not None:
        stored.append(float(smae))

def stat(name, a):
    a = np.array(a, dtype=float); a = a[np.isfinite(a)]
    if a.size == 0:
        print(f"{name}: empty"); return
    print(f"{name:26s} n={a.size:7,} frac==0={float((a==0).mean()):.4f} "
          f"p50={np.percentile(a,50):9.1f} p10={np.percentile(a,10):10.1f} min={a.min():12.1f}")

print(f"\nwindow size == 1 trade: {size1:,} / {n:,}")
stat("mae_incl_entry (current)", mae_incl)
stat("mae_excl_entry (alt)", mae_excl)
stat("mae stored in ledger", stored)
