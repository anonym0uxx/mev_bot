# Build Plan: Feed Confirming Buy into RideState on Watchlist Promotion

## Bug Analysis

When a watchlist entry is promoted in `hot_path.rs` step 2b (`try_promote` succeeds):

1. `position_manager.open_position(trade, ...)` is called with the confirming buy trade
2. `open_position` sets `trigger_sig = event.sig` and initializes `RideState::new()` with:
   - `last_buy_ms = now_ms` ✓ (timing is correct)
   - `buys_after_entry = 0` ✗ (should be 1)
   - `alpha_x16` = prior only, no positive evidence ✗
   - `bloom_filter` = empty, no wallet recorded ✗
   - `buy_ts_ring` / `buy_sol_ring` = empty ✗
   - `confirming_vol_msol = 0` ✗
3. The confirming buy can never enter via `on_subsequent_trade` because `event.sig == pos.trigger_sig` → early return

**Impact:** The Bayesian model starts with zero positive evidence. Decay ticks erode the prior. With no confirming evidence in the ring buffers, the f-hat drops faster → SignalState transitions to Weakening/Exit sooner → premature RideSignalExit or overly-tight trailing stop fires early.

**Note on ExitStateMachine:** The bug description mentions `ExitStateMachine` with `ExitState::Unconfirmed` and `MomentumDecayFlat`. This is **dead code** (see `engine/mod.rs` line 7: "RIDE-only engine: exit_machine is dead code"). The live exit path is `RideState` exclusively. The equivalent bug in RideState is: missing Bayesian evidence from the confirming buy → faster decay → premature exits.

## Fix: One Method, Two Lines

### Change 1: Add `feed_initial_buy` method to `PositionManager`

**File:** `rust/pump-quant-core/src/engine/positions.rs`
**Location:** After `get_position_mut` method (around line 232)

```rust
/// Feed the confirming buy event into a newly opened position's RideState.
///
/// Called immediately after `open_position()` in the watchlist promotion path.
/// The confirming buy trade that triggered promotion is used as the trigger event
/// in open_position (trigger_sig = event.sig), which means on_subsequent_trade
/// will skip it. This method injects the buy evidence directly into RideState
/// so the Bayesian model starts with correct positive evidence.
///
/// PERF: #[inline] — called once per promotion (~rare), not hot path.
#[inline]
pub fn feed_initial_buy(&mut self, mint: &[u8; 32], sol_amount: u64, now_ms: u64, sig: &[u8; 64]) {
    let pos = match self.positions.get_mut(mint) {
        Some(p) => p,
        None => return,
    };
    match &mut pos.exit_mode {
        ExitMode::Ride(ref mut rs) => {
            let buy_mvsol = lamports_to_mvsol(sol_amount);
            let wallet_hash = u64::from_le_bytes([
                sig[0], sig[1], sig[2], sig[3],
                sig[4], sig[5], sig[6], sig[7],
            ]);
            // source = PumpPortal (confirming buys come from PumpPortal watchlist flow)
            // weight_mult = 10 (standard buy weight)
            rs.on_buy_event(buy_mvsol, now_ms, wallet_hash, crate::feeds::FeedSource::PumpPortal, 10);
        }
    }
    // Also update position-level counters (these are used in JSONL logging)
    pos.confirming_buy_sol = pos.confirming_buy_sol.saturating_add(sol_amount);
    if sol_amount >= 50_000_000 {
        pos.confirming_unique_wallets = pos.confirming_unique_wallets.saturating_add(1);
    }
}
```

### Change 2: Call `feed_initial_buy` after `open_position` in watchlist promotion path

**File:** `rust/pump-quant-core/src/engine/hot_path.rs`
**Location:** In `on_trade`, step 2b, after the `open_position` call and before context enrichment (around line after `self.stats.gates_passed += 1;`)

Add this single line:

```rust
self.position_manager.feed_initial_buy(&trade.mint, trade.sol_amount, now, &trade.sig);
```

### Exact Diff

**positions.rs** — after `get_position_mut` method:

```diff
     /// Get a mutable reference to an open position (for enriching entry context).
     pub fn get_position_mut(&mut self, mint: &[u8; 32]) -> Option<&mut OpenPosition> {
         self.positions.get_mut(mint)
     }
+
+    /// Feed the confirming buy event into a newly opened position's RideState.
+    ///
+    /// Called immediately after `open_position()` in the watchlist promotion path.
+    /// The confirming buy that triggered promotion is the trigger event (trigger_sig),
+    /// so on_subsequent_trade skips it. This injects buy evidence directly.
+    #[inline]
+    pub fn feed_initial_buy(&mut self, mint: &[u8; 32], sol_amount: u64, now_ms: u64, sig: &[u8; 64]) {
+        let pos = match self.positions.get_mut(mint) {
+            Some(p) => p,
+            None => return,
+        };
+        match &mut pos.exit_mode {
+            ExitMode::Ride(ref mut rs) => {
+                let buy_mvsol = lamports_to_mvsol(sol_amount);
+                let wallet_hash = u64::from_le_bytes([
+                    sig[0], sig[1], sig[2], sig[3],
+                    sig[4], sig[5], sig[6], sig[7],
+                ]);
+                rs.on_buy_event(buy_mvsol, now_ms, wallet_hash, crate::feeds::FeedSource::PumpPortal, 10);
+            }
+        }
+        pos.confirming_buy_sol = pos.confirming_buy_sol.saturating_add(sol_amount);
+        if sol_amount >= 50_000_000 {
+            pos.confirming_unique_wallets = pos.confirming_unique_wallets.saturating_add(1);
+        }
+    }
```

**hot_path.rs** — in step 2b, after stats increment:

```diff
                 self.stats.positions_opened += 1;
                 self.stats.gates_passed += 1;
+                // Feed the confirming buy into RideState's Bayesian model.
+                // open_position sets trigger_sig = this trade's sig, so
+                // on_subsequent_trade would skip it. Inject evidence directly.
+                self.position_manager.feed_initial_buy(&trade.mint, trade.sol_amount, now, &trade.sig);
                 // Enrich with entry context from cached mint history
```

## What This Fixes

After the fix, when a watchlist entry is promoted:

| Field | Before Fix | After Fix |
|-------|-----------|-----------|
| `buys_after_entry` | 0 | 1 |
| `alpha_x16` | prior only | prior + confirming buy evidence |
| `bloom_filter` | empty | confirming wallet recorded |
| `buy_ts_ring[0]` | empty | confirming buy timestamp |
| `buy_sol_ring[0]` | empty | confirming buy amount |
| `confirming_vol_msol` | 0 | confirming buy amount |
| `unique_wallets` | 0 | 1 |
| `last_buy_ms` | now_ms (already correct from RideState::new) | now_ms (refreshed) |

**Effect on Bayesian f-hat:** The confirming buy adds alpha evidence, boosting the posterior probability. This means:
- f-hat starts higher → stays in StrongPump/Sustained longer
- Trail distance is properly scaled to the initial evidence
- SignalExit requires actual degradation, not just prior decay
- `buys_after_entry >= 1` allows SignalExit to fire when appropriate (before: blocked forever until first subsequent buy)

## Test Case

**File:** `rust/pump-quant-core/src/engine/positions.rs` (add to `mod tests`)

```rust
/// Test: feed_initial_buy injects evidence into RideState.
/// Verifies that buys_after_entry, unique_wallets, and confirming_vol
/// are updated after calling feed_initial_buy.
#[test]
fn test_feed_initial_buy_injects_evidence() {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut pm = PositionManager::new(test_config(), tx);

    let mint = [0xFFu8; 32];
    let sig = [0xEEu8; 64];
    let entry_vsol = 30_000_000_000u64;
    let event = make_trade_event(mint, sig, 200_000_000, entry_vsol, 1_000_000_000_000_000, true);

    // Open position (simulates watchlist promotion)
    pm.open_position(&event, 80.0, 1000, 60.0, 100_000_000, EntryConviction::default());
    assert_eq!(pm.open_count(), 1);

    // Before feed_initial_buy: RideState has no buy evidence
    {
        let pos = pm.positions.get(&mint).unwrap();
        match &pos.exit_mode {
            ExitMode::Ride(rs) => {
                assert_eq!(rs.buys_after_entry, 0, "no buys before feed_initial_buy");
                assert_eq!(rs.unique_wallets, 0, "no wallets before feed_initial_buy");
                assert_eq!(rs.confirming_vol_msol, 0, "no volume before feed_initial_buy");
            }
        }
        assert_eq!(pos.confirming_buy_sol, 0);
        assert_eq!(pos.confirming_unique_wallets, 0);
    }

    // Feed the confirming buy
    pm.feed_initial_buy(&mint, 200_000_000, 1000, &sig);

    // After feed_initial_buy: RideState has evidence
    {
        let pos = pm.positions.get(&mint).unwrap();
        match &pos.exit_mode {
            ExitMode::Ride(rs) => {
                assert_eq!(rs.buys_after_entry, 1, "confirming buy counted");
                assert!(rs.unique_wallets >= 1, "wallet recorded in bloom");
                assert!(rs.confirming_vol_msol > 0, "volume recorded");
            }
        }
        assert_eq!(pos.confirming_buy_sol, 200_000_000, "position-level confirming vol");
        assert_eq!(pos.confirming_unique_wallets, 1, "position-level unique wallets");
    }
}
```

## Summary

- **2 files changed:** `positions.rs` (new method), `hot_path.rs` (1 line added)
- **0 function signatures changed** (new method only)
- **1 new public method:** `PositionManager::feed_initial_buy`
- **1 new call site:** hot_path.rs step 2b
- **1 test added**
- **Risk:** Minimal — additive change, no existing behavior modified. The confirming buy evidence is strictly beneficial for the Bayesian model. The `last_buy_ms` is already correct from `RideState::new()` but gets harmlessly refreshed.
