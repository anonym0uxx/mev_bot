#!/usr/bin/env python3
"""
Exhaustive permutation backtesting across filter parameter space.
Uses the enriched trade dataset (856 trades with grad_speed_s > 0).
"""

import json
import sys
from collections import defaultdict
from itertools import product
import math

# Load all trades
trades = []
with open("/data/.openclaw/workspace/projects/pump-quant/data/momentum_paper_trades.jsonl") as f:
    for line in f:
        line = line.strip()
        if not line:
            continue
        try:
            t = json.loads(line)
            trades.append(t)
        except:
            pass

# Focus on enriched trades (have real grad data)
enriched = [t for t in trades if t.get('grad_speed_s', 0) > 0]
print(f"Enriched trades for permutation testing: {len(enriched)}")

# Also separate the full post-overhaul dataset (has size_sol)
post_overhaul = [t for t in trades if 'size_sol' in t]
print(f"Post-overhaul trades: {len(post_overhaul)}")

# ── Utility functions ─────────────────────────────────────

def compute_stats(tlist):
    """Return (n, wr%, expectancy_msol, total_pnl, sharpe_proxy)"""
    n = len(tlist)
    if n == 0:
        return (0, 0.0, 0.0, 0.0, 0.0)
    wins = sum(1 for t in tlist if t.get('net_pnl_sol', 0) > 0)
    wr = wins / n * 100
    pnls = [t.get('net_pnl_sol', 0) for t in tlist]
    total = sum(pnls)
    exp = total / n * 1000  # mSOL per trade
    if n > 1:
        mean = total / n
        var = sum((p - mean)**2 for p in pnls) / (n - 1)
        std = var**0.5
        sharpe = mean / std if std > 0 else 0.0
    else:
        sharpe = 0.0
    return (n, wr, exp, total, sharpe)

# ══════════════════════════════════════════════════════════════
# PART 1: ENRICHED TRADES PERMUTATION (856 trades)
# ══════════════════════════════════════════════════════════════

print("\n" + "="*80)
print("PART 1: PERMUTATION BACKTEST ON ENRICHED TRADES (n=856)")
print("="*80)

# Define parameter space
speed_mins = [60, 90, 120, 150, 180]  # min grad_speed_s to accept
vol_maxes = [99999, 200, 300, 400, 500]  # max grad_volume_sol
vol_mins = [0, 50, 100]  # min grad_volume_sol
score_mins = [30, 40, 50, 60, 70]  # min grad_score

# ToD hour blocks: 
# current = no blocking, block_12_13 = block hours 12-13 UTC, block_18_20 = block 18-20
tod_configs = {
    'none': set(),
    'block_02_06': {2,3,4,5,6},
    'block_18_20': {18,19,20},
    'block_02_06+18_20': {2,3,4,5,6,18,19,20},
}

# Collect results
results = []

total_perms = len(speed_mins) * len(vol_maxes) * len(vol_mins) * len(score_mins) * len(tod_configs)
print(f"Total permutations: {total_perms}")

count = 0
for speed_min in speed_mins:
    for vol_max in vol_maxes:
        for vol_min in vol_mins:
            for score_min in score_mins:
                for tod_name, blocked_hours in tod_configs.items():
                    count += 1
                    
                    # Filter trades
                    filtered = []
                    for t in enriched:
                        gs = t.get('grad_speed_s', 0)
                        gv = t.get('grad_volume_sol', 0)
                        gscore = t.get('grad_score', 0)
                        ts_ms = t.get('entry_timestamp_ms', 0)
                        utc_hour = (ts_ms // 3_600_000) % 24
                        
                        if gs < speed_min:
                            continue
                        if gv > vol_max:
                            continue
                        if gv < vol_min:
                            continue
                        if gscore < score_min:
                            continue
                        if utc_hour in blocked_hours:
                            continue
                        filtered.append(t)
                    
                    n, wr, exp, total, sharpe = compute_stats(filtered)
                    results.append({
                        'speed_min': speed_min,
                        'vol_max': vol_max,
                        'vol_min': vol_min,
                        'score_min': score_min,
                        'tod': tod_name,
                        'n': n,
                        'wr': wr,
                        'exp_msol': exp,
                        'total_pnl': total,
                        'sharpe': sharpe,
                    })

# Sort by expectancy
results_by_exp = sorted(results, key=lambda r: r['exp_msol'], reverse=True)

# Top 30 by expectancy (with min 10 trades)
print("\n=== TOP 30 BY EXPECTANCY (n >= 10) ===")
shown = 0
for r in results_by_exp:
    if r['n'] >= 10:
        print(f"  speed>={r['speed_min']:3d}s vol<={r['vol_max']:5d} vol>={r['vol_min']:3d} "
              f"score>={r['score_min']:2d} tod={r['tod']:20s} | n={r['n']:4d} WR={r['wr']:5.1f}% "
              f"Exp={r['exp_msol']:7.2f}mSOL PnL={r['total_pnl']:.4f} Sharpe={r['sharpe']:.3f}")
        shown += 1
        if shown >= 30:
            break

# Top 20 by Sharpe (n >= 15)
print("\n=== TOP 20 BY SHARPE (n >= 15) ===")
results_by_sharpe = sorted(results, key=lambda r: r['sharpe'], reverse=True)
shown = 0
for r in results_by_sharpe:
    if r['n'] >= 15:
        print(f"  speed>={r['speed_min']:3d}s vol<={r['vol_max']:5d} vol>={r['vol_min']:3d} "
              f"score>={r['score_min']:2d} tod={r['tod']:20s} | n={r['n']:4d} WR={r['wr']:5.1f}% "
              f"Exp={r['exp_msol']:7.2f}mSOL PnL={r['total_pnl']:.4f} Sharpe={r['sharpe']:.3f}")
        shown += 1
        if shown >= 20:
            break

# Best WR configurations (n >= 20)
print("\n=== TOP 20 BY WIN RATE (n >= 20) ===")
results_by_wr = sorted(results, key=lambda r: r['wr'], reverse=True)
shown = 0
for r in results_by_wr:
    if r['n'] >= 20:
        print(f"  speed>={r['speed_min']:3d}s vol<={r['vol_max']:5d} vol>={r['vol_min']:3d} "
              f"score>={r['score_min']:2d} tod={r['tod']:20s} | n={r['n']:4d} WR={r['wr']:5.1f}% "
              f"Exp={r['exp_msol']:7.2f}mSOL PnL={r['total_pnl']:.4f} Sharpe={r['sharpe']:.3f}")
        shown += 1
        if shown >= 20:
            break

# ══════════════════════════════════════════════════════════════
# PART 2: WS_NOTIF SCALE-IN THRESHOLD ANALYSIS
# ══════════════════════════════════════════════════════════════

print("\n" + "="*80)
print("PART 2: WS_NOTIF SCALE-IN THRESHOLD ANALYSIS")
print("="*80)

# Among enriched trades with ws_notif_count_at_close
ws_trades = [t for t in enriched if 'ws_notif_count_at_close' in t]
print(f"Trades with ws_notif data: {len(ws_trades)}")

ws_thresholds = [0, 5, 10, 20, 50, 100, 200]
for thresh in ws_thresholds:
    above = [t for t in ws_trades if t.get('ws_notif_count_at_close', 0) >= thresh]
    below = [t for t in ws_trades if t.get('ws_notif_count_at_close', 0) < thresh]
    n_a, wr_a, exp_a, _, sharpe_a = compute_stats(above)
    n_b, wr_b, exp_b, _, sharpe_b = compute_stats(below)
    print(f"  ws_notif >= {thresh:4d}: n={n_a:4d} WR={wr_a:5.1f}% Exp={exp_a:7.2f}mSOL Sharpe={sharpe_a:.3f}")
    print(f"  ws_notif <  {thresh:4d}: n={n_b:4d} WR={wr_b:5.1f}% Exp={exp_b:7.2f}mSOL Sharpe={sharpe_b:.3f}")
    print()

# ══════════════════════════════════════════════════════════════
# PART 3: PRICE TRAJECTORY GATE ANALYSIS
# ══════════════════════════════════════════════════════════════

print("\n" + "="*80)
print("PART 3: PRICE TRAJECTORY ANALYSIS AT SCALE-IN POINT")
print("="*80)

# For enriched trades: analyze first 2-3 price samples as predictors of outcomes
for t in enriched:
    samples = t.get('price_samples_bps', [])
    # s[0] = first sample (~1s after entry)
    t['_s0'] = samples[0] if len(samples) > 0 else None
    t['_s1'] = samples[1] if len(samples) > 1 else None
    t['_s2'] = samples[2] if len(samples) > 2 else None
    # Max of first 3 samples
    first3 = [s for s in samples[:3] if s != 0] if len(samples) >= 3 else []
    t['_max3'] = max(first3) if first3 else 0
    t['_min3'] = min(first3) if first3 else 0

# s[0] as predictor
print("\n--- s[0] (first price sample) as predictor ---")
s0_buckets = {
    's0 <= -500': lambda t: t['_s0'] is not None and t['_s0'] <= -500,
    's0 -500 to -200': lambda t: t['_s0'] is not None and -500 < t['_s0'] <= -200,
    's0 -200 to 0': lambda t: t['_s0'] is not None and -200 < t['_s0'] <= 0,
    's0 0 (flat)': lambda t: t['_s0'] is not None and t['_s0'] == 0,
    's0 1 to 100': lambda t: t['_s0'] is not None and 0 < t['_s0'] <= 100,
    's0 101 to 300': lambda t: t['_s0'] is not None and 100 < t['_s0'] <= 300,
    's0 301 to 500': lambda t: t['_s0'] is not None and 300 < t['_s0'] <= 500,
    's0 > 500': lambda t: t['_s0'] is not None and t['_s0'] > 500,
}
for label, filt in s0_buckets.items():
    subset = [t for t in enriched if filt(t)]
    n, wr, exp, total, sharpe = compute_stats(subset)
    print(f"  {label:20s}: n={n:4d} WR={wr:5.1f}% Exp={exp:7.2f}mSOL Sharpe={sharpe:.3f}")

# s[1] as predictor
print("\n--- s[1] (second price sample) as predictor ---")
s1_buckets = {
    's1 <= -300': lambda t: t['_s1'] is not None and t['_s1'] <= -300,
    's1 -300 to -100': lambda t: t['_s1'] is not None and -300 < t['_s1'] <= -100,
    's1 -100 to 0': lambda t: t['_s1'] is not None and -100 < t['_s1'] <= 0,
    's1 0 (flat)': lambda t: t['_s1'] is not None and t['_s1'] == 0,
    's1 1 to 200': lambda t: t['_s1'] is not None and 0 < t['_s1'] <= 200,
    's1 201 to 500': lambda t: t['_s1'] is not None and 200 < t['_s1'] <= 500,
    's1 > 500': lambda t: t['_s1'] is not None and t['_s1'] > 500,
}
for label, filt in s1_buckets.items():
    subset = [t for t in enriched if filt(t)]
    n, wr, exp, total, sharpe = compute_stats(subset)
    print(f"  {label:20s}: n={n:4d} WR={wr:5.1f}% Exp={exp:7.2f}mSOL Sharpe={sharpe:.3f}")

# Combined s0+s1 gate for scale-in
print("\n--- Combined s0+s1 scale-in gates ---")
scale_combos = {
    's0>0 AND s1>0': lambda t: t['_s0'] is not None and t['_s1'] is not None and t['_s0'] > 0 and t['_s1'] > 0,
    's0>0 AND s1>100': lambda t: t['_s0'] is not None and t['_s1'] is not None and t['_s0'] > 0 and t['_s1'] > 100,
    's0>100 AND s1>0': lambda t: t['_s0'] is not None and t['_s1'] is not None and t['_s0'] > 100 and t['_s1'] > 0,
    's0>100 AND s1>100': lambda t: t['_s0'] is not None and t['_s1'] is not None and t['_s0'] > 100 and t['_s1'] > 100,
    's0>200': lambda t: t['_s0'] is not None and t['_s0'] > 200,
    's0>300': lambda t: t['_s0'] is not None and t['_s0'] > 300,
    's0>=0 (not negative)': lambda t: t['_s0'] is not None and t['_s0'] >= 0,
    'any first 3 > 200': lambda t: t['_max3'] > 200,
    'max first 3 > 300': lambda t: t['_max3'] > 300,
    'min first 3 >= 0': lambda t: t['_s0'] is not None and t['_min3'] >= 0 and len(t.get('price_samples_bps',[])) >= 3,
}
for label, filt in scale_combos.items():
    subset = [t for t in enriched if filt(t)]
    n, wr, exp, total, sharpe = compute_stats(subset)
    print(f"  {label:30s}: n={n:4d} WR={wr:5.1f}% Exp={exp:7.2f}mSOL Sharpe={sharpe:.3f}")

# ══════════════════════════════════════════════════════════════
# PART 4: VOLUME-SPEED INTERACTION MATRIX
# ══════════════════════════════════════════════════════════════

print("\n" + "="*80)
print("PART 4: VOLUME × SPEED INTERACTION MATRIX")
print("="*80)

speed_ranges = [(60, 90, '60-90'), (90, 120, '90-120'), (120, 180, '120-180'), (180, 300, '180-300'), (300, 99999, '300+')]
vol_ranges = [(0, 100, '<100'), (100, 200, '100-200'), (200, 400, '200-400'), (400, 700, '400-700'), (700, 99999, '700+')]

header = f"{'':15s}"
for _, _, vl in vol_ranges:
    header += f" | {vl:>12s}"
print(header)
print("-" * len(header))

for slo, shi, sl in speed_ranges:
    row = f"{sl:15s}"
    for vlo, vhi, vl in vol_ranges:
        subset = [t for t in enriched 
                  if slo <= t.get('grad_speed_s', 0) < shi
                  and vlo <= t.get('grad_volume_sol', 0) < vhi]
        n, wr, exp, _, _ = compute_stats(subset)
        if n > 0:
            row += f" | n={n:3d} WR={wr:4.0f}%"
        else:
            row += f" |        --    "
    print(row)

# ══════════════════════════════════════════════════════════════
# PART 5: PARETO FRONTIER  
# ══════════════════════════════════════════════════════════════

print("\n" + "="*80)
print("PART 5: PARETO FRONTIER (WR × Expectancy × TradeCount)")
print("="*80)

# Find Pareto-optimal points: no other point dominates on ALL of (WR, Exp, N)
# Filter to n >= 10
viable = [r for r in results if r['n'] >= 10 and r['exp_msol'] > 0]
print(f"Viable configurations (n>=10, Exp>0): {len(viable)}")

pareto = []
for r in viable:
    dominated = False
    for other in viable:
        if other is r:
            continue
        # other dominates r if better on all 3 dimensions
        if (other['wr'] >= r['wr'] and 
            other['exp_msol'] >= r['exp_msol'] and 
            other['n'] >= r['n'] and
            (other['wr'] > r['wr'] or other['exp_msol'] > r['exp_msol'] or other['n'] > r['n'])):
            dominated = True
            break
    if not dominated:
        pareto.append(r)

pareto.sort(key=lambda r: r['exp_msol'], reverse=True)
print(f"Pareto-optimal points: {len(pareto)}")
for r in pareto[:30]:
    print(f"  speed>={r['speed_min']:3d}s vol<={r['vol_max']:5d} vol>={r['vol_min']:3d} "
          f"score>={r['score_min']:2d} tod={r['tod']:20s} | n={r['n']:4d} WR={r['wr']:5.1f}% "
          f"Exp={r['exp_msol']:7.2f}mSOL PnL={r['total_pnl']:.4f} Sharpe={r['sharpe']:.3f}")

# ══════════════════════════════════════════════════════════════
# PART 6: DEEP DIVE INTO THE BAD TRADES
# ══════════════════════════════════════════════════════════════

print("\n" + "="*80)
print("PART 6: CHARACTERIZING BAD TRADES")
print("="*80)

# Fast grad (60s) + high volume (655.35 = saturated) trades
fast_saturated = [t for t in enriched if t.get('grad_speed_s', 0) == 60 and t.get('grad_volume_sol', 0) > 600]
print(f"\nFast-60s + vol>600 (saturated): {len(fast_saturated)} trades")
n, wr, exp, total, sharpe = compute_stats(fast_saturated)
print(f"  WR={wr:.1f}%, Exp={exp:.2f} mSOL, PnL={total:.4f}, Sharpe={sharpe:.3f}")

# Exit reason distribution for these
exit_dist = defaultdict(int)
for t in fast_saturated:
    exit_dist[t.get('exit_reason', '?')] += 1
print(f"  Exit reasons: {dict(exit_dist)}")

# Price sample analysis for these
flat_count = 0
for t in fast_saturated:
    samples = t.get('price_samples_bps', [])
    if samples and all(s == 0 for s in samples[:5]):
        flat_count += 1
print(f"  First 5 samples all zero: {flat_count}/{len(fast_saturated)} ({100*flat_count/max(len(fast_saturated),1):.1f}%)")

# Score distribution for bad trades
print(f"\n  Score distribution:")
score_dist = defaultdict(int)
for t in fast_saturated:
    s = t.get('grad_score', 0)
    bucket = (s // 10) * 10
    score_dist[bucket] += 1
for k in sorted(score_dist.keys()):
    print(f"    score {k}-{k+9}: {score_dist[k]}")

# ws_notif distribution
print(f"\n  ws_notif distribution:")
ws_dist = defaultdict(int)
for t in fast_saturated:
    ws = t.get('ws_notif_count_at_close', -1)
    if ws == -1:
        ws_dist['no_data'] += 1
    elif ws == 0:
        ws_dist['0'] += 1
    elif ws <= 10:
        ws_dist['1-10'] += 1
    elif ws <= 50:
        ws_dist['11-50'] += 1
    else:
        ws_dist['50+'] += 1
for k, v in sorted(ws_dist.items()):
    print(f"    ws_notif={k}: {v}")

# Now characterize the GOOD trades  
good_trades = [t for t in enriched if t.get('net_pnl_sol', 0) > 0.001]
print(f"\nGood trades (net_pnl > 0.001 SOL): {len(good_trades)}")

# Characteristics of winners
avg_speed_win = sum(t.get('grad_speed_s', 0) for t in good_trades) / max(len(good_trades), 1)
avg_vol_win = sum(t.get('grad_volume_sol', 0) for t in good_trades) / max(len(good_trades), 1)
avg_score_win = sum(t.get('grad_score', 0) for t in good_trades) / max(len(good_trades), 1)

bad_trades = [t for t in enriched if t.get('net_pnl_sol', 0) < -0.001]
avg_speed_loss = sum(t.get('grad_speed_s', 0) for t in bad_trades) / max(len(bad_trades), 1)
avg_vol_loss = sum(t.get('grad_volume_sol', 0) for t in bad_trades) / max(len(bad_trades), 1)
avg_score_loss = sum(t.get('grad_score', 0) for t in bad_trades) / max(len(bad_trades), 1)

print(f"\n  Winners (n={len(good_trades)}): avg_speed={avg_speed_win:.0f}s, avg_vol={avg_vol_win:.1f} SOL, avg_score={avg_score_win:.1f}")
print(f"  Losers  (n={len(bad_trades)}): avg_speed={avg_speed_loss:.0f}s, avg_vol={avg_vol_loss:.1f} SOL, avg_score={avg_score_loss:.1f}")

# Winner speed distribution
print("\n  Winner speed distribution:")
for speed_min, speed_max, label in [(60, 90, '60-90'), (90, 120, '90-120'), (120, 180, '120-180'), (180, 300, '180-300')]:
    w = [t for t in good_trades if speed_min <= t.get('grad_speed_s', 0) < speed_max]
    l = [t for t in bad_trades if speed_min <= t.get('grad_speed_s', 0) < speed_max]
    all_bucket = [t for t in enriched if speed_min <= t.get('grad_speed_s', 0) < speed_max]
    wr = len(w) / max(len(all_bucket), 1) * 100
    print(f"    {label}s: {len(w)} wins / {len(all_bucket)} total = {wr:.1f}% WR")

print("\n=== PERMUTATION ANALYSIS COMPLETE ===")
