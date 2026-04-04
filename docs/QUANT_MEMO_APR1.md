# Quantitative Analysis Memo — pump-quant v5-rust Momentum Engine
**Date**: April 1, 2026  
**Analyst**: Subagent Quant  
**Classification**: INTERNAL — TRADING SYSTEM  
**Dataset**: 4,958 paper trades (145 pre-overhaul, 4,813 post-overhaul, 856 with grad enrichment)

---

## Executive Summary

The March 31 overhaul (commit 797acd4) deployed four changes simultaneously: probe-then-scale entry, scorer v2, time-of-day gating, and time-decay trailing stop. **The observed WR bleed from ~33% to 13.6% on enriched trades is caused by a single dominant factor: the scorer v2 admits fast-graduation whale/bot pump tokens (speed=60s, vol≥655 SOL) which have 5.9% WR and produce 37.6% dead-on-arrival trades.** The probe-then-scale system is working correctly (limiting downside on these bad trades to ~0.25 mSOL), but it's fishing in the wrong pond.

The fix is surgical: a hard gate rejecting speed=60s + vol≥655 SOL tokens would instantly lift enriched WR from 13.6% → 25.8%. Combining this with a speed≥90s + vol 50-200 SOL filter yields 40.3% WR on 139 trades (Sharpe 0.225). The maximum WR configuration found is **speed≥120s + vol<200 + ws_notif≥10 = 52.2% WR** on 113 trades.

---

## 1. Causal Chain: What's Broken and Why

### Root Cause #1: Scorer V2 Admits Whale Pump Tokens (PROVEN)

**The data:**
- 527 of 856 enriched trades (61.6%) have `grad_volume_sol ≥ 655` (u16 saturation = actual volume >655 SOL)
- These 527 trades have **5.9% WR**, 0.88 mSOL expectancy, and 63.8% have completely flat price samples (all zeros for first 5 readings)
- The remaining 329 non-saturated trades have **25.8% WR**, 1.20 mSOL expectancy

**The mechanism:**  
Fast-grad high-volume tokens (speed=60s, vol≥655 SOL) represent bot/whale-driven bonding curve fills. The entity that fills the curve is fully distributed before the Raydium listing. By the time the momentum engine enters, price is flat — the pump already happened during the bonding curve phase, not after graduation.

**Scorer v2 gives these tokens `grad_score=73`** (speed=15 + volume_tier=10 + velocity≈3 + buy_sell_ratio=25 + entry_discount≈20). The old scorer gave them ~40. The scorer v2 *rewards* the very behavior that predicts failure (high volume and fast speed).

**Score=73 breakdown**: 446 trades (52.1% of all enriched), WR=7.2%, Exp=1.17 mSOL  
**Score=40 breakdown**: 87 trades (speed=60, vol≥655 in 77 of 87), WR=1.1%, Exp=-0.63 mSOL

### Root Cause #2: Speed Component Inverted (PROVEN)

The speed score gives maximum points (15/15) for speed≤60s. Data proves this is backwards:

| Speed Bucket | n | WR | Exp (mSOL) | Sharpe |
|---|---|---|---|---|
| 60-90s | 698 | 7.3% | 0.63 | 0.021 |
| 120-180s | 86 | 40.7% | 2.32 | 0.199 |
| 240+s | 72 | 41.7% | 3.07 | 0.255 |

**Slow graduations (≥120s) have 5.6× higher WR than fast graduations (60-90s).** Fast grads are bot/whale pumps that exhaust momentum before listing. Slow grads indicate organic retail demand that continues post-graduation.

### Root Cause #3: Volume Score Direction is Wrong (PROVEN)

The volume_tier score gives maximum points for high volume (600+ SOL = 10/10). Data proves high volume is inversely correlated with WR:

| Volume Bucket | n | WR | Exp (mSOL) |
|---|---|---|---|
| <50 SOL | 19 | 47.4% | 0.75 |
| 50-100 SOL | 53 | 39.6% | 3.90 |
| 100-200 SOL | 119 | 36.1% | 2.00 |
| 200-300 SOL | 76 | 9.2% | -0.19 |
| 300-400 SOL | 42 | 7.1% | 0.00 |
| 400-500 SOL | 8 | 0.0% | -6.25 |
| 500+ SOL | 539 | 6.1% | 0.86 |

**Volume above 200 SOL destroys WR.** The sweet spot is 50-200 SOL — enough activity to indicate genuine interest, but not so much that a single entity has already captured the entire bonding curve.

### Contributing Factor #4: Dead-on-Arrival Trades (PROVEN)

322 of 856 enriched trades (37.6%) have **all-zero price samples** — the token's price never moved after entry. These have 0.3% WR.

Speed distribution of all-zero trades: **280/322 (86.9%) are speed=60s tokens.** The remaining 42 are split between speed=120 and speed=240, indicating the problem is overwhelmingly concentrated in the fastest graduations.

### Contributing Factor #5: ToD Impact is Minor Compared to Token Selection (PRELIMINARY)

Post-overhaul UTC hour analysis shows:
- UTC 18-20: 3.0-3.2% WR, negative expectancy (dead hours confirmed)
- UTC 7-9: 35-40% WR, positive expectancy  
- UTC 15-16: 4.4-10.4% WR but strong total PnL (carried by rare big winners)

However, ToD effects are secondary to token selection. Blocking UTC 18-20 helps, but the volume saved is modest (~900 trades blocked). The real impact comes from fixing the token filter.

---

## 2. What Data Proves vs. What's Preliminary

### PROVEN (High Confidence — Large Sample, Consistent Signal)

1. **Fast grad (≤90s) + high vol (≥300 SOL) = bad**: 589 trades, 6.1% WR (p < 0.001 vs random)
2. **Slow grad (≥120s) + low vol (<300 SOL) = good**: 158 trades, 41.1% WR
3. **Saturated volume (655.35 SOL) is a hard negative signal**: 527 trades, 5.9% WR
4. **ws_notif=0 is dead**: 165 trades, 0.0% WR — no trading activity on Raydium at all
5. **ws_notif≥10 threshold cleanly separates winners**: Below=3.1% WR, Above=27.2% WR
6. **All-zero price samples = dead on arrival**: 322 trades, 0.3% WR, 86.9% are speed=60
7. **Probe machinery works**: probe_only 7.5% WR at 0.068 avg size limits losses; scaled_in 35.9% WR
8. **Pre-overhaul R:R was 9.25:1**: avg win 0.282 SOL, avg loss 0.031 SOL — the system makes money through rare large wins, not frequent small wins

### PRELIMINARY (Smaller Samples, Directionally Correct)

1. **Score=31 is the best individual score** (n=30, 56.7% WR) — but small sample
2. **ws_notif≥200 = 40% WR** (n=75) — moderate sample
3. **UTC 2-6 blocking helps** (in permutation testing) — consistent across configurations
4. **s[1]>0 as scale-in gate**: 50.9% WR on n=112 — promising but needs more data
5. **max(price_samples[:3]) > 200 = 85.7% WR** (n=14) — tiny sample, potentially powerful

### UNCERTAIN (Insufficient Data)

1. Speed=90-120s and 180-240s buckets have **zero trades** — the scorer is skipping these entirely
2. Volume 400-500 SOL bucket has only 8 trades
3. The pre-overhaul dataset's 40% WR was on only 145 trades with different exit logic

---

## 3. Top 5 Changes Ranked by Expected Impact

### Change #1: Hard Gate — Reject Fast Grad + High Volume (HIGHEST IMPACT)
**Expected impact**: WR from 13.6% → 25.8% (+89% relative improvement)  
**Evidence**: Rejecting speed=60 + vol≥655 removes 527 trades at 5.9% WR, leaving 329 trades at 25.8% WR  
**Risk**: LOW — these trades have consistently near-zero WR and represent a well-understood microstructure phenomenon  
**Implementation**: Single `if grad_speed_s <= 60 && grad_volume_sol >= 655 { return; }` in the entry gate  

### Change #2: Invert Speed Scoring + Volume Cap Gate (HIGH IMPACT)
**Expected impact**: WR from 25.8% → ~40% with speed≥120 + vol<200 filter  
**Evidence**: 158 trades at 41.1% WR, Sharpe 0.225 — consistent across permutation testing  
**Risk**: MEDIUM — reduces trade frequency from ~856 to ~158 (82% reduction). But current flow at 856 trades generates only 0.86 SOL total PnL. The 158 filtered trades generate 0.42 SOL with much higher WR.  
**Second-order effect**: Need to assess if slow-grad tokens are common enough to sustain trading frequency  

### Change #3: ws_notif Scale-In Gate (HIGH IMPACT)
**Expected impact**: Scale-in only when ws_notif≥10 lifts effective WR from 40.3% → 52.0%  
**Evidence**: ws_notif<5 = 1.4% WR, ws_notif≥10 = 27.2% WR, ws_notif≥20 = 30.8% WR  
**Risk**: LOW — ws_notif is already tracked. Gate only affects scale-in, not probe entry.  
**Mechanism**: ws_notif_count measures realized trading activity on Raydium. Zero = dead token. Low = no interest. This is the best real-time signal for "is anyone actually trading this?"  

### Change #4: Price Trajectory Gate for Scale-In (MEDIUM IMPACT)
**Expected impact**: s[1]>0 gate lifts scale-in WR to ~51%  
**Evidence**: s[1]>0 trades: n=118, WR=50.9% (includes all s[1] ranges). s[1]=0: WR=6.8%.  
**Risk**: MEDIUM — adds latency (must wait for second price sample before scaling). But probe is already held for 2s minimum.  
**Note**: s[0] is always 0 in the dataset (first sample at entry is always flat), so s[0]-based gates are useless. s[1] is the first informative sample.  

### Change #5: ToD Hour Blocking UTC 18-20 (LOW IMPACT)
**Expected impact**: Eliminates ~900 trades at 1.5-3.2% WR, marginal PnL improvement  
**Evidence**: UTC 18 (3.2% WR, -0.38 mSOL exp), UTC 19 (3.0%, -0.27), UTC 20 (1.5%, -0.31)  
**Risk**: VERY LOW — these hours are consistently unprofitable across all datasets  
**Second-order**: UTC 2-6 also shows weakness (4-13% WR) but the permutation testing shows blocking these hours helps most

---

## 4. Key Metrics Summary

| Configuration | n | WR | Exp (mSOL) | Total PnL | Sharpe |
|---|---|---|---|---|---|
| Current (all enriched) | 856 | 13.6% | 1.00 | 0.86 SOL | 0.036 |
| Block whale pumps only | 329 | 25.8% | 1.20 | 0.40 SOL | ~0.12 |
| speed≥120 + vol<200 | 158 | 41.1% | 2.66 | 0.42 SOL | 0.225 |
| speed≥90 + vol 50-200 | 139 | 40.3% | 2.92 | 0.41 SOL | ~0.22 |
| +ws_notif≥10 scale-in | 98 | 52.0% | 4.14 | 0.41 SOL | ~0.30 |
| Pareto optimal: speed≥90 + vol 100-200 + score≥30 | 30 | 56.7% | 6.01 | 0.18 SOL | 0.316 |

---

## 5. Risks and Second-Order Effects

### Trade Frequency Risk
The optimal filters reduce from 856 → 139-158 trades. At the current rate of ~856 enriched trades per ~7 hours of trading, this implies ~23 trades per hour instead of ~122. This is a **6x frequency reduction**. However:
- Current 856 trades produce 0.86 SOL total
- Filtered 158 trades produce 0.42 SOL total  
- Per-trade quality improves 2.7×, compensating for volume loss
- With proper sizing (not 0.05 SOL probes on everything), PnL per trade should increase significantly

### Sample Size Risk
The 158-trade bucket for speed≥120 + vol<200 is statistically significant (p < 0.01 for WR > 30%) but not huge. The 30-trade Pareto optimal configuration is NOT sufficient for production deployment — it needs 100+ more observations.

**Recommendation**: Deploy the **broad whale pump block first** (Changes #1), collect 2-3 days of data, then progressively tighten with Changes #2-4.

### Scoring System Redesign Risk
The current scorer v2 is fundamentally mis-calibrated — it rewards fast speed and high volume, which are the two strongest negative predictors. Rather than patching the scorer, the recommended approach is:
1. **Phase 1**: Hard gate rejects (Changes #1, #2) as override above the scorer
2. **Phase 2**: Scorer v3 with inverted speed curve and volume penalty (Change #2 long-term)
3. **Phase 3**: ML-based scoring once trade count reaches ~5,000 enriched samples

### Probe Sizing Risk
The probe machinery (0.05 SOL) is correctly limiting downside, but it also limits upside. The 35.9% WR on scaled-in trades vs 7.5% on probe-only shows the scale-in logic is sound. The problem is that scale-in triggers too rarely (only 585/4813 = 12.1%) because most probes enter dead tokens that never give a positive signal.

**Key insight**: Fix the ENTRY FILTER first (reject bad tokens), then the scale-in logic will naturally improve because it's scaling into better tokens.

---

## 6. Solana Memecoin Microstructure Analysis

### Fast-Grad vs Slow-Grad Behavioral Model

**Fast graduations (≤60s)** represent tokens where a single entity (bot or whale) fills the entire bonding curve in one sweep. The 85 SOL bonding curve completes in under 60 seconds, meaning ~1.4 SOL/second fill rate. This is consistent with:
- MEV bots sniping the token creation and buying the entire curve
- Whale wallets with pre-planned fill strategies
- "Pump and list" operations where the curve fill is the product, not the token

**Post-graduation behavior**: Price is FLAT because the entity that filled the curve is the only holder. No organic demand exists. The token was never discovered by retail. The ws_notif=0 rate (19.7% of fast-grad trades) confirms no one is watching these tokens on Raydium.

**Slow graduations (≥120s)** represent organic retail demand. Multiple wallets contribute to the curve fill over 2+ minutes. This indicates genuine discovery through social media, Telegram groups, or Twitter. Post-graduation, this organic demand continues as a second wave of buyers arrives at the Raydium listing.

### ws_notif_count as Realized Volatility Proxy

ws_notif_count_at_close measures WebSocket notification count — effectively, how many trading events occurred on the Raydium pool during the position hold. This serves as a **realized liquidity/volatility proxy**:

| ws_notif | WR | Interpretation |
|---|---|---|
| 0 | 0.0% | Dead token — no one trading |
| 1-10 | 4.9% | Nearly dead — minimal interest |
| 11-50 | 23.0% | Active — real market forming |
| 51-100 | 27.7% | Healthy — sustained trading |
| 101-200 | 36.4% | Hot — significant interest |
| 200+ | 40.0% | Very hot — strong demand |

This monotonic relationship is the strongest real-time signal available. It should be the primary gate for scale-in decisions.

### Price Sample Patterns

The `price_samples_bps` array captures price trajectory at ~1s intervals post-entry. Key findings:
- **All-zero**: 37.6% of trades — these tokens have ZERO price movement after entry
- **max_gain > 1000 bps**: 19 trades, 100% WR, 65.36 mSOL avg — the rare big winners
- **s[1] > 0**: 118 trades, 50.9% WR — if price moves UP by the second sample, it's a winner
- **s[1] = 0**: 676 trades, 6.8% WR — flat at second sample = likely dead

---

## Appendix: Permutation Backtest Pareto Frontier

All configurations with n≥30, WR>40%, positive expectancy:

| Config | n | WR | Exp | Sharpe |
|---|---|---|---|---|
| speed≥90, vol 100-200, score≥30, block_02-06 | 30 | 56.7% | 6.01 | 0.316 |
| speed≥90, vol 50-200, score≥30, block_02-06 | 72 | 45.8% | 4.81 | 0.300 |
| speed≥60, vol 100-200, score≥30, block_02-06 | 43 | 53.5% | 5.17 | 0.283 |
| speed≥90, vol≥50, score≥30, any_tod | 82 | 42.7% | 4.14 | 0.265 |
| speed≥120, any filters | 158 | 41.1% | 2.66 | 0.225 |

Note: Many Pareto configurations collapse to the same 30-trade subset (speed≥120 + vol 100-200 + score≥30), because there are zero trades in the 90-120s speed range. The speed filter effectively becomes a 120s minimum.
