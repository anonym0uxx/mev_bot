# Quant Analysis Report v1 — 2026-03-30

## Executive Summary

v5-rust WITHOUT momentum_decay_flat exits: **84.6% WR** on 370 trades.
The engine's entry logic is sound. The problem is structural: 66.5% of v5-rust entries die in <100ms with zero follow-through buys.

### Root Causes (Priority Order)

1. **Watchlist → Promotion gap**: Tokens pass entry gates, go to watchlist, get promoted on any confirming buy ≥0.03 SOL. But 66.5% of promoted entries then get NO further buys and exit as momentum_decay_flat in 74ms avg. The two-phase entry system IS working (it requires a confirming buy), but one confirming buy isn't enough signal.

2. **Kelly sizing is effectively flat**: The Kelly LUT expects mag_score and entry_score in 0-100 range. The entry_engine.rs DOES output 0-100. BUT the WatchSlot stores score as `(score * 10_000.0) as u32` and PromoteResult recovers it as `slot.score as f64 / 10_000.0`. This means the paper_logger gets `pos.score` which is the 0-100 scale divided back to 0-1 in the watchlist promotion path. Kelly operates on the correct 0-100 range internally, but the LUT boundaries are designed for a different score distribution than what the engine actually produces.

3. **Fee drag destroys all edge**: 15.15 SOL in fees on 2.77 SOL gross profit. Round-trip cost is 2.1% (Pump.fun 2% + Jito 0.1%). With avg trade size 0.095 SOL, each trade costs ~0.002 SOL in fees. At 5,729 trades, that's 11.46 SOL minimum floor.

4. **Score threshold too low**: min_entry_score=50.0 admits trades down to 0.5 score. The data shows score<0.7 has <45% WR, while score>0.85 has ~50% WR. But even 50% isn't enough given fee structure.

## Detailed Findings

### Finding 1: The Momentum Decay Flat Problem

momentum_decay_flat = 733 trades, ALL v5-rust, 6.1% WR, avg hold 74ms.
- These are entries that passed the watchlist (got 1 confirming buy) but then NOTHING happened
- avg buysAfterEntry = 0.08 (essentially zero)
- avg score = 0.577 (mediocre)
- They dominate v5-rust because the EXIT STATE MACHINE's `confirmation_window_ms=200ms` fires MomentumDecayFlat when `last_buy_time_ms == 0` AND elapsed >= 200ms
- BUT the watchlist already provided ONE buy. The exit machine doesn't know about it.

**Root cause**: The watchlist's confirming buy happens BEFORE the position opens. The exit state machine starts at `ExitState::Unconfirmed` with `last_buy_time_ms=0`. The confirming buy that triggered promotion is NOT fed into `on_buy_event`. So the exit machine thinks no buys arrived and fires MomentumDecayFlat.

### Finding 2: Kelly Sizing Flat Output

Kelly produces sizes between 0.05-0.20 SOL (the clamp range). In practice, for the current score/magnitude distribution, nearly all trades land in the same LUT bucket because:
- P_LUT expects magnitude in [40, 50, 60, 70] buckets
- R_LUT expects entry_score in [50, 60, 70, 80] buckets
- Actual magnitude distribution clusters around 50-65 (most in one bucket)
- Actual entry_score distribution clusters around 55-75 (spans 2-3 buckets)
- Result: bilinear interpolation produces nearly identical p/R for all trades
- After fee-adjustment, R drops from ~1100 to ~485 → Kelly fraction is tiny
- All trades hit MIN_SIZE_LAMPORTS (0.05 SOL) or barely above it

### Finding 3: Exit Architecture Issues

The ExitStateMachine is sound but the integration has gaps:
- `on_entry()` sets initial TP/SL from tiers, starts Unconfirmed
- `on_buy_event()` transitions to Confirmed → ConvictionScaled
- `on_price_tick()` handles SL/TP/trail/stall checks
- **BUT** the watchlist's confirming buy is never fed as an `on_buy_event`
- So positions start Unconfirmed and stay that way unless ANOTHER buy arrives

### Finding 4: Score Distribution vs LUT Design

The entry engine produces scores in 0-100 range:
- Typical entry_score: 55-75 (8-feature weighted sum × 100)
- Typical magnitude: 45-65 (7-feature weighted sum × 100)
- min_entry_score=50 threshold admits the bottom 40%
- The score-to-WR relationship is monotonic but weak:
  - score 0.5-0.6: 40.4% WR (n=1458)
  - score 0.7-0.8: 45.7% WR (n=1042)
  - score 0.9-1.0: 54.6% WR (n=216)
  - Even the BEST bucket is only 54.6% — not 75%

### Finding 5: Feature Importance (from backtest)

Most predictive features (win avg vs loss avg):
1. **buysAfterEntry**: wins=2.4, losses=0.13 (+1769% diff) — THE dominant predictor
2. **flowAfterEntrySol**: wins=1.46, losses=0.03 (+4378% diff) — post-entry flow
3. **tradesAfterEntry**: wins=2.63, losses=0.29 (+813% diff)
4. **preTriggerBuys2s**: wins=7.79, losses=6.20 (+25.6% diff)
5. **preTriggerBuys1s**: wins=5.44, losses=4.35 (+24.8% diff)
6. **holdMs**: wins=625ms, losses=1239ms (-49.5% diff) — winners resolve FAST
7. **uniqueBuyerCount**: wins=17.4, losses=19.4 (-10.2% diff) — fewer is better!

## Proposed Changes

### Change 1: Feed Confirming Buy to Exit State Machine (CRITICAL)
When watchlist promotes a position, call `exit_machine.on_buy_event()` with the confirming buy data. This transitions from Unconfirmed → Confirmed immediately, preventing false MomentumDecayFlat exits.

**Projected impact**: Eliminates ~500 of 733 MDF exits, converting v5-rust from 32.5% → ~60% WR.

### Change 2: Require 2 Confirming Buys for Watchlist Promotion
Instead of promoting on ANY single buy ≥0.03 SOL, require either:
- 2 separate confirming buys, OR
- 1 large confirming buy ≥0.1 SOL
This eliminates dead-on-arrival entries where one tiny follow-up buy triggered promotion.

**Projected impact**: Cuts v5-rust trade count by ~40%, raises WR to ~55%.

### Change 3: Raise min_entry_score from 50 → 70
The backtest shows score<0.7 contributes negative net PnL at every threshold.

**Projected impact**: Reduces trades by ~40%, raises WR by ~4-5pp.

### Change 4: Recalibrate Kelly LUT from 5,729-trade dataset
Current LUTs were "precomputed from 392 historical trades" — stale.
Recompute P_LUT and R_LUT from the actual 5,729-trade distribution by magnitude × score bucket.
Also adjust MIN_SIZE from 0.05 → 0.03 SOL and MAX_SIZE from 0.20 → 0.30 SOL to allow more differentiation.

### Change 5: Add Fee-Aware Entry Gate
Before accepting any entry, compute expected_edge = p × avg_win - (1-p) × avg_loss.
Reject if expected_edge < 2 × round_trip_fee. This prevents entries where fees would eat all profit.

### Combined Projected Impact

If we implement Changes 1+2+3:
- v5-rust MDF eliminated (Change 1 alone gets us to ~60% WR)
- Tighter entry gates (Change 3) removes low-conviction entries
- 2-buy confirmation (Change 2) ensures real momentum
- Projected: 65-75% WR on ~500-700 trades/session
- With Change 4 (Kelly differentiation): high-conviction trades get 2-3× more capital
- With Change 5 (fee gate): eliminates break-even trades that just pay fees

## Build Priorities

1. **P0**: Feed confirming buy to exit state machine (Change 1) — single code change in hot_path.rs
2. **P0**: Raise min_entry_score to 70 (Change 3) — single config change
3. **P1**: 2-buy watchlist confirmation (Change 2) — watchlist.rs modification
4. **P1**: Recalibrate Kelly LUTs (Change 4) — kelly_sizing.rs LUT update
5. **P2**: Fee-aware entry gate (Change 5) — entry_engine.rs addition
