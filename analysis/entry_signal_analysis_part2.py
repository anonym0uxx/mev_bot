#!/usr/bin/env python3
"""
Part 2: Deeper multi-signal analysis + final recommendations
"""

import json
from collections import defaultdict
from datetime import datetime, timezone

DATA_FILE = "/data/.openclaw/workspace/projects/pump-quant/data/momentum_paper_trades.jsonl"

def load_trades():
    trades = []
    with open(DATA_FILE) as f:
        for line in f:
            line = line.strip()
            if line:
                trades.append(json.loads(line))
    return trades

def is_win(t):
    return t.get("net_pnl_sol", 0) > 0

def main():
    trades = load_trades()
    wins = [t for t in trades if is_win(t)]
    losses = [t for t in trades if not is_win(t)]
    
    print("=" * 60)
    print("MULTI-SIGNAL INTERACTION ANALYSIS")
    print("=" * 60)
    
    # ===== grad_speed_s ranges =====
    print("\n--- grad_speed_s ranges ---")
    speed_ranges = [(0, 30), (30, 60), (60, 90), (90, 120), (120, 121)]
    for lo, hi in speed_ranges:
        subset = [t for t in trades if lo <= t.get("grad_speed_s", 0) < hi]
        if not subset:
            continue
        sw = sum(1 for t in subset if is_win(t))
        sp = sum(t.get("net_pnl_sol", 0) for t in subset)
        wr = sw / len(subset) * 100
        ev = sp / len(subset)
        print(f"  [{lo:>3}-{hi:<3}s]: {len(subset):>4} trades, {sw:>3} wins, {wr:>5.1f}% WR, {sp:>+8.4f} PnL, EV={ev:>+8.5f}")
    
    # Also check: speed exactly 120 (max/default?)
    speed_120 = [t for t in trades if t.get("grad_speed_s") == 120]
    speed_not_120 = [t for t in trades if t.get("grad_speed_s") != 120]
    if speed_120:
        sw = sum(1 for t in speed_120 if is_win(t))
        sp = sum(t.get("net_pnl_sol", 0) for t in speed_120)
        wr = sw / len(speed_120) * 100
        print(f"\n  Exactly 120s: {len(speed_120)} trades, {wr:.1f}% WR, {sp:+.4f} PnL")
    if speed_not_120:
        sw = sum(1 for t in speed_not_120 if is_win(t))
        sp = sum(t.get("net_pnl_sol", 0) for t in speed_not_120)
        wr = sw / len(speed_not_120) * 100
        print(f"  Not 120s:     {len(speed_not_120)} trades, {wr:.1f}% WR, {sp:+.4f} PnL")
    
    # ===== grad_volume_sol ranges =====
    print("\n--- grad_volume_sol ranges ---")
    vol_ranges = [(0, 25), (25, 50), (50, 75), (75, 100), (100, 150), (150, 300), (300, 99999)]
    for lo, hi in vol_ranges:
        subset = [t for t in trades if lo <= t.get("grad_volume_sol", 0) < hi]
        if not subset:
            continue
        sw = sum(1 for t in subset if is_win(t))
        sp = sum(t.get("net_pnl_sol", 0) for t in subset)
        wr = sw / len(subset) * 100
        ev = sp / len(subset)
        print(f"  [{lo:>4}-{hi:<5}]: {len(subset):>4} trades, {sw:>3} wins, {wr:>5.1f}% WR, {sp:>+8.4f} PnL, EV={ev:>+8.5f}")
    
    # ===== pre_grad_buys_5s ranges =====
    print("\n--- pre_grad_buys_5s ranges ---")
    for buys in range(0, 20):
        subset = [t for t in trades if t.get("pre_grad_buys_5s", 0) == buys]
        if not subset:
            continue
        sw = sum(1 for t in subset if is_win(t))
        sp = sum(t.get("net_pnl_sol", 0) for t in subset)
        wr = sw / len(subset) * 100
        print(f"  buys_5s={buys}: {len(subset):>4} trades, {sw:>3} wins, {wr:>5.1f}% WR, {sp:>+8.4f} PnL")
    # Also 20+
    subset = [t for t in trades if t.get("pre_grad_buys_5s", 0) >= 20]
    if subset:
        sw = sum(1 for t in subset if is_win(t))
        sp = sum(t.get("net_pnl_sol", 0) for t in subset)
        wr = sw / len(subset) * 100
        print(f"  buys_5s>=20: {len(subset):>4} trades, {sw:>3} wins, {wr:>5.1f}% WR, {sp:>+8.4f} PnL")
    
    # ===== ws_notif_count analysis =====
    print("\n--- ws_notif_count_at_close ranges ---")
    ws_ranges = [(0, 5), (5, 50), (50, 500), (500, 5000), (5000, 50000), (50000, 999999)]
    for lo, hi in ws_ranges:
        subset = [t for t in trades if lo <= t.get("ws_notif_count_at_close", 0) < hi]
        if not subset:
            continue
        sw = sum(1 for t in subset if is_win(t))
        sp = sum(t.get("net_pnl_sol", 0) for t in subset)
        wr = sw / len(subset) * 100
        print(f"  [{lo:>6}-{hi:<6}): {len(subset):>4} trades, {sw:>3} wins, {wr:>5.1f}% WR, {sp:>+8.4f} PnL")
    
    # ===== price_sample_count analysis =====
    print("\n--- price_sample_count ranges ---")
    psc_ranges = [(1, 3), (3, 6), (6, 10), (10, 15), (15, 25), (25, 50), (50, 999)]
    for lo, hi in psc_ranges:
        subset = [t for t in trades if lo <= t.get("price_sample_count", 0) < hi]
        if not subset:
            continue
        sw = sum(1 for t in subset if is_win(t))
        sp = sum(t.get("net_pnl_sol", 0) for t in subset)
        wr = sw / len(subset) * 100
        print(f"  [{lo:>3}-{hi:<3}): {len(subset):>4} trades, {sw:>3} wins, {wr:>5.1f}% WR, {sp:>+8.4f} PnL")
    
    # ===== INTERACTION: score x pool x volume =====
    print()
    print("=" * 60)
    print("INTERACTION: Score x Pool x Volume")
    print("=" * 60)
    
    # High score + raydium + various volume thresholds
    for min_score in [55, 60, 65, 70]:
        for pool in [None, "raydium_amm_v4"]:
            for min_vol in [0, 50, 75, 100]:
                subset = [t for t in trades 
                         if t.get("grad_score", 0) >= min_score
                         and (pool is None or t.get("pool_type") == pool)
                         and t.get("grad_volume_sol", 0) >= min_vol]
                if len(subset) < 5:
                    continue
                sw = sum(1 for t in subset if is_win(t))
                sp = sum(t.get("net_pnl_sol", 0) for t in subset)
                wr = sw / len(subset) * 100
                ev = sp / len(subset)
                pool_str = "ray" if pool else "all"
                print(f"  score>={min_score} pool={pool_str:>3} vol>={min_vol:>3}: {len(subset):>4} trades, {sw:>3} wins, {wr:>5.1f}% WR, {sp:>+8.4f} PnL, EV={ev:>+8.5f}")
    
    # ===== INTERACTION: score x speed =====
    print()
    print("=" * 60)
    print("INTERACTION: Score x Speed")
    print("=" * 60)
    
    for min_score in [55, 60, 65]:
        for max_speed in [30, 60, 90, 120]:
            subset = [t for t in trades 
                     if t.get("grad_score", 0) >= min_score
                     and t.get("grad_speed_s", 0) <= max_speed]
            if len(subset) < 5:
                continue
            sw = sum(1 for t in subset if is_win(t))
            sp = sum(t.get("net_pnl_sol", 0) for t in subset)
            wr = sw / len(subset) * 100
            ev = sp / len(subset)
            print(f"  score>={min_score} speed<={max_speed:>3}s: {len(subset):>4} trades, {sw:>3} wins, {wr:>5.1f}% WR, {sp:>+8.4f} PnL, EV={ev:>+8.5f}")
    
    # ===== FIRST PRICE SAMPLE ANALYSIS =====
    print()
    print("=" * 60)
    print("FIRST N PRICE SAMPLES — Early Momentum Signal")
    print("=" * 60)
    
    # Analyze: how does the first 3 samples' trend predict outcome?
    for n in [2, 3, 5]:
        positive_start = []
        negative_start = []
        flat_start = []
        
        for t in trades:
            samples = t.get("price_samples_bps", [])
            if len(samples) < n:
                continue
            
            # Sum of first n samples (excluding index 0 which is always 0)
            early_trend = samples[min(n, len(samples)-1)] if len(samples) > n else samples[-1]
            
            if early_trend > 50:
                positive_start.append(t)
            elif early_trend < -50:
                negative_start.append(t)
            else:
                flat_start.append(t)
        
        print(f"\n  First {n} samples momentum (>50bps / flat / <-50bps):")
        for label, subset in [("Positive", positive_start), ("Flat", flat_start), ("Negative", negative_start)]:
            if not subset:
                continue
            sw = sum(1 for t in subset if is_win(t))
            sp = sum(t.get("net_pnl_sol", 0) for t in subset)
            wr = sw / len(subset) * 100
            print(f"    {label:>10}: {len(subset):>4} trades, {sw:>3} wins, {wr:>5.1f}% WR, {sp:>+8.4f} PnL")
    
    # ===== ULTIMATE COMBINED FILTER SCAN =====
    print()
    print("=" * 70)
    print("ULTIMATE COMBINED FILTER OPTIMIZATION (with re-entry filter)")
    print("=" * 70)
    
    negative_hours = set()
    hour_stats = defaultdict(lambda: {"count": 0, "wins": 0, "pnl": 0.0})
    for t in trades:
        ts_ms = t.get("entry_timestamp_ms", 0)
        if ts_ms > 0:
            dt = datetime.fromtimestamp(ts_ms / 1000, tz=timezone.utc)
            hour_stats[dt.hour]["count"] += 1
            hour_stats[dt.hour]["wins"] += 1 if is_win(t) else 0
            hour_stats[dt.hour]["pnl"] += t.get("net_pnl_sol", 0)
    
    for h in range(24):
        if h in hour_stats and hour_stats[h]["pnl"] < -0.02 and hour_stats[h]["count"] >= 10:
            negative_hours.add(h)
    
    # Track which mints have been seen (to limit re-entries)
    results = []
    
    for min_score in [50, 55, 60, 65, 70]:
        for pool in [None, "raydium_amm_v4"]:
            for use_tod in [False, True]:
                for max_reentries in [0, 1, 2, 3, 999]:
                    for min_vol in [0, 50, 75]:
                        sorted_t = sorted(trades, key=lambda x: x.get("entry_timestamp_ms", 0))
                        mint_counts = defaultdict(int)
                        subset = []
                        
                        for t in sorted_t:
                            if t.get("grad_score", 0) < min_score:
                                continue
                            if pool and t.get("pool_type") != pool:
                                continue
                            if t.get("grad_volume_sol", 0) < min_vol:
                                continue
                            if use_tod:
                                ts_ms = t.get("entry_timestamp_ms", 0)
                                if ts_ms > 0:
                                    dt = datetime.fromtimestamp(ts_ms / 1000, tz=timezone.utc)
                                    if dt.hour in negative_hours:
                                        continue
                            
                            mint = t.get("mint", "")
                            if mint_counts[mint] >= max_reentries + 1:  # +1 because first entry = 0 re-entries
                                continue
                            mint_counts[mint] += 1
                            subset.append(t)
                        
                        if len(subset) < 10:
                            continue
                        
                        sw = sum(1 for t in subset if is_win(t))
                        sp = sum(t.get("net_pnl_sol", 0) for t in subset)
                        wr = sw / len(subset) * 100
                        ev = sp / len(subset)
                        
                        # Only track interesting results
                        if wr >= 10 or ev >= 0.0005:
                            results.append({
                                "min_score": min_score,
                                "pool": "ray" if pool else "all",
                                "tod": use_tod,
                                "max_reentries": max_reentries,
                                "min_vol": min_vol,
                                "count": len(subset),
                                "wins": sw,
                                "wr": wr,
                                "pnl": sp,
                                "ev": ev
                            })
    
    # Sort by EV
    results.sort(key=lambda x: x["ev"], reverse=True)
    
    print(f"\nTop 30 filters by EV/trade:")
    print(f"{'Score':>5} {'Pool':>4} {'TOD':>4} {'ReEnt':>5} {'MinV':>5} {'N':>5} {'W':>4} {'WR':>6} {'PnL':>9} {'EV':>9}")
    print("-" * 70)
    for r in results[:30]:
        tod_str = "Y" if r["tod"] else "N"
        reent_str = str(r["max_reentries"]) if r["max_reentries"] < 999 else "∞"
        print(f"{r['min_score']:>5} {r['pool']:>4} {tod_str:>4} {reent_str:>5} {r['min_vol']:>5} {r['count']:>5} {r['wins']:>4} {r['wr']:>5.1f}% {r['pnl']:>+8.4f} {r['ev']:>+8.5f}")
    
    # Also sort by WR for perspective
    results.sort(key=lambda x: x["wr"], reverse=True)
    print(f"\nTop 20 filters by WR:")
    print(f"{'Score':>5} {'Pool':>4} {'TOD':>4} {'ReEnt':>5} {'MinV':>5} {'N':>5} {'W':>4} {'WR':>6} {'PnL':>9} {'EV':>9}")
    print("-" * 70)
    for r in results[:20]:
        tod_str = "Y" if r["tod"] else "N"
        reent_str = str(r["max_reentries"]) if r["max_reentries"] < 999 else "∞"
        print(f"{r['min_score']:>5} {r['pool']:>4} {tod_str:>4} {reent_str:>5} {r['min_vol']:>5} {r['count']:>5} {r['wins']:>4} {r['wr']:>5.1f}% {r['pnl']:>+8.4f} {r['ev']:>+8.5f}")
    
    # Sort by net PnL
    results.sort(key=lambda x: x["pnl"], reverse=True)
    print(f"\nTop 15 filters by total net PnL:")
    print(f"{'Score':>5} {'Pool':>4} {'TOD':>4} {'ReEnt':>5} {'MinV':>5} {'N':>5} {'W':>4} {'WR':>6} {'PnL':>9} {'EV':>9}")
    print("-" * 70)
    for r in results[:15]:
        tod_str = "Y" if r["tod"] else "N"
        reent_str = str(r["max_reentries"]) if r["max_reentries"] < 999 else "∞"
        print(f"{r['min_score']:>5} {r['pool']:>4} {tod_str:>4} {reent_str:>5} {r['min_vol']:>5} {r['count']:>5} {r['wins']:>4} {r['wr']:>5.1f}% {r['pnl']:>+8.4f} {r['ev']:>+8.5f}")

if __name__ == "__main__":
    main()
