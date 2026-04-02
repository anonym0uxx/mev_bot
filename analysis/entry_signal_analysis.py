#!/usr/bin/env python3
"""
Entry Signal Optimization Analysis for Pump.fun Momentum Strategy
Analyzes 776 paper trades to find optimal entry filters.
"""

import json
import sys
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

def analyze():
    trades = load_trades()
    print(f"=== ENTRY SIGNAL OPTIMIZATION ANALYSIS ===")
    print(f"Total trades: {len(trades)}")
    print()

    # ===== BASIC STATS =====
    wins = [t for t in trades if is_win(t)]
    losses = [t for t in trades if not is_win(t)]
    total_pnl = sum(t.get("net_pnl_sol", 0) for t in trades)
    print(f"Overall: {len(wins)} wins / {len(losses)} losses = {len(wins)/len(trades)*100:.1f}% WR")
    print(f"Total net PnL: {total_pnl:.4f} SOL")
    print()

    # ===== 1. GRADUATION SCORE THRESHOLD ANALYSIS =====
    print("=" * 60)
    print("1. GRADUATION SCORE THRESHOLD ANALYSIS")
    print("=" * 60)
    
    # Score buckets
    score_buckets = defaultdict(lambda: {"count": 0, "wins": 0, "pnl": 0.0, "trades": []})
    for t in trades:
        score = t.get("grad_score", 0)
        bucket = (score // 5) * 5  # 5-point buckets
        score_buckets[bucket]["count"] += 1
        score_buckets[bucket]["wins"] += 1 if is_win(t) else 0
        score_buckets[bucket]["pnl"] += t.get("net_pnl_sol", 0)
        score_buckets[bucket]["trades"].append(t)
    
    print(f"\n{'Score':>8} {'Count':>6} {'Wins':>5} {'WR':>7} {'Net PnL':>10} {'Avg PnL':>10}")
    print("-" * 55)
    for bucket in sorted(score_buckets.keys()):
        d = score_buckets[bucket]
        wr = d["wins"] / d["count"] * 100 if d["count"] > 0 else 0
        avg = d["pnl"] / d["count"] if d["count"] > 0 else 0
        print(f"{bucket:>5}-{bucket+4:<3} {d['count']:>6} {d['wins']:>5} {wr:>6.1f}% {d['pnl']:>+10.4f} {avg:>+10.5f}")
    
    # Cumulative from threshold upward
    print(f"\n--- Cumulative: Score >= Threshold ---")
    print(f"{'Threshold':>10} {'Count':>6} {'Wins':>5} {'WR':>7} {'Net PnL':>10} {'EV/Trade':>10}")
    print("-" * 60)
    for threshold in range(20, 85, 5):
        subset = [t for t in trades if t.get("grad_score", 0) >= threshold]
        if not subset:
            continue
        sw = sum(1 for t in subset if is_win(t))
        sp = sum(t.get("net_pnl_sol", 0) for t in subset)
        wr = sw / len(subset) * 100
        ev = sp / len(subset)
        print(f"{'>=' + str(threshold):>10} {len(subset):>6} {sw:>5} {wr:>6.1f}% {sp:>+10.4f} {ev:>+10.5f}")
    
    # ===== 2. POOL TYPE ANALYSIS =====
    print()
    print("=" * 60)
    print("2. POOL TYPE ANALYSIS")
    print("=" * 60)
    
    pool_stats = defaultdict(lambda: {"count": 0, "wins": 0, "pnl": 0.0})
    for t in trades:
        pool = t.get("pool_type", "unknown")
        pool_stats[pool]["count"] += 1
        pool_stats[pool]["wins"] += 1 if is_win(t) else 0
        pool_stats[pool]["pnl"] += t.get("net_pnl_sol", 0)
    
    print(f"\n{'Pool':>20} {'Count':>6} {'Wins':>5} {'WR':>7} {'Net PnL':>10} {'EV/Trade':>10}")
    print("-" * 65)
    for pool in sorted(pool_stats.keys()):
        d = pool_stats[pool]
        wr = d["wins"] / d["count"] * 100
        ev = d["pnl"] / d["count"]
        print(f"{pool:>20} {d['count']:>6} {d['wins']:>5} {wr:>6.1f}% {d['pnl']:>+10.4f} {ev:>+10.5f}")
    
    # Cross: Pool x Score
    print(f"\n--- Pool x Score Cross Analysis ---")
    for pool in sorted(pool_stats.keys()):
        print(f"\n  {pool}:")
        print(f"  {'Score':>8} {'Count':>6} {'Wins':>5} {'WR':>7} {'Net PnL':>10}")
        print(f"  " + "-" * 45)
        for threshold in [30, 40, 50, 55, 60, 65, 70]:
            subset = [t for t in trades if t.get("pool_type") == pool and t.get("grad_score", 0) >= threshold]
            if not subset:
                continue
            sw = sum(1 for t in subset if is_win(t))
            sp = sum(t.get("net_pnl_sol", 0) for t in subset)
            wr = sw / len(subset) * 100
            print(f"  {'>=' + str(threshold):>8} {len(subset):>6} {sw:>5} {wr:>6.1f}% {sp:>+10.4f}")
    
    # ===== 3. TOKEN RE-ENTRY ANALYSIS (7dpaUoCb) =====
    print()
    print("=" * 60)
    print("3. TOKEN RE-ENTRY ANALYSIS")
    print("=" * 60)
    
    mint_stats = defaultdict(lambda: {"count": 0, "wins": 0, "pnl": 0.0, "trades": []})
    for t in trades:
        mint = t.get("mint", "")
        short = mint[:8]
        mint_stats[short]["count"] += 1
        mint_stats[short]["wins"] += 1 if is_win(t) else 0
        mint_stats[short]["pnl"] += t.get("net_pnl_sol", 0)
        mint_stats[short]["trades"].append(t)
    
    # Top re-entered tokens
    re_entries = [(k, v) for k, v in mint_stats.items() if v["count"] > 1]
    re_entries.sort(key=lambda x: x[1]["count"], reverse=True)
    
    print(f"\nTokens with >1 trade:")
    print(f"{'Mint':>12} {'Count':>6} {'Wins':>5} {'WR':>7} {'Net PnL':>10}")
    print("-" * 50)
    for mint, d in re_entries[:20]:
        wr = d["wins"] / d["count"] * 100
        print(f"{mint:>12} {d['count']:>6} {d['wins']:>5} {wr:>6.1f}% {d['pnl']:>+10.4f}")
    
    # Overall: re-entry vs first-entry trades
    first_entry_trades = []
    re_entry_trades = []
    seen_mints = set()
    # Sort by timestamp to identify first vs re-entry
    sorted_trades = sorted(trades, key=lambda t: t.get("entry_timestamp_ms", 0))
    for t in sorted_trades:
        mint = t.get("mint", "")
        if mint not in seen_mints:
            first_entry_trades.append(t)
            seen_mints.add(mint)
        else:
            re_entry_trades.append(t)
    
    for label, subset in [("First entry", first_entry_trades), ("Re-entry", re_entry_trades)]:
        if not subset:
            continue
        sw = sum(1 for t in subset if is_win(t))
        sp = sum(t.get("net_pnl_sol", 0) for t in subset)
        wr = sw / len(subset) * 100
        print(f"\n{label}: {len(subset)} trades, {sw} wins, {wr:.1f}% WR, {sp:+.4f} PnL")
    
    # Specifically for the 7dpaUoCb token
    target_mints = [k for k in mint_stats if k.startswith("7dpaUoCb")]
    if target_mints:
        for tm in target_mints:
            d = mint_stats[tm]
            print(f"\n7dpaUoCb specifically: {d['count']} trades, {d['wins']} wins, {d['pnl']:+.4f} PnL")
            # Analyze by trade sequence
            for i, t in enumerate(d["trades"]):
                w = "W" if is_win(t) else "L"
                print(f"  Trade {i+1}: {w} {t.get('net_pnl_sol', 0):+.5f} | exit={t.get('exit_reason')} | hold={t.get('hold_ms', 0)}ms | gain={t.get('raw_gain_bps', 0)}bps")
    
    # ===== 4. SIGNAL ANALYSIS =====
    print()
    print("=" * 60)
    print("4. ENTRY SIGNAL PREDICTOR ANALYSIS")
    print("=" * 60)
    
    signals = ["grad_score", "grad_speed_s", "grad_volume_sol", "pre_grad_buys_5s", 
               "structural_discount_pct", "bc_terminal_price_fp"]
    
    for sig in signals:
        vals_win = [t.get(sig, 0) for t in wins if t.get(sig) is not None]
        vals_loss = [t.get(sig, 0) for t in losses if t.get(sig) is not None]
        
        if not vals_win or not vals_loss:
            continue
        
        avg_w = sum(vals_win) / len(vals_win)
        avg_l = sum(vals_loss) / len(vals_loss)
        med_w = sorted(vals_win)[len(vals_win)//2]
        med_l = sorted(vals_loss)[len(vals_loss)//2]
        min_w = min(vals_win)
        max_w = max(vals_win)
        min_l = min(vals_loss)
        max_l = max(vals_loss)
        
        print(f"\n{sig}:")
        print(f"  WINS   (n={len(vals_win):>3}): avg={avg_w:>10.2f}  med={med_w:>10.2f}  min={min_w:>10.2f}  max={max_w:>10.2f}")
        print(f"  LOSSES (n={len(vals_loss):>3}): avg={avg_l:>10.2f}  med={med_l:>10.2f}  min={min_l:>10.2f}  max={max_l:>10.2f}")
        
        # Distribution for key signals
        if sig in ["grad_score", "grad_speed_s", "grad_volume_sol"]:
            # Quartile analysis
            vals_all = [(t.get(sig, 0), is_win(t), t.get("net_pnl_sol", 0)) for t in trades if t.get(sig) is not None]
            vals_all.sort(key=lambda x: x[0])
            q_size = len(vals_all) // 4
            for qi in range(4):
                start = qi * q_size
                end = (qi + 1) * q_size if qi < 3 else len(vals_all)
                q_trades = vals_all[start:end]
                q_wins = sum(1 for v, w, p in q_trades if w)
                q_pnl = sum(p for v, w, p in q_trades)
                q_wr = q_wins / len(q_trades) * 100 if q_trades else 0
                vmin = q_trades[0][0]
                vmax = q_trades[-1][0]
                print(f"  Q{qi+1} [{vmin:.1f}-{vmax:.1f}]: {len(q_trades)} trades, {q_wr:.1f}% WR, {q_pnl:+.4f} PnL")
    
    # ===== 4b. structural_discount_pct deep dive =====
    print(f"\nstructural_discount_pct ranges:")
    disc_ranges = [(-200, -50), (-50, 0), (0, 25), (25, 50), (50, 75), (75, 100)]
    for lo, hi in disc_ranges:
        subset = [t for t in trades if lo <= t.get("structural_discount_pct", -999) < hi]
        if not subset:
            continue
        sw = sum(1 for t in subset if is_win(t))
        sp = sum(t.get("net_pnl_sol", 0) for t in subset)
        wr = sw / len(subset) * 100
        print(f"  [{lo:>4},{hi:>4}): {len(subset):>4} trades, {sw:>3} wins, {wr:>5.1f}% WR, {sp:>+10.4f} PnL")
    
    # Also check extreme negatives (price above terminal)
    extreme_neg = [t for t in trades if t.get("structural_discount_pct", 0) < -200]
    if extreme_neg:
        sw = sum(1 for t in extreme_neg if is_win(t))
        sp = sum(t.get("net_pnl_sol", 0) for t in extreme_neg)
        wr = sw / len(extreme_neg) * 100
        print(f"  [< -200]:   {len(extreme_neg):>4} trades, {sw:>3} wins, {wr:>5.1f}% WR, {sp:>+10.4f} PnL")
    
    # ===== 5. BACKTEST: Score >= X AND Raydium =====
    print()
    print("=" * 60)
    print("5. BACKTEST: Score Threshold + Raydium Filter")
    print("=" * 60)
    
    print(f"\n{'Filter':>30} {'Count':>6} {'Wins':>5} {'WR':>7} {'Net PnL':>10} {'EV/Trade':>10} {'Max DD':>8}")
    print("-" * 85)
    
    for threshold in range(30, 80, 5):
        for pool_filter in [None, "raydium_amm_v4"]:
            subset = [t for t in trades if t.get("grad_score", 0) >= threshold]
            if pool_filter:
                subset = [t for t in subset if t.get("pool_type") == pool_filter]
            
            if not subset:
                continue
            
            sw = sum(1 for t in subset if is_win(t))
            sp = sum(t.get("net_pnl_sol", 0) for t in subset)
            wr = sw / len(subset) * 100
            ev = sp / len(subset)
            
            # Calculate max drawdown
            running = 0.0
            peak = 0.0
            max_dd = 0.0
            for t in sorted(subset, key=lambda x: x.get("entry_timestamp_ms", 0)):
                running += t.get("net_pnl_sol", 0)
                if running > peak:
                    peak = running
                dd = peak - running
                if dd > max_dd:
                    max_dd = dd
            
            label = f"score>={threshold}"
            if pool_filter:
                label += "+raydium"
            print(f"{label:>30} {len(subset):>6} {sw:>5} {wr:>6.1f}% {sp:>+10.4f} {ev:>+10.5f} {max_dd:>8.4f}")
    
    # ===== 6. TIME OF DAY ANALYSIS =====
    print()
    print("=" * 60)
    print("6. TIME OF DAY ANALYSIS (UTC)")
    print("=" * 60)
    
    hour_stats = defaultdict(lambda: {"count": 0, "wins": 0, "pnl": 0.0})
    for t in trades:
        ts_ms = t.get("entry_timestamp_ms", 0)
        if ts_ms > 0:
            dt = datetime.fromtimestamp(ts_ms / 1000, tz=timezone.utc)
            hour = dt.hour
            hour_stats[hour]["count"] += 1
            hour_stats[hour]["wins"] += 1 if is_win(t) else 0
            hour_stats[hour]["pnl"] += t.get("net_pnl_sol", 0)
    
    print(f"\n{'Hour UTC':>10} {'Count':>6} {'Wins':>5} {'WR':>7} {'Net PnL':>10} {'EV/Trade':>10}")
    print("-" * 55)
    for hour in range(24):
        if hour not in hour_stats:
            continue
        d = hour_stats[hour]
        wr = d["wins"] / d["count"] * 100 if d["count"] > 0 else 0
        ev = d["pnl"] / d["count"] if d["count"] > 0 else 0
        flag = " ❌" if d["pnl"] < -0.01 else (" ✅" if d["pnl"] > 0.01 else "")
        print(f"{hour:>7}:00 {d['count']:>6} {d['wins']:>5} {wr:>6.1f}% {d['pnl']:>+10.4f} {ev:>+10.5f}{flag}")
    
    # Also by PDT (UTC-7)
    print(f"\n--- Time of Day (PDT = UTC-7) ---")
    pdt_stats = defaultdict(lambda: {"count": 0, "wins": 0, "pnl": 0.0})
    for t in trades:
        ts_ms = t.get("entry_timestamp_ms", 0)
        if ts_ms > 0:
            dt = datetime.fromtimestamp(ts_ms / 1000, tz=timezone.utc)
            pdt_hour = (dt.hour - 7) % 24
            pdt_stats[pdt_hour]["count"] += 1
            pdt_stats[pdt_hour]["wins"] += 1 if is_win(t) else 0
            pdt_stats[pdt_hour]["pnl"] += t.get("net_pnl_sol", 0)
    
    print(f"{'Hour PDT':>10} {'Count':>6} {'Wins':>5} {'WR':>7} {'Net PnL':>10} {'EV/Trade':>10}")
    print("-" * 55)
    for hour in range(24):
        if hour not in pdt_stats:
            continue
        d = pdt_stats[hour]
        wr = d["wins"] / d["count"] * 100 if d["count"] > 0 else 0
        ev = d["pnl"] / d["count"] if d["count"] > 0 else 0
        flag = " ❌" if d["pnl"] < -0.01 else (" ✅" if d["pnl"] > 0.01 else "")
        print(f"{hour:>7}:00 {d['count']:>6} {d['wins']:>5} {wr:>6.1f}% {d['pnl']:>+10.4f} {ev:>+10.5f}{flag}")
    
    # ===== 7. COMBINED OPTIMAL FILTER =====
    print()
    print("=" * 60)
    print("7. COMBINED FILTER OPTIMIZATION")
    print("=" * 60)
    
    # Test combinations
    best_ev = -999
    best_config = None
    results = []
    
    # Negative hours (UTC)
    negative_hours = set()
    for h in range(24):
        if h in hour_stats and hour_stats[h]["pnl"] < -0.02 and hour_stats[h]["count"] >= 10:
            negative_hours.add(h)
    
    print(f"\nIdentified negative hours (UTC): {sorted(negative_hours)}")
    
    for min_score in range(40, 75, 5):
        for pool_filter in [None, "raydium_amm_v4"]:
            for use_tod in [False, True]:
                subset = []
                for t in trades:
                    if t.get("grad_score", 0) < min_score:
                        continue
                    if pool_filter and t.get("pool_type") != pool_filter:
                        continue
                    if use_tod:
                        ts_ms = t.get("entry_timestamp_ms", 0)
                        if ts_ms > 0:
                            dt = datetime.fromtimestamp(ts_ms / 1000, tz=timezone.utc)
                            if dt.hour in negative_hours:
                                continue
                    subset.append(t)
                
                if len(subset) < 10:
                    continue
                
                sw = sum(1 for t in subset if is_win(t))
                sp = sum(t.get("net_pnl_sol", 0) for t in subset)
                wr = sw / len(subset) * 100
                ev = sp / len(subset)
                
                # Max drawdown
                running = 0.0
                peak = 0.0
                max_dd = 0.0
                for t in sorted(subset, key=lambda x: x.get("entry_timestamp_ms", 0)):
                    running += t.get("net_pnl_sol", 0)
                    if running > peak:
                        peak = running
                    dd = peak - running
                    if dd > max_dd:
                        max_dd = dd
                
                result = {
                    "min_score": min_score,
                    "pool": pool_filter or "all",
                    "tod_filter": use_tod,
                    "count": len(subset),
                    "wins": sw,
                    "wr": wr,
                    "pnl": sp,
                    "ev": ev,
                    "max_dd": max_dd
                }
                results.append(result)
    
    # Sort by EV per trade descending
    results.sort(key=lambda x: x["ev"], reverse=True)
    
    print(f"\n{'Score':>6} {'Pool':>10} {'TOD':>5} {'Count':>6} {'Wins':>5} {'WR':>7} {'Net PnL':>10} {'EV/Trade':>10} {'MaxDD':>8}")
    print("-" * 80)
    for r in results[:25]:
        tod_str = "yes" if r["tod_filter"] else "no"
        pool_str = "ray" if r["pool"] == "raydium_amm_v4" else "all"
        print(f"{r['min_score']:>6} {pool_str:>10} {tod_str:>5} {r['count']:>6} {r['wins']:>5} {r['wr']:>6.1f}% {r['pnl']:>+10.4f} {r['ev']:>+10.5f} {r['max_dd']:>8.4f}")
    
    # ===== 8. EXIT REASON BREAKDOWN BY FILTER =====
    print()
    print("=" * 60)
    print("8. EXIT REASON ANALYSIS BY OPTIMAL FILTER")
    print("=" * 60)
    
    # Use top-performing filter
    if results:
        best = results[0]
        print(f"\nBest filter: score>={best['min_score']}, pool={best['pool']}, tod={best['tod_filter']}")
        
        # Get the filtered trades
        subset = []
        for t in trades:
            if t.get("grad_score", 0) < best["min_score"]:
                continue
            if best["pool"] == "raydium_amm_v4" and t.get("pool_type") != "raydium_amm_v4":
                continue
            if best["tod_filter"]:
                ts_ms = t.get("entry_timestamp_ms", 0)
                if ts_ms > 0:
                    dt = datetime.fromtimestamp(ts_ms / 1000, tz=timezone.utc)
                    if dt.hour in negative_hours:
                        continue
            subset.append(t)
        
        exit_stats = defaultdict(lambda: {"count": 0, "wins": 0, "pnl": 0.0, "avg_hold": 0.0})
        for t in subset:
            reason = t.get("exit_reason", "unknown")
            exit_stats[reason]["count"] += 1
            exit_stats[reason]["wins"] += 1 if is_win(t) else 0
            exit_stats[reason]["pnl"] += t.get("net_pnl_sol", 0)
            exit_stats[reason]["avg_hold"] += t.get("hold_ms", 0)
        
        print(f"\n{'Exit Reason':>20} {'Count':>6} {'Wins':>5} {'WR':>7} {'Net PnL':>10} {'Avg Hold ms':>12}")
        print("-" * 70)
        for reason in sorted(exit_stats.keys()):
            d = exit_stats[reason]
            wr = d["wins"] / d["count"] * 100
            avg_hold = d["avg_hold"] / d["count"]
            print(f"{reason:>20} {d['count']:>6} {d['wins']:>5} {wr:>6.1f}% {d['pnl']:>+10.4f} {avg_hold:>12.0f}")
    
    # ===== 9. SIZE/POSITION IMPACT =====
    print()
    print("=" * 60)
    print("9. SIZE/CONFIG VERSION ANALYSIS")
    print("=" * 60)
    
    size_stats = defaultdict(lambda: {"count": 0, "wins": 0, "pnl": 0.0})
    for t in trades:
        size = t.get("size_sol", 0)
        size_bucket = f"{size:.2f}"
        size_stats[size_bucket]["count"] += 1
        size_stats[size_bucket]["wins"] += 1 if is_win(t) else 0
        size_stats[size_bucket]["pnl"] += t.get("net_pnl_sol", 0)
    
    print(f"\n{'Size SOL':>10} {'Count':>6} {'Wins':>5} {'WR':>7} {'Net PnL':>10}")
    print("-" * 45)
    for size in sorted(size_stats.keys()):
        d = size_stats[size]
        wr = d["wins"] / d["count"] * 100
        print(f"{size:>10} {d['count']:>6} {d['wins']:>5} {wr:>6.1f}% {d['pnl']:>+10.4f}")
    
    # Config version
    version_stats = defaultdict(lambda: {"count": 0, "wins": 0, "pnl": 0.0})
    for t in trades:
        ver = t.get("config_version", "unknown")
        version_stats[ver]["count"] += 1
        version_stats[ver]["wins"] += 1 if is_win(t) else 0
        version_stats[ver]["pnl"] += t.get("net_pnl_sol", 0)
    
    print(f"\n{'Config Version':>30} {'Count':>6} {'Wins':>5} {'WR':>7} {'Net PnL':>10}")
    print("-" * 65)
    for ver in sorted(version_stats.keys()):
        d = version_stats[ver]
        wr = d["wins"] / d["count"] * 100
        print(f"{ver:>30} {d['count']:>6} {d['wins']:>5} {wr:>6.1f}% {d['pnl']:>+10.4f}")
    
    # ===== 10. HOLD TIME vs OUTCOME =====
    print()
    print("=" * 60)
    print("10. HOLD TIME ANALYSIS")
    print("=" * 60)
    
    hold_buckets = [(0, 1000), (1000, 3000), (3000, 5000), (5000, 8000), (8000, 10000), 
                     (10000, 15000), (15000, 30000), (30000, 60000), (60000, 120000), (120000, 999999)]
    
    print(f"\n{'Hold Range':>18} {'Count':>6} {'Wins':>5} {'WR':>7} {'Net PnL':>10}")
    print("-" * 50)
    for lo, hi in hold_buckets:
        subset = [t for t in trades if lo <= t.get("hold_ms", 0) < hi]
        if not subset:
            continue
        sw = sum(1 for t in subset if is_win(t))
        sp = sum(t.get("net_pnl_sol", 0) for t in subset)
        wr = sw / len(subset) * 100
        print(f"{lo:>7}-{hi:<7}ms {len(subset):>6} {sw:>5} {wr:>6.1f}% {sp:>+10.4f}")
    
    # ===== 11. MULTI-SIGNAL