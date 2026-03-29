# Entry Engine Quant Research Briefing

## 1. MISSION

Determine the absolute most profitable entry signal engine for Pump.fun bonding curve momentum backrunning. Produce:
1. **ENTRY_QUANT_ANALYSIS.md** — Full research with ArXiv-backed signal theory, exhaustive data analysis, signal ranking, optimal gate/scorer algorithms, backtested results
2. **ENTRY_ARCHITECT_SPEC.md** — Complete Rust architecture spec for a principal architect to implement the optimal entry engine

## 2. CURRENT STATE — WHAT EXISTS

### 2.1 Data Connectors (Feeds)
- **PumpPortal WebSocket** (`feeds/pumpportal.rs`, 321 lines): Primary trade stream. Provides full TradeEvent with mint, trader, sol_amount, vtoken/vsol reserves, bonding_curve addresses. ~80-120ms latency from on-chain.
- **Helius logsSubscribe** (`feeds/helius.rs`, 517 lines): Pre-warm feed. CANNOT trigger entries because it doesn't provide accountKeys (mint address). Used for lead-time measurement only. Avg lead over PumpPortal: measured via sig_prefix ring buffer correlation.
- **CoreCast/Bitquery gRPC** (`feeds/corecast.rs`, 749 lines): Secondary stream. Provides trades + token creation + graduation events. Higher latency than PumpPortal for trades.
- **ShredStream** (`feeds/shredstream.rs`, 264 lines): NOT ACTIVATED. Would provide ~80ms latency reduction (raw shred data before block confirmation).

### 2.2 Current Entry Pipeline
```
TradeEvent → hot_path.on_trade():
  1. Push to MintHistory ring buffer (64 entries, recompute cached aggregates)
  2. If existing position → route to exit machine, return
  3. Regime exclusion check (mayhem/tokenized agent)
  4. Graduation boundary check (vToken progress)
  5. Health check (feed staleness)
  6. Extract cached aggregates from MintHistory:
     - unique_buyers_30s, buy_count_{1s,2s,5s}, sell_count_5s
     - volume_sol_5s, vsol_delta_3s, time_since_last_buy
     - max_wallet_buy_vol_30s, total_buy_vol_30s
  7. Score computation (6 weighted components + adversarial penalty)
  8. Gate stack evaluation (17+ sequential gates, short-circuit on first fail)
  9. Safety checks (daily loss cap, consecutive SL circuit breaker)
  10. Open position
```

### 2.3 Current Scorer (scorer.rs)
Weighted composite score, 6 components:
```
momentum_trend (10%):    buy_1s / max(buy_2s - buy_1s, 0.1) → accelerating?
buyers_banded (25%):     nonlinear map of unique_buyers_30s (sweet spot 5-10)
buyer_diversity (10%):   unique_buyers / estimated_total_buys × 1.5
curve_fill (20%):        1 - (vsol - min) / (max - min) → earlier = better
crowd_depth_5s (20%):    volume_5s / 5.0 SOL norm
recent_buyers_1s (15%):  buy_count_1s / 6 norm
adversarial_penalty:     if max_wallet_vol/total > 0.6 → 0.5x
```

### 2.4 Current Gate Stack (gates.rs)
17+ gates, all integer arithmetic, zero allocation:
```
0a. BlockedHour (bitmask)          0b. SourceBlocked
1.  Must be buy                    2.  Trigger size [0.15, 5.0] SOL
2b. MaxCurveProgress (vtoken)      3.  VSol reserves [15, 70] SOL
4.  Token age < 300s               5.  Unique buyers >= 3
5b. Unique buyers <= 27            6.  Large trigger needs 5+ buyers
7.  Time since last buy < 1000ms   8.  Crowd 2s >= 2, 5s >= 3
9.  Recent 1s buys >= 3            10. vSol accel >= 0.3 SOL
11. vSol delta 3s < 6 SOL          12. Sell count (unused, min=0)
13. Creator sell recency 30s        14. Sell pressure ratio
14b. Buy/sell ratio >= 1.5         14c. Flow concentration >= 0.25
15. Volume 5s >= 3.0 SOL           16. Trigger isolation <= 0.35
17. Score >= 0.50
```

### 2.5 Current Config (canary.json mev section — live right now)
```json
trigger_min_buy_sol: 0.15, trigger_max_buy_sol: 5.0
trigger_min_score: 0.50
min_vsol_in_curve: 15, max_vsol_in_curve: 70
max_token_age_s: 300, min_unique_buyers: 3
pre_trigger_min_buys_1s: 3, min_buys_2s: 2, min_buys_5s: 3
pre_trigger_max_gap_ms: 1000, min_vsol_accel: 0.3
pre_trigger_min_volume_5s: 3.0, max_trigger_isolation: 0.35
min_buy_sell_ratio_5s: 1.5, min_flow_concentration: 0.25
max_unique_buyers_30s: 27
Gate pass rate: ~0.9%
```

## 3. TRADE DATA — COMPLETE ANALYSIS (974 trades)

### 3.1 Overall Performance
```
Total: 974 trades | WR: 34.2% | Net: -0.70 SOL | Gross: +1.15 SOL | Fees: 1.85 SOL
Fee drag: 133% of gross wins
Avg position: 0.097 SOL | Avg fee: 2.03 mSOL/trade
Break-even WR (with fees): ~65%
Kelly criterion: NEGATIVE
```

### 3.2 The Critical Signal: buysAfterEntry
```
buysAfter=0: n=331 (36.3%), WR=0.0%,  net=-0.829 SOL  ← ALL LOSSES
buysAfter=1: n=154 (16.8%), WR=27.3%, net=-0.176 SOL
buysAfter=2: n=193 (21.1%), WR=60.6%, net=+0.081 SOL  ← breakeven
buysAfter≥3: n=234 (25.6%), WR=64.5%, net=+0.225 SOL  ← profitable
buysAfter≥5: n=68  (7.4%),  WR=64.7%, net=+0.134 SOL  ← most profitable
```

**36% of entries are dead-on-arrival (zero follow-through buys) and generate 100% of the loss.**

### 3.3 Feature → buysAfterEntry Correlations
```
holdMs:             r=+0.398  (outcome, not predictor)
mfePct:             r=+0.655  (outcome, not predictor)
preTriggerBuys2s:   r=+0.070  (weak positive)
preTriggerBuys1s:   r=+0.053  (weak positive)
preTriggerSellCt5s: r=-0.045  (weak negative — fewer sells = better)
uniqueBuyerCount:   r=-0.063  (negative — fewer unique buyers = better!)
triggerBuySol:      r=-0.062  (negative — larger triggers slightly worse)
preTriggerVol5s:    r=-0.043  (negative — higher volume slightly worse)
score:              r=-0.021  (ZERO predictive value!)
```

**CRITICAL FINDING: The current score has near-zero correlation with the outcome that matters (buysAfterEntry). The scorer is basically random noise.**

### 3.4 Feature Quintile Analysis (WR by feature bucket)

**Best discriminators by quintile WR spread:**
```
uniqueBuyerCount: Q1(avg=5.4)=41.6% WR → Q5(avg=32.5)=28.5% WR (Δ=13pp)
triggerBuySol:    Q1(avg=0.27)=24.9% → Q5(avg=1.64)=41.0% (Δ=16pp, inverted!)
score:            Q1(avg=0.59)=25.9% → Q5(avg=0.84)=40.0% (Δ=14pp)
preTriggerSells:  Q1(avg=0.19)=34.0% → Q5(avg=12.0)=30.5% (Δ=3.5pp)
```

### 3.5 Combined Feature Filters (Backtested)
```
triggerBuySol>=0.5 & uniqueBuyers<=12: n=274, WR=43.8%, flat=31.4%, -0.21 mSOL/trade
triggerBuySol>=0.3 & uniqueBuyers<=10: n=298, WR=43.3%, flat=32.6%, -0.32 mSOL/trade
buys1s>=5 & sells5s<=3 & buyers<=12:  n=189, WR=41.8%, flat=30.7%, -0.49 mSOL/trade
```

**Best combined filter reduces flat rate from 36% to 31% and raises WR from 34% to 44% — still not profitable at current fee structure, but directionally correct.**

### 3.6 Config Version Performance
```
v0.35sol_1500ms_30vsol: n=360, WR=37.5%, net=-0.28 SOL (newer, tighter gates)
v0.15sol_1500ms_15vsol: n=614, WR=30.8%, net=-0.51 SOL (older, looser gates)
```
Tighter gates improved WR by 6.7pp but still net negative.

### 3.7 Exit Reason Distribution (post-exit-machine, n=222)
```
momentum_decay_flat: 31.5%  (all losses, avg hold 223ms)  ← DEAD ON ARRIVAL
max_hold:            19.3%  (41.9% WR, all at 1500ms)     ← BUG: config override
stop_loss:           15.3%  (0% WR)
take_profit_scaled:  14.9%  (100% WR, avg hold 398ms)     ← THE EDGE
intra_hold_trail:     9.0%  (40% WR)
momentum_stall:       7.7%  (0% WR)
take_profit:          2.3%  (100% WR)
```

## 4. WHAT THE EXIT ENGINE TELLS US ABOUT ENTRY

The exit engine (exit_machine.rs) was built around the buysAfterEntry signal:
- Unconfirmed state: tight TP/SL, 200ms confirmation window
- Confirmed (buysAfter=1): wider TP/SL, momentum stall detection
- ConvictionScaled (buysAfter≥2): TP multipliers 1.4x/1.8x/2.2x, trailing stop

**The exit engine is well-designed but the entry engine feeds it garbage.** 36% of entries never get a confirming buy and exit at 200ms as momentum_decay_flat, costing 2.03 mSOL each in fees for zero gross P&L.

**The entry engine's job is to minimize unconfirmed flat exits while maximizing entries that reach ConvictionScaled state.**

## 5. AVAILABLE SIGNALS NOT YET USED

### 5.1 From MintHistory (already computed, in cache)
- `max_wallet_buy_vol_30s`: Whale concentration (Amihud proxy)
- `total_buy_vol_30s`: Total flow
- `cached_vsol_oldest_3s`: Price momentum
- `wallet_vol` array: Per-wallet flow distribution (up to 32 wallets)
- Trade-by-trade ring buffer: Full tick history for microstructure analysis

### 5.2 From TradeEvent (available but unused for scoring)
- `vtoken_reserves`: Bonding curve position (graduation proximity)
- `trader`: Wallet address (smart money tracking, repeat buyer detection)
- `slot`: Solana slot (block timing, slot latency)
- `bonding_curve` / `assoc_bonding_curve`: Account addresses for direct RPC queries

### 5.3 From Helius (not yet available for triggers)
- Pre-warm lead time (measured, avg ~50ms before PumpPortal)
- Would need accountSubscribe or LaserStream to get mint + reserves for trigger use

### 5.4 From CoreCast
- Token creation metadata (mayhem/agent detection already used)
- Migration/graduation events

### 5.5 Potentially Computable (need new code)
- **VPIN (Volume-synchronized Probability of Informed Trading)**: Measures informed vs uninformed flow
- **PIN proxy**: Probability of informed trading from buy/sell imbalance
- **Kyle's Lambda**: Price impact per unit volume (liquidity depth)
- **Order flow toxicity**: Adverse selection from trade sequence analysis
- **Hurst exponent**: Mean reversion vs trending regime from price series
- **Autocorrelation of returns**: Short-term momentum persistence

## 6. CONSTRAINTS

- **Latency budget:** Total entry path must be <100ms from signal to Jito bundle submission
- **Zero allocation in hot path:** All scoring/gating must use stack-allocated, Copy types
- **Gate stack must short-circuit:** Most expensive checks last
- **Position lifetime:** ~200ms-5000ms (bonding curve trades are ultra-short-term)
- **Fee floor:** 2% round-trip (Pump.fun 1% buy + 1% sell) — cannot be reduced
- **Current position size:** 0.097 SOL avg — fee drag is 45.7% of gross wins
