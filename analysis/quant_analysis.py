#!/usr/bin/env python3
"""
Exhaustive quantitative analysis of pump-quant momentum paper trades.
Segments, permutation backtesting, volatility characterization.
"""

import json
import sys
from collections import defaultdict
from datetime import datetime, timezone
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

print(f"Total trades loaded: {len(trades)}")

# Basic field availability
fields_present = defaultdict(int)
for t in trades:
    for k in t:
        fields_present[k] += 1

print("\n=== FIELD AVAILABILITY ===")
for k, v in sorted(fields_present.items(), key=lambda x: -x[1]):
    print(f"  {k}: {v}/{len(trades)} ({100*v/len(trades):.1f}%)")

# Identify overhaul boundary
# The last 202 trades should be post-overhaul
# Let's find the boundary by looking at config changes / timestamps
# Post-overhaul trades have probe_entry fields, size_sol, etc.

# Let's check what distinguishes old vs new trades
print("\n=== TRADE SCHEMA EVOLUTION ===")
for i, t in enumerate(trades[:10]):
    print(f"  Trade {i}: config={t.get('config_version','?')}, grad_score={t.get('grad_score','?')}, "
          f"speed={t.get('grad_speed_s','?')}, vol={t.get('grad_volume_sol','?')}, "
          f"size={t.get('size_sol','?')}, exit={t.get('exit_reason','?')}")

print("...")
for i in range(max(0, len(trades)-10), len(trades)):
    t = trades[i]
    print(f"  Trade {i}: config={t.get('config_version','?')}, grad_score={t.get('grad_score','?')}, "
          f"speed={t.get('grad_speed_s','?')}, vol={t.get('grad_volume_sol','?')}, "
          f"size={t.get('size_sol','?')}, exit={t.get('exit_reason','?')}")

# Identify post-overhaul trades (those with probe entry data / new scorer fields)
# Key markers: grad_score_final field, size_sol field, ws_notif_count_at_close field
post_overhaul = []
pre_overhaul = []
for i, t in enumerate(trades):
    # Post-overhaul trades have size_sol and grad_score_final
    if 'size_sol' in t and t.get('grad_speed_s', 0) > 0:
        post_overhaul.append(t)
    elif t.get('grad_speed_s', 0) > 0 or t.get('grad_volume_sol', 0) > 0:
        pre_overhaul.append(t)
    else:
        pre_overhaul.append(t)

print(f"\n=== EPOCH SPLIT ===")
print(f"Pre-overhaul: {len(pre_overhaul)} trades")
print(f"Post-overhaul: {len(post_overhaul)} trades")

# Let's be more precise - find where size_sol first appears
first_post_idx = None
for i, t in enumerate(trades):
    if 'size_sol' in t:
        first_post_idx = i
        break

if first_post_idx:
    print(f"First post-overhaul trade at index {first_post_idx}")
    pre_overhaul = trades[:first_post_idx]
    post_overhaul = trades[first_post_idx:]
    print(f"Revised: Pre={len(pre_overhaul)}, Post={len(post_overhaul)}")

# ── Helper functions ──────────────────────────────────────────

def win_rate(tlist):
    if not tlist: return 0.0
    wins = sum(1 for t in tlist if t.get('net_pnl_sol', 0) > 0)
    return wins / len(tlist) * 100

def avg_pnl(tlist):
    if not tlist: return 0.0
    return sum(t.get('net_pnl_sol', 0) for t in tlist) / len(tlist)

def total_pnl(tlist):
    return sum(t.get('net_pnl_sol', 0) for t in tlist)

def expectancy(tlist):
    """Per-trade expectancy in mSOL"""
    if not tlist: return 0.0
    return total_pnl(tlist) / len(tlist) * 1000  # mSOL

def sharpe_proxy(tlist):
    """Simple Sharpe proxy: mean(pnl) / std(pnl)"""
    if len(tlist) < 2: return 0.0
    pnls = [t.get('net_pnl_sol', 0) for t in tlist]
    mean = sum(pnls) / len(pnls)
    variance = sum((p - mean) ** 2 for p in pnls) / (len(pnls) - 1)
    std = variance ** 0.5
    if std == 0: return 0.0
    return mean / std

def segment_stats(label, tlist):
    n = len(tlist)
    if n == 0:
        return f"  {label}: n=0"
    wr = win_rate(tlist)
    tp = total_pnl(tlist)
    exp = expectancy(tlist)
    sp = sharpe_proxy(tlist)
    avg_size = sum(t.get('size_sol', t.get('position_size_sol', 0.3)) for t in tlist) / n
    return f"  {label}: n={n}, WR={wr:.1f}%, PnL={tp:.4f} SOL, Exp={exp:.2f} mSOL/trade, Sharpe={sp:.3f}, AvgSize={avg_size:.3f}"

# ══════════════════════════════════════════════════════════════
# SECTION 1: PRE vs POST OVERHAUL
# ══════════════════════════════════════════════════════════════

print("\n" + "="*70)
print("SECTION 1: PRE vs POST OVERHAUL COMPARISON")
print("="*70)
print(segment_stats("PRE-OVERHAUL", pre_overhaul))
print(segment_stats("POST-OVERHAUL", post_overhaul))

# ══════════════════════════════════════════════════════════════
# SECTION 2: POST-OVERHAUL DEEP SEGMENTATION
# ══════════════════════════════════════════════════════════════

print("\n" + "="*70)
print("SECTION 2: POST-OVERHAUL SEGMENTATION")
print("="*70)

# --- By grad_speed_s buckets ---
print("\n--- By grad_speed_s ---")
speed_buckets = {
    '60-90': lambda t: 60 <= t.get('grad_speed_s', 0) < 90,
    '90-120': lambda t: 90 <= t.get('grad_speed_s', 0) < 120,
    '120-180': lambda t: 120 <= t.get('grad_speed_s', 0) < 180,
    '180-240': lambda t: 180 <= t.get('grad_speed_s', 0) < 240,
    '240+': lambda t: t.get('grad_speed_s', 0) >= 240,
}
for label, filt in speed_buckets.items():
    subset = [t for t in post_overhaul if filt(t)]
    print(segment_stats(f"speed={label}s", subset))

# --- By grad_volume_sol buckets ---
print("\n--- By grad_volume_sol ---")
vol_buckets = {
    '<50': lambda t: t.get('grad_volume_sol', 0) < 50,
    '50-100': lambda t: 50 <= t.get('grad_volume_sol', 0) < 100,
    '100-200': lambda t: 100 <= t.get('grad_volume_sol', 0) < 200,
    '200-300': lambda t: 200 <= t.get('grad_volume_sol', 0) < 300,
    '300-400': lambda t: 300 <= t.get('grad_volume_sol', 0) < 400,
    '400-500': lambda t: 400 <= t.get('grad_volume_sol', 0) < 500,
    '500+': lambda t: t.get('grad_volume_sol', 0) >= 500,
}
for label, filt in vol_buckets.items():
    subset = [t for t in post_overhaul if filt(t)]
    print(segment_stats(f"vol={label}", subset))

# --- By grad_score buckets ---
print("\n--- By grad_score ---")
score_buckets = {
    '<20': lambda t: t.get('grad_score', 0) < 20,
    '20-30': lambda t: 20 <= t.get('grad_score', 0) < 30,
    '30-40': lambda t: 30 <= t.get('grad_score', 0) < 40,
    '40-50': lambda t: 40 <= t.get('grad_score', 0) < 50,
    '50-60': lambda t: 50 <= t.get('grad_score', 0) < 60,
    '60-70': lambda t: 60 <= t.get('grad_score', 0) < 70,
    '70-80': lambda t: 70 <= t.get('grad_score', 0) < 80,
    '80+': lambda t: t.get('grad_score', 0) >= 80,
}
for label, filt in score_buckets.items():
    subset = [t for t in post_overhaul if filt(t)]
    print(segment_stats(f"score={label}", subset))

# --- By UTC hour ---
print("\n--- By UTC hour ---")
for hour in range(24):
    subset = [t for t in post_overhaul if
              t.get('entry_timestamp_ms', 0) > 0 and
              ((t['entry_timestamp_ms'] // 3_600_000) % 24) == hour]
    if subset:
        print(segment_stats(f"UTC_{hour:02d}", subset))

# --- By exit reason ---
print("\n--- By exit_reason ---")
exit_reasons = defaultdict(list)
for t in post_overhaul:
    exit_reasons[t.get('exit_reason', 'unknown')].append(t)
for reason, tlist in sorted(exit_reasons.items(), key=lambda x: -len(x[1])):
    print(segment_stats(f"exit={reason}", tlist))

# --- By ws_notif_count_at_close ---
print("\n--- By ws_notif_count_at_close ---")
ws_buckets = {
    '0': lambda t: t.get('ws_notif_count_at_close', -1) == 0,
    '1-10': lambda t: 1 <= t.get('ws_notif_count_at_close', -1) <= 10,
    '11-50': lambda t: 11 <= t.get('ws_notif_count_at_close', -1) <= 50,
    '51-100': lambda t: 51 <= t.get('ws_notif_count_at_close', -1) <= 100,
    '101-200': lambda t: 101 <= t.get('ws_notif_count_at_close', -1) <= 200,
    '201+': lambda t: t.get('ws_notif_count_at_close', -1) > 200,
}
for label, filt in ws_buckets.items():
    subset = [t for t in post_overhaul if filt(t)]
    print(segment_stats(f"ws_notif={label}", subset))

# --- Price trajectory analysis ---
print("\n--- By price trajectory ---")
def classify_trajectory(t):
    samples = t.get('price_samples_bps', [])
    if not samples or len(samples) < 3:
        return 'insufficient'
    # Remove leading zeros
    nonzero = [s for s in samples if s != 0]
    if not nonzero:
        return 'flat'
    max_bps = max(samples)
    min_bps = min(samples)
    last_bps = samples[-1]
    # Rising: last > 0 and trending up
    if last_bps > 100 and max_bps > 200:
        return 'rising'
    # Falling: last < -100
    if last_bps < -100:
        return 'falling'
    # Pump_and_dump: max > 500 but last < max * 0.5
    if max_bps > 500 and last_bps < max_bps * 0.5:
        return 'pump_and_dump'
    # Flat: all within ±100
    if max_bps < 100 and min_bps > -100:
        return 'flat'
    return 'mixed'

traj_groups = defaultdict(list)
for t in post_overhaul:
    traj = classify_trajectory(t)
    traj_groups[traj].append(t)

for traj, tlist in sorted(traj_groups.items()):
    print(segment_stats(f"traj={traj}", tlist))

# --- Probe vs scale-in ---
print("\n--- Probe vs Scale-in ---")
probed_only = [t for t in post_overhaul if t.get('size_sol', 0) <= 0.10 + 0.001]
scaled_in = [t for t in post_overhaul if t.get('size_sol', 0) > 0.10 + 0.001]
print(segment_stats("probe_only (<=0.10 SOL)", probed_only))
print(segment_stats("scaled_in (>0.10 SOL)", scaled_in))

# ══════════════════════════════════════════════════════════════
# SECTION 3: COMBINED FILTERS ON POST-OVERHAUL
# ══════════════════════════════════════════════════════════════

print("\n" + "="*70)
print("SECTION 3: KEY CROSS-SECTIONS")
print("="*70)

# Fast grad + high vol (the broken zone)
fast_high = [t for t in post_overhaul 
             if t.get('grad_speed_s', 0) <= 90 and t.get('grad_volume_sol', 0) >= 300]
print(segment_stats("fast(<=90s)+high_vol(>=300)", fast_high))

# Slow grad + low vol (the winning zone)
slow_low = [t for t in post_overhaul
            if t.get('grad_speed_s', 0) >= 120 and t.get('grad_volume_sol', 0) < 300]
print(segment_stats("slow(>=120s)+low_vol(<300)", slow_low))

# Mid speed + mid vol
mid_mid = [t for t in post_overhaul
           if 90 <= t.get('grad_speed_s', 0) < 180 and 100 <= t.get('grad_volume_sol', 0) < 400]
print(segment_stats("mid(90-180s)+mid_vol(100-400)", mid_mid))

# High ws_notif + any
high_ws = [t for t in post_overhaul if t.get('ws_notif_count_at_close', 0) >= 50]
print(segment_stats("high_ws(>=50)", high_ws))

low_ws = [t for t in post_overhaul if 0 < t.get('ws_notif_count_at_close', -1) <= 20]
print(segment_stats("low_ws(1-20)", low_ws))

# ══════════════════════════════════════════════════════════════
# SECTION 4: FULL DATASET SEGMENTATION (ALL 4958 TRADES)
# ══════════════════════════════════════════════════════════════

print("\n" + "="*70)
print("SECTION 4: ALL TRADES (n=4958) - KEY METRICS")
print("="*70)

# Filter to trades that have grad_speed_s data
enriched = [t for t in trades if t.get('grad_speed_s', 0) > 0]
print(f"Enriched trades (have grad data): {len(enriched)}")
print(segment_stats("ALL enriched", enriched))

# By speed on full dataset
print("\n--- Full dataset by grad_speed_s ---")
for label, filt in speed_buckets.items():
    subset = [t for t in enriched if filt(t)]
    print(segment_stats(f"speed={label}s", subset))

# By volume on full dataset
print("\n--- Full dataset by grad_volume_sol ---")
for label, filt in vol_buckets.items():
    subset = [t for t in enriched if filt(t)]
    print(segment_stats(f"vol={label}", subset))

# By grad_score on full dataset
print("\n--- Full dataset by grad_score ---")
for label, filt in score_buckets.items():
    subset = [t for t in enriched if filt(t)]
    print(segment_stats(f"score={label}", subset))

# By exit reason on full dataset
print("\n--- Full dataset by exit_reason ---")
exit_reasons_all = defaultdict(list)
for t in enriched:
    exit_reasons_all[t.get('exit_reason', 'unknown')].append(t)
for reason, tlist in sorted(exit_reasons_all.items(), key=lambda x: -len(x[1])):
    print(segment_stats(f"exit={reason}", tlist))

# UTC hours on full dataset
print("\n--- Full dataset by UTC hour ---")
for hour in range(24):
    subset = [t for t in enriched if
              t.get('entry_timestamp_ms', 0) > 0 and
              ((t['entry_timestamp_ms'] // 3_600_000) % 24) == hour]
    if subset:
        print(segment_stats(f"UTC_{hour:02d}", subset))

print("\n=== ANALYSIS COMPLETE ===")
