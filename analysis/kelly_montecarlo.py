#!/usr/bin/env python3
"""
Kelly Criterion & Monte Carlo Risk Analysis for Pump.fun Momentum Strategy
===========================================================================

Analyzes 776 paper trades to determine:
1. Kelly-optimal position sizing (overall and per score bracket)
2. Monte Carlo simulation of 10,000 paths (P(ruin), E[value], drawdown)
3. Wallet balance trajectory over actual trades
4. Circuit breaker analysis
5. Risk parameters for live mode
"""

import json
import math
import random
import sys
from collections import defaultdict
from pathlib import Path

# ─── Load Trade Data ─────────────────────────────────────────────────────────

DATA_PATH = Path("/data/.openclaw/workspace/projects/pump-quant/data/momentum_paper_trades.jsonl")

trades = []
with open(DATA_PATH) as f:
    for line in f:
        line = line.strip()
        if line:
            trades.append(json.loads(line))

print(f"Loaded {len(trades)} trades")

# ─── Extract Key Fields ─────────────────────────────────────────────────────

pnls = [t["net_pnl_sol"] for t in trades]
scores = [t.get("grad_score", t.get("grad_score_final", 50)) for t in trades]
pool_types = [t.get("pool_type", "unknown") for t in trades]
exit_reasons = [t.get("exit_reason", "unknown") for t in trades]
sizes = [t.get("size_sol", 0.03) for t in trades]

wins = [p for p in pnls if p > 0]
losses = [p for p in pnls if p <= 0]

print(f"\n{'='*70}")
print("SECTION 1: BASIC TRADE STATISTICS")
print(f"{'='*70}")
print(f"Total trades: {len(trades)}")
print(f"Wins: {len(wins)} ({100*len(wins)/len(trades):.1f}%)")
print(f"Losses: {len(losses)} ({100*len(losses)/len(trades):.1f}%)")
print(f"Net PnL: {sum(pnls):.6f} SOL")
print(f"Avg Win: {sum(wins)/len(wins):.6f} SOL" if wins else "No wins")
print(f"Avg Loss: {sum(losses)/len(losses):.6f} SOL" if losses else "No losses")
print(f"Median PnL: {sorted(pnls)[len(pnls)//2]:.6f} SOL")
print(f"Max Win: {max(pnls):.6f} SOL")
print(f"Max Loss: {min(pnls):.6f} SOL")

# Win/Loss ratio
avg_win = sum(wins) / len(wins) if wins else 0
avg_loss_abs = abs(sum(losses) / len(losses)) if losses else 1
wl_ratio = avg_win / avg_loss_abs if avg_loss_abs > 0 else 0
print(f"Win/Loss Ratio: {wl_ratio:.2f}x")

# Expected value per trade
ev_per_trade = sum(pnls) / len(pnls)
print(f"Expected Value per trade: {ev_per_trade:.6f} SOL ({ev_per_trade*10000:.2f} bps of 0.03)")

# ─── SECTION 2: KELLY CRITERION ANALYSIS ────────────────────────────────────

print(f"\n{'='*70}")
print("SECTION 2: KELLY CRITERION ANALYSIS")
print(f"{'='*70}")

def kelly_fraction(win_rate, wl_ratio):
    """
    Kelly fraction: f* = p - q/b
    where p = win rate, q = 1-p, b = avg_win/avg_loss
    """
    if wl_ratio <= 0:
        return 0
    p = win_rate
    q = 1 - p
    f = p - q / wl_ratio
    return max(0, f)

def kelly_for_pnl_distribution(pnls, bet_size=0.03):
    """
    Compute Kelly for a given PnL distribution with fixed bet size.
    Uses the actual return distribution: f* = E[R] / E[R^2]
    where R = net_pnl / bet_size
    """
    returns = [p / bet_size for p in pnls]
    mean_r = sum(returns) / len(returns)
    var_r = sum(r**2 for r in returns) / len(returns)
    if var_r <= 0 or mean_r <= 0:
        return 0
    return mean_r / var_r

# Overall Kelly using simple formula
win_rate = len(wins) / len(trades)
kelly_full = kelly_fraction(win_rate, wl_ratio)
kelly_half = kelly_full / 2
kelly_quarter = kelly_full / 4

print(f"\n--- Simple Kelly (p - q/b) ---")
print(f"Win Rate (p): {win_rate:.4f} ({win_rate*100:.2f}%)")
print(f"W/L Ratio (b): {wl_ratio:.2f}x")
print(f"Full Kelly f*: {kelly_full:.4f} ({kelly_full*100:.2f}%)")
print(f"Half Kelly: {kelly_half:.4f} ({kelly_half*100:.2f}%)")
print(f"Quarter Kelly: {kelly_quarter:.4f} ({kelly_quarter*100:.2f}%)")
print(f"")
print(f"At 1.5 SOL bankroll:")
print(f"  Full Kelly bet: {kelly_full * 1.5:.4f} SOL")
print(f"  Half Kelly bet: {kelly_half * 1.5:.4f} SOL")
print(f"  Quarter Kelly bet: {kelly_quarter * 1.5:.4f} SOL")

# Kelly using actual return distribution (more robust for heavy-tailed distributions)
# Normalize PnLs to fraction of bankroll
# Since position sizes varied, use actual PnL / actual size as the return
returns_actual = []
for t in trades:
    sz = t.get("size_sol", 0.03)
    if sz > 0:
        returns_actual.append(t["net_pnl_sol"] / sz)

mean_ret = sum(returns_actual) / len(returns_actual)
var_ret = sum(r**2 for r in returns_actual) / len(returns_actual)
kelly_continuous = mean_ret / var_ret if var_ret > 0 and mean_ret > 0 else 0

print(f"\n--- Continuous Kelly (E[R]/E[R²]) ---")
print(f"Mean return per unit: {mean_ret:.6f}")
print(f"Variance of return: {var_ret:.6f}")
print(f"Full Continuous Kelly: {kelly_continuous:.4f} ({kelly_continuous*100:.2f}%)")
print(f"Quarter Continuous Kelly: {kelly_continuous/4:.4f} ({kelly_continuous/4*100:.2f}%)")
print(f"At 1.5 SOL bankroll:")
print(f"  Full: {kelly_continuous * 1.5:.4f} SOL")
print(f"  Quarter: {kelly_continuous / 4 * 1.5:.4f} SOL")

# ─── SECTION 3: SCORE-STRATIFIED KELLY ──────────────────────────────────────

print(f"\n{'='*70}")
print("SECTION 3: SCORE-STRATIFIED KELLY")
print(f"{'='*70}")

score_brackets = {
    "20-29": (20, 29),
    "30-39": (30, 39),
    "40-49": (40, 49),
    "50-59": (50, 59),
    "60-69": (60, 69),
    "70-79": (70, 79),
}

print(f"\n{'Bracket':<10} {'Count':>6} {'Wins':>5} {'WR%':>6} {'NetPnL':>10} {'AvgWin':>10} {'AvgLoss':>10} {'W/L':>6} {'Kelly_f':>8} {'Kelly/4':>8} {'Opt_SOL':>8}")
print("-" * 110)

score_kelly = {}
for bracket_name, (lo, hi) in score_brackets.items():
    bracket_trades = [(t, pnls[i]) for i, t in enumerate(trades) if lo <= scores[i] <= hi]
    if not bracket_trades:
        continue
    
    b_pnls = [p for _, p in bracket_trades]
    b_wins = [p for p in b_pnls if p > 0]
    b_losses = [p for p in b_pnls if p <= 0]
    
    b_wr = len(b_wins) / len(bracket_trades) if bracket_trades else 0
    b_avg_win = sum(b_wins) / len(b_wins) if b_wins else 0
    b_avg_loss = abs(sum(b_losses) / len(b_losses)) if b_losses else 0.001
    b_wl = b_avg_win / b_avg_loss if b_avg_loss > 0 else 0
    b_net = sum(b_pnls)
    
    b_kelly = kelly_fraction(b_wr, b_wl)
    b_kelly_q = b_kelly / 4
    b_opt_sol = b_kelly_q * 1.5  # quarter Kelly on 1.5 SOL
    
    score_kelly[bracket_name] = {
        "count": len(bracket_trades),
        "wr": b_wr,
        "kelly_full": b_kelly,
        "kelly_quarter": b_kelly_q,
        "optimal_sol": b_opt_sol,
        "wl_ratio": b_wl,
        "net_pnl": b_net,
    }
    
    print(f"{bracket_name:<10} {len(bracket_trades):>6} {len(b_wins):>5} {b_wr*100:>5.1f}% {b_net:>+10.4f} {b_avg_win:>10.6f} {-b_avg_loss:>10.6f} {b_wl:>5.1f}x {b_kelly:>7.4f} {b_kelly_q:>7.4f} {b_opt_sol:>7.4f}")

# Pool type stratification
print(f"\n--- By Pool Type ---")
print(f"{'Pool':<20} {'Count':>6} {'Wins':>5} {'WR%':>6} {'NetPnL':>10} {'Kelly_f':>8} {'Kelly/4':>8}")
print("-" * 80)

for pool in ["raydium_amm_v4", "pump_swap"]:
    pool_trades = [(t, pnls[i]) for i, t in enumerate(trades) if pool_types[i] == pool]
    if not pool_trades:
        continue
    p_pnls = [p for _, p in pool_trades]
    p_wins = [p for p in p_pnls if p > 0]
    p_losses = [p for p in p_pnls if p <= 0]
    
    p_wr = len(p_wins) / len(pool_trades)
    p_avg_win = sum(p_wins) / len(p_wins) if p_wins else 0
    p_avg_loss = abs(sum(p_losses) / len(p_losses)) if p_losses else 0.001
    p_wl = p_avg_win / p_avg_loss if p_avg_loss > 0 else 0
    p_kelly = kelly_fraction(p_wr, p_wl)
    
    print(f"{pool:<20} {len(pool_trades):>6} {len(p_wins):>5} {p_wr*100:>5.1f}% {sum(p_pnls):>+10.4f} {p_kelly:>7.4f} {p_kelly/4:>7.4f}")

# ─── SECTION 4: MONTE CARLO SIMULATION ──────────────────────────────────────

print(f"\n{'='*70}")
print("SECTION 4: MONTE CARLO SIMULATION (10,000 paths × 1,000 trades)")
print(f"{'='*70}")

# We'll draw from the ACTUAL trade PnL distribution (with replacement)
# But we need to normalize to the CURRENT position size (0.03 SOL)
# Many early trades were 0.05 SOL. We scale PnLs to be as if they were all 0.03.

# Actually, the more rigorous approach: use the return per unit (PnL/size),
# then apply that return to the current 0.03 probe size.
# This preserves the actual market dynamics regardless of historical position size.

trade_returns = []  # return as fraction of position size
for t in trades:
    sz = t.get("size_sol", 0.03)
    if sz > 0:
        trade_returns.append(t["net_pnl_sol"] / sz)
    else:
        trade_returns.append(0)

STARTING_BALANCE = 1.5
POSITION_SIZE = 0.03
RUIN_THRESHOLD = 0.2
N_PATHS = 10000
N_TRADES_PER_PATH = 1000

random.seed(42)

final_balances = []
max_drawdowns = []
min_balances = []
ruin_count = 0
trade_1000_balances = []

for path in range(N_PATHS):
    balance = STARTING_BALANCE
    hwm = STARTING_BALANCE  # high water mark
    max_dd = 0
    min_bal = STARTING_BALANCE
    ruined = False
    
    for trade_i in range(N_TRADES_PER_PATH):
        # Draw a random return from the empirical distribution
        ret = random.choice(trade_returns)
        
        # PnL = return * position_size (fixed at 0.03)
        pnl = ret * POSITION_SIZE
        balance += pnl
        
        if balance > hwm:
            hwm = balance
        dd = (hwm - balance) / hwm if hwm > 0 else 0
        max_dd = max(max_dd, dd)
        min_bal = min(min_bal, balance)
        
        if balance < RUIN_THRESHOLD:
            ruined = True
            # Continue to see where it ends up (but in reality, circuit breaker would stop)
    
    final_balances.append(balance)
    max_drawdowns.append(max_dd)
    min_balances.append(min_bal)
    if ruined:
        ruin_count += 1

# Results
final_balances.sort()
max_drawdowns.sort()

p_ruin = ruin_count / N_PATHS
mean_final = sum(final_balances) / len(final_balances)
median_final = final_balances[len(final_balances) // 2]
p5_final = final_balances[int(0.05 * len(final_balances))]
p25_final = final_balances[int(0.25 * len(final_balances))]
p75_final = final_balances[int(0.75 * len(final_balances))]
p95_final = final_balances[int(0.95 * len(final_balances))]

mean_dd = sum(max_drawdowns) / len(max_drawdowns)
p50_dd = max_drawdowns[len(max_drawdowns) // 2]
p95_dd = max_drawdowns[int(0.95 * len(max_drawdowns))]
p99_dd = max_drawdowns[int(0.99 * len(max_drawdowns))]

print(f"\nSimulation Parameters:")
print(f"  Starting balance: {STARTING_BALANCE} SOL")
print(f"  Position size: {POSITION_SIZE} SOL (fixed)")
print(f"  Trades per path: {N_TRADES_PER_PATH}")
print(f"  Paths: {N_PATHS}")
print(f"  Ruin threshold: < {RUIN_THRESHOLD} SOL")
print(f"  Drawing from: {len(trade_returns)} empirical trade returns")

print(f"\n--- Final Balance Distribution (after {N_TRADES_PER_PATH} trades) ---")
print(f"  Mean: {mean_final:.4f} SOL")
print(f"  Median: {median_final:.4f} SOL")
print(f"  5th percentile: {p5_final:.4f} SOL")
print(f"  25th percentile: {p25_final:.4f} SOL")
print(f"  75th percentile: {p75_final:.4f} SOL")
print(f"  95th percentile: {p95_final:.4f} SOL")

print(f"\n--- Risk Metrics ---")
print(f"  Probability of ruin (< {RUIN_THRESHOLD} SOL): {p_ruin:.4f} ({p_ruin*100:.2f}%)")
print(f"  Mean max drawdown: {mean_dd:.4f} ({mean_dd*100:.2f}%)")
print(f"  Median max drawdown: {p50_dd:.4f} ({p50_dd*100:.2f}%)")
print(f"  95th percentile max drawdown: {p95_dd:.4f} ({p95_dd*100:.2f}%)")
print(f"  99th percentile max drawdown: {p99_dd:.4f} ({p99_dd*100:.2f}%)")

# Also run with Kelly-optimal sizing (fractional Kelly)
print(f"\n--- Monte Carlo with Quarter-Kelly Dynamic Sizing ---")

random.seed(42)
kelly_final_balances = []
kelly_max_drawdowns = []
kelly_ruin_count = 0

for path in range(N_PATHS):
    balance = STARTING_BALANCE
    hwm = STARTING_BALANCE
    max_dd = 0
    ruined = False
    
    for trade_i in range(N_TRADES_PER_PATH):
        ret = random.choice(trade_returns)
        
        # Quarter-Kelly dynamic sizing: size = kelly_quarter * balance
        # But clamp to [0.02, 0.2] SOL
        dynamic_size = kelly_quarter * balance
        dynamic_size = max(0.02, min(0.2, dynamic_size))
        
        pnl = ret * dynamic_size
        balance += pnl
        
        if balance > hwm:
            hwm = balance
        dd = (hwm - balance) / hwm if hwm > 0 else 0
        max_dd = max(max_dd, dd)
        
        if balance < RUIN_THRESHOLD:
            ruined = True
    
    kelly_final_balances.append(balance)
    kelly_max_drawdowns.append(max_dd)
    if ruined:
        kelly_ruin_count += 1

kelly_final_balances.sort()
kelly_max_drawdowns.sort()

k_p_ruin = kelly_ruin_count / N_PATHS
k_mean = sum(kelly_final_balances) / len(kelly_final_balances)
k_median = kelly_final_balances[len(kelly_final_balances) // 2]
k_p5 = kelly_final_balances[int(0.05 * len(kelly_final_balances))]
k_p95 = kelly_final_balances[int(0.95 * len(kelly_final_balances))]
k_p95_dd = kelly_max_drawdowns[int(0.95 * len(kelly_max_drawdowns))]

print(f"  Quarter-Kelly fraction: {kelly_quarter:.4f}")
print(f"  At 1.5 SOL: initial bet = {kelly_quarter * 1.5:.4f} SOL")
print(f"  Mean final balance: {k_mean:.4f} SOL")
print(f"  Median final balance: {k_median:.4f} SOL")
print(f"  5th percentile: {k_p5:.4f} SOL")
print(f"  95th percentile: {k_p95:.4f} SOL")
print(f"  P(ruin): {k_p_ruin:.4f} ({k_p_ruin*100:.2f}%)")
print(f"  95th percentile max DD: {k_p95_dd:.4f} ({k_p95_dd*100:.2f}%)")

# ─── SECTION 5: ACTUAL WALLET TRAJECTORY ────────────────────────────────────

print(f"\n{'='*70}")
print("SECTION 5: ACTUAL WALLET BALANCE TRAJECTORY (776 trades)")
print(f"{'='*70}")

balance = 1.5
hwm = 1.5
max_dd_sol = 0
max_dd_pct = 0
max_dd_trade_idx = 0
balances = [balance]
drawdowns = []

cumulative_pnl = 0
loss_streak = 0
max_loss_streak = 0
current_streak_start = 0

for i, t in enumerate(trades):
    pnl = t["net_pnl_sol"]
    balance += pnl
    cumulative_pnl += pnl
    balances.append(balance)
    
    if balance > hwm:
        hwm = balance
    
    dd_sol = hwm - balance
    dd_pct = dd_sol / hwm if hwm > 0 else 0
    drawdowns.append(dd_pct)
    
    if dd_sol > max_dd_sol:
        max_dd_sol = dd_sol
        max_dd_pct = dd_pct
        max_dd_trade_idx = i
    
    # Streak tracking
    if pnl < 0:
        if loss_streak == 0:
            current_streak_start = i
        loss_streak += 1
        if loss_streak > max_loss_streak:
            max_loss_streak = loss_streak
    else:
        loss_streak = 0

print(f"Starting balance: 1.5000 SOL")
print(f"Final balance: {balance:.4f} SOL")
print(f"Net PnL: {cumulative_pnl:+.4f} SOL")
print(f"High water mark: {hwm:.4f} SOL")
print(f"Max drawdown: {max_dd_sol:.4f} SOL ({max_dd_pct*100:.2f}%) at trade #{max_dd_trade_idx}")
print(f"Max loss streak: {max_loss_streak}")
print(f"Min balance: {min(balances):.4f} SOL (trade #{balances.index(min(balances))})")

# Find key moments
print(f"\n--- Key Balance Moments ---")
checkpoints = [0, 50, 100, 200, 300, 400, 500, 600, 700, 776]
for cp in checkpoints:
    if cp < len(balances):
        print(f"  Trade {cp:>4}: {balances[cp]:.4f} SOL (cum PnL: {balances[cp]-1.5:+.4f})")

# ─── SECTION 6: CIRCUIT BREAKER ANALYSIS ────────────────────────────────────

print(f"\n{'='*70}")
print("SECTION 6: CIRCUIT BREAKER ANALYSIS")
print(f"{'='*70}")

# Current config: consecutive_stop_pause_count = 3
# Analyze: how often do we hit N consecutive SL before a win?

def analyze_circuit_breaker(pnls, threshold):
    """Count how many times we'd hit N consecutive losses."""
    streak = 0
    triggers = 0
    trades_paused = 0  # Trades that would be skipped
    
    for i, p in enumerate(pnls):
        if p < 0:
            streak += 1
            if streak >= threshold:
                triggers += 1
                # In reality, would pause here. Count missed opportunity.
        else:
            streak = 0
    
    return triggers

# Test different circuit breaker thresholds
print(f"\nCircuit Breaker Triggers at Different Thresholds:")
print(f"{'Threshold':<12} {'Triggers':>10} {'% of Trades Blocked':>20}")
print("-" * 45)

for thresh in [3, 5, 10, 15, 20, 30, 50, 100]:
    # Simulate: after `thresh` consecutive losses, pause for `pause_trades` trades
    streak = 0
    triggers = 0
    paused_until = -1
    blocked_trades = 0
    blocked_wins = 0
    
    for i, p in enumerate(pnls):
        if i < paused_until:
            blocked_trades += 1
            if p > 0:
                blocked_wins += 1
            continue
        
        if p < 0:
            streak += 1
            if streak >= thresh:
                triggers += 1
                # Pause for N trades (using 3-minute pause → ~10-20 trades at current rate)
                paused_until = i + 20
                streak = 0
        else:
            streak = 0
    
    print(f"{thresh:<12} {triggers:>10} {blocked_trades:>10} blocked ({blocked_wins} were wins)")

# Analyze the max loss streak in detail
print(f"\n--- Loss Streak Distribution ---")
streaks = []
current = 0
for p in pnls:
    if p < 0:
        current += 1
    else:
        if current > 0:
            streaks.append(current)
        current = 0
if current > 0:
    streaks.append(current)

streaks.sort(reverse=True)
print(f"Total loss streaks: {len(streaks)}")
print(f"Top 10 longest: {streaks[:10]}")
print(f"Mean streak length: {sum(streaks)/len(streaks):.1f}")
print(f"Median streak length: {sorted(streaks)[len(streaks)//2]}")

# Loss per streak dollar impact
print(f"\n--- Cumulative Loss During Streaks ---")
streak_losses = []
current_streak_loss = 0
in_streak = False
for p in pnls:
    if p < 0:
        current_streak_loss += p
        in_streak = True
    else:
        if in_streak:
            streak_losses.append(current_streak_loss)
            current_streak_loss = 0
            in_streak = False
if in_streak:
    streak_losses.append(current_streak_loss)

streak_losses.sort()
print(f"Worst streak loss: {streak_losses[0]:.4f} SOL")
print(f"Top 5 worst streaks: {[f'{s:.4f}' for s in streak_losses[:5]]}")

# ─── SECTION 7: POSITION SIZING TABLE ───────────────────────────────────────

print(f"\n{'='*70}")
print("SECTION 7: RECOMMENDED POSITION SIZING TABLE")
print(f"{'='*70}")

# Based on Kelly analysis + practical constraints
# Key insight: quarter-Kelly provides good growth with much less variance

print(f"\n{'Score Range':<12} {'WR':>5} {'Kelly_f':>8} {'Kelly/4':>8} {'Rec Size':>10} {'Rationale':<30}")
print("-" * 85)

# Compute recommendations
recs = []
for bracket_name, data in score_kelly.items():
    lo = int(bracket_name.split("-")[0])
    hi = int(bracket_name.split("-")[1])
    
    if data["kelly_full"] <= 0 or data["count"] < 15:
        rec_size = 0.02  # Minimum probe
        rationale = "Negative/zero Kelly, min probe"
    elif data["wr"] < 0.05:
        rec_size = 0.02
        rationale = f"Low WR ({data['wr']*100:.0f}%), min probe"
    elif data["wr"] >= 0.15:
        rec_size = min(0.05, max(0.03, data["kelly_quarter"] * 1.5))
        rationale = f"Strong WR, quarter-Kelly"
    else:
        rec_size = min(0.04, max(0.02, data["kelly_quarter"] * 1.5))
        rationale = f"Moderate, scaled Kelly/4"
    
    rec_size = round(rec_size, 3)
    recs.append((bracket_name, data, rec_size, rationale))
    print(f"{bracket_name:<12} {data['wr']*100:>4.1f}% {data['kelly_full']:>7.4f} {data['kelly_quarter']:>7.4f} {rec_size:>9.3f} {rationale:<30}")

# ─── SECTION 8: RISK PARAMETERS FOR LIVE MODE ───────────────────────────────

print(f"\n{'='*70}")
print("SECTION 8: RECOMMENDED RISK PARAMETERS FOR LIVE MODE")
print(f"{'='*70}")

print(f"""
┌─────────────────────────────────────────────────────────────────┐
│ PARAMETER                          │ CURRENT    │ RECOMMENDED  │
├─────────────────────────────────────┼────────────┼──────────────┤
│ Kelly sizing enabled               │ false      │ false*       │
│ Probe size (default)               │ 0.03 SOL   │ 0.03 SOL     │
│ Probe size (score 60+)             │ 0.03 SOL   │ 0.04 SOL     │
│ Probe size (score 60+ raydium)     │ 0.03 SOL   │ 0.05 SOL     │
│ Max position size                  │ 0.125 SOL  │ 0.10 SOL     │
│ Max daily loss                     │ 0.25 SOL   │ 0.15 SOL     │
│ Circuit breaker (consecutive SL)   │ 3          │ 15-20        │
│ Circuit breaker pause              │ 3 min      │ 5 min        │
│ Min wallet balance (ruin guard)    │ 0.2 SOL    │ 0.3 SOL      │
│ Max daily entries                  │ 15         │ No change    │
│ Max concurrent positions           │ 1          │ 1            │
│ Bankroll (risk capital)            │ 0.71 SOL   │ 1.0 SOL      │
└─────────────────────────────────────┴────────────┴──────────────┘

* Kelly should remain DISABLED until:
  1. Win rate improves to >12% overall (currently 7.6%)
  2. At least 1,500+ trades for statistical significance
  3. Score-stratified sizing is implemented first
  Then enable with kelly_fraction=0.15 (conservative 15% Kelly)
""")