# Kelly Criterion & Monte Carlo Risk Analysis Report
## Pump.fun Momentum Strategy — 776 Paper Trades

**Date:** 2026-04-02  
**Analyst:** Apollo (quant subagent)  
**Data:** `data/momentum_paper_trades.jsonl` (776 trades)  
**Scripts:** `analysis/kelly_montecarlo.py`, `analysis/kelly_mc_lite.py`

---

## Executive Summary

The momentum strategy is **genuinely profitable** (+0.243 SOL net, +16.2% on 1.5 SOL) despite a 7.6% win rate, driven by a massive 17.49x win/loss asymmetry. Monte Carlo analysis shows **zero probability of ruin** at the current 0.03 SOL probe size. Kelly criterion recommends keeping sizing conservative but **implementing score-stratified sizing** to increase exposure on high-conviction (score 60+) trades.

**Key recommendations:**
1. **Kelly: keep DISABLED** — but implement static score-based sizing tiers
2. **Circuit breaker: relax from 3 to 15-20** — current setting blocks 47 winning trades
3. **PumpSwap: avoid or minimize** — negative Kelly, -0.239 SOL net
4. **Score 60+ trades: increase to 0.04-0.05 SOL** — 18.1% WR with strong Kelly edge

---

## 1. Kelly Criterion Analysis

### 1.1 Overall Kelly

| Metric | Value |
|--------|-------|
| Win Rate (p) | 7.60% |
| Win/Loss Ratio (b) | 17.49x |
| Full Kelly f* | 2.32% |
| Half Kelly | 1.16% |
| Quarter Kelly | 0.58% |
| Full Kelly bet (1.5 SOL) | 0.035 SOL |
| Quarter Kelly bet (1.5 SOL) | 0.009 SOL |

**Interpretation:** Full Kelly says bet ~0.035 SOL. The current 0.03 SOL probe is ~86% of full Kelly — slightly aggressive in theory, but acceptable because:
- It's a **flat bet**, not proportional (no compounding risk)
- The distribution is **heavily right-skewed** (huge winners, tiny losses)
- The continuous Kelly estimate (E[R]/E[R²]) suggests 10.1% = 0.15 SOL, which is much higher

### 1.2 Score-Stratified Kelly

| Score Range | Count | WR | W/L Ratio | Full Kelly | Kelly/4 | Optimal SOL |
|------------|-------|-----|-----------|-----------|---------|-------------|
| 20-29 | 20 | 5.0% | 10.8x | 0.000 | 0.000 | 0.020 (min) |
| 30-39 | 18 | 0.0% | 0.0x | 0.000 | 0.000 | **SKIP** |
| 40-49 | 52 | 7.7% | 63.5x | 0.062 | 0.016 | 0.023 |
| 50-59 | 429 | 4.2% | 7.7x | **0.000** | 0.000 | 0.020 (min) |
| 60-69 | 105 | **18.1%** | 14.8x | **0.126** | 0.031 | **0.047** |
| 70-79 | 152 | 11.2% | 24.5x | 0.076 | 0.019 | 0.028 |

**Critical insight:** Score 50-59 has **ZERO Kelly edge** (429 trades, 4.2% WR). This bracket accounts for 55% of all trades but nets -0.237 SOL. Meanwhile, score 60-69 has the strongest Kelly edge at 12.6%.

### 1.3 Pool Type Kelly

| Pool | Count | WR | Kelly | Kelly/4 |
|------|-------|-----|-------|---------|
| Raydium AMM v4 | 352 | **13.4%** | **0.103** | 0.026 |
| PumpSwap | 424 | 2.8% | **0.000** | 0.000 |

**PumpSwap has zero Kelly edge.** The entire +0.482 SOL profit comes from Raydium. PumpSwap is a -0.239 SOL drag.

---

## 2. Monte Carlo Simulation Results

### 2.1 Fixed 0.03 SOL Probe (10K paths × 1K trades)

| Metric | Value |
|--------|-------|
| **P(ruin < 0.2 SOL)** | **0.00%** |
| Mean final balance | 1.641 SOL (+9.4%) |
| Median final balance | 1.620 SOL |
| 5th percentile | 1.336 SOL |
| 95th percentile | 2.022 SOL |
| Mean max drawdown | 8.23% |
| 95th %ile max drawdown | **14.95%** |
| 99th %ile max drawdown | 18.57% |

**Verdict:** At 0.03 SOL fixed probe, the strategy is **extremely robust**. Even the worst 5% of paths still end at 1.34 SOL (down only 11%).

### 2.2 Position Size Sensitivity

| Size | Mean | P5 | P(ruin) | 95% DD |
|------|------|-----|---------|--------|
| 0.01 | 1.548 | 1.447 | 0.00% | 5.0% |
| 0.02 | 1.596 | 1.393 | 0.00% | 10.0% |
| **0.03** | **1.644** | **1.340** | **0.00%** | **14.8%** |
| 0.04 | 1.693 | 1.286 | 0.00% | 19.5% |
| 0.05 | 1.741 | 1.233 | 0.00% | 24.3% |
| 0.08 | 1.885 | 1.073 | 0.00% | 38.3% |
| 0.10 | 1.981 | 0.955 | 0.05% | 47.8% |
| 0.15 | 2.221 | 0.684 | 1.10% | 68.8% |
| 0.20 | 2.446 | 0.199 | **6.15%** | 86.8% |

**Key finding:** Up to 0.05 SOL is still zero ruin probability. At 0.10+ SOL, ruin risk appears. The log-optimal growth rate peaks around f=0.15 (0.225 SOL) but the variance is unacceptable. Practical sweet spot: **0.03-0.05 SOL**.

### 2.3 Score-Filtered Monte Carlo

Drawing only from score 60+ trades (258 data points):

| Scenario | Mean Final | P5 | P(ruin) | 95% DD |
|----------|-----------|-----|---------|--------|
| All trades | 1.644 | 1.340 | 0.00% | 14.8% |
| **Score 60+** | **2.463** | **1.923** | **0.00%** | **5.1%** |
| Raydium only | 2.114 | 1.837 | 0.00% | 3.3% |
| 60+ Raydium | 2.171 | 1.854 | 0.00% | 4.3% |

**If we only traded score 60+ tokens, mean return jumps from +9.4% to +64.2%, with drawdown dropping from 15% to 5%.** This is the single highest-impact filter available.

### 2.4 Time to Double

At 0.03 SOL fixed probe, 15 trades/day:
- Only **6% of paths** double (1.5 → 3.0 SOL) within 5,000 trades
- Median time to double: **4,204 trades (~280 days)**
- This confirms: at 0.03 flat sizing, growth is very slow. You're trading for edge validation, not compounding.

---

## 3. Actual Wallet Trajectory

### 3.1 Balance Over 776 Trades

| Trade # | Balance | Cum PnL | Notes |
|---------|---------|---------|-------|
| 0 | 1.500 | 0.000 | Start |
| 100 | 1.405 | -0.095 | Grinding down (early phase, 0.05 sizing) |
| 200 | 1.349 | -0.151 | Continued bleed |
| 300 | 1.273 | -0.227 | Deep drawdown territory |
| **380** | **1.229** | **-0.271** | **MIN BALANCE** (max DD: 18.5%) |
| 400 | 1.476 | -0.024 | Major recovery (big winner cluster) |
| 500 | 1.500 | +0.000 | Breakeven recovery |
| 600 | 1.469 | -0.031 | Slight dip |
| 700 | 1.568 | +0.068 | Turning profitable |
| **776** | **1.743** | **+0.243** | **END** |

### 3.2 Rolling 100-Trade Windows

| Window | WR | Net PnL | Phase |
|--------|-----|---------|-------|
| 1-100 | 2% | -0.095 | Bleeding (early 0.05 size, PumpSwap heavy) |
| 101-200 | 1% | -0.056 | Continued bleed |
| 201-300 | 3% | -0.076 | Worst stretch |
| **301-400** | **10%** | **+0.204** | **Big winner cluster, recovery** |
| 401-500 | 3% | +0.024 | Flat |
| 501-600 | 2% | -0.031 | Slight negative |
| **601-700** | **23%** | **+0.099** | **Strong recent performance** |
| **701-776** | **21%** | **+0.175** | **Best period, config improvements paying off** |

**Recent trend is strongly positive.** Win rate improved from 2-3% early on to 21-23% in the last 176 trades.

---

## 4. Circuit Breaker Analysis

### 4.1 Current Setting is Catastrophically Tight

Current config: `consecutive_stop_pause_count = 3` (pause after 3 consecutive losses)

| Threshold | Triggers | Trades Blocked | Wins Blocked |
|-----------|----------|---------------|--------------|
| **3 (current)** | **35** | **649** | **47 wins** |
| 5 | 29 | 551 | 28 wins |
| 10 | 23 | 429 | 21 wins |
| **15** | **16** | **304** | **8 wins** |
| **20** | **14** | **266** | **10 wins** |
| 50 | 5 | 95 | 0 wins |
| 100 | 2 | 38 | 0 wins |

**At threshold=3, we'd block 83.6% of all trades!** With a 7.6% win rate, consecutive losses of 3+ are the **norm**, not the exception. The median loss streak is 4, and the mean is 14.9.

### 4.2 Loss Streak Distribution

- Top 10 streaks: 169, 140, 79, 45, 33, 31, 23, 20, 20, 12
- Median streak: 4
- Mean streak: 14.9
- **Even the 169-trade max streak only cost 0.164 SOL** (10.9% of bankroll)

### 4.3 Recommendation

Set `consecutive_stop_pause_count = 20` with `pause_duration_ms = 300000` (5 min).

**Rationale:**
- At threshold=20, only 14 triggers across 776 trades
- Blocks 266 trades but only 10 would have been wins
- The worst streak (169 losses) costs ~0.10 SOL at 0.03 sizing — painful but not ruinous
- A 20-loss streak at 0.03 SOL × ~0.0008 avg loss = ~0.016 SOL pause trigger cost
- This is survivable and doesn't block winners

---

## 5. Recommended Configuration

### 5.1 Position Sizing Table

| Score Range | Pool | Recommended Size | Rationale |
|------------|------|-----------------|-----------|
| < 30 | any | **SKIP** | Zero/negative Kelly, no edge |
| 30-39 | any | **SKIP** | 0% WR on 18 trades |
| 40-49 | raydium | 0.03 SOL | Marginal Kelly (63.5x W/L carries it) |
| 40-49 | pumpswap | 0.02 SOL | Minimum probe, PumpSwap drag |
| 50-59 | any | 0.02 SOL | Negative Kelly (4.2% WR), min probe |
| **60-69** | **raydium** | **0.05 SOL** | **Strongest bracket: 18.1% WR, Kelly=0.126** |
| **60-69** | **pumpswap** | **0.04 SOL** | **Strong WR, PumpSwap fee drag** |
| 70-79 | raydium | 0.04 SOL | Solid Kelly (0.076), good WR |
| 70-79 | pumpswap | 0.03 SOL | Standard, PumpSwap discount |

### 5.2 Risk Parameters

| Parameter | Current | Recommended | Rationale |
|-----------|---------|------------|-----------|
| `kelly_sizing_enabled` | false | **false** | Not until WR > 12% and 1500+ trades |
| `probe_size_sol` | 0.03 | 0.03 | Good for default/low-score |
| `min_probe_size_sol` | 0.02 | 0.02 | Keep |
| `max_probe_size_sol` | 0.20 | **0.08** | Reduce — 0.10+ has ruin risk |
| `max_position_size_sol` | 0.125 | **0.10** | Conservative until more data |
| `max_daily_loss_sol` | 0.25 | **0.15** | Tighter stop for live |
| `circuit_breaker` | 3 consecutive | **20 consecutive** | Current too tight |
| `circuit_breaker_pause` | 180s | **300s** | Slightly longer cooldown |
| `min_wallet_balance_lamports` | 200M (0.2 SOL) | **300M (0.3 SOL)** | More margin |
| `bankroll_sol` (risk) | 0.71 | **1.0** | Better sizing denominator |
| `min_grad_score` | 30 | **40** | Score 30-39 has 0% WR |

### 5.3 When to Enable Kelly

Prerequisites:
1. ☐ Overall WR improves to > 12% (currently 7.6%)
2. ☐ Accumulate 1,500+ trades for statistical significance
3. ☐ Implement score-stratified static sizing first (validate it improves results)
4. ☐ Then enable with `kelly_fraction = 0.15` (conservative)
5. ☐ Use lookback of 200 trades for rolling Kelly calculation

### 5.4 The Real Alpha Lever

Kelly sizing is **not the bottleneck**. The data clearly shows:

1. **Score filtering is 10x more impactful** — score 60+ MC shows +64% vs +9% for all trades
2. **Pool filtering matters** — Raydium WR 13.4% vs PumpSwap 2.8%
3. **Recent trend is strongly positive** — last 176 trades: 21-23% WR vs 2% early on

The engine improvements (config tuning, dead zone adjustments) are the real growth driver. Position sizing optimization is a refinement, not a game-changer, until the base WR is higher.

---

## Appendix: Log-Optimal Growth Rate

The log-optimal fraction (maximizing E[log(1+fR)]) peaks around f=0.15 (0.225 SOL position size) with g=0.000315/trade. But at that level, 95th percentile drawdown is 69% — unacceptable for a 1.5 SOL bankroll.

The practical sweet spot maximizing growth-per-unit-risk is **f=0.02-0.04** (0.03-0.06 SOL), where drawdown stays under 20%.

| Fraction | Size | Growth/trade | Expected after 1K trades |
|----------|------|-------------|--------------------------|
| 0.005 | 0.0075 | 0.0000231 | 1.535 SOL |
| 0.010 | 0.0150 | 0.0000450 | 1.569 SOL |
| **0.020** | **0.0300** | **0.0000856** | **1.634 SOL** |
| 0.030 | 0.0450 | 0.0001222 | 1.695 SOL |
| 0.050 | 0.0750 | 0.0001840 | 1.803 SOL |
| 0.100 | 0.1500 | 0.0002826 | 1.990 SOL |
| 0.150 | 0.2250 | 0.0003154 | 2.056 SOL |
| 0.200 | 0.3000 | 0.0002951 | 2.015 SOL ← declining |

---

*Analysis scripts saved to `analysis/kelly_montecarlo.py` and `analysis/kelly_mc_lite.py`*
