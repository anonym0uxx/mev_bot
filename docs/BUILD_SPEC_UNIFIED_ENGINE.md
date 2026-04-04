# BUILD SPEC: Unified Post-Graduation Kelly Engine

## Objective
Remove all graduation arb code. Fix the momentum_decay_flat bug. Wire Kelly/Bayesian scoring into the momentum engine so post-graduation trades use our proven 84.6% gross WR entry logic. Fix momentum WS price feed disconnects. Zero regressions.

## Bankroll: 4 SOL | Mode: Paper | Strategy: Post-graduation momentum with Kelly scoring

---

## PHASE 1: Delete Graduation Arb Code

### Files to DELETE entirely:
- `src/arb/graduation.rs` (1765 lines)
- `src/arb/grad_dex_backrun.rs` (447 lines)
- `src/arb/pool_resolver.rs` (247 lines) — BUT momentum/mod.rs imports `resolve_pool_from_transaction` and `PoolInfo` from here. MOVE these to momentum module first.
- `src/arb/brent_sizing.rs` (205 lines)
- `src/arb/raydium_math.rs` (271 lines)
- `src/arb/raydium_swap_ix.rs` (301 lines)
- `src/arb/jito_bundle.rs` (326 lines)
- `src/arb/price_feed.rs` (341 lines)
- `src/arb/dedup.rs` (282 lines)
- `src/arb/blockhash_manager.rs` (231 lines)

### Dependencies to preserve:
- `src/arb/pool_resolver.rs` exports: `PoolInfo`, `PoolType`, `BC_TERMINAL_PRICE_LAMPORTS_PER_ATOM`, `WSOL_MINT`, `resolve_pool_from_transaction`, `make_pool_resolution_client`
- These are used by `src/momentum/mod.rs` lines 7-8:
  ```rust
  use crate::arb::graduation::{PoolType, resolve_pool_from_transaction};
  use crate::arb::pool_resolver::{PoolInfo, BC_TERMINAL_PRICE_LAMPORTS_PER_ATOM};
  ```
- **Action**: Move `PoolInfo`, `PoolType`, `BC_TERMINAL_PRICE_LAMPORTS_PER_ATOM`, `WSOL_MINT` into `src/momentum/pool.rs` (new file). Move `resolve_pool_from_transaction` and `make_pool_resolution_client` there too.

### `src/arb/mod.rs` — Strip to empty or delete:
- Remove all pub mod declarations and pub use exports
- If no files remain in arb/, delete the module entirely

### `src/main.rs` changes:
- Remove `GraduationArbEngine` instantiation and spawn
- Remove graduation arb config loading
- Remove any `grad_arb` related tokio tasks
- Keep: ShredStream feed spawn, PumpPortal feed, Helius feed, CoreCast feed
- Keep: momentum engine spawn
- Keep: hot_path spawn (but bonding_curve_enabled stays false)

### `config/canary.json` changes:
- Remove: `graduation_arb_enabled`, `graduation_arb_max_sol`, `graduation_arb_min_spread_pct`, `graduation_arb_tp_pct`, `graduation_arb_sl_pct`, `graduation_arb_max_hold_ms`, `graduation_arb_jito_tip_sol`
- Keep everything else

---

## PHASE 2: Fix momentum_decay_flat Bug (P0 — Biggest Impact)

### The Bug:
In `engine/hot_path.rs`, when a watchlist token gets promoted to a position (line ~390-410), the confirming buy that triggered promotion is NOT fed into the exit state machine. The exit machine starts with `last_buy_time_ms = 0` and fires MomentumDecayFlat at 74ms because it thinks no buys ever arrived.

### The Fix:
In `engine/hot_path.rs`, after `self.position_manager.open_position(...)` (around line 395), call:
```rust
// Feed the confirming buy into the exit state machine
// so it knows a buy already arrived (prevents instant MomentumDecayFlat)
self.position_manager.on_subsequent_trade(trade, now, false);
```

Wait — there's already `self.position_manager.feed_initial_buy(...)` at line ~400. Check if that feeds the exit state machine or just the RideState Bayesian model.

Read `engine/positions.rs` `feed_initial_buy()` to verify. If it only feeds RideState, also need to feed exit_machine.

### Verification:
After fix, run paper trades for 1 hour. momentum_decay_flat count should be near zero (only legitimate cases where truly no follow-up buys arrive in 200ms).

---

## PHASE 3: Wire Kelly Scoring into Momentum Engine

### Current state:
- Momentum engine uses its own `scorer::score_graduation()` which is a simple weighted sum (grad_speed, volume, pre_grad_buys)
- Kelly/Bayesian scoring lives in `engine/entry_engine.rs` + `engine/kelly_sizing.rs` + `engine/bayesian_signal.rs`

### What to do:
The momentum engine already scores and filters. But we want Kelly to SIZE the positions (not flat 0.3 SOL for everything).

1. In `momentum/mod.rs`, add Kelly sizing:
   - Import `engine::kelly_sizing::kelly_fraction` (or the LUT function)
   - After `score_graduation()`, compute Kelly fraction from (grad_score mapped to entry_score range, magnitude estimate)
   - Set `size_lamports = kelly_fraction * bankroll` (clamped to min 0.05, max 1.0 SOL)
   - Replace the flat `self.config.position_size_sol` with Kelly-computed size

2. Add bankroll tracking to momentum engine:
   - Track `current_bankroll_lamports: AtomicU64` starting at 4_000_000_000 (4 SOL)
   - On position close: update bankroll with net PnL
   - Kelly fraction computed against current bankroll

### Alternative (simpler, less risk):
Just use tiered sizing based on grad_score:
- score >= 80: 0.5 SOL
- score >= 60: 0.3 SOL  
- score >= 40: 0.15 SOL
This is simpler and less likely to regress.

**Decision: Use tiered sizing first (simpler). Add full Kelly later once we have momentum paper trade data.**

---

## PHASE 4: Fix Momentum WS Price Feed

### The Bug:
`momentum/price_feed.rs` WS connection drops every ~5 minutes with "Connection reset without closing handshake". The backoff increases but it keeps reconnecting.

### The Fix:
1. Add WebSocket ping/pong keepalive. The Helius WSS probably drops idle connections.
   - Send a ping frame every 30 seconds
   - Or send an `accountSubscribe` re-subscribe on reconnect for all active subscriptions

2. On reconnect, re-subscribe all active vault accounts:
   - The `PriceFeedManager` has a DashMap of active subscriptions
   - After WS reconnects, iterate all active subscriptions and re-send accountSubscribe for each

3. Add connection health metric:
   - Track `last_message_ts` 
   - If no message in 10s, proactively reconnect (don't wait for error)

---

## PHASE 5: Config Tuning (from QUANT_ANALYSIS_v1.md)

### In config/canary.json:
1. `min_entry_score`: already 70.0 ✅
2. Add `min_confirming_buys: 2` — require 2 confirming buys for watchlist promotion (or 1 >= 0.1 SOL)
3. `momentum.position_size_sol`: change from 0.3 to use tiered sizing (see Phase 3)
4. `momentum.max_concurrent`: keep at 3
5. `momentum.check_ms`: keep at 150ms (fast enough)

---

## REGRESSION CHECKS

Before deploying, verify against saved data:
1. The 370 v5-rust proper-exit trades should still have >= 80% gross WR
2. No compilation errors after arb module removal
3. Momentum engine initializes and receives graduation events
4. ShredStream continues to detect migrations and forwards them to momentum
5. Price feed connects and tracks vault accounts
6. Paper trades are logged to `data/momentum_paper_trades.jsonl`

---

## FILE CHANGES SUMMARY

### Delete (10 files, 4416 lines):
- arb/graduation.rs, grad_dex_backrun.rs, pool_resolver.rs, brent_sizing.rs
- arb/raydium_math.rs, raydium_swap_ix.rs, jito_bundle.rs, price_feed.rs  
- arb/dedup.rs, blockhash_manager.rs

### New (1 file):
- momentum/pool.rs — moved from arb/pool_resolver.rs (only PoolInfo, PoolType, resolve_pool_from_transaction, make_pool_resolution_client, constants)

### Modify:
- arb/mod.rs — gut or delete
- main.rs — remove grad arb engine, keep momentum + feeds
- momentum/mod.rs — update imports (arb::* → momentum::pool::*), add tiered sizing
- momentum/price_feed.rs — fix WS keepalive + reconnect resubscribe
- engine/hot_path.rs — fix MDF bug (feed confirming buy to exit state machine)
- config/canary.json — remove graduation_arb_* fields

### Do NOT touch:
- engine/kelly_sizing.rs
- engine/bayesian_signal.rs
- engine/entry_engine.rs
- engine/positions.rs
- engine/exit_machine.rs
- engine/watchlist.rs
- feeds/shredstream.rs
- feeds/pumpportal.rs
- feeds/event_joiner.rs
- tx/* (entire tx module preserved for future live trading)
