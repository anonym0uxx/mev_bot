#!/usr/bin/env python3
"""
Kelly LUT Recalibration from 5,729-trade Paper Trading Dataset
==============================================================
Computes optimal P_LUT (win probability) and R_LUT (reward ratio) values
from actual trade data, along with new bucket boundaries that match the
real score/magnitude distribution.

Since magnitudeScore is not present in the paper trade logs, we derive
a magnitude proxy from available features:
  - curvePct (curve position → remaining upside)
  - preTriggerBuys5s (buy momentum intensity)
  - uniqueBuyerCount (buyer diversity → healthier pump)
  - triggerBuySol (trigger quality)
  - preTriggerVSolDelta3s (fill rate proxy)

The derived magnitude correlates with the Rust entry_engine's magnitude_score
which combines fill_rate, buy_velocity_accel, wallet_quality, curve_remaining,
volume_intensity, sell_vacuum, and token_age.
"""

import json
import sys
import math
from collections import defaultdict
import statistics

DATA_FILE = "/data/.openclaw/workspace/projects/pump-quant/data/mev_paper_trades.jsonl"

# ─── Load trades ─────────────────────────────────────────────────────────

def load_trades():
    trades = []
    with open(DATA_FILE) as f:
        for line in f:
            t = json.loads(line)
            trades.append(t)
    return trades

# ─── Derive magnitude score (0-100) from available features ─────────────

def derive_magnitude(t):
    """
    Proxy for the Rust engine's magnitude_score.
    Uses the features that most correlate with HOW FAR a token pumps.
    
    Components (weighted to match Rust engine priorities):
      1. Curve remaining upside (30%): earlier entry = more room
      2. Pre-trigger buy velocity (25%): strong momentum = bigger move
      3. Buyer diversity (20%): more unique buyers = healthier pump
      4. Trigger quality (15%): bigger trigger buy = more conviction
      5. Fill rate proxy (10%): vsol_delta_3s if available
    """
    # Component 1: Curve remaining (lower curvePct = more upside)
    curve_pct = t.get('curvePct')
    if curve_pct is not None and curve_pct > 0:
        # curvePct ranges 0-77 in data, most trades 35-50
        # Normalize: 0% → 1.0, 50% → 0.5, 80% → 0.2
        m_curve = max(0.0, min(1.0, 1.0 - curve_pct / 100.0))
    else:
        # Estimate from entryVSol: 30 SOL → ~0%, 85 SOL → ~65%
        vsol = t.get('entryVSol', 40.0)
        curve_est = max(0.0, (vsol - 30.0) / 85.0)  
        m_curve = max(0.0, min(1.0, 1.0 - curve_est))

    # Component 2: Pre-trigger buy velocity
    buys_5s = t.get('preTriggerBuys5s')
    if buys_5s is not None:
        # Range 1-67, p50=9, p75=14
        # Sigmoid-like: saturates around 20
        m_velocity = min(1.0, buys_5s / 20.0)
    else:
        # Fallback: estimate from entryVSol progression
        buys_2s = t.get('preTriggerBuys2s')
        if buys_2s is not None:
            m_velocity = min(1.0, buys_2s / 12.0)
        else:
            m_velocity = 0.4  # neutral default

    # Component 3: Buyer diversity
    unique = t.get('uniqueBuyerCount')
    if unique is not None:
        # Range 3-157, p50=13, p75=25
        m_diversity = min(1.0, unique / 30.0)
    else:
        m_diversity = 0.4

    # Component 4: Trigger quality
    trig_sol = t.get('triggerBuySol')
    if trig_sol is not None:
        # Range 0.1-5.9, p50=0.74, p75=1.07
        m_trigger = min(1.0, trig_sol / 2.0)
    else:
        m_trigger = 0.35

    # Component 5: Fill rate proxy (vsol delta 3s)
    delta_3s = t.get('preTriggerVSolDelta3s')
    if delta_3s is not None and delta_3s > 0:
        # Higher delta = faster fill
        m_fill = min(1.0, delta_3s / 5.0)
    else:
        m_fill = m_velocity * 0.6  # correlated fallback

    # Weighted composite
    magnitude = (
        0.30 * m_curve +
        0.25 * m_velocity +
        0.20 * m_diversity +
        0.15 * m_trigger +
        0.10 * m_fill
    ) * 100.0

    return magnitude

# ─── Analyze distribution ────────────────────────────────────────────────

def analyze_distribution(trades):
    """Analyze score and magnitude distributions to set bucket boundaries."""
    scores = []
    magnitudes = []
    
    for t in trades:
        score_100 = t['score'] * 100.0
        mag = derive_magnitude(t)
        scores.append(score_100)
        magnitudes.append(mag)
    
    print("=" * 70)
    print("DISTRIBUTION ANALYSIS")
    print("=" * 70)
    
    for name, vals in [("Entry Score (0-100)", scores), ("Magnitude (0-100)", magnitudes)]:
        vals_s = sorted(vals)
        n = len(vals_s)
        percentiles = {
            'p5': vals_s[int(n*0.05)],
            'p10': vals_s[int(n*0.10)],
            'p20': vals_s[int(n*0.20)],
            'p25': vals_s[int(n*0.25)],
            'p30': vals_s[int(n*0.30)],
            'p33': vals_s[int(n*0.33)],
            'p40': vals_s[int(n*0.40)],
            'p50': vals_s[int(n*0.50)],
            'p60': vals_s[int(n*0.60)],
            'p67': vals_s[int(n*0.67)],
            'p70': vals_s[int(n*0.70)],
            'p75': vals_s[int(n*0.75)],
            'p80': vals_s[int(n*0.80)],
            'p90': vals_s[int(n*0.90)],
            'p95': vals_s[int(n*0.95)],
        }
        print(f"\n{name}:")
        print(f"  Range: [{vals_s[0]:.1f}, {vals_s[-1]:.1f}]")
        print(f"  Mean:  {statistics.mean(vals):.1f}, Stdev: {statistics.stdev(vals):.1f}")
        for k, v in percentiles.items():
            print(f"  {k}: {v:.1f}")
    
    return scores, magnitudes

# ─── Compute LUT values ─────────────────────────────────────────────────

def compute_luts(trades, score_bounds, mag_bounds):
    """
    Compute win rates and reward ratios for each [mag_bucket][score_bucket] cell.
    
    Win/loss is determined by netPnlPct (includes fees).
    A trade is a "win" if netPnlPct > 0.
    """
    n_mag = len(mag_bounds)
    n_score = len(score_bounds)
    
    # Collect trades per cell
    cells = defaultdict(list)
    
    for t in trades:
        score_100 = t['score'] * 100.0
        mag = derive_magnitude(t)
        net_pnl_pct = t.get('netPnlPct', 0.0)
        
        # Find score bucket
        si = n_score - 1
        for i in range(n_score - 1, -1, -1):
            if score_100 >= score_bounds[i]:
                si = i
                break
        if score_100 < score_bounds[0]:
            si = 0
            
        # Find magnitude bucket
        mi = n_mag - 1
        for i in range(n_mag - 1, -1, -1):
            if mag >= mag_bounds[i]:
                mi = i
                break
        if mag < mag_bounds[0]:
            mi = 0
        
        cells[(mi, si)].append(net_pnl_pct)
    
    print("\n" + "=" * 70)
    print("CELL STATISTICS")
    print("=" * 70)
    
    # Compute stats per cell
    p_lut = [[0]*n_score for _ in range(n_mag)]
    r_lut = [[0]*n_score for _ in range(n_mag)]
    
    for mi in range(n_mag):
        mag_label = f"mag[{mag_bounds[mi]:.0f}+" if mi == n_mag-1 else f"mag[{mag_bounds[mi]:.0f}-{mag_bounds[mi+1] if mi+1 < n_mag else '∞'})"
        if mi < n_mag - 1:
            mag_label = f"mag[{mag_bounds[mi]:.0f}-{mag_bounds[mi+1]:.0f})"
        else:
            mag_label = f"mag[{mag_bounds[mi]:.0f}+)"
            
        for si in range(n_score):
            if si < n_score - 1:
                score_label = f"score[{score_bounds[si]:.0f}-{score_bounds[si+1]:.0f})"
            else:
                score_label = f"score[{score_bounds[si]:.0f}+)"
            
            pnls = cells[(mi, si)]
            n = len(pnls)
            
            if n == 0:
                print(f"  {mag_label} × {score_label}: EMPTY CELL")
                p_lut[mi][si] = 400  # conservative default
                r_lut[mi][si] = 100  # conservative default
                continue
            
            wins = [p for p in pnls if p > 0]
            losses = [abs(p) for p in pnls if p <= 0]
            
            n_wins = len(wins)
            n_losses = len(losses)
            win_rate = n_wins / n
            
            avg_win = statistics.mean(wins) if wins else 0
            avg_loss = statistics.mean(losses) if losses else 0.001
            
            # Reward ratio: R = avg_win / avg_loss
            if avg_loss > 0 and avg_win > 0:
                R = avg_win / avg_loss
            elif avg_win > 0:
                R = 10.0  # all wins, cap R
            else:
                R = 0.0
            
            # Kelly edge: f* = p - (1-p)/R
            if R > 0:
                kelly = win_rate - (1 - win_rate) / R
            else:
                kelly = 0
            
            # Expectancy: E = p*avg_win - (1-p)*avg_loss
            expectancy = win_rate * avg_win - (1 - win_rate) * avg_loss
            
            p_permille = int(win_rate * 1000 + 0.5)
            r_x100 = int(R * 100 + 0.5)
            
            p_lut[mi][si] = p_permille
            r_lut[mi][si] = r_x100
            
            print(f"  {mag_label} × {score_label}:")
            print(f"    n={n:4d} (W:{n_wins:3d}, L:{n_losses:3d}) | "
                  f"p={win_rate:.3f} | avg_win={avg_win:.4f} | avg_loss={avg_loss:.4f} | "
                  f"R={R:.3f} | Kelly={kelly:.4f} | E={expectancy:.4f}")
    
    return p_lut, r_lut, cells

# ─── Design optimal bucket boundaries ───────────────────────────────────

def design_boundaries(scores, magnitudes):
    """
    Design bucket boundaries that:
    1. Spread trades approximately evenly across cells (for statistical power)
    2. Capture meaningful regime changes in the data
    3. Use roughly quartile-based splits
    """
    scores_s = sorted(scores)
    mags_s = sorted(magnitudes)
    n = len(scores_s)
    
    # For 4 buckets, we want 4 boundaries (lower bound of each bucket)
    # Strategy: Use ~quartiles but round to clean values
    
    # Score boundaries: need to cover 14.9-98.7, mean=64.7, median=63.1
    # Quartile approach:
    s_q = [scores_s[int(n*p)] for p in [0.0, 0.25, 0.50, 0.75]]
    print(f"\n  Score quartiles: {[f'{v:.1f}' for v in s_q]}")
    
    # Magnitude boundaries
    m_q = [mags_s[int(n*p)] for p in [0.0, 0.25, 0.50, 0.75]]
    print(f"  Magnitude quartiles: {[f'{v:.1f}' for v in m_q]}")
    
    # Round to clean values for the LUT
    # Score: ~[<50, 50-60, 60-75, 75+] based on distribution
    # But let's try data-driven quartiles first
    
    return s_q, m_q

# ─── Main analysis ──────────────────────────────────────────────────────

def main():
    trades = load_trades()
    print(f"Loaded {len(trades)} trades")
    
    # Filter out trades marked for exclusion
    excluded = [t for t in trades if t.get('excludeFromAnalysis')]
    trades_clean = [t for t in trades if not t.get('excludeFromAnalysis')]
    print(f"After exclusion filter: {len(trades_clean)} trades ({len(excluded)} excluded)")
    
    # ─── Step 1: Distribution analysis ───────────────────────────────
    scores, magnitudes = analyze_distribution(trades_clean)
    
    # ─── Step 2: Design bucket boundaries ────────────────────────────
    print("\n" + "=" * 70)
    print("BUCKET BOUNDARY DESIGN")
    print("=" * 70)
    
    s_q, m_q = design_boundaries(scores, magnitudes)
    
    # Try multiple boundary schemes and pick the best
    schemes = {
        "quartile_clean": {
            "score": [round(s_q[0]), round(s_q[1]), round(s_q[2]), round(s_q[3])],
            "mag": [round(m_q[0]), round(m_q[1]), round(m_q[2]), round(m_q[3])],
        },
        "manual_tuned": {
            # Based on distribution: scores cluster 50-80, mags cluster 35-60
            "score": [40, 55, 68, 80],
            "mag": [30, 42, 52, 62],
        },
        "quintile_4": {
            # Slightly more aggressive splits
            "score": [35, 55, 65, 78],
            "mag": [28, 40, 50, 60],
        },
    }
    
    best_scheme = None
    best_score = -999
    best_data = None
    
    for name, bounds in schemes.items():
        print(f"\n{'─'*50}")
        print(f"Scheme: {name}")
        print(f"  Score bounds: {bounds['score']}")
        print(f"  Mag bounds:   {bounds['mag']}")
        
        p_lut, r_lut, cells = compute_luts(trades_clean, bounds['score'], bounds['mag'])
        
        # Evaluate scheme quality: 
        # 1. No empty cells
        # 2. Maximum spread in p values (want differentiation)
        # 3. Minimum cell size > 50 trades
        all_p = [p_lut[mi][si] for mi in range(4) for si in range(4)]
        all_r = [r_lut[mi][si] for mi in range(4) for si in range(4)]
        min_cell = min(len(cells[(mi, si)]) for mi in range(4) for si in range(4))
        max_cell = max(len(cells[(mi, si)]) for mi in range(4) for si in range(4))
        
        p_spread = max(all_p) - min(all_p)
        r_spread = max(all_r) - min(all_r)
        
        # Quality metric: spread * log(min_cell)
        if min_cell > 0:
            quality = p_spread * math.log(min_cell + 1) + r_spread * 0.1
        else:
            quality = -999
        
        print(f"\n  P range: [{min(all_p)}-{max(all_p)}] spread={p_spread}")
        print(f"  R range: [{min(all_r)}-{max(all_r)}] spread={r_spread}")
        print(f"  Cell sizes: min={min_cell}, max={max_cell}")
        print(f"  Quality metric: {quality:.1f}")
        
        if quality > best_score:
            best_score = quality
            best_scheme = name
            best_data = (bounds, p_lut, r_lut, cells)
    
    # ─── Step 3: Output the winning scheme ───────────────────────────
    print("\n" + "=" * 70)
    print(f"WINNER: {best_scheme} (quality={best_score:.1f})")
    print("=" * 70)
    
    bounds, p_lut, r_lut, cells = best_data
    score_bounds = bounds['score']
    mag_bounds = bounds['mag']
    
    # Also compute Kelly fractions for each cell to verify differentiation
    print("\n" + "=" * 70)
    print("KELLY FRACTION ANALYSIS (raw, before half-Kelly)")
    print("=" * 70)
    
    for mi in range(4):
        for si in range(4):
            p = p_lut[mi][si] / 1000.0
            r = r_lut[mi][si] / 100.0
            if r > 0:
                kelly = p - (1-p)/r
            else:
                kelly = 0
            kelly_half = kelly / 2
            
            # After fee adjustment (210bp round-trip, 200bp avg loss):
            # R_adj = (R*avg_loss - fee) / (avg_loss + fee)
            avg_loss_bp = 200
            fee_bp = 210
            avg_win_bp = r * avg_loss_bp
            if avg_win_bp > fee_bp:
                r_adj = (avg_win_bp - fee_bp) / (avg_loss_bp + fee_bp)
                kelly_adj = p - (1-p)/r_adj
                kelly_half_adj = max(0, kelly_adj) / 2
            else:
                r_adj = 0
                kelly_half_adj = 0
            
            # Sizing at 1 SOL = 1B lamports
            size_raw = kelly_half_adj * 1_000_000_000
            size_clamped = max(30_000_000, min(300_000_000, size_raw))
            
            if mi == 0:
                mag_label = f"mag<{mag_bounds[1]}"
            elif mi == 3:
                mag_label = f"mag≥{mag_bounds[3]}"
            else:
                mag_label = f"mag {mag_bounds[mi]}-{mag_bounds[mi+1]}"
            
            if si == 0:
                score_label = f"sc<{score_bounds[1]}"
            elif si == 3:
                score_label = f"sc≥{score_bounds[3]}"
            else:
                score_label = f"sc {score_bounds[si]}-{score_bounds[si+1]}"
            
            n_trades = len(cells[(mi, si)])
            print(f"  [{mi}][{si}] {mag_label:12s} × {score_label:10s}: "
                  f"p={p:.3f} R={r:.2f} → Kelly_raw={kelly:.4f} | "
                  f"R_adj={r_adj:.2f} Kelly_half_adj={kelly_half_adj:.4f} | "
                  f"size={size_clamped/1e6:.0f}M lamps ({n_trades} trades)")
    
    # ─── Step 4: Check for edge after fees ───────────────────────────
    print("\n" + "=" * 70)
    print("CRITICAL: Edge after 210bp round-trip fees")
    print("=" * 70)
    
    # The paper trades already include fees in netPnlPct
    # So the raw win rates and R values already reflect fee-adjusted reality
    # But the Rust code applies fee_adjust_r ON TOP of the LUT values
    # This means we have two choices:
    # A) Store RAW (pre-fee) p and R in LUT, let Rust adjust → need to back out fees
    # B) Store post-fee p and R, and disable fee adjustment in Rust
    
    # Since netPnlPct ALREADY includes fees (feesSol is subtracted), 
    # the win rates and R values from our data are POST-fee.
    # The Rust code then applies ANOTHER fee adjustment via fee_adjust_r.
    # This would double-count fees!
    
    # SOLUTION: We should store the RAW (pre-fee) values in the LUT.
    # To back out fees from our observed data:
    #   observed_avg_win = true_avg_win - fee_pct
    #   observed_avg_loss = true_avg_loss + fee_pct (larger loss due to fees)
    # So:
    #   true_avg_win = observed_avg_win + fee_pct
    #   true_avg_loss = observed_avg_loss - fee_pct (but clamped to >0)
    
    # Actually wait - the fee is already in pnlPct vs netPnlPct.
    # Let's check both fields
    
    print("\nChecking fee structure in data...")
    sample = trades_clean[:5]
    for t in sample:
        print(f"  pnlPct={t.get('pnlPct', 'N/A'):.4f}, netPnlPct={t.get('netPnlPct', 'N/A'):.4f}, "
              f"feesSol={t.get('feesSol', 'N/A')}, sizeSol={t.get('sizeSol', 'N/A')}")
    
    # netPnlPct = pnlPct - fees/size, so netPnlPct is the fee-adjusted PnL
    # We used netPnlPct for win/loss classification
    # So our P and R are POST-fee
    
    # For the Rust LUT, we need PRE-fee values because fee_adjust_r will be applied
    # Recompute using pnlPct (pre-fee) for R values, but keep p from netPnlPct
    # (a trade is a "win" only if it's net positive after fees)
    
    print("\n" + "=" * 70)
    print("RECOMPUTING WITH PRE-FEE R VALUES")
    print("(Win/loss determined by netPnlPct, R computed from pnlPct)")
    print("=" * 70)
    
    # Recompute R using pnlPct (gross) while keeping win classification from netPnlPct
    p_lut_final = [[0]*4 for _ in range(4)]
    r_lut_final = [[0]*4 for _ in range(4)]
    
    cells_detailed = defaultdict(lambda: {'wins_gross': [], 'losses_gross': [], 'wins_net': [], 'losses_net': []})
    
    for t in trades_clean:
        score_100 = t['score'] * 100.0
        mag = derive_magnitude(t)
        net_pnl_pct = t.get('netPnlPct', 0.0)
        gross_pnl_pct = t.get('pnlPct', 0.0)
        
        si = 3
        for i in range(3, -1, -1):
            if score_100 >= score_bounds[i]:
                si = i
                break
        if score_100 < score_bounds[0]:
            si = 0
            
        mi = 3
        for i in range(3, -1, -1):
            if mag >= mag_bounds[i]:
                mi = i
                break
        if mag < mag_bounds[0]:
            mi = 0
        
        is_win = net_pnl_pct > 0
        cell = cells_detailed[(mi, si)]
        if is_win:
            cell['wins_gross'].append(gross_pnl_pct)
            cell['wins_net'].append(net_pnl_pct)
        else:
            cell['losses_gross'].append(abs(gross_pnl_pct))
            cell['losses_net'].append(abs(net_pnl_pct))
    
    for mi in range(4):
        for si in range(4):
            cell = cells_detailed[(mi, si)]
            n_wins = len(cell['wins_gross'])
            n_losses = len(cell['losses_gross'])
            n_total = n_wins + n_losses
            
            if n_total == 0:
                p_lut_final[mi][si] = 400
                r_lut_final[mi][si] = 100
                continue
            
            win_rate = n_wins / n_total
            
            # Use GROSS (pre-fee) PnL for R computation
            avg_win_gross = statistics.mean(cell['wins_gross']) if cell['wins_gross'] else 0
            avg_loss_gross = statistics.mean(cell['losses_gross']) if cell['losses_gross'] else 0.001
            
            if avg_loss_gross > 0 and avg_win_gross > 0:
                R_gross = avg_win_gross / avg_loss_gross
            elif avg_win_gross > 0:
                R_gross = 10.0
            else:
                R_gross = 0.0
            
            p_lut_final[mi][si] = int(win_rate * 1000 + 0.5)
            r_lut_final[mi][si] = int(R_gross * 100 + 0.5)
            
            print(f"  [{mi}][{si}]: n={n_total:4d} (W:{n_wins:3d} L:{n_losses:3d}) "
                  f"p={win_rate:.3f} avg_win_gross={avg_win_gross:.4f} "
                  f"avg_loss_gross={avg_loss_gross:.4f} R_gross={R_gross:.3f}")
    
    # ─── Step 5: Generate Rust constants ─────────────────────────────
    print("\n" + "=" * 70)
    print("GENERATED RUST CONSTANTS")
    print("=" * 70)
    
    # Determine bucket widths
    # For non-uniform buckets, we need a different interpolation approach
    # OR we can make uniform buckets that match our data
    
    # Let's check if we can make uniform widths work
    score_width = score_bounds[1] - score_bounds[0]
    mag_width = mag_bounds[1] - mag_bounds[0]
    uniform_score = all(abs((score_bounds[i+1] - score_bounds[i]) - score_width) < 0.5 for i in range(3))
    uniform_mag = all(abs((mag_bounds[i+1] - mag_bounds[i]) - mag_width) < 0.5 for i in range(3))
    
    print(f"\n// Score bounds: {score_bounds} (uniform={uniform_score}, width={score_width})")
    print(f"// Mag bounds:   {mag_bounds} (uniform={uniform_mag}, width={mag_width})")
    
    if not uniform_score or not uniform_mag:
        print("// WARNING: Non-uniform bucket widths detected!")
        print("// The bilinear interpolation code needs per-axis widths.")
        print("// Generating with best-fit uniform widths...")
        
        # Refit to uniform widths
        # Score: use the mean width
        score_width_uniform = (score_bounds[-1] - score_bounds[0]) / 3.0
        mag_width_uniform = (mag_bounds[-1] - mag_bounds[0]) / 3.0
        
        # Regenerate bounds
        score_bounds_uniform = [score_bounds[0] + i * score_width_uniform for i in range(4)]
        mag_bounds_uniform = [mag_bounds[0] + i * mag_width_uniform for i in range(4)]
        
        print(f"// Uniform score bounds: {[f'{v:.1f}' for v in score_bounds_uniform]}")
        print(f"// Uniform mag bounds:   {[f'{v:.1f}' for v in mag_bounds_uniform]}")
    
    # For the output, let's use the non-uniform approach since the Rust code
    # already uses BUCKET_WIDTH. We'll generate new per-axis widths.
    
    print(f"""
// ─── Recalibrated Kelly LUT Constants ───────────────────────────────────
// Generated from {len(trades_clean)} paper trades (v3 engine)
// Score = entry_score (0-100), Magnitude = magnitude_score (0-100)
//
// Win rates (p) computed from netPnlPct > 0 (post-fee wins)
// Reward ratios (R) computed from gross pnlPct (pre-fee) so that
// fee_adjust_r() in the sizing pipeline doesn't double-count.
//
// Bucket boundaries optimized for actual score/magnitude distributions:
//   Score distribution: mean=64.7, median=63.1, stdev=14.5
//   Magnitude distribution: derived from curvePct + momentum features

/// Minimum