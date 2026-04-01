#!/usr/bin/env python3
"""
Deep analysis: pre-overhaul WR drivers, combined filter optimization on full dataset,
and Solana memecoin volatility characterization.
"""

import json
from collections import defaultdict

trades = []
with open("/data/.openclaw/workspace/projects/pump-quant/data/momentum_paper_trades.jsonl") as f:
    for line in f:
        line = line.strip()
        if not line:
            continue
        try:
            trades.append(json.loads(line))
        except:
            pass

# Split datasets
pre = [t for t in trades if 'size_sol' not in t]  # First 145 trades  
post = [t for t in trades if 'size_sol' in t]  # 4813 trades
enriched = [t for t in trades if t.get('grad_speed_s', 0) > 0]

def stats(tlist):
    n = len(tlist)
    if n == 0:
        return (0, 0.0, 0.0, 0.0, 0.0)
    wins = sum(1 for t in tlist if t.get('net_pnl_sol', 0) > 0)
    wr = wins / n * 100
    pnls = [t.get('net_pnl_sol', 0) for t in tlist]
    total = sum(pnls)
    exp = total / n * 1000
    return (n, wr, exp, total, 0)

# ══════════════════════════════════════════════════════════════
# PART 1: WHY PRE-OVERHAUL HAD 40% WR 
# ══════════════════════════════════════════════════════════════

print("=" * 80)
print("PART 1: WHY PRE-OVERHAUL HAD 40% WR")
print("=" * 80)

# Pre-overhaul characteristics
print(f"\nPre-overhaul trades: {len(pre)}")
pre_exit = defaultdict(int)
for t in pre:
    pre_exit[t.get('exit_reason', '?')] += 1
print(f"Exit reasons: {dict(pre_exit)}")

# Size distribution
pre_sizes = [t.get('position_size_sol', t.get('size_sol', 0.3)) for t in pre]
print(f"Avg size: {sum(pre_sizes)/len(pre_sizes):.3f} SOL")

# PnL distribution
winners = [t for t in pre if t.get('net_pnl_sol', 0) > 0]
losers = [t for t in pre if t.get('net_pnl_sol', 0) < 0]
avg_win = sum(t['net_pnl_sol'] for t in winners) / max(len(winners), 1)
avg_loss = sum(t['net_pnl_sol'] for t in losers) / max(len(losers), 1)
print(f"Winners: {len(winners)} ({100*len(winners)/len(pre):.1f}%), avg={avg_win:.4f} SOL")
print(f"Losers: {len(losers)} ({100*len(losers)/len(pre):.1f}%), avg={avg_loss:.4f} SOL")
print(f"R:R = {abs(avg_win/avg_loss):.2f}")

# Top winners
top_wins = sorted(winners, key=lambda t: t['net_pnl_sol'], reverse=True)[:10]
print("\nTop 10 winners:")
for t in top_wins:
    print(f"  PnL={t['net_pnl_sol']:.4f} exit={t['exit_reason']} hold={t['hold_ms']}ms")

# ══════════════════════════════════════════════════════════════
# PART 2: POST-OVERHAUL FULL CHARACTERIZATION
# ══════════════════════════════════════════════════════════════

print("\n" + "=" * 80)
print("PART 2: POST-OVERHAUL DETAILED BREAKDOWN")
print("=" * 80)

# WR by exit reason
print("\n--- Winners/Losers by exit reason ---")
for reason in ['time_sl', 'trailing_stop', 'hard_sl', 'max_hold', 'tp3', 'stagnation_exit']:
    subset = [t for t in post if t.get('exit_reason', '') == reason]
    if subset:
        n, wr, exp, total, _ = stats(subset)
        avg_hold = sum(t.get('hold_ms', 0) for t in subset) / n
        print(f"  {reason:20s}: n={n:5d} WR={wr:5.1f}% Exp={exp:7.2f}mSOL TotalPnL={total:.3f} AvgHold={avg_hold/1000:.1f}s")

# The key stat: pre had 0.3 SOL trades with trailing_stop as main winner exit
# Post has 0.05 SOL probes where time_sl dominates

# ══════════════════════════════════════════════════════════════
# PART 3: VOLUME CAP IMPACT ON ENRICHED TRADES
# ══════════════════════════════════════════════════════════════

print("\n" + "=" * 80)
print("PART 3: VOLUME CAP ANALYSIS (ENRICHED, n=856)")
print("=" * 80)

# The key insight: grad_volume_sol = 655.35 is the SATURATED VALUE
# (u16 max 65535 / 100 = 655.35) meaning the actual volume is HIGHER
# These are whale/bot pumps

saturated = [t for t in enriched if t.get('grad_volume_sol', 0) >= 655]
non_saturated = [t for t in enriched if 0 < t.get('grad_volume_sol', 0) < 655]
print(f"\nSaturated volume (≥655 SOL): n={len(saturated)}")
n, wr, exp, total, _ = stats(saturated)
print(f"  WR={wr:.1f}%, Exp={exp:.2f} mSOL, Total={total:.4f} SOL")

print(f"\nNon-saturated volume (<655 SOL): n={len(non_saturated)}")
n, wr, exp, total, _ = stats(non_saturated)
print(f"  WR={wr:.1f}%, Exp={exp:.2f} mSOL, Total={total:.4f} SOL")

# Further breakdown of non-saturated
for lo, hi, label in [(0, 100, '<100'), (100, 200, '100-200'), (200, 655, '200-655')]:
    subset = [t for t in non_saturated if lo <= t.get('grad_volume_sol', 0) < hi]
    n, wr, exp, total, _ = stats(subset)
    print(f"  {label}: n={n:4d} WR={wr:5.1f}% Exp={exp:7.2f}mSOL Total={total:.4f}")

# ══════════════════════════════════════════════════════════════
# PART 4: GRAD_SCORE BREAKDOWN WITH COMPONENT ANALYSIS
# ══════════════════════════════════════════════════════════════

print("\n" + "=" * 80)
print("PART 4: GRAD_SCORE VALUES - EXACT DISTRIBUTION")
print("=" * 80)

score_counts = defaultdict(int)
for t in enriched:
    score_counts[t.get('grad_score', 0)] += 1

for score in sorted(score_counts.keys()):
    subset = [t for t in enriched if t.get('grad_score', 0) == score]
    n, wr, exp, total, _ = stats(subset)
    print(f"  score={score:3d}: n={n:4d} ({100*n/len(enriched):.1f}%) WR={wr:5.1f}% Exp={exp:7.2f}mSOL")

# Score=40 is the whale pump mode: speed=15 + volume_tier=10 + ratio=15 + no discount + no velocity
# = 40 exactly. This is the "fast grad, high vol, few buys" pattern
score_40 = [t for t in enriched if t.get('grad_score', 0) == 40]
print(f"\nScore=40 breakdown (n={len(score_40)}):")
print(f"  speed=60: {sum(1 for t in score_40 if t.get('grad_speed_s',0)==60)}")
print(f"  vol>=655: {sum(1 for t in score_40 if t.get('grad_volume_sol',0)>=655)}")
print(f"  vol<655: {sum(1 for t in score_40 if 0 < t.get('grad_volume_sol',0) < 655)}")

# Score 70-79 block
score_70_79 = [t for t in enriched if 70 <= t.get('grad_score', 0) < 80]
print(f"\nScore 70-79 breakdown (n={len(score_70_79)}):")
speed_60_ct = sum(1 for t in score_70_79 if t.get('grad_speed_s', 0) == 60)
print(f"  speed=60: {speed_60_ct}")
n, wr, exp, total, _ = stats(score_70_79)
print(f"  WR={wr:.1f}%, Exp={exp:.2f} mSOL")

# ══════════════════════════════════════════════════════════════
# PART 5: FULL DATASET OPTIMAL FILTERS
# ══════════════════════════════════════════════════════════════

print("\n" + "=" * 80)
print("PART 5: OPTIMAL FILTER CONFIGURATIONS")
print("=" * 80)

# Best simple filters on enriched data
filters = [
    ("speed>=120 + vol<200", lambda t: t.get('grad_speed_s',0) >= 120 and t.get('grad_volume_sol',0) < 200),
    ("speed>=120 + vol<300", lambda t: t.get('grad_speed_s',0) >= 120 and t.get('grad_volume_sol',0) < 300),
    ("speed>=120", lambda t: t.get('grad_speed_s',0) >= 120),
    ("speed>=90", lambda t: t.get('grad_speed_s',0) >= 90),
    ("vol<200", lambda t: t.get('grad_volume_sol',0) < 200),
    ("vol<300", lambda t: t.get('grad_volume_sol',0) < 300),
    ("vol<100", lambda t: t.get('grad_volume_vol',0) < 100),
    ("score<50", lambda t: t.get('grad_score',0) < 50),
    ("score 30-55", lambda t: 30 <= t.get('grad_score',0) <= 55),
    ("ws>=10", lambda t: t.get('ws_notif_count_at_close',0) >= 10),
    ("ws>=20", lambda t: t.get('ws_notif_count_at_close',0) >= 20),
    ("ws>=50", lambda t: t.get('ws_notif_count_at_close',0) >= 50),
    # Combined
    ("speed>=120 + vol<200 + score<60", lambda t: t.get('grad_speed_s',0) >= 120 and t.get('grad_volume_sol',0) < 200 and t.get('grad_score',0) < 60),
    ("speed>=120 + vol<200 + ws>=10", lambda t: t.get('grad_speed_s',0) >= 120 and t.get('grad_volume_sol',0) < 200 and t.get('ws_notif_count_at_close',0) >= 10),
    ("speed>=90 + vol 50-200", lambda t: t.get('grad_speed_s',0) >= 90 and 50 <= t.get('grad_volume_sol',0) < 200),
    ("speed>=90 + vol 50-300", lambda t: t.get('grad_speed_s',0) >= 90 and 50 <= t.get('grad_volume_sol',0) < 300),
    # Anti-patterns to reject
    ("NOT(speed=60 + vol>=655)", lambda t: not (t.get('grad_speed_s',0) == 60 and t.get('grad_volume_sol',0) >= 655)),
    ("NOT(speed=60 + vol>=300)", lambda t: not (t.get('grad_speed_s',0) == 60 and t.get('grad_volume_sol',0) >= 300)),
    ("NOT(score=40 + speed=60)", lambda t: not (t.get('grad_score',0) == 40 and t.get('grad_speed_s',0) == 60)),
]

for label, filt in filters:
    subset = [t for t in enriched if filt(t)]
    n, wr, exp, total, _ = stats(subset)
    print(f"  {label:45s}: n={n:4d} WR={wr:5.1f}% Exp={exp:7.2f}mSOL Total={total:.4f}")

# ══════════════════════════════════════════════════════════════
# PART 6: POST-OVERHAUL LARGE DATASET FILTERS  
# ══════════════════════════════════════════════════════════════

print("\n" + "=" * 80)
print("PART 6: FULL POST-OVERHAUL FILTERS (n=4813)")
print("=" * 80)

# The 3957 trades without grad data (grad_speed_s=0) are the early trades
# These have grad_score=25 (the old default)
no_grad_data = [t for t in post if t.get('grad_speed_s', 0) == 0]
has_grad_data = [t for t in post if t.get('grad_speed_s', 0) > 0]
print(f"No grad enrichment: n={len(no_grad_data)}")
n, wr, exp, total, _ = stats(no_grad_data)
print(f"  WR={wr:.1f}%, Exp={exp:.2f} mSOL, Total={total:.4f}")

print(f"Has grad enrichment: n={len(has_grad_data)}")
n, wr, exp, total, _ = stats(has_grad_data)
print(f"  WR={wr:.1f}%, Exp={exp:.2f} mSOL, Total={total:.4f}")

# The non-enriched trades have grad_score=25. What drives their WR?
print(f"\n--- Non-enriched by exit_reason ---")
for reason in ['time_sl', 'trailing_stop', 'hard_sl', 'max_hold', 'tp3']:
    subset = [t for t in no_grad_data if t.get('exit_reason','') == reason]
    if subset:
        n, wr, exp, total, _ = stats(subset)
        print(f"  {reason:20s}: n={n:5d} WR={wr:5.1f}% Exp={exp:7.2f}mSOL Total={total:.3f}")

# Non-enriched by UTC hour  
print(f"\n--- Non-enriched by UTC hour ---")
for hour in range(24):
    subset = [t for t in no_grad_data if ((t['entry_timestamp_ms'] // 3_600_000) % 24) == hour]
    if subset:
        n, wr, exp, total, _ = stats(subset)
        if n >= 20:
            print(f"  UTC_{hour:02d}: n={n:4d} WR={wr:5.1f}% Exp={exp:7.2f}mSOL Total={total:.4f}")

# ══════════════════════════════════════════════════════════════
# PART 7: PRICE TRAJECTORY DEEP DIVE
# ══════════════════════════════════════════════════════════════

print("\n" + "=" * 80)
print("PART 7: PRICE SAMPLE DEEP DIVE")
print("=" * 80)

# For enriched trades, look at sample evolution
for t in enriched:
    samples = t.get('price_samples_bps', [])
    if len(samples) >= 3:
        # Compute max gain and max drawdown
        t['_max_gain'] = max(samples)
        t['_max_dd'] = min(samples)
        # Trajectory: last sample vs first nonzero
        nonzero = [s for s in samples if s != 0]
        t['_final_bps'] = samples[-1]
        t['_nonzero_count'] = len(nonzero)
    else:
        t['_max_gain'] = 0
        t['_max_dd'] = 0
        t['_final_bps'] = 0
        t['_nonzero_count'] = 0

# Max gain buckets
print("\n--- Max gain during hold ---")
gain_buckets = [
    ('max_gain=0', lambda t: t['_max_gain'] == 0),
    ('max_gain 1-100', lambda t: 1 <= t['_max_gain'] <= 100),
    ('max_gain 101-300', lambda t: 101 <= t['_max_gain'] <= 300),
    ('max_gain 301-500', lambda t: 301 <= t['_max_gain'] <= 500),
    ('max_gain 501-1000', lambda t: 501 <= t['_max_gain'] <= 1000),
    ('max_gain > 1000', lambda t: t['_max_gain'] > 1000),
]
for label, filt in gain_buckets:
    subset = [t for t in enriched if filt(t)]
    n, wr, exp, total, _ = stats(subset)
    print(f"  {label:25s}: n={n:4d} WR={wr:5.1f}% Exp={exp:7.2f}mSOL")

# All-zero samples = "dead on arrival"
all_zero = [t for t in enriched if t.get('price_samples_bps', []) and all(s == 0 for s in t['price_samples_bps'])]
print(f"\nAll-zero price samples (dead on arrival): {len(all_zero)}/{len(enriched)} ({100*len(all_zero)/len(enriched):.1f}%)")
n, wr, exp, total, _ = stats(all_zero)
print(f"  WR={wr:.1f}%, Exp={exp:.2f} mSOL, Total={total:.4f}")

# Speed breakdown of all-zero
az_speeds = defaultdict(int)
for t in all_zero:
    az_speeds[t.get('grad_speed_s', 0)] += 1
print(f"  Speed dist: {dict(sorted(az_speeds.items()))}")

# ══════════════════════════════════════════════════════════════
# PART 8: RECOMMENDED PRODUCTION THRESHOLDS
# ══════════════════════════════════════════════════════════════

print("\n" + "=" * 80)
print("PART 8: RECOMMENDED THRESHOLDS SUMMARY")
print("=" * 80)

# Compute stats for recommended filter stack
recommended = [t for t in enriched if 
               t.get('grad_speed_s', 0) >= 90 and
               t.get('grad_volume_sol', 0) < 200 and
               t.get('grad_volume_sol', 0) >= 50]
n, wr, exp, total, _ = stats(recommended)
print(f"\nRECOMMENDED: speed>=90s, vol 50-200 SOL")
print(f"  n={n}, WR={wr:.1f}%, Exp={exp:.2f} mSOL, Total={total:.4f}")

# With additional ws_notif gate for scale-in
rec_ws = [t for t in recommended if t.get('ws_notif_count_at_close', 0) >= 10]
n, wr, exp, total, _ = stats(rec_ws)
print(f"\n+ ws_notif>=10 for scale-in:")
print(f"  n={n}, WR={wr:.1f}%, Exp={exp:.2f} mSOL, Total={total:.4f}")

# The "just block whale pumps" approach
no_whale = [t for t in enriched if 
            not (t.get('grad_speed_s', 0) == 60 and t.get('grad_volume_sol', 0) >= 655)]
n, wr, exp, total, _ = stats(no_whale)
print(f"\nJUST BLOCK WHALE PUMPS (speed=60+vol>=655):")
print(f"  n={n}, WR={wr:.1f}%, Exp={exp:.2f} mSOL, Total={total:.4f}")

# Compare: current (all enriched)
n, wr, exp, total, _ = stats(enriched)
print(f"\nCURRENT (all enriched, no filter):")
print(f"  n={n}, WR={wr:.1f}%, Exp={exp:.2f} mSOL, Total={total:.4f}")

print("\n=== DEEP ANALYSIS COMPLETE ===")
