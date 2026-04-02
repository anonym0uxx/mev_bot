#!/usr/bin/env python3
"""Lightweight extended Monte Carlo — fewer paths for tractability."""
import json, random, math
from pathlib import Path

trades = []
with open(Path("/data/.openclaw/workspace/projects/pump-quant/data/momentum_paper_trades.jsonl")) as f:
    for line in f:
        if line.strip():
            trades.append(json.loads(line.strip()))

# Build return distributions
returns_all, returns_60p, returns_ray, returns_60p_ray = [], [], [], []
for t in trades:
    sz = t.get("size_sol", 0.03)
    if sz <= 0: continue
    ret = t["net_pnl_sol"] / sz
    score = t.get("grad_score", 50)
    pool = t.get("pool_type", "")
    returns_all.append(ret)
    if score >= 60: returns_60p.append(ret)
    if pool == "raydium_amm_v4": returns_ray.append(ret)
    if score >= 60 and pool == "raydium_amm_v4": returns_60p_ray.append(ret)

print(f"Distributions: all={len(returns_all)}, 60+={len(returns_60p)}, ray={len(returns_ray)}, 60+ray={len(returns_60p_ray)}")

START, RUIN, N, T = 1.5, 0.2, 2000, 1000

def run_mc(rets, size, n_paths=N, n_trades=T):
    random.seed(42)
    finals, dds, ruins = [], [], 0
    for _ in range(n_paths):
        bal, hwm, mdd = START, START, 0
        for _ in range(n_trades):
            bal += random.choice(rets) * size
            if bal > hwm: hwm = bal
            dd = (hwm - bal) / hwm if hwm > 0 else 0
            mdd = max(mdd, dd)
            if bal < RUIN: ruins += 1; break
        finals.append(bal); dds.append(mdd)
    finals.sort(); dds.sort()
    return {
        "mean": sum(finals)/len(finals),
        "med": finals[len(finals)//2],
        "p5": finals[int(.05*len(finals))],
        "p95": finals[int(.95*len(finals))],
        "ruin": ruins/n_paths,
        "dd95": dds[int(.95*len(dds))],
    }

# A: Position size sensitivity
print(f"\n{'='*70}")
print("SECTION A: POSITION SIZE SENSITIVITY (2K paths × 1K trades)")
print(f"{'='*70}")
print(f"{'Size':>6} {'Mean':>8} {'Median':>8} {'P5':>8} {'P95':>8} {'P(ruin)':>8} {'95%DD':>8}")
print("-" * 60)
for size in [0.01, 0.02, 0.03, 0.04, 0.05, 0.06, 0.08, 0.10, 0.15, 0.20]:
    r = run_mc(returns_all, size)
    print(f"{size:>5.2f}  {r['mean']:>7.3f}  {r['med']:>7.3f}  {r['p5']:>7.3f}  {r['p95']:>7.3f}  {r['ruin']:>7.4f}  {r['dd95']:>7.2%}")

# B: Score-filtered scenarios
print(f"\n{'='*70}")
print("SECTION B: SCORE/POOL FILTERED SCENARIOS (0.03 SOL)")
print(f"{'='*70}")
print(f"{'Scenario':<22} {'N':>5} {'Mean':>8} {'Median':>8} {'P5':>8} {'P95':>8} {'P(ruin)':>8} {'95%DD':>8}")
print("-" * 80)
for name, rets in [("All trades", returns_all), ("Score 60+", returns_60p), ("Raydium only", returns_ray), ("60+ Raydium", returns_60p_ray)]:
    if len(rets) < 10:
        print(f"{name:<22} {len(rets):>5}  INSUFFICIENT DATA"); continue
    r = run_mc(rets, 0.03)
    print(f"{name:<22} {len(rets):>5}  {r['mean']:>7.3f}  {r['med']:>7.3f}  {r['p5']:>7.3f}  {r['p95']:>7.3f}  {r['ruin']:>7.4f}  {r['dd95']:>7.2%}")

# C: Kelly sensitivity
print(f"\n{'='*70}")
print("SECTION C: KELLY AT DIFFERENT WIN RATES (W/L=17.49x)")
print(f"{'='*70}")
WL = 17.49
print(f"{'WR':>5} {'Kelly':>7} {'K/4':>7} {'Bet@1.5':>8} {'Monthly EV':>11}")
print("-" * 45)
for wr_pct in [3, 5, 7.6, 10, 12, 15, 18, 20, 25]:
    wr = wr_pct/100; kelly = max(0, wr - (1-wr)/WL); k4 = kelly/4
    ev = (wr * 17.49 * 0.00077 + (1-wr) * -0.00077) * 450
    print(f"{wr_pct:>4.1f}% {kelly:>6.4f} {k4:>6.4f} {k4*1.5:>7.4f}  {ev:>+10.4f} SOL")

# D: Log-optimal growth rate
print(f"\n{'='*70}")
print("SECTION D: LOG-OPTIMAL GROWTH RATE vs FRACTION")
print(f"{'='*70}")
print(f"{'f':>8} {'Size':>8} {'g/trade':>12} {'E[final@1K]':>12}")
print("-" * 45)
for f in [0.005, 0.01, 0.02, 0.03, 0.04, 0.05, 0.07, 0.10, 0.15, 0.20]:
    g = sum(math.log(max(1e-10, 1 + f * r)) for r in returns_all) / len(returns_all)
    print(f"{f:>7.3f} {f*1.5:>7.4f}  {g:>11.8f}  {1.5*math.exp(g*1000):>11.4f}")

# E: Time-to-double (lighter: 1K paths × max 5K trades)
print(f"\n{'='*70}")
print("SECTION E: TIME TO DOUBLE (1K paths, 0.03 SOL)")
print(f"{'='*70}")
random.seed(42)
doubles = []
for _ in range(1000):
    bal = START
    for tn in range(5000):
        bal += random.choice(returns_all) * 0.03
        if bal >= 3.0: doubles.append(tn); break
if doubles:
    doubles.sort()
    print(f"Doubled: {len(doubles)}/1000 ({len(doubles)/10:.1f}%)")
    print(f"Median trades to double: {doubles[len(doubles)//2]} (~{doubles[len(doubles)//2]//15} days @ 15/day)")
    print(f"25th pctl: {doubles[int(.25*len(doubles))]} | 75th pctl: {doubles[int(.75*len(doubles))]}")
else:
    print("No paths doubled in 5K trades")

# F: Rolling 100-trade windows
print(f"\n{'='*70}")
print("SECTION F: 100-TRADE ROLLING WINDOWS")
print(f"{'='*70}")
pnls = [t["net_pnl_sol"] for t in trades]
print(f"{'Window':>12} {'WR':>6} {'Net':>10} {'Avg':>10}")
print("-" * 42)
for s in range(0, len(trades)-99, 100):
    w = pnls[s:s+100]; wins = sum(1 for p in w if p > 0)
    print(f"  {s+1:>4}-{s+100:>4}  {wins:>4}%  {sum(w):>+9.4f}  {sum(w)/100:>+9.6f}")
rem = pnls[(len(trades)//100)*100:]
if rem:
    s = (len(trades)//100)*100
    wins = sum(1 for p in rem if p > 0)
    print(f"  {s+1:>4}-{len(trades):>4}  {100*wins//len(rem):>4}%  {sum(rem):>+9.4f}  {sum(rem)/len(rem):>+9.6f}")
print("\nDONE")
