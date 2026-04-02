#!/usr/bin/env python3
"""
Extended Monte Carlo Analysis
- Score-filtered MC paths (what if we only trade score 60+?)
- Position size sensitivity (0.02 to 0.10 SOL)
- Kelly growth rate comparison
- Win rate sensitivity analysis
"""

import json
import random
from pathlib import Path

DATA_PATH = Path("/data/.openclaw/workspace/projects/pump-quant/data/momentum_paper_trades.jsonl")

trades = []
with open(DATA_PATH) as f:
    for line in f:
        line = line.strip()
        if line:
            trades.append(json.loads(line))

# Separate return distributions by score bracket
returns_all = []
returns_60plus = []
returns_raydium = []
returns_60plus_raydium = []

for t in trades:
    sz = t.get("size_sol", 0.03)
    if sz <= 0:
        continue
    ret = t["net_pnl_sol"] / sz
    score = t.get("grad_score", 50)
    pool = t.get("pool_type", "")
    
    returns_all.append(ret)
    if score >= 60:
        returns_60plus.append(ret)
    if pool == "raydium_amm_v4":
        returns_raydium.append(ret)
    if score >= 60 and pool == "raydium_amm_v4":
        returns_60plus_raydium.append(ret)

print(f"Return distributions loaded:")
print(f"  All: {len(returns_all)} trades")
print(f"  Score 60+: {len(returns_60plus)} trades")
print(f"  Raydium only: {len(returns_raydium)} trades")
print(f"  Score 60+ & Raydium: {len(returns_60plus_raydium)} trades")

# ─── SECTION A: POSITION SIZE SENSITIVITY ────────────────────────────────────

print(f"\n{'='*70}")
print("SECTION A: POSITION SIZE SENSITIVITY (10,000 paths × 1,000 trades)")
print(f"{'='*70}")

START = 1.5
N = 10000
T = 1000
RUIN = 0.2

print(f"\n{'Size':>6} {'Mean':>8} {'Median':>8} {'P5':>8} {'P95':>8} {'P(ruin)':>8} {'95%DD':>8} {'Growth/trade':>13}")
print("-" * 80)

for size in [0.01, 0.02, 0.03, 0.04, 0.05, 0.06, 0.08, 0.10, 0.15, 0.20]:
    random.seed(42)
    finals = []
    drawdowns = []
    ruins = 0
    
    for _ in range(N):
        bal = START
        hwm = START
        mdd = 0
        ruined = False
        
        for _ in range(T):
            ret = random.choice(returns_all)
            pnl = ret * size
            bal += pnl
            if bal > hwm:
                hwm = bal
            dd = (hwm - bal) / hwm if hwm > 0 else 0
            mdd = max(mdd, dd)
            if bal < RUIN:
                ruined = True
        
        finals.append(bal)
        drawdowns.append(mdd)
        if ruined:
            ruins += 1
    
    finals.sort()
    drawdowns.sort()
    mean_f = sum(finals) / len(finals)
    med_f = finals[len(finals)//2]
    p5_f = finals[int(0.05*len(finals))]
    p95_f = finals[int(0.95*len(finals))]
    p_ruin = ruins / N
    dd95 = drawdowns[int(0.95*len(drawdowns))]
    growth = (mean_f / START) ** (1/T) - 1  # geometric mean growth per trade
    
    print(f"{size:>5.2f}  {mean_f:>7.3f}  {med_f:>7.3f}  {p5_f:>7.3f}  {p95_f:>7.3f}  {p_ruin:>7.4f}  {dd95:>7.2%}  {growth*10000:>10.2f} bps")

# ─── SECTION B: SCORE-FILTERED MC ───────────────────────────────────────────

print(f"\n{'='*70}")
print("SECTION B: WHAT IF WE ONLY TRADE SCORE 60+? (vs ALL)")
print(f"{'='*70}")

scenarios = [
    ("All trades", returns_all),
    ("Score 60+", returns_60plus),
    ("Raydium only", returns_raydium),
    ("Score 60+ Raydium", returns_60plus_raydium),
]

SIZE = 0.03
print(f"\nPosition size: {SIZE} SOL, Starting: {START} SOL, {T} trades, {N} paths")
print(f"\n{'Scenario':<22} {'N_dist':>6} {'Mean':>8} {'Median':>8} {'P5':>8} {'P95':>8} {'P(ruin)':>8} {'95%DD':>8} {'EV/trade':>10}")
print("-" * 95)

for name, rets in scenarios:
    if len(rets) < 10:
        print(f"{name:<22} {len(rets):>6}  INSUFFICIENT DATA")
        continue
    
    random.seed(42)
    finals = []
    drawdowns = []
    ruins = 0
    ev = sum(rets) / len(rets) * SIZE
    
    for _ in range(N):
        bal = START
        hwm = START
        mdd = 0
        ruined = False
        
        for _ in range(T):
            ret = random.choice(rets)
            pnl = ret * SIZE
            bal += pnl
            if bal > hwm:
                hwm = bal
            dd = (hwm - bal) / hwm if hwm > 0 else 0
            mdd = max(mdd, dd)
            if bal < RUIN:
                ruined = True
        
        finals.append(bal)
        drawdowns.append(mdd)
        if ruined:
            ruins += 1
    
    finals.sort()
    drawdowns.sort()
    mean_f = sum(finals) / len(finals)
    med_f = finals[len(finals)//2]
    p5_f = finals[int(0.05*len(finals))]
    p95_f = finals[int(0.95*len(finals))]
    p_ruin = ruins / N
    dd95 = drawdowns[int(0.95*len(drawdowns))]
    
    print(f"{name:<22} {len(rets):>6}  {mean_f:>7.3f}  {med_f:>7.3f}  {p5_f:>7.3f}  {p95_f:>7.3f}  {p_ruin:>7.4f}  {dd95:>7.2%}  {ev:>9.6f}")

# ─── SECTION C: WIN RATE SENSITIVITY ────────────────────────────────────────

print(f"\n{'='*70}")
print("SECTION C: KELLY AT DIFFERENT WIN RATES (W/L=17.49x held constant)")
print(f"{'='*70}")

WL = 17.49
print(f"\n{'WR':>5} {'Kelly_f':>8} {'Kelly/2':>8} {'Kelly/4':>8} {'Bet@1.5':>10} {'Monthly EV':>11}")
print("-" * 60)
# Monthly EV assumes 15 trades/day × 30 days = 450 trades
MONTHLY_TRADES = 450
for wr_pct in [3, 5, 7.6, 10, 12, 15, 18, 20, 25, 30]:
    wr = wr_pct / 100
    kelly = max(0, wr - (1-wr)/WL)
    k4 = kelly / 4
    bet = k4 * 1.5
    # EV = per_trade_ev * monthly_trades
    avg_win = WL * 0.00077  # avg_loss_abs from data * WL ratio
    avg_loss = -0.00077
    per_trade_ev = wr * avg_win + (1-wr) * avg_loss
    monthly_ev = per_trade_ev * MONTHLY_TRADES
    print(f"{wr_pct:>4.1f}% {kelly:>7.4f} {kelly/2:>7.4f} {k4:>7.4f} {bet:>9.4f}  {monthly_ev:>+10.4f} SOL")

# ─── SECTION D: GROWTH RATE OPTIMIZATION ────────────────────────────────────

print(f"\n{'='*70}")
print("SECTION D: GEOMETRIC GROWTH RATE vs POSITION SIZE")
print(f"{'='*70}")

# The log-optimal growth rate g(f) = E[log(1 + f*R)] where R is the return
# f = fraction of bankroll bet
# For our case: R = return per unit (PnL/size)
# f = size / bankroll = size / 1.5

import math

print(f"\n{'f (fraction)':>12} {'Size @1.5':>10} {'g(f) per trade':>15} {'g×1000 trades':>15} {'E[final]':>10}")
print("-" * 70)

for f_pct in [0.005, 0.01, 0.015, 0.02, 0.025, 0.03, 0.04, 0.05, 0.07, 0.10, 0.15, 0.20]:
    # g(f) = E[log(1 + f*R)]
    g = 0
    for ret in returns_all:
        val = 1 + f_pct * ret
        if val > 0:
            g += math.log(val)
        else:
            g += math.log(1e-10)  # catastrophic loss
    g /= len(returns_all)
    
    size = f_pct * START
    g_1000 = g * 1000
    expected_final = START * math.exp(g_1000)
    
    print(f"{f_pct:>11.3f} {size:>9.4f}  {g:>14.8f}  {g_1000:>14.6f}  {expected_final:>9.4f}")

# ─── SECTION E: TIME-TO-DOUBLE / TIME-TO-RUIN ───────────────────────────────

print(f"\n{'='*70}")
print("SECTION E: TIME-TO-DOUBLE / TIME-TO-RUIN ESTIMATES")
print(f"{'='*70}")

random.seed(42)
SIZE = 0.03
N_SIM = 5000

doubles = []
ruins_at = []

for _ in range(N_SIM):
    bal = START
    for trade_num in range(10000):
        ret = random.choice(returns_all)
        bal += ret * SIZE
        if bal >= START * 2:
            doubles.append(trade_num)
            break
        if bal < RUIN:
            ruins_at.append(trade_num)
            break
    else:
        # Neither doubled nor ruined in 10K trades
        pass

if doubles:
    doubles.sort()
    print(f"\nTime to double (1.5 → 3.0 SOL) at 0.03 SOL per trade:")
    print(f"  Paths that doubled: {len(doubles)} / {N_SIM} ({100*len(doubles)/N_SIM:.1f}%)")
    print(f"  Median trades to double: {doubles[len(doubles)//2]}")
    print(f"  25th percentile: {doubles[int(0.25*len(doubles))]}")
    print(f"  75th percentile: {doubles[int(0.75*len(doubles))]}")
    # At 15 trades/day
    med_days = doubles[len(doubles)//2] / 15
    print(f"  Median days to double (at 15 trades/day): {med_days:.0f} days")
else:
    print(f"\n  No paths doubled in 10K trades")

if ruins_at:
    print(f"\nTime to ruin (1.5 → 0.2 SOL):")
    print(f"  Paths ruined: {len(ruins_at)} / {N_SIM} ({100*len(ruins_at)/N_SIM:.1f}%)")
else:
    print(f"\n  Zero paths hit ruin threshold in 10,000 trades — extremely robust")

# ─── SECTION F: ACTUAL TRADE SEQUENCE — 100-TRADE ROLLING METRICS ───────────

print(f"\n{'='*70}")
print("SECTION F: 100-TRADE ROLLING WINDOW ANALYSIS")
print(f"{'='*70}")

all_pnls = [t["net_pnl_sol"] for t in trades]

print(f"\n{'Window':>12} {'WR':>6} {'Net PnL':>10} {'Avg PnL':>10} {'Max Win':>10} {'Max Loss':>10}")
print("-" * 65)

for start in range(0, len(trades) - 99, 100):
    end = start + 100
    window_pnls = all_pnls[start:end]
    w = sum(1 for p in window_pnls if p > 0)
    wr = w / len(window_pnls)
    net = sum(window_pnls)
    avg = net / len(window_pnls)
    mx = max(window_pnls)
    mn = min(window_pnls)
    print(f"  {start+1:>4}-{end:>4}  {wr*100:>5.1f}% {net:>+10.4f} {avg:>+10.6f} {mx:>+10.6f} {mn:>+10.6f}")

# Final partial window
if len(trades) % 100 > 0:
    start = (len(trades) // 100) * 100
    window_pnls = all_pnls[start:]
    w = sum(1 for p in window_pnls if p > 0)
    wr = w / len(window_pnls) if window_pnls else 0
    net = sum(window_pnls)
    avg = net / len(window_pnls) if window_pnls else 0
    mx = max(window_pnls) if window_pnls else 0
    mn = min(window_pnls) if window_pnls else 0
    print(f"  {start+1:>4}-{len(trades):>4}  {wr*100:>5.1f}% {net:>+10.4f} {avg:>+10.6f} {mx:>+10.6f} {mn:>+10.6f}")

print(f"\n{'='*70}")
print("ANALYSIS COMPLETE")
print(f"{'='*70}")
