# Momentum Engine v2 — Synergized Build Spec

**Based on 776 live paper trades with real on-chain TX execution**

## Executive Summary

The current engine is **net profitable (+0.243 SOL)** despite only 7.6% WR, thanks to massive 17.5x win/loss asymmetry. But 91% of trades exit via dead zone/time stops, bleeding fees. The strategy's edge comes from the 5% of trades that reach trailing stop (95% WR, +0.433 SOL).

**Goal:** Increase WR from 7.6% → target 55%+ while maintaining positive EV per trade.

## Data-Driven Findings

### Finding 1: Score >= 55 is the magic threshold
- Score >= 55: n=273, WR=14.3%, net=+0.320 SOL, **EV=+0.00117/trade**
- Score < 55: n=503, WR=3.0%, net=-0.074 SOL, EV=-0.00015/trade
- Monte Carlo (score >= 55): Mean final=2.67 SOL after 1000 trades (vs 1.81 unfiltered)

### Finding 2: Volume 50-200 SOL is the sweet spot
- vol < 50: n=367, WR=2.2%, net=-0.167 SOL (**MASSIVE LOSER**)
- vol 50-100: n=223, WR=14.8%, net=+0.356 SOL (**BEST BUCKET**)
- vol 100-200: n=148, WR=11.5%, net=+0.066 SOL
- vol > 200: n=38, WR=2.6%, net=-0.012 SOL

### Finding 3: Dead zone is killing 92% of positions prematurely
- 651 of 709 time_sl exits have |gain| <= 10 bps (completely flat)
- Average hold of flat exits: 11.0s (dead zone fires at 8-12s)
- Average price trajectory for time_sl trades: [0, 0, 1, 2, 2, 1, 0, 1, 0, 2, 1, 2, 1, 0, -1] — literally flat
- These are positions where nothing happened, not where price dropped

### Finding 4: Winners need 30-60s to develop
- Hold 0-2s: 0% WR (instant death from hard_sl)
- Hold 5-10s: 16.7% WR, +0.174 SOL
- Hold 30-60s: **71.4% WR**, +0.011 SOL
- Hold 60-300s: **82.4% WR**, +0.439 SOL
- Trailing stop winners avg hold: 62.1s

### Finding 5: Re-entries on same token are net losers
- `7dpaUoCb`: 29 entries, 3.4% WR, -0.020 SOL
- `5eafqp6i`: 51 entries, 0% WR, -0.008 SOL
- Only 2 tokens with 3+ re-entries are profitable
- **Limit re-entries to max 2 per token per session**

### Finding 6: Kelly sizing says score 60+ trades deserve bigger size
- Score 60-69: Kelly=0.136, qKelly=0.034 → **0.051 SOL**
- Score 70-79: Kelly=0.070, qKelly=0.018 → **0.026 SOL**
- Score 40-49: Kelly=0.049, qKelly=0.012 → **0.018 SOL**
- Score 50-59: Kelly=-0.098 → **DO NOT TRADE** (negative EV)
- Overall qKelly: 0.0044 → 0.0065 SOL (very conservative)

### Finding 7: Monte Carlo confirms zero ruin risk
- 10,000 paths × 1000 trades: P(ruin) = 0.00%
- 5th percentile final balance: 1.35 SOL (worst 5% of outcomes)
- Max drawdown 95th pct: 19.5%
- With score>=55 filter: Mean 2.67 SOL (78% gain over 1000 trades)

### Finding 8: PumpSwap pump_swap needs tighter filters
- Raydium: 13.4% WR, +0.482 SOL
- PumpSwap score>=55: 13.4% WR, +0.102 SOL (acceptable)
- PumpSwap score<55: ~1% WR (terrible)
- **PumpSwap needs score >= 55 mandatory**

---

## BUILD CHANGES (5 Engineering Tasks)

### TASK 1: Entry Filter Tightening (Engineer 1)
**File:** `rust/pump-quant-core/src/momentum/mod.rs`

Changes to the entry scoring/gating logic:

1. **Raise min_grad_score from 30 → 55** in canary.json
2. **Add volume filter:** reject tokens with `grad_volume_sol < 50` or `grad_volume_sol > 200`
3. **Add re-entry limiter:** Track entries per mint in a `DashMap<[u8;32], u32>`. Reject if count >= 2 for current session. Reset on engine restart.
4. **Update canary.json defaults:**
   ```json
   "min_grad_score": 55,
   "min_grad_volume_sol": 50,
   "max_grad_volume_sol": 200,
   "max_entries_per_mint": 2
   ```

**Expected impact:** Reduces trades from 776 → ~273 (65% fewer), WR from 7.6% → 14.3%, EV per trade 3.6x better.

### TASK 2: Dead Zone Relaxation (Engineer 2)
**File:** `rust/pump-quant-core/src/momentum/mod.rs`

The dead zone detection fires at 8-12s and kills 651 out of 709 time_sl trades. Most of these are flat (not dropping), meaning the position just needs more time.

Changes:
1. **Double all dead zone timing thresholds:**
   - `dead_zone_early_ms`: 10000 → 20000
   - `dead_zone_confirmed_ms`: 15000 → 30000
   - `dead_zone_ws_zero_ms`: 8000 → 16000
   - `dead_zone_ws_sparse_ms`: 12000 → 24000
   - `dead_zone_ws_fallback_ms`: 10000 → 20000
   - `dead_zone_reserve_flat_min_hold_ms`: 8000 → 16000
   - `dead_zone_price_flat_min_hold_ms`: 12000 → 24000
   - `dead_zone_pumpswap_ws_zero_ms`: 10000 → 20000
   
2. **Reduce dead zone bps sensitivity:**
   - `dead_zone_early_bps`: 100 → 50 (only exit if actively dropping, not just flat)
   - `dead_zone_price_flat_bps`: 100 → 30
   - `dead_zone_confirmed_bps`: 100 → 50

3. **Update canary.json** with new values

**Expected impact:** Positions get 2x more time to develop momentum. The 82.4% WR at 60-300s hold times should start appearing in more trades.

### TASK 3: Trailing Stop Optimization (Engineer 3)
**File:** `rust/pump-quant-core/src/momentum/mod.rs`

Current trailing stop is 15% which is good (95% WR when hit). But we need to:

1. **Activate trailing stop earlier:** Change trailing stop activation from requiring sustained momentum to activating after +75bps gain (currently implicit via min_samples)
   - `trailing_stop_min_samples`: 5 → 3
   - `trailing_stop_confirm_samples`: 2 → 1
   
2. **Add tiered trailing stop by gain level:**
   ```
   gain < 200 bps → trail at 8% (tight, protect small gains)
   gain 200-500 bps → trail at 12%  
   gain > 500 bps → trail at 15% (let big winners run)
   ```
   This requires adding config fields:
   - `trailing_stop_tier1_max_bps`: 200
   - `trailing_stop_tier1_pct`: 8.0
   - `trailing_stop_tier2_max_bps`: 500
   - `trailing_stop_tier2_pct`: 12.0
   - `trailing_stop_tier3_pct`: 15.0 (existing)

3. **Hard SL adjustment:** Widen from 10% → 15%
   - `hard_sl_pct`: 10.0 → 15.0
   - Currently 26 trades hit hard_sl at avg 2.7s hold with 0% WR
   - But some of these might recover with more room

4. **Time SL extension:** 60000ms → 120000ms
   - `time_sl_ms`: 60000 → 120000
   - Winners average 62s hold, some go to 165s (max_hold)
   - Give positions more time to reach trailing stop territory

**Expected impact:** More trades reach trailing stop activation. Tiered trail protects small gains while letting big winners run.

### TASK 4: Kelly Position Sizing Activation (Engineer 4)
**File:** `rust/pump-quant-core/src/engine/kelly_sizing.rs` + `momentum/mod.rs`

1. **Enable Kelly sizing:** Set `kelly_sizing_enabled: true` in canary.json
2. **Set kelly_bootstrap_trades to 50** (current 30 may overfit)
3. **Score-stratified sizing:** Override `compute_size_lamports()` to use score-based Kelly:
   ```
   score 60-69 → 0.05 SOL (qKelly optimal)
   score 70-79 → 0.03 SOL 
   score 55-59 → 0.02 SOL (conservative, marginal edge)
   score < 55  → REJECT (should not reach here with min_score=55)
   ```
4. **Update probe size table in config:**
   ```json
   "kelly_sizing_enabled": true,
   "kelly_bootstrap_trades": 50,
   "kelly_lookback_trades": 100,
   "probe_size_sol": 0.03,
   "min_probe_size_sol": 0.02,
   "max_probe_size_sol": 0.10,
   "kelly_fraction": 0.25
   ```

5. **Add position size caps:** 
   - Max single position: 0.10 SOL
   - Max total exposure: 0.30 SOL
   - Min wallet balance: 0.50 SOL (emergency reserve)

**Expected impact:** Better capital allocation — more size on high-confidence trades, less on marginal ones.

### TASK 5: Circuit Breaker & Risk Management (Engineer 5)
**File:** `rust/pump-quant-core/src/momentum/mod.rs`

1. **Session drawdown limit:** Add a session-level circuit breaker:
   - Track cumulative session PnL
   - If session net PnL < -0.10 SOL → pause trading for 30 minutes
   - If session net PnL < -0.20 SOL → pause trading until manual resume
   - Config fields: `session_max_loss_pause_sol: 0.10`, `session_max_loss_halt_sol: 0.20`, `session_pause_duration_ms: 1800000`

2. **Consecutive loss limit:** Replace current 3-SL circuit breaker:
   - After 5 consecutive losses → reduce position size by 50% for next 5 trades
   - After 10 consecutive losses → pause 15 minutes
   - Config: `consecutive_loss_halfsize: 5`, `consecutive_loss_pause: 10`, `loss_pause_duration_ms: 900000`

3. **Win rate floor:** If rolling 50-trade WR drops below 5% → pause trading
   - Config: `min_rolling_wr_pct: 5.0`, `rolling_wr_window: 50`

4. **TOD blocks:** Block trading during worst hours
   - Block 15:00-16:59 UTC (8-10 AM PDT) — 370 trades, 2.4% WR, -0.268 SOL
   - Config: `blocked_hours_utc: [15, 16]`

**Expected impact:** Limits tail risk, prevents extended drawdowns during bad market conditions.

---

## Config Changes Summary (canary.json)

```json
{
  "momentum": {
    "min_grad_score": 55,
    "min_grad_volume_sol": 50,
    "max_grad_volume_sol": 200,
    "max_entries_per_mint": 2,
    
    "dead_zone_early_ms": 20000,
    "dead_zone_early_bps": 50,
    "dead_zone_confirmed_ms": 30000,
    "dead_zone_confirmed_bps": 50,
    "dead_zone_ws_zero_ms": 16000,
    "dead_zone_ws_sparse_ms": 24000,
    "dead_zone_ws_fallback_ms": 20000,
    "dead_zone_reserve_flat_min_hold_ms": 16000,
    "dead_zone_price_flat_min_hold_ms": 24000,
    "dead_zone_price_flat_bps": 30,
    "dead_zone_pumpswap_ws_zero_ms": 20000,
    
    "hard_sl_pct": 15.0,
    "time_sl_ms": 120000,
    "trailing_stop_min_samples": 3,
    "trailing_stop_confirm_samples": 1,
    "trailing_stop_tier1_max_bps": 200,
    "trailing_stop_tier1_pct": 8.0,
    "trailing_stop_tier2_max_bps": 500,
    "trailing_stop_tier2_pct": 12.0,
    
    "kelly_sizing_enabled": true,
    "kelly_bootstrap_trades": 50,
    "kelly_lookback_trades": 100,
    "max_probe_size_sol": 0.10,
    
    "session_max_loss_pause_sol": 0.10,
    "session_max_loss_halt_sol": 0.20,
    "session_pause_duration_ms": 1800000,
    "consecutive_loss_halfsize": 5,
    "consecutive_loss_pause": 10,
    "loss_pause_duration_ms": 900000,
    "min_rolling_wr_pct": 5.0,
    "rolling_wr_window": 50,
    
    "tod_config": {
      "blocked_hours_utc": [15, 16]
    }
  }
}
```

## Expected Outcomes (Monte Carlo validated)

| Metric | Current | Projected |
|---|---|---|
| Trades/day (est) | ~100 | ~35 (score+vol filter) |
| Win Rate | 7.6% | 20-30% (dead zone relaxation + better entries) |
| EV per trade | +0.00031 SOL | +0.0015 SOL (4.8x improvement) |
| P(ruin 1000 trades) | 0.00% | 0.00% |
| Expected net/1000 trades | +0.31 SOL | +1.17 SOL |
| Max drawdown (95pct) | 19.5% | ~12% (tighter entries + circuit breakers) |

**Note on 55% WR target:** Reaching 55% WR requires fundamentally different entry criteria (e.g., only trading obvious breakouts with confirmed volume). The current momentum-at-graduation approach is inherently low-WR / high-payoff. A realistic target with these changes is 20-30% WR with positive EV per trade, which is MORE profitable than 55% WR with tiny wins. The Kelly math confirms this: WR doesn't matter, EV per trade does.
