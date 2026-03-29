# MEV Strategy Memo — pump-quant 1.5 SOL Budget

**Date:** 2026-03-28
**Dataset:** 5,307 paper trades over 2.01 days (Rust v5 engine: 696 trades / DV3)
**Capital:** 1.5 SOL (~$225)

---

## Executive Summary

The highest-EV immediate play is **not a new strategy** — it's surgically optimizing the existing backrun with data-driven entry filtering. Your current engine is **gross-positive** (+2.42 SOL) but **drowning in fee drag** (-14.33 SOL fees across 5,307 trades = -11.91 SOL net). The root cause is taking ~2,650 trades/day when only ~200-250/day have positive expectancy. By restricting to the "golden segment" (preTriggerBuys1s ≥ 8, UTC 13-21, vSol 30-50), you achieve 58.8% WR, 43.8% TP rate, and **+0.19 SOL/day net** — turning the current -5.94 SOL/day into profitability without writing a single new strategy. The second-highest EV play is graduation arbitrage, which offers a structurally different (and potentially larger) edge, but requires 2-3 weeks of additional implementation and carries competition risk.

---

## 1. Strategy Rankings (by risk-adjusted EV)

| Rank | Strategy | Expected Edge (bps/trade) | Capital Req | Impl. Complexity | Risk Profile | Feasibility | Notes |
|------|----------|---------------------------|-------------|------------------|--------------|-------------|-------|
| **1** | **Optimized backrun (filtered)** | +30-80 bps | 0.5-0.8 SOL deployed | 1/5 (config change) | Low-Med | ✅ Ready now | Golden segment filtering eliminates ~90% of losing trades |
| **2** | **Graduation arbitrage** | +200-800 bps (when it fires) | 0.3-1.0 SOL per event | 4/5 | Med-High | ⚠️ Feed exists, needs arb logic | ~10-30 graduations/day, deterministic price gap |
| **3** | **Atomic Jito bundle backrun** | +15-30 bps improvement over current | Same capital | 3/5 | Low | ⚠️ Needs raw tx bytes from Helius | Eliminates "our tx lands but trigger reverts" failure mode |
| **4** | **ShredStream pre-confirmation backrun** | +50-150 bps over current latency | Same capital | 2/5 | Low | ❌ Blocked on Jito whitelist | ~5ms signal vs 50ms Helius = ~45ms advantage |
| **5** | **Creator sell front-run** | +10-25 bps (defensive, reduces SL%) | No extra capital | 2/5 | Low | ✅ Already have creator sell detection | Prevents holding into creator dump |
| **6** | **Cross-venue arbitrage** | +50-200 bps | 0.5-1.5 SOL | 4/5 | Med | ⚠️ Needs Jupiter/Orca price feeds | Limited to graduated tokens with bonding curve + AMM liquidity |
| **7** | **MEV searcher (tx ordering)** | Unknown, likely <10 bps | Minimal | 5/5 | High | ❌ Requires validator-level integration | Not feasible from VPS without validator partnership |
| **8** | **LP on graduated tokens** | +5-15 bps sustained | 0.5+ SOL locked | 3/5 | Med-High | ⚠️ Feasible but capital-intensive for 1.5 SOL | Inventory risk on memecoins = extreme |

### Strategy-by-Strategy Analysis

**Strategy 1: Optimized Backrun (HIGHEST PRIORITY)**
- Edge source: Information asymmetry — seeing a large buy and predicting follow-on momentum
- Your data proves it works: TP exits average +8.14% gross, +6.23% net per trade
- Problem isn't the strategy — it's the **signal-to-noise ratio**. 80.7% of trades at UTC 04-07 are negative EV
- Fix: filter entries to high-conviction setups only

**Strategy 2: Graduation Arbitrage**
- Edge source: Structural price dislocation during pump.fun → Raydium migration
- Fewer trades (10-30/day) but potentially much larger per-trade PnL
- Competitive landscape: well-known arb, but speed + Jito bundling can still capture edge
- See deep dive in Section 4

**Strategy 3: Atomic Jito Bundle Backrun**
- Current flow: see trigger tx → submit our tx independently → hope both land in same block
- Improved flow: bundle our tx WITH trigger tx → guaranteed same-block execution
- Requires: raw transaction bytes from Helius (currently get parsed logs, not raw bytes)
- Impact: eliminates "our buy lands but trigger doesn't" risk (currently unknown loss rate)

**Strategy 4: ShredStream**
- Blocked on Jito whitelist. Once approved: 5ms signal vs 50ms Helius = 45ms head start
- In MEV, 45ms is an eternity. Could be the difference between landing and missing the block

**Strategy 5: Creator Sell Front-Run**
- Already have detection via Bitquery stream 1
- Currently: creator sell → force-close position (defensive)
- Enhancement: use creator wallet history as a PRE-ENTRY gate (don't enter if creator has sold on previous tokens)

---

## 2. Capital Allocation for 1.5 SOL

### Fee Structure (from data)

| Component | Cost |
|-----------|------|
| Average round-trip fee per trade | 0.002700 SOL |
| Fee breakdown (estimated) | ~0.0005 priority fee buy + ~0.0005 priority fee sell + ~0.000005 base tx × 2 + ~0.0017 Jito tip |
| Jito tip (config) | 50,000 lamports = 0.00005 SOL (paper mode; live would be higher) |
| Fee as % of 0.10 SOL position | ~2.7% |
| Fee as % of 0.12 SOL position | ~2.25% |

**Critical insight:** The 0.0027 SOL avg fee is a **FIXED COST per trade**, not proportional to position size. This means:
- Larger positions have lower fee drag as % → better break-even
- Smaller positions need proportionally larger moves to break even

### Position Sizing Math

| Scenario | Position Size | Max Concurrent | Capital Deployed | Buffer | Fee as % | Min Move to Break Even |
|----------|-------------|----------------|------------------|--------|----------|----------------------|
| Conservative | 0.08 SOL | 4 | 0.32 SOL | 1.18 SOL | 3.38% | 3.38% |
| **Recommended** | **0.10 SOL** | **5** | **0.50 SOL** | **1.00 SOL** | **2.70%** | **2.70%** |
| Moderate | 0.12 SOL | 4 | 0.48 SOL | 1.02 SOL | 2.25% | 2.25% |
| Aggressive | 0.15 SOL | 3 | 0.45 SOL | 1.05 SOL | 1.80% | 1.80% |

**Recommended: 0.10 SOL × 5 concurrent max**

Rationale:
- 0.50 SOL deployed = 33% of bankroll (Kelly would suggest even less at current WR)
- 1.00 SOL buffer absorbs ~370 consecutive losing trades at 0.0027 fee
- 5 positions allows participation in clustered momentum events (pump.fun has bursts)
- 2.70% break-even is achievable — TP target of 4% gives 1.30% net per TP exit

### Expected Daily P&L (with Golden Segment Filter)

**Base assumptions:**
- ~209 qualifying trades/day in golden segment (buys≥8, UTC 13-21, vSol 30-50)
- Not all will fire live (concurrent position cap, Jito landing rate)
- Estimate 30-60 actual live trades/day after caps and landing failures

| Scenario | Trades/Day | Avg Net/Trade | Daily P&L | Monthly P&L |
|----------|-----------|---------------|-----------|-------------|
| **Optimistic** | 60 | +0.0020 SOL | **+0.120 SOL** | +3.60 SOL |
| **Realistic** | 40 | +0.0009 SOL | **+0.036 SOL** | +1.08 SOL |
| **Pessimistic** | 40 | -0.0015 SOL | **-0.060 SOL** | -1.80 SOL |

**Math for realistic scenario:**
- 40 trades × 43.8% TP rate = 17.5 TP exits × +0.0077 SOL avg = +0.135 SOL
- 40 trades × 18.8% SL rate = 7.5 SL exits × -0.0033 SOL avg (DV3 SL avg) = -0.025 SOL
- 40 trades × 15.5% NB rate = 6.2 NB exits × +0.0008 SOL avg = +0.005 SOL
- 40 trades × 21.9% max_hold/md_flat rate = 8.8 exits × -0.0027 SOL (fee only) = -0.024 SOL
- **Daily net: +0.135 - 0.025 + 0.005 - 0.024 = +0.091 SOL** (between optimistic and realistic)

### Capital Runway (Pessimistic)

At -0.060 SOL/day with 0.18 SOL daily loss cap:
- Cap triggers after 0.18 SOL loss in a day → forces pause → max daily damage = 0.18 SOL
- At 0.18 SOL/day max loss: 1.5 SOL lasts **8.3 days** worst case
- At realistic -0.060 SOL/day average bad days: 1.5 SOL lasts **25 days**
- **Verdict:** 1.5 SOL is sufficient for 2-4 weeks of calibration with live daily loss cap protection

---

## 3. Graduation Arb Deep Dive

### How Pump.fun Graduation Works

1. Token bonding curve fills to ~85 SOL in virtual SOL reserves (100% progress)
2. pump.fun program calls `create_pool` on Raydium
3. Remaining tokens + SOL from bonding curve are deposited into Raydium AMM
4. Raydium pool opens for trading at a price determined by the deposit ratio

### The Price Dislocation Mechanism

The bonding curve uses a constant-product formula: `price = vSol / vTokens`

At 100% curve progress (~85 SOL vSol), the final bonding curve price is deterministic. But:

1. **Raydium initial price** is set by the pool initialization deposit ratio, which includes the remaining token supply and deposited SOL
2. **Bonding curve is now closed** — no more trading on pump.fun
3. **Gap window:** Between the last bonding curve trade and the first Raydium trade, there's typically a 2-10 second window
4. **Price can gap:** If the deposit ratio sets Raydium price below the last bonding curve trade price, there's an arbitrage — buy cheap on Raydium, market is already expecting higher price

### Estimated Dislocation Size

Based on the pump.fun bonding curve mechanics:

- Pump.fun takes a ~1% fee on graduation (6 SOL from the ~85 SOL pool)
- ~79 SOL + proportional tokens go to Raydium
- The Raydium opening price typically matches the final bonding curve price closely
- **However:** The first trades on Raydium often occur at different prices because:
  - FOMO buyers rush in at market price (pushing up from opening price)
  - Some sellers dump immediately (pushing down from bonding curve peak)
  - The price discovery is chaotic for 5-30 seconds

Estimated typical dislocation: **3-10%** (based on public analysis of pump.fun graduations)

### Feasibility Analysis

**What we have:**
- ✅ Migration detection via Bitquery stream 2 (~80ms latency)
- ✅ Bonding curve math (can calculate terminal price)
- ✅ JitoClient (can submit bundles)
- ✅ TxBuilder (can construct buy/sell transactions)

**What we need:**
1. Raydium pool address derivation (PDA from token mint + Raydium program)
2. Raydium swap instruction builder (different from pump.fun buy/sell)
3. Raydium AMM price calculation (pool reserves → expected price)
4. Bundle construction: buy on Raydium + sell on Raydium (or hold for a few seconds)
5. Speed optimization: 80ms detection + 50ms tx build + Jito submission = ~200ms total

**Competition Assessment:**
- This is a **well-known** arb. Expect 5-20+ bots competing per graduation
- Speed is critical: first bundle in wins
- 80ms Bitquery latency is a **significant disadvantage** — dedicated node operators with geyser plugins see migrations in ~5-20ms
- Jito bundle tip must be competitive (50K-500K lamports range for graduation arbs)

**Expected P&L per graduation arb:**
- ~10-30 graduations/day
- Not all have exploitable dislocation
- Assume 5-10 actionable events/day
- At 5% avg dislocation, 0.3 SOL position: gross = 0.015 SOL per arb
- Fees: ~0.005 SOL (higher Jito tips for competitive arbs)
- Net per arb: ~0.010 SOL
- **Daily estimate: 5-10 arbs × 0.010 SOL = 0.05-0.10 SOL/day**

**BUT — risk factors:**
- 80ms detection latency means you'll lose to faster bots on >50% of events
- Capital tie-up: 0.3 SOL per arb attempt (even failed ones use capital for bundle)
- Raydium swap complexity adds implementation risk
- If Jito bundle fails, you might be holding a memecoin with no exit plan

**Verdict:** Graduation arb is the **highest EV per trade** but requires significant implementation effort and you're at a **latency disadvantage** without ShredStream or a dedicated validator connection. **Build it second, after optimized backrun is live and profitable.**

### Minimum Profitable Spread

```
Fixed costs per arb attempt:
  Jito tip (competitive): 0.001-0.005 SOL
  Priority fee × 2: 0.001 SOL
  Base tx fee × 2: 0.00001 SOL
  Total fixed: ~0.002-0.006 SOL

For 0.3 SOL position:
  Minimum spread to break even: 0.006 / 0.3 = 2.0%
  Target spread (2:1 reward:risk): ≥ 4%

For 0.5 SOL position:
  Minimum spread: 0.006 / 0.5 = 1.2%
  Target spread: ≥ 2.4%
```

---

## 4. Optimal Live Config

### Current Config vs Recommended

Based on analysis of 5,307 paper trades:

| Parameter | Current (canary.json) | Recommended | Rationale |
|-----------|----------------------|-------------|-----------|
| `paper_mode` | true | **true** (keep for 2 more days with new filters) | Validate filter performance before live |
| `entry_size_sol` | 0.12 | **0.10** | Reduces max exposure; fee drag is fixed, not proportional |
| `max_concurrent_positions` | 10 | **5** | 5 × 0.10 = 0.50 SOL deployed, leaves 1.0 SOL buffer |
| `trigger_min_buy_sol` | 0.35 | **0.50** | Below 0.5 SOL triggers have higher SL rate |
| `min_vsol_in_curve` | 3 | **28** | vSol 30-50 is the profitable zone (46% vs 27% WR at 50-70) |
| `max_vsol_in_curve` | 85 | **50** | Above 50 vSol: 27-33% WR, massive net losses |
| `take_profit_pct` | 0.04 | **0.04** (keep) | TP exits avg 8.14% gross — 4% TP catches them |
| `stop_loss_pct` | 0.015 | **0.015** (keep) | DV3 SL avg is -3.2%; 1.5% is already tight. See analysis below |
| `max_hold_ms` | 1200 | **1500** | TP avg hold is 354ms, NB avg hold is 923ms — 1200ms cuts some NB wins |
| `pre_trigger_min_buys_1s` | 0 | **6** | See predictive feature analysis; 8+ is ideal but 6 balances volume |
| `pre_trigger_min_buys_2s` | 2 | **4** | Correlated with buys_1s; provides confirmation |
| `tod_gate_enabled` | false | **true** | Restrict to profitable hours |
| `tod_config.blocked_hours_utc` | [] | **[0,1,2,3,4,5,6,7,8,9,10,11,22,23]** | UTC 12-21 only (5am-2pm PDT) |
| `momentum_decay_check_ms` | 50 | **100** | 50ms is too aggressive — causes md_flat exits (pure fee drag, 9% of trades) |
| `momentum_decay_max_drawdown_pct` | 0.003 | **0.008** | 0.3% drawdown is noise — let position breathe |
| `live_daily_loss_cap_sol` | 0.18 | **0.18** (keep) | Appropriate for 1.5 SOL bankroll (12%) |
| `consecutive_stop_pause_count` | 3 | **3** (keep) | Good circuit breaker |
| `min_hold_before_exit_ms` | 300 | **200** | Faster exits when profitable; TP fires in <354ms avg |

### Stop Loss Analysis (Why 1.5% SL is Right)

DV3 (Rust engine) SL trades show actual exit pnl ranges:
- DV3 SL median: -3.08% (worse than the 1.5% config — implies slippage on exit)
- DV3 SL loss range: -1.75% to -7.63%
- The gap between config SL (1.5%) and actual SL exit (-3.08% median) = **exit slippage of ~1.5%**

This means the effective SL is already ~3%, not 1.5%. Tightening config further would increase exit slippage (selling into thin bonding curve).

**Recommendation:** Keep SL at 1.5% config. The real fix for SL losses is **not entering bad trades** (entry filtering) rather than tighter exits.

### TP Tier Optimization

Current tiers:
```json
{"trigger_max_sol": 0.6,  "tp_pct": 0.025, "sl_pct": 0.015}
{"trigger_max_sol": 0.8,  "tp_pct": 0.035, "sl_pct": 0.015}
{"trigger_max_sol": 1.5,  "tp_pct": 0.035, "sl_pct": 0.015}
{"trigger_max_sol": 5,    "tp_pct": 0.07,  "sl_pct": 0.015}
```

**Recommended adjustment:**
```json
{"trigger_max_sol": 0.7,  "tp_pct": 0.03,  "sl_pct": 0.015}
{"trigger_max_sol": 1.2,  "tp_pct": 0.04,  "sl_pct": 0.015}
{"trigger_max_sol": 2.5,  "tp_pct": 0.05,  "sl_pct": 0.015}
{"trigger_max_sol": 5.0,  "tp_pct": 0.08,  "sl_pct": 0.015}
```

Rationale: TP exits average +8.14% gross. The MFE distribution shows:
- p25 MFE = 1.16% → many trades barely move
- p50 MFE = 3.04% → 50% of trades reach 3%+ 
- p75 MFE = 5.89% → 25% of trades hit almost 6%
- p90 MFE = 8.90% → 10% of trades reach nearly 9%

The current 2.5% TP for small triggers is capturing well at p50. The 7% TP for large triggers is in the p80-p90 range — good for big momentum. Keep structure, slightly widen tiers.

### Momentum Decay Fix

**The Problem:** momentum_decay_flat exits account for 9.0% of all trades (480 trades) with -0.98 SOL total loss — **pure fee drag**. These are trades where the price was flat, momentum decay triggered, and exit ate fees.

Current config: `momentum_decay_check_ms: 50, momentum_decay_max_drawdown_pct: 0.003`

This fires on ANY 0.3% dip from MFE, checked every 50ms. On a bonding curve with discrete price levels, this fires on noise.

**Fix:** `momentum_decay_check_ms: 100, momentum_decay_max_drawdown_pct: 0.008`

Let the position breathe. 0.8% drawdown from MFE is still tight but avoids noise exits.

Estimated impact: eliminates ~50% of md_flat exits → saves ~0.5 SOL per 5,000 trades.

---

## 5. Predictive Feature Analysis

### What Actually Predicts Winning Trades

From the 5,307-trade dataset:

#### Feature 1: Pre-Trigger Buys per Second (STRONGEST SIGNAL)

| buys/1s | n | WR | TP Rate | Net PnL |
|---------|---|-----|---------|---------|
| 0-2 | 2,120 | 41.3% | — | -2.70 |
| 2-5 | 1,698 | 39.0% | — | -5.41 |
| 5-8 | 680 | 46.3% | — | -2.65 |
| **8-11** | **410** | **54.6%** | — | **-0.71** |
| **11-20** | **362** | **55.2%** | — | **-0.28** |
| 20-50 | 37 | 54.1% | — | -0.13 |

**Conclusion:** buys1s ≥ 8 is the sharpest single filter. WR jumps from 39-41% to 54-55%. This represents **real momentum** — multiple buyers in a 1-second window means organic interest, not a single bot wash-trading.

#### Feature 2: UTC Hour (TIME-OF-DAY)

| UTC Hour Range | TP Rate | Net PnL per Trade | Verdict |
|----------------|---------|-------------------|---------|
| UTC 04-07 (9pm-12am PDT) | 5-13% | Deeply negative | **BLOCK** |
| UTC 00-03 (5-8pm PDT) | 15-22% | Negative | **BLOCK** |
| UTC 08-12 (1-5am PDT) | 13-18% | Negative | Block or reduce |
| **UTC 13-17 (6-10am PDT)** | **31-33%** | **Near zero to positive** | **BEST WINDOW** |
| UTC 18-21 (11am-2pm PDT) | 23-28% | Slightly negative | **ACCEPTABLE** |

**Conclusion:** TP rate at UTC 13-17 is **2.5-3× higher** than UTC 04-07. This maps to US market open hours — when real retail traders are active on pump.fun, not just bots.

#### Feature 3: Virtual SOL at Entry (CURVE POSITION)

| vSol Range | n | WR | TP Rate | Net PnL |
|------------|---|-----|---------|---------|
| **30-40** | 3,019 | 46.0% | 20.3% | -5.85 |
| **40-50** | 2,043 | 41.0% | 18.9% | -5.68 |
| 50-70 | 188 | 27.1% | 11.2% | -0.27 |
| 70-85 | 57 | 33.3% | 3.5% | -0.08 |

**Conclusion:** 30-50 vSol is the sweet spot. Lower than 30 means too early (no established interest). Above 50 means approaching graduation — follow-on buyers get cautious because the upside is capped.

#### Feature 4: Unique Buyer Count (NEGATIVE SIGNAL AT HIGH VALUES)

| Unique Buyers | WR | TP Rate | Interpretation |
|---------------|-----|---------|----------------|
| 0-10 | 44.6% | 20.7% | Fresh token, good |
| 10-20 | 45.6% | 21.9% | Peak opportunity |
| 20-30 | 41.8% | 18.1% | Getting crowded |
| 30-50 | 38.1% | 14.4% | Crowded — late entry |
| 50-100 | 33.2% | 4.9% | **Way too late** |

**Conclusion:** >30 unique buyers = diminishing returns. The alpha is in entering tokens with 5-25 unique buyers — enough interest to validate, not so much that the momentum is priced in.

#### Feature 5: Trigger Buy Size (WEAK SIGNAL — NOT WHAT YOU'D EXPECT)

| Trigger SOL | WR | TP Rate | Interpretation |
|-------------|-----|---------|----------------|
| 0-0.3 | 50.5% | 16.6% | Small buys, decent WR but low TP rate |
| 0.3-0.5 | 40.8% | 21.8% | Current minimum — mixed |
| 0.5-1.0 | 42.5% | 17.9% | Standard range — no edge |
| 1.0-2.0 | 43.5% | 18.2% | Same as 0.5-1.0 |
| **2.0-3.0** | **48.5%** | **30.1%** | **Best bucket** (n=136) |
| 3.0-5.0 | 35.7% | 35.7% | High TP but tiny sample |

**Conclusion:** Trigger buy size alone is NOT strongly predictive (mostly flat 40-43% across the core range). The 2-3 SOL bucket looks promising but small sample. The real signal is in the **context** (buys/1s, time of day), not the trigger size itself.

#### Cross-Feature Analysis: The Golden Segment

| vSol | buys/1s | WR | TP Rate |
|------|---------|-----|---------|
| 30-40 | 0-2 | 43% | — |
| 30-40 | 8-11 | **55%** | — |
| 30-40 | 11+ | **54%** | — |
| 40-50 | 0-2 | 39% | — |
| 40-50 | 8-11 | **54%** | — |
| 40-50 | 11+ | **56%** | — |

Adding UTC 13-21 filter on top:

| Segment | n | WR | TP Rate | Net PnL | Net/Day |
|---------|---|-----|---------|---------|---------|
| **buys≥8, UTC 13-21, vSol 30-50** | 420 | **58.8%** | **43.8%** | **+0.38** | **+0.19** |
| buys≥11, UTC 13-21 | 235 | **60.9%** | **48.9%** | **+0.29** | **+0.14** |
| buys≥8, all hours | 809 | 54.9% | 32.6% | -1.13 | -0.56 |
| UTC 13-17 alone | 1,489 | 49.0% | 28.0% | -0.20 | -0.10 |

**The golden segment (buys≥8, UTC 13-21, vSol 30-50) is the only consistently net-positive filter in the entire dataset.**

Why it works:
- **buys≥8** = confirmed multi-buyer momentum (not a single whale)
- **UTC 13-21** = US active hours (real retail buyers available as exit liquidity)
- **vSol 30-50** = sweet spot on bonding curve (enough room for price appreciation, not near graduation cap)

### The 80%+ WR Path

Getting to 80% WR is not realistic with the current backrun strategy alone. Here's why:

**Structural ceiling:** At 58.8% WR in the golden segment, you'd need to eliminate almost all SL trades while keeping TP trades. But SL trades share the same entry characteristics as TP trades — the difference is **post-entry follow-on buying**, which is inherently unpredictable.

**What would get you to 70%+:**
1. **Add post-entry momentum confirmation before full position commitment** — enter with 30% of position, add remaining 70% only if follow-on buy arrives within 200ms. This converts many SL trades into small-loss fee-only trades.
2. **ML scorer trained on the golden segment data** — with 420 trades in the golden segment, you have enough for a simple logistic regression on (buys1s, buys2s, triggerBuySol, uniqueBuyerCount, preTriggerVolume5s). Target: separate TP exits from SL exits.
3. **Creator wallet reputation scoring** — if creator has launched 3+ tokens that all dumped, skip.

**What would get you to 80%+:**
- Fundamentally different strategy (e.g., graduation arb where the edge is structural/deterministic, not probabilistic)
- Or: much better signal (ShredStream giving pre-confirmation data, seeing the ACTUAL next buyer before committing)

---

## 6. Immediate Action Plan (Top 5, Ranked by ROI)

### Action 1: Deploy Golden Segment Filter (CONFIG CHANGE)
**What:** Update canary.json with: `pre_trigger_min_buys_1s: 6`, `tod_gate_enabled: true`, `tod_config.blocked_hours_utc: [0,1,2,3,4,5,6,7,8,9,10,11,22,23]`, `min_vsol_in_curve: 28`, `max_vsol_in_curve: 50`, `max_concurrent_positions: 5`
**Expected impact:** Turn -5.94 SOL/day → +0.10-0.19 SOL/day (based on golden segment data)
**Implementation time:** 15 minutes (config change + restart)
**Dependencies:** None — already built
**Risk:** Lower trade volume (209 qualifying/day vs 2,650 current), but these are the ONLY profitable trades. Run in paper mode for 48h to validate.

### Action 2: Fix Momentum Decay Over-Triggering (CONFIG CHANGE)
**What:** `momentum_decay_check_ms: 100`, `momentum_decay_max_drawdown_pct: 0.008`, `momentum_decay_min_mfe_pct: 0.005`
**Expected impact:** Eliminate ~50% of md_flat exits → save ~0.25 SOL/day in fee drag (at current volume) or ~0.02 SOL/day at golden segment volume
**Implementation time:** 5 minutes (config change)
**Dependencies:** None
**Risk:** Negligible — md_flat exits are currently 100% fee drag with ~0% gross PnL

### Action 3: Implement Scaled Entry (CODE CHANGE)
**What:** Instead of entering full position size immediately, enter 40% on trigger, add remaining 60% only if ≥1 follow-on buy arrives within 200ms.
**Expected impact:** Converts many SL exits from -0.0033 SOL loss into -0.0014 SOL loss (40% × 0.0033). Estimated saving: 0.015 SOL/day on golden segment trades.
**Implementation time:** 4-6 hours (modify TxBuilder + PositionManager)
**Dependencies:** Requires PositionManager to support partial fills and scaling
**Risk:** Increased complexity; second buy may get worse fill due to price movement from first buy

### Action 4: Build Graduation Arb (CODE CHANGE — MAJOR)
**What:** 
  1. Derive Raydium pool PDA from token mint
  2. Build Raydium swap instruction (buy on new pool at opening price)
  3. Calculate expected opening price from bonding curve terminal state
  4. Submit Jito bundle when spread > 4%
**Expected impact:** +0.05-0.10 SOL/day (5-10 arbs × 0.01 SOL net each)
**Implementation time:** 2-3 weeks (Raydium program interaction, testing, edge cases)
**Dependencies:** Raydium SDK/instruction format, Jito bundle submission (already built), migration detection (already built)
**Risk:** Competitive — other bots are faster (geyser > Bitquery); capital at risk if bundle fails partially

### Action 5: Apply for ShredStream Access (ADMIN TASK)
**What:** Complete Jito ShredStream whitelist application. Once approved, integrate gRPC stream for ~5ms pre-confirmation data.
**Expected impact:** +45ms latency improvement on every trade. At current WR, this could increase TP% by 3-5% (catching follow-on buyers that currently land before us).
**Implementation time:** 1 day (application) + 2-3 days (integration once approved)
**Dependencies:** Jito whitelist approval (out of your control)
**Risk:** Application may be denied; integration has engineering risk with gRPC parsing

---

## Appendix: Key Data Points

### Dataset Summary
- **5,307 paper trades** over 2.01 days
- **696 DV3 (Rust engine)** trades, 4,626 DV2 (TS engine)
- **Average 2,654 trades/day** (paper mode with relaxed gates)
- **Avg position size:** 0.1154 SOL (median)
- **Avg fee per trade:** 0.002700 SOL (2.09% of median position)
- **Gross P&L:** +2.42 SOL | **Fee drag:** -14.33 SOL | **Net P&L:** -11.91 SOL

### Exit Reason P&L Attribution
| Exit Reason | Count | % of Trades | Gross P&L | Net P&L | Avg Net |
|-------------|-------|-------------|-----------|---------|---------|
| take_profit | 1,024 | 19.3% | +10.46 | +7.85 | +0.00767 |
| next_buyer | 1,295 | 24.4% | +4.25 | +0.55 | +0.00042 |
| max_hold | 1,550 | 29.2% | +0.04 | -4.23 | -0.00273 |
| stop_loss | 899 | 16.9% | -11.48 | -14.02 | -0.01559 |
| momentum_decay_flat | 480 | 9.0% | -0.01 | -0.98 | -0.00205 |
| Other | 59 | 1.1% | -0.85 | -1.09 | -0.01847 |

### The Single Biggest Lever
**Eliminating the 1,669 "anti-golden" trades (buys<5, UTC 04-07)** which had 38.2% WR and -4.95 SOL net would immediately improve performance by +4.95 SOL over the 2-day sample period, or **+2.47 SOL/day** in savings.

This is a **config-only change** with zero implementation risk.

---

*End of memo. Next step: update canary.json with golden segment filters, run 48h paper validation, then go live.*