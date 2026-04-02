import json, sys, math, random
from collections import defaultdict

# Load trades
with open('data/momentum_paper_trades.jsonl') as f:
    trades = [json.loads(l) for l in f if l.strip()]

print(f"=== FULL ANALYSIS OF {len(trades)} TRADES ===\n")

# ── ENTRY SIGNAL ANALYSIS ──
print("=" * 60)
print("SECTION 1: ENTRY SIGNAL ANALYSIS")
print("=" * 60)

# Score bracket analysis
brackets = defaultdict(list)
for t in trades:
    s = t.get('grad_score', 0)
    bracket = (s // 10) * 10
    brackets[bracket].append(t)

print("\nScore Bracket | Count | WR    | Net PnL    | Avg Win    | Avg Loss   | E[V]")
print("-" * 90)
for bracket in sorted(brackets.keys()):
    ts = brackets[bracket]
    n = len(ts)
    wins = [t for t in ts if t.get('net_pnl_sol', 0) > 0]
    losses = [t for t in ts if t.get('net_pnl_sol', 0) <= 0]
    wr = len(wins) / n * 100 if n else 0
    net = sum(t.get('net_pnl_sol', 0) for t in ts)
    avg_win = sum(t['net_pnl_sol'] for t in wins) / len(wins) if wins else 0
    avg_loss = sum(t['net_pnl_sol'] for t in losses) / len(losses) if losses else 0
    ev = net / n if n else 0
    print(f"  {bracket:3d}-{bracket+9:3d}   | {n:5d} | {wr:5.1f}% | {net:+10.6f} | {avg_win:+10.6f} | {avg_loss:+10.6f} | {ev:+10.6f}")

# Cumulative score filter
print("\nCumulative score filter (trades with score >= X):")
print("Min Score | Count | WR    | Net PnL    | E[V]")
print("-" * 60)
for min_s in range(30, 85, 5):
    ts = [t for t in trades if t.get('grad_score', 0) >= min_s]
    if not ts: continue
    n = len(ts)
    wins = [t for t in ts if t.get('net_pnl_sol', 0) > 0]
    wr = len(wins) / n * 100
    net = sum(t.get('net_pnl_sol', 0) for t in ts)
    ev = net / n
    print(f"  >= {min_s:3d}   | {n:5d} | {wr:5.1f}% | {net:+10.6f} | {ev:+10.6f}")

# Pool type analysis
print("\nPool type breakdown:")
pools = defaultdict(list)
for t in trades:
    pools[t.get('pool_type', 'unknown')].append(t)
for pool, ts in pools.items():
    n = len(ts)
    wins = [t for t in ts if t.get('net_pnl_sol', 0) > 0]
    wr = len(wins) / n * 100
    net = sum(t.get('net_pnl_sol', 0) for t in ts)
    print(f"  {pool}: n={n} WR={wr:.1f}% net={net:+.6f}")

# Combined filter: score + pool
print("\nCombined filter (score >= X AND pool_type):")
for min_s in [50, 55, 60, 65, 70]:
    for pool in ['raydium_amm_v4', 'pump_swap']:
        ts = [t for t in trades if t.get('grad_score', 0) >= min_s and t.get('pool_type') == pool]
        if not ts: continue
        n = len(ts)
        wins = [t for t in ts if t.get('net_pnl_sol', 0) > 0]
        wr = len(wins) / n * 100
        net = sum(t.get('net_pnl_sol', 0) for t in ts)
        print(f"  score>={min_s} + {pool[:8]}: n={n} WR={wr:.1f}% net={net:+.6f} EV={net/n:+.6f}")

# Re-entry analysis (same mint traded multiple times)
print("\nRe-entry analysis (tokens traded 3+ times):")
mints = defaultdict(list)
for t in trades:
    mints[t.get('mint', '')[:8]].append(t)
for mint, ts in sorted(mints.items(), key=lambda x: -len(x[1])):
    if len(ts) < 3: continue
    n = len(ts)
    wins = [t for t in ts if t.get('net_pnl_sol', 0) > 0]
    wr = len(wins) / n * 100
    net = sum(t.get('net_pnl_sol', 0) for t in ts)
    print(f"  {mint}: n={n} WR={wr:.1f}% net={net:+.6f}")

# Volume analysis
print("\nVolume bracket analysis:")
for lo, hi in [(0,50),(50,100),(100,200),(200,500),(500,9999)]:
    ts = [t for t in trades if lo <= t.get('grad_volume_sol', 0) < hi]
    if not ts: continue
    n = len(ts)
    wins = [t for t in ts if t.get('net_pnl_sol', 0) > 0]
    wr = len(wins) / n * 100
    net = sum(t.get('net_pnl_sol', 0) for t in ts)
    print(f"  vol {lo}-{hi} SOL: n={n} WR={wr:.1f}% net={net:+.6f} EV={net/n:+.6f}")

# Speed analysis
print("\nGrad speed analysis:")
for lo, hi in [(0,30),(30,60),(60,120),(120,300),(300,9999)]:
    ts = [t for t in trades if lo <= t.get('grad_speed_s', 0) < hi]
    if not ts: continue
    n = len(ts)
    wins = [t for t in ts if t.get('net_pnl_sol', 0) > 0]
    wr = len(wins) / n * 100
    net = sum(t.get('net_pnl_sol', 0) for t in ts)
    print(f"  speed {lo}-{hi}s: n={n} WR={wr:.1f}% net={net:+.6f} EV={net/n:+.6f}")

# ── EXIT STRATEGY ANALYSIS ──
print("\n" + "=" * 60)
print("SECTION 2: EXIT STRATEGY ANALYSIS")
print("=" * 60)

# Price trajectory for first 10 samples
print("\nPrice trajectory by exit reason (first 15 samples, bps):")
for reason in ['trailing_stop', 'time_sl', 'hard_sl']:
    group = [t for t in trades if t.get('exit_reason') == reason]
    if not group: continue
    max_len = max(len(t.get('price_samples_bps', [])) for t in group)
    max_len = min(max_len, 15)
    avgs = []
    for i in range(max_len):
        vals = [t['price_samples_bps'][i] for t in group if i < len(t.get('price_samples_bps', []))]
        avgs.append(sum(vals) / len(vals) if vals else 0)
    print(f"  {reason}: {[round(a) for a in avgs]}")

# Dead zone analysis
print("\nDead zone analysis (time_sl trades with gain=0bps):")
flat_trades = [t for t in trades if t.get('exit_reason') == 'time_sl' and abs(t.get('raw_gain_bps', 0)) <= 10]
print(f"  Flat trades (|gain| <= 10bps): {len(flat_trades)} / {len([t for t in trades if t.get('exit_reason') == 'time_sl'])} time_sl trades")
print(f"  Avg hold: {sum(t.get('hold_ms', 0) for t in flat_trades) / len(flat_trades) / 1000:.1f}s" if flat_trades else "  No flat trades")

# Hold time vs outcome
print("\nHold time vs outcome:")
for lo, hi in [(0,2000),(2000,5000),(5000,10000),(10000,30000),(30000,60000),(60000,300000),(300000,999999)]:
    ts = [t for t in trades if lo <= t.get('hold_ms', 0) < hi]
    if not ts: continue
    n = len(ts)
    wins = [t for t in ts if t.get('net_pnl_sol', 0) > 0]
    wr = len(wins) / n * 100
    net = sum(t.get('net_pnl_sol', 0) for t in ts)
    print(f"  {lo/1000:.0f}-{hi/1000:.0f}s: n={n} WR={wr:.1f}% net={net:+.6f}")

# Trailing stop analysis - what was the peak gain before exit?
print("\nTrailing stop winners - peak gain analysis:")
ts_winners = [t for t in trades if t.get('exit_reason') == 'trailing_stop' and t.get('net_pnl_sol', 0) > 0]
for t in ts_winners[:10]:
    samples = t.get('price_samples_bps', [])
    peak = max(samples) if samples else 0
    exit_bps = t.get('raw_gain_bps', 0)
    print(f"  {t.get('mint','')[:8]}: peak={peak}bps exit={exit_bps}bps hold={t.get('hold_ms',0)/1000:.1f}s pnl={t.get('net_pnl_sol',0):+.5f}")

# ── KELLY & MONTE CARLO ──
print("\n" + "=" * 60)
print("SECTION 3: KELLY CRITERION & MONTE CARLO")
print("=" * 60)

# Overall Kelly
pnls = [t.get('net_pnl_sol', 0) for t in trades]
sizes = [t.get('size_sol', 0.03) for t in trades]
returns = [p / s if s > 0 else 0 for p, s in zip(pnls, sizes)]
wins_r = [r for r in returns if r > 0]
losses_r = [r for r in returns if r <= 0]
wr = len(wins_r) / len(returns) if returns else 0
avg_win_r = sum(wins_r) / len(wins_r) if wins_r else 0
avg_loss_r = abs(sum(losses_r) / len(losses_r)) if losses_r else 0

if avg_loss_r > 0:
    kelly_f = wr - (1 - wr) / (avg_win_r / avg_loss_r)
else:
    kelly_f = 0
kelly_quarter = kelly_f * 0.25

print(f"\nOverall Kelly:")
print(f"  WR: {wr*100:.1f}%")
print(f"  Avg win return: {avg_win_r*100:.2f}%")
print(f"  Avg loss return: {avg_loss_r*100:.2f}%")
print(f"  W/L ratio: {avg_win_r/avg_loss_r:.2f}x" if avg_loss_r > 0 else "  W/L ratio: inf")
print(f"  Full Kelly f*: {kelly_f:.4f}")
print(f"  Quarter Kelly: {kelly_quarter:.4f}")
print(f"  Quarter Kelly size @ 1.5 SOL: {1.5 * kelly_quarter:.4f} SOL")

# Kelly by score bracket
print("\nKelly by score bracket:")
for bracket in sorted(brackets.keys()):
    ts = brackets[bracket]
    pnls_b = [t.get('net_pnl_sol', 0) for t in ts]
    sizes_b = [t.get('size_sol', 0.03) for t in ts]
    returns_b = [p / s if s > 0 else 0 for p, s in zip(pnls_b, sizes_b)]
    wins_b = [r for r in returns_b if r > 0]
    losses_b = [r for r in returns_b if r <= 0]
    wr_b = len(wins_b) / len(returns_b) if returns_b else 0
    avg_w = sum(wins_b) / len(wins_b) if wins_b else 0
    avg_l = abs(sum(losses_b) / len(losses_b)) if losses_b else 0
    if avg_l > 0:
        k = wr_b - (1 - wr_b) / (avg_w / avg_l)
    else:
        k = 0
    print(f"  {bracket}-{bracket+9}: WR={wr_b*100:.1f}% kelly={k:.4f} qKelly={k*0.25:.4f} size={1.5*k*0.25:.4f} SOL")

# Monte Carlo
print("\nMonte Carlo Simulation (10,000 paths, 1000 trades each):")
random.seed(42)
starting_balance = 1.5
n_paths = 10000
n_trades_mc = 1000

final_balances = []
max_drawdowns = []
ruins = 0  # < 0.2 SOL

for _ in range(n_paths):
    balance = starting_balance
    peak = balance
    max_dd = 0
    
    for _ in range(n_trades_mc):
        # Draw a random trade
        trade = random.choice(trades)
        pnl = trade.get('net_pnl_sol', 0)
        size = trade.get('size_sol', 0.03)
        
        # Scale PnL proportionally to current position size (0.03 SOL probe)
        balance += pnl
        
        if balance > peak:
            peak = balance
        dd = (peak - balance) / peak if peak > 0 else 0
        if dd > max_dd:
            max_dd = dd
            
        if balance < 0.2:
            ruins += 1
            break
    
    final_balances.append(balance)
    max_drawdowns.append(max_dd)

final_balances.sort()
max_drawdowns.sort()

print(f"  P(ruin < 0.2 SOL): {ruins/n_paths*100:.2f}%")
print(f"  Mean final balance: {sum(final_balances)/len(final_balances):.4f} SOL")
print(f"  Median final balance: {final_balances[len(final_balances)//2]:.4f} SOL")
print(f"  5th percentile: {final_balances[int(n_paths*0.05)]:.4f} SOL")
print(f"  95th percentile: {final_balances[int(n_paths*0.95)]:.4f} SOL")
print(f"  Mean max drawdown: {sum(max_drawdowns)/len(max_drawdowns)*100:.1f}%")
print(f"  95th percentile max DD: {max_drawdowns[int(n_paths*0.95)]*100:.1f}%")
print(f"  Worst case balance: {final_balances[0]:.4f} SOL")
print(f"  Best case balance: {final_balances[-1]:.4f} SOL")

# Monte Carlo with score >= 60 filter
print("\nMonte Carlo with score >= 60 filter:")
filtered_trades = [t for t in trades if t.get('grad_score', 0) >= 60]
print(f"  Filtered trades pool: {len(filtered_trades)}")

if filtered_trades:
    final_f = []
    ruins_f = 0
    max_dds_f = []
    for _ in range(n_paths):
        balance = starting_balance
        peak = balance
        max_dd = 0
        for _ in range(n_trades_mc):
            trade = random.choice(filtered_trades)
            pnl = trade.get('net_pnl_sol', 0)
            balance += pnl
            if balance > peak: peak = balance
            dd = (peak - balance) / peak if peak > 0 else 0
            if dd > max_dd: max_dd = dd
            if balance < 0.2:
                ruins_f += 1
                break
        final_f.append(balance)
        max_dds_f.append(max_dd)
    
    final_f.sort()
    max_dds_f.sort()
    print(f"  P(ruin): {ruins_f/n_paths*100:.2f}%")
    print(f"  Mean final: {sum(final_f)/len(final_f):.4f} SOL")
    print(f"  Median final: {final_f[len(final_f)//2]:.4f} SOL")
    print(f"  5th pct: {final_f[int(n_paths*0.05)]:.4f} SOL")
    print(f"  95th pct: {final_f[int(n_paths*0.95)]:.4f} SOL")
    print(f"  Mean max DD: {sum(max_dds_f)/len(max_dds_f)*100:.1f}%")

# TOD analysis
print("\n\nTime of Day (UTC):")
hours = defaultdict(list)
for t in trades:
    from datetime import datetime
    h = datetime.utcfromtimestamp(t.get('entry_timestamp_ms', 0) / 1000).hour
    hours[h].append(t)
for h in sorted(hours.keys()):
    ts = hours[h]
    n = len(ts)
    wins = [t for t in ts if t.get('net_pnl_sol', 0) > 0]
    wr = len(wins) / n * 100
    net = sum(t.get('net_pnl_sol', 0) for t in ts)
    print(f"  {h:02d}:00 UTC: n={n:3d} WR={wr:5.1f}% net={net:+10.6f}")

# Wallet trajectory
print("\nWallet Balance Trajectory:")
balance = 1.5
peak = 1.5
max_dd_sol = 0
max_dd_pct = 0
for i, t in enumerate(trades):
    pnl = t.get('net_pnl_sol', 0)
    balance += pnl
    if balance > peak:
        peak = balance
    dd = peak - balance
    dd_pct = dd / peak * 100 if peak > 0 else 0
    if dd > max_dd_sol:
        max_dd_sol = dd
        max_dd_pct = dd_pct
    if i % 100 == 0 or i == len(trades) - 1:
        print(f"  Trade {i:4d}: balance={balance:.4f} peak={peak:.4f} DD={dd:.4f} ({dd_pct:.1f}%)")

print(f"\n  Max drawdown: {max_dd_sol:.4f} SOL ({max_dd_pct:.1f}%)")
print(f"  Final balance: {balance:.4f} SOL")

print("\n=== ANALYSIS COMPLETE ===")
