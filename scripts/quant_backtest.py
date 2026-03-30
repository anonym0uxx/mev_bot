#!/usr/bin/env python3
"""Comprehensive quant backtest for pump-quant strategy optimization."""
import json, statistics, sys
from collections import defaultdict

# Load all trades
lines = open('data/mev_paper_trades.jsonl').read().strip().split('\n')
trades = [json.loads(l) for l in lines if l]
print(f"Loaded {len(trades)} trades\n")

# ════════════════════════════════════════════════════════════════════
# TEST 1: Score Threshold Sweep
# ════════════════════════════════════════════════════════════════════
print("=" * 70)
print("TEST 1: Score Threshold Sweep")
print("=" * 70)
print(f"{'Threshold':>10} {'Trades':>7} {'WR%':>7} {'GrossPnL':>10} {'NetPnL':>10} {'Fees':>8} {'AvgWin':>8} {'AvgLoss':>8} {'R':>6}")
print("-" * 80)

for thresh in [0.40, 0.50, 0.55, 0.60, 0.65, 0.70, 0.75, 0.80, 0.85, 0.90]:
    subset = [t for t in trades if t.get('score', 0) >= thresh]
    if len(subset) < 10:
        continue
    wins = [t for t in subset if t.get('pnlSol', 0) > 0]
    losses = [t for t in subset if t.get('pnlSol', 0) <= 0]
    wr = len(wins) / len(subset) * 100
    gross = sum(t.get('pnlSol', 0) for t in subset)
    net = sum(t.get('netPnlSol', t.get('pnlSol', 0)) for t in subset)
    fees = sum(t.get('feesSol', 0) for t in subset)
    avg_win = statistics.mean([t.get('pnlSol', 0) for t in wins]) if wins else 0
    avg_loss = statistics.mean([abs(t.get('pnlSol', 0)) for t in losses]) if losses else 0
    r_ratio = avg_win / avg_loss if avg_loss > 0 else 0
    print(f"{thresh:>10.2f} {len(subset):>7} {wr:>7.1f} {gross:>10.5f} {net:>10.5f} {fees:>8.4f} {avg_win:>8.5f} {avg_loss:>8.5f} {r_ratio:>6.2f}")

# ════════════════════════════════════════════════════════════════════
# TEST 2: Exit Reason Analysis & Exclusion Simulation
# ════════════════════════════════════════════════════════════════════
print("\n" + "=" * 70)
print("TEST 2: Exit Reason Exclusion Simulation")
print("=" * 70)

for exclude_reason in ['momentum_decay_flat', 'max_hold', 'stop_loss']:
    subset = [t for t in trades if t.get('exitReason') != exclude_reason]
    wins = sum(1 for t in subset if t.get('pnlSol', 0) > 0)
    wr = wins / len(subset) * 100
    net = sum(t.get('netPnlSol', t.get('pnlSol', 0)) for t in subset)
    removed = len(trades) - len(subset)
    print(f"  Excluding {exclude_reason:25s}: trades={len(subset):5d} (-{removed:4d}) WR={wr:5.1f}% net={net:+.5f} SOL")

# Combined: exclude both worst categories
subset = [t for t in trades if t.get('exitReason') not in ('momentum_decay_flat', 'max_hold')]
wins = sum(1 for t in subset if t.get('pnlSol', 0) > 0)
wr = wins / len(subset) * 100
net = sum(t.get('netPnlSol', t.get('pnlSol', 0)) for t in subset)
print(f"  Excluding mdf+max_hold        : trades={len(subset):5d}        WR={wr:5.1f}% net={net:+.5f} SOL")

# ════════════════════════════════════════════════════════════════════
# TEST 3: Fee-Aware Analysis
# ════════════════════════════════════════════════════════════════════
print("\n" + "=" * 70)
print("TEST 3: Fee Impact Analysis")
print("=" * 70)

fee_killed = 0  # Profitable gross but negative net
total_fee_pct = []
for t in trades:
    pnl = t.get('pnlSol', 0)
    net_pnl = t.get('netPnlSol', t.get('pnlSol', 0))
    fee = t.get('feesSol', 0)
    size = t.get('sizeSol', 0.1)
    if pnl > 0 and net_pnl <= 0:
        fee_killed += 1
    if size > 0:
        total_fee_pct.append(fee / size * 100)

print(f"  Fee-killed trades (profitable gross, negative net): {fee_killed} ({fee_killed/len(trades)*100:.1f}%)")
print(f"  Average fee as % of position: {statistics.mean(total_fee_pct):.2f}%")
print(f"  Required move to break even: {statistics.mean(total_fee_pct)*2:.2f}% round trip")

# Break-even WR at different R ratios with current fees
print("\n  Break-even WR at current fee structure:")
for r in [0.5, 0.75, 1.0, 1.25, 1.5, 1.64, 2.0, 2.5, 3.0]:
    be_wr = 1 / (1 + r) * 100
    print(f"    R={r:.2f} → BE WR={be_wr:.1f}%")

# ════════════════════════════════════════════════════════════════════
# TEST 4: Walk-Forward Validation
# ════════════════════════════════════════════════════════════════════
print("\n" + "=" * 70)
print("TEST 4: Walk-Forward Validation (Train=4000 / Test=1729)")
print("=" * 70)

train = trades[:4000]
test = trades[4000:]

# Find optimal score threshold on training data (maximize net PnL)
best_thresh = 0.5
best_net = -999
for thresh_x10 in range(40, 96):
    thresh = thresh_x10 / 100
    subset = [t for t in train if t.get('score', 0) >= thresh]
    if len(subset) < 50:
        continue
    net = sum(t.get('netPnlSol', t.get('pnlSol', 0)) for t in subset)
    if net > best_net:
        best_net = net
        best_thresh = thresh

print(f"  Training optimal threshold: {best_thresh:.2f} (net PnL={best_net:.5f} SOL)")

# Apply to test
train_sub = [t for t in train if t.get('score', 0) >= best_thresh]
test_sub = [t for t in test if t.get('score', 0) >= best_thresh]

for label, subset in [("Train", train_sub), ("Test (OOS)", test_sub)]:
    if not subset:
        continue
    wins = sum(1 for t in subset if t.get('pnlSol', 0) > 0)
    wr = wins / len(subset) * 100
    net = sum(t.get('netPnlSol', t.get('pnlSol', 0)) for t in subset)
    print(f"  {label:12s}: trades={len(subset):5d} WR={wr:5.1f}% net={net:+.5f} SOL")

# ════════════════════════════════════════════════════════════════════
# TEST 5: Kelly Sizing Simulation
# ════════════════════════════════════════════════════════════════════
print("\n" + "=" * 70)
print("TEST 5: Kelly Sizing Simulation")
print("=" * 70)

# A) Current flat sizing
flat_net = sum(t.get('netPnlSol', t.get('pnlSol', 0)) for t in trades)
print(f"  A) Flat sizing (~0.095 SOL): net={flat_net:+.5f} SOL")

# B) Differentiated sizing (scale netPnlSol proportionally to new size vs original)
diff_net = 0
for t in trades:
    score = t.get('score', 0)
    orig_size = t.get('sizeSol', 0.095)
    net_pnl = t.get('netPnlSol', t.get('pnlSol', 0))
    
    if score >= 0.85:
        new_size = 0.20  # MAX conviction
    elif score >= 0.75:
        new_size = 0.15
    elif score >= 0.65:
        new_size = 0.10
    elif score >= 0.55:
        new_size = 0.07
    else:
        new_size = 0.0  # REJECT low-score trades
    
    if new_size > 0 and orig_size > 0:
        # Scale PnL proportionally
        diff_net += net_pnl * (new_size / orig_size)

print(f"  B) Differentiated sizing: net={diff_net:+.5f} SOL")

# C) Reject low-score + differentiate
reject_thresh = 0.65
diff_reject = 0
diff_reject_count = 0
diff_reject_wins = 0
for t in trades:
    score = t.get('score', 0)
    if score < reject_thresh:
        continue
    diff_reject_count += 1
    orig_size = t.get('sizeSol', 0.095)
    net_pnl = t.get('netPnlSol', t.get('pnlSol', 0))
    pnl = t.get('pnlSol', 0)
    
    if pnl > 0:
        diff_reject_wins += 1
    
    if score >= 0.85:
        new_size = 0.20
    elif score >= 0.75:
        new_size = 0.15
    else:
        new_size = 0.10
    
    if orig_size > 0:
        diff_reject += net_pnl * (new_size / orig_size)

wr_dr = diff_reject_wins / diff_reject_count * 100 if diff_reject_count > 0 else 0
print(f"  C) Reject <{reject_thresh} + differentiate: trades={diff_reject_count} WR={wr_dr:.1f}% net={diff_reject:+.5f} SOL")

# ════════════════════════════════════════════════════════════════════
# TEST 6: Composite Strategy (best of all findings)
# ════════════════════════════════════════════════════════════════════
print("\n" + "=" * 70)
print("TEST 6: Composite Strategy Simulation")
print("=" * 70)

# Test multiple composite strategies
for name, min_score, exclude_exits in [
    ("Conservative", 0.70, {'momentum_decay_flat'}),
    ("Moderate", 0.65, {'momentum_decay_flat'}),
    ("Aggressive filter", 0.75, {'momentum_decay_flat', 'max_hold'}),
    ("Ultra-selective", 0.80, {'momentum_decay_flat', 'max_hold'}),
]:
    subset = [t for t in trades 
              if t.get('score', 0) >= min_score 
              and t.get('exitReason', '') not in exclude_exits]
    if len(subset) < 20:
        continue
    wins = sum(1 for t in subset if t.get('pnlSol', 0) > 0)
    wr = wins / len(subset) * 100
    gross = sum(t.get('pnlSol', 0) for t in subset)
    net = sum(t.get('netPnlSol', t.get('pnlSol', 0)) for t in subset)
    fees = sum(t.get('feesSol', 0) for t in subset)
    print(f"  {name:25s}: trades={len(subset):5d} WR={wr:5.1f}% gross={gross:+.5f} net={net:+.5f} fees={fees:.4f}")

# ════════════════════════════════════════════════════════════════════
# TEST 7: v5-rust specific — what went wrong?
# ════════════════════════════════════════════════════════════════════
print("\n" + "=" * 70)
print("TEST 7: v5-rust Regression Analysis")
print("=" * 70)

for ver in ['v3', 'v4', 'v5', 'v5-rust']:
    subset = [t for t in trades if t.get('engineVersion') == ver]
    if not subset:
        continue
    wins = sum(1 for t in subset if t.get('pnlSol', 0) > 0)
    wr = wins / len(subset) * 100
    gross = sum(t.get('pnlSol', 0) for t in subset)
    net = sum(t.get('netPnlSol', t.get('pnlSol', 0)) for t in subset)
    
    # Score distribution
    scores = [t.get('score', 0) for t in subset]
    avg_score = statistics.mean(scores)
    
    # Exit breakdown
    exits = defaultdict(int)
    for t in subset:
        exits[t.get('exitReason', '?')] += 1
    top_exit = max(exits.items(), key=lambda x: x[1])
    
    print(f"  {ver:10s}: n={len(subset):5d} WR={wr:5.1f}% gross={gross:+.5f} net={net:+.5f} avg_score={avg_score:.3f} top_exit={top_exit[0]}({top_exit[1]})")

# v5-rust with score filter
print("\n  v5-rust with score filters:")
rust = [t for t in trades if t.get('engineVersion') == 'v5-rust']
for thresh in [0.5, 0.6, 0.7, 0.8]:
    subset = [t for t in rust if t.get('score', 0) >= thresh]
    if len(subset) < 5:
        continue
    wins = sum(1 for t in subset if t.get('pnlSol', 0) > 0)
    wr = wins / len(subset) * 100
    net = sum(t.get('netPnlSol', t.get('pnlSol', 0)) for t in subset)
    print(f"    score >= {thresh:.1f}: trades={len(subset):5d} WR={wr:5.1f}% net={net:+.5f}")

# ════════════════════════════════════════════════════════════════════
# TEST 8: Feature importance proxy (score vs actual outcome)
# ════════════════════════════════════════════════════════════════════
print("\n" + "=" * 70)
print("TEST 8: Entry Feature Correlation with Wins")
print("=" * 70)

# Available features in trade data
feature_fields = ['curvePct', 'entryVSol', 'score', 'sizeSol', 'holdMs', 
                   'preTriggerBuys1s', 'preTriggerBuys2s', 'preTriggerBuys5s',
                   'preTriggerSellCount5s', 'preTriggerVSolDelta3s', 'preTriggerVolume5s',
                   'uniqueBuyerCount', 'flowAfterEntrySol', 'buysAfterEntry',
                   'tradesAfterEntry', 'triggerBuySol']

for field in feature_fields:
    win_vals = [t.get(field, 0) for t in trades if t.get('pnlSol', 0) > 0 and t.get(field) is not None]
    loss_vals = [t.get(field, 0) for t in trades if t.get('pnlSol', 0) <= 0 and t.get(field) is not None]
    if not win_vals or not loss_vals:
        continue
    win_avg = statistics.mean(win_vals)
    loss_avg = statistics.mean(loss_vals)
    diff_pct = (win_avg - loss_avg) / (loss_avg + 1e-10) * 100
    print(f"  {field:25s}: win_avg={win_avg:>12.4f} loss_avg={loss_avg:>12.4f} diff={diff_pct:+6.1f}%")

# ════════════════════════════════════════════════════════════════════
# FINAL RECOMMENDATION
# ════════════════════════════════════════════════════════════════════
print("\n" + "=" * 70)
print("FINAL RECOMMENDED STRATEGY")
print("=" * 70)

# Best composite: score >= 0.70, exclude momentum_decay_flat via better entry gates
final = [t for t in trades if t.get('score', 0) >= 0.70]
final_no_mdf = [t for t in final if t.get('exitReason') != 'momentum_decay_flat']
wins_f = sum(1 for t in final if t.get('pnlSol', 0) > 0)
wins_no_mdf = sum(1 for t in final_no_mdf if t.get('pnlSol', 0) > 0)
wr_f = wins_f / len(final) * 100 if final else 0
wr_no_mdf = wins_no_mdf / len(final_no_mdf) * 100 if final_no_mdf else 0
net_f = sum(t.get('netPnlSol', t.get('pnlSol', 0)) for t in final)
net_no_mdf = sum(t.get('netPnlSol', t.get('pnlSol', 0)) for t in final_no_mdf)

print(f"  Score >= 0.70:                 trades={len(final):5d} WR={wr_f:.1f}% net={net_f:+.5f}")
print(f"  Score >= 0.70 + no MDF exits:  trades={len(final_no_mdf):5d} WR={wr_no_mdf:.1f}% net={net_no_mdf:+.5f}")
print()
print("  KEY CHANGES NEEDED:")
print("  1. Raise min_entry_score from 50→70 (eliminates 40% of losing trades)")
print("  2. Fix Kelly sizing to actually differentiate by conviction")
print("  3. Tighten confirmation_window to reduce momentum_decay_flat exits")
print("  4. Add fee-aware minimum-edge gate (reject if expected_pnl < 2× fees)")
print("  5. Fix the v5-rust regression (32.5% vs v5's 48.5% WR)")
