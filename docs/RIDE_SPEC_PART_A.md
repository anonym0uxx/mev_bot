# RIDE_SPEC_PART_A — Position Manager, Hot Path, Paper Logger

**Status:** SPEC COMPLETE — Ready for implementation  
**Date:** 2026-03-29  
**Scope:** Add RIDE exit mode to positions.rs, wire magnitude through hot_path.rs, extend paper_logger.rs logging

---

## Dependencies

This spec assumes `ride_state.rs` (RIDE_SPEC_PART_B) is implemented and exposes:

```rust
// crate::engine::ride_state

pub struct RideState { /* opaque */ }
pub struct RideConfig { /* opaque */ }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RideExitReason {
    TrailingStop,
    HardFloor,
    WhaleExit,
    BuyGapTimeout,
    SellCascade,
    CreatorSell,
    MaxHold,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RideAction {
    Hold,
    Exit(RideExitReason),
}

impl RideState {
    /// Create a new RideState. entry_mvsol is the virtual SOL price at entry in milli-vSOL.
    /// peak_mvsol is the current highest price seen. now_ms is current timestamp.
    pub fn new(entry_mvsol: u32, peak_mvsol: u32, now_ms: u64) -> Self;

    /// Called when a buy event occurs on the token while we hold.
    /// sell_mvsol: current virtual SOL in milli-vSOL after this trade.
    /// buyer_id: 0 if unknown. now_ms: event timestamp.
    pub fn on_buy_event(&mut self, sell_mvsol: u32, buyer_id: u64, now_ms: u64, cfg: &RideConfig) -> RideAction;

    /// Called when a sell event occurs on the token while we hold.
    /// sell_mvsol: current virtual SOL in milli-vSOL after this trade.
    pub fn on_sell_event(&mut self, sell_mvsol: u32, now_ms: u64, cfg: &RideConfig) -> RideAction;

    /// Periodic tick. Call on every trade and on timer ticks.
    /// current_mvsol: current virtual SOL price in milli-vSOL.
    pub fn on_tick(&mut self, current_mvsol: u32, now_ms: u64, cfg: &RideConfig) -> RideAction;

    /// Returns current phase: 1=early, 2=momentum, 3=tighten
    pub fn phase(&self) -> u8;

    /// Returns the peak milli-vSOL seen during this ride.
    pub fn peak_mvsol(&self) -> u32;

    /// Returns the timestamp when RIDE mode began.
    pub fn ride_start_ms(&self) -> u64;
}

impl Default for RideConfig {
    fn default() -> Self; // returns sane defaults for paper trading
}
```

This spec also assumes `risk_manager.rs` exists (or will be created) at:
```
crate::engine::risk_manager::RiskManager
```
With at minimum:
```rust
pub struct RiskManager { /* fields TBD */ }
impl RiskManager {
    pub fn allows_entry(&self) -> bool;
}
```

---

# SECTION 1: Engineer 1 — positions.rs Modifications

**Target file:** `rust/pump-quant-core/src/engine/positions.rs`

Read this section top-to-bottom. Implement every change in order. Do NOT skip any item. After all changes, run `cargo test` and ensure all existing tests still pass plus the 5 new tests listed at the end.

---

## 1A) Add imports at the top of positions.rs

Add these imports near the existing `use` block:

```rust
use crate::engine::ride_state::{RideState, RideConfig, RideAction, RideExitReason};
```

---

## 1B) Add ExitMode enum

Add this enum **above** the `OpenPosition` struct definition:

```rust
/// Determines the active exit strategy for an open position.
/// All positions start as Scalp. Qualified positions promote to Ride.
#[derive(Debug)]
pub enum ExitMode {
    /// Fast scalp exit via the existing ExitStateMachine.
    Scalp(crate::engine::exit_machine::ExitStateMachine),
    /// Trailing-stop ride exit for confirmed pumps.
    Ride(crate::engine::ride_state::RideState),
}
```

---

## 1C) Modify OpenPosition struct

**Remove** this field from `OpenPosition`:
```rust
pub exit_sm: ExitStateMachine,
```

**Replace** it with these fields:
```rust
    /// Active exit strategy — Scalp or Ride.
    pub exit_mode: ExitMode,

    /// Magnitude estimate from EntryDecision (0.0–100.0). Used for RIDE qualification.
    pub magnitude_estimate: f64,

    /// Cumulative SOL from buy events after our entry (in lamports).
    pub confirming_buy_sol: u64,

    /// Count of unique wallet-sized buys (sol_amount >= 0.05 SOL). Capped at 255.
    pub confirming_unique_wallets: u8,

    /// Number of sell events observed while we hold this position.
    pub sells_during_hold: u16,
```

**Important:** Every place in the file that currently accesses `pos.exit_sm` must be updated to use `pos.exit_mode` with a match. The compiler will guide you — fix every reference.

---

## 1D) Add ExitReason variants

Find the existing `ExitReason` enum. **Append** these variants (do NOT remove any existing variants):

```rust
    // --- RIDE mode exit reasons ---
    RideTrailingStop,
    RideHardFloor,
    RideWhaleExit,
    RideBuyGapTimeout,
    RideSellCascade,
    RideCreatorSell,
    RideMaxHold,
```

If `ExitReason` derives `PartialEq, Eq, Clone, Copy, Debug` — good, keep those. The new variants need no special data.

---

## 1E) Add ClosedPosition fields

Find the `ClosedPosition` struct. **Append** these fields:

```rust
    /// "scalp" or "ride" — which exit strategy was active at close.
    pub exit_mode_str: &'static str,

    /// RIDE phase at close: 0=n/a (scalp), 1=early, 2=momentum, 3=tighten.
    pub ride_phase: u8,

    /// Peak milli-vSOL seen during RIDE. 0 for scalp exits.
    pub ride_peak_mvsol: u32,

    /// Duration of RIDE mode in milliseconds. 0 for scalp exits.
    pub ride_hold_ms: u64,

    /// Unique confirming wallets at close. 0 for scalp exits.
    pub ride_unique_wallets: u8,
```

---

## 1F) Add conversion helper

Add this private function anywhere in the file (suggest near the top, after imports):

```rust
/// Convert lamports to milli-vSOL (rounds to nearest).
/// 1 milli-vSOL = 1_000_000 lamports (0.001 SOL).
fn lamports_to_mvsol(lamports: u64) -> u32 {
    ((lamports + 500_000) / 1_000_000) as u32
}
```

---

## 1G) Add RideExitReason mapping function

Add this private function:

```rust
/// Map a RideExitReason from ride_state into our ExitReason enum.
fn map_ride_exit_reason(r: RideExitReason) -> ExitReason {
    match r {
        RideExitReason::TrailingStop   => ExitReason::RideTrailingStop,
        RideExitReason::HardFloor      => ExitReason::RideHardFloor,
        RideExitReason::WhaleExit      => ExitReason::RideWhaleExit,
        RideExitReason::BuyGapTimeout  => ExitReason::RideBuyGapTimeout,
        RideExitReason::SellCascade    => ExitReason::RideSellCascade,
        RideExitReason::CreatorSell    => ExitReason::RideCreatorSell,
        RideExitReason::MaxHold        => ExitReason::RideMaxHold,
    }
}
```

---

## 1H) Add ride_qualified() function

Add this private function:

```rust
/// Check if an open position qualifies for promotion from SCALP to RIDE.
/// All conditions must be met simultaneously.
fn ride_qualified(pos: &OpenPosition) -> bool {
    // At least 2 buy events after our entry
    pos.buys_since_entry >= 2
    // At least 0.3 SOL cumulative confirming buy volume
    && pos.confirming_buy_sol >= 300_000_000
    // At least 2 distinct wallet-sized buyers
    && pos.confirming_unique_wallets >= 2
    // Zero sells during our hold (pure buy pressure)
    && pos.sells_during_hold == 0
    // Price has moved up at least ~1.5% from entry
    && pos.current_vsol > pos.entry_vsol + pos.entry_vsol / 66
    // Magnitude estimate from entry model is >= 40
    && pos.magnitude_estimate >= 40.0
}
```

**Note:** `buys_since_entry`, `current_vsol`, and `entry_vsol` are assumed to be existing fields on `OpenPosition`. If they have different names, use the actual field names. The key semantics:
- `buys_since_entry`: count of buy trades seen after position open
- `current_vsol`: current virtual SOL reserve (or equivalent price proxy)
- `entry_vsol`: virtual SOL reserve at time of entry

---

## 1I) Add ride_config to PositionConfig

Find the `PositionConfig` struct. **Add** this field:

```rust
    /// Configuration for RIDE trailing-stop exits.
    pub ride_config: crate::engine::ride_state::RideConfig,
```

Update any `PositionConfig` constructors / `Default` impls to include:
```rust
ride_config: RideConfig::default(),
```

---

## 1J) Modify open_position() — new signature

Find the `open_position()` method. Change its signature to accept `magnitude_estimate`:

**Before:**
```rust
pub fn open_position(&mut self, event: &TradeEvent, score: f64, now_ms: u64)
```

**After:**
```rust
pub fn open_position(&mut self, event: &TradeEvent, score: f64, now_ms: u64, magnitude_estimate: f64)
```

Inside the function body, when constructing the `OpenPosition`:
- Replace `exit_sm: <whatever>` with `exit_mode: ExitMode::Scalp(<whatever was exit_sm>)`
- Add: `magnitude_estimate,`
- Add: `confirming_buy_sol: 0,`
- Add: `confirming_unique_wallets: 0,`
- Add: `sells_during_hold: 0,`

The position **always** opens as `ExitMode::Scalp`. RIDE promotion happens later via `ride_qualified()`.

---

## 1K) Modify on_subsequent_trade() — COMPLETE new logic

This is the most complex change. Replace the existing `on_subsequent_trade()` body with the following logic. Preserve the function signature (it takes `&mut self, event: &TradeEvent, now_ms: u64` or similar).

```rust
pub fn on_subsequent_trade(&mut self, mint: &[u8; 32], event: &TradeEvent, now_ms: u64) {
    let Some(pos) = self.positions.get_mut(mint) else {
        return;
    };

    // --- Update tracking fields ---
    // (Keep any existing price/vsol update logic that was here before.)

    let is_buy = event.is_buy; // adjust to actual field name

    if is_buy {
        // Track confirming buy volume
        pos.confirming_buy_sol = pos.confirming_buy_sol.saturating_add(event.sol_amount);

        // Track unique wallet-sized buyers (>= 0.05 SOL)
        if event.sol_amount >= 50_000_000 {
            pos.confirming_unique_wallets = pos.confirming_unique_wallets.saturating_add(1);
            // u8 saturating_add caps at 255 automatically
        }

        // Increment buys_since_entry if that's done here (may already exist)
        // pos.buys_since_entry += 1;

        // --- Feed the active exit strategy ---
        let current_mvsol = lamports_to_mvsol(pos.current_vsol);
        let action = match &mut pos.exit_mode {
            ExitMode::Scalp(ref mut sm) => {
                // Feed the scalp state machine
                let scalp_action = sm.on_buy_event(event, now_ms);

                // Check for RIDE promotion
                if ride_qualified(pos) {
                    let entry_mvsol = lamports_to_mvsol(pos.entry_vsol);
                    let peak_mvsol = current_mvsol.max(entry_mvsol);
                    let ride_state = RideState::new(entry_mvsol, peak_mvsol, now_ms);
                    pos.exit_mode = ExitMode::Ride(ride_state);
                    // After promotion, do an immediate tick
                    if let ExitMode::Ride(ref mut rs) = &mut pos.exit_mode {
                        rs.on_tick(current_mvsol, now_ms, &self.config.ride_config)
                    } else {
                        RideAction::Hold
                    }
                } else {
                    // Return a hold-equivalent; scalp exits are handled below
                    // If scalp_action signals exit, handle it via existing scalp logic
                    return; // scalp exit handled by existing sm logic path
                }
            }
            ExitMode::Ride(ref mut rs) => {
                rs.on_buy_event(current_mvsol, /* buyer_id */ 0, now_ms, &self.config.ride_config)
            }
        };

        // Handle RIDE exit action from buy path
        if let RideAction::Exit(reason) = action {
            let exit_reason = map_ride_exit_reason(reason);
            self.close_position_inner(mint, exit_reason, now_ms);
            return;
        }
    } else {
        // --- SELL event ---
        pos.sells_during_hold = pos.sells_during_hold.saturating_add(1);

        let current_mvsol = lamports_to_mvsol(pos.current_vsol);
        let action = match &mut pos.exit_mode {
            ExitMode::Scalp(_) => {
                // Scalp ExitStateMachine does not handle sells. No-op.
                RideAction::Hold
            }
            ExitMode::Ride(ref mut rs) => {
                rs.on_sell_event(current_mvsol, now_ms, &self.config.ride_config)
            }
        };

        if let RideAction::Exit(reason) = action {
            let exit_reason = map_ride_exit_reason(reason);
            self.close_position_inner(mint, exit_reason, now_ms);
            return;
        }
    }

    // --- Post buy/sell: tick the active exit strategy ---
    // Re-borrow pos (may have been consumed above if closed)
    let Some(pos) = self.positions.get_mut(mint) else {
        return; // position was closed above
    };
    let current_mvsol = lamports_to_mvsol(pos.current_vsol);
    let tick_action = match &mut pos.exit_mode {
        ExitMode::Scalp(ref mut sm) => {
            sm.on_price_tick(event, now_ms);
            return; // scalp handles its own exit signaling
        }
        ExitMode::Ride(ref mut rs) => {
            rs.on_tick(current_mvsol, now_ms, &self.config.ride_config)
        }
    };

    if let RideAction::Exit(reason) = tick_action {
        let exit_reason = map_ride_exit_reason(reason);
        self.close_position_inner(mint, exit_reason, now_ms);
    }
}
```

### ⚠️ IMPORTANT NOTES on on_subsequent_trade():

1. **The above is a structural template.** You MUST adapt it to the actual field names, method signatures, and borrow patterns in the existing code. The key logic flow is:
   - Buy → update counters → feed exit mode → check RIDE promotion (if Scalp) → tick
   - Sell → update counter → feed exit mode → tick
   - On any `RideAction::Exit` → call `close_position_inner` with mapped reason

2. **Borrow checker challenge:** After `pos.exit_mode = ExitMode::Ride(ride_state)`, the mutable borrow on `pos` is still active. You may need to restructure to avoid the double-borrow. One pattern:

```rust
// Calculate qualification BEFORE borrowing exit_mode mutably
let should_promote = matches!(pos.exit_mode, ExitMode::Scalp(_)) && ride_qualified(pos);

// Now handle the scalp SM
if let ExitMode::Scalp(ref mut sm) = &mut pos.exit_mode {
    sm.on_buy_event(event, now_ms);
}

// Promote if qualified
if should_promote {
    let entry_mvsol = lamports_to_mvsol(pos.entry_vsol);
    let current_mvsol = lamports_to_mvsol(pos.current_vsol);
    let peak_mvsol = current_mvsol.max(entry_mvsol);
    pos.exit_mode = ExitMode::Ride(RideState::new(entry_mvsol, peak_mvsol, now_ms));
}

// Tick the (possibly new) exit mode
let current_mvsol = lamports_to_mvsol(pos.current_vsol);
let action = match &mut pos.exit_mode {
    ExitMode::Scalp(ref mut sm) => {
        sm.on_price_tick(event, now_ms);
        return;
    }
    ExitMode::Ride(ref mut rs) => {
        rs.on_tick(current_mvsol, now_ms, &self.config.ride_config)
    }
};
```

3. **Existing scalp exit handling:** The current code likely has logic where `sm.on_buy_event()` returns some action/signal and the position is closed on scalp exit. **Keep all that logic intact.** The RIDE additions are layered on top.

---

## 1L) Modify on_tick() — timer-based ticks

Find `on_tick()` (the periodic timer tick, not trade-driven). Add RIDE handling:

```rust
pub fn on_tick(&mut self, now_ms: u64) {
    // Collect mints to close (avoid borrow issues)
    let mut to_close: Vec<([u8; 32], ExitReason)> = Vec::new();

    for (mint, pos) in self.positions.iter_mut() {
        // --- Existing max_hold safety check ---
        // (Keep this exactly as-is. It applies to BOTH Scalp and Ride.)
        if now_ms - pos.entry_time_ms > self.config.max_hold_ms {
            to_close.push((*mint, ExitReason::MaxHold));
            continue;
        }

        // --- Tick the exit strategy ---
        match &mut pos.exit_mode {
            ExitMode::Scalp(ref mut sm) => {
                // Existing scalp tick logic — keep as-is
                sm.on_tick(now_ms);
                // ... existing scalp exit check ...
            }
            ExitMode::Ride(ref mut rs) => {
                let current_mvsol = lamports_to_mvsol(pos.current_vsol);
                let action = rs.on_tick(current_mvsol, now_ms, &self.config.ride_config);
                if let RideAction::Exit(reason) = action {
                    to_close.push((*mint, map_ride_exit_reason(reason)));
                }
            }
        }
    }

    // Close positions
    for (mint, reason) in to_close {
        self.close_position_inner(&mint, reason, now_ms);
    }
}
```

---

## 1M) Modify close_position_inner()

When constructing the `ClosedPosition`, populate the new RIDE fields:

```rust
fn close_position_inner(&mut self, mint: &[u8; 32], reason: ExitReason, now_ms: u64) {
    let Some(pos) = self.positions.remove(mint) else {
        return;
    };

    // Determine RIDE metadata
    let (exit_mode_str, ride_phase, ride_peak_mvsol, ride_hold_ms, ride_unique_wallets) =
        match &pos.exit_mode {
            ExitMode::Scalp(_) => ("scalp", 0u8, 0u32, 0u64, 0u8),
            ExitMode::Ride(rs) => (
                "ride",
                rs.phase(),
                rs.peak_mvsol(),
                now_ms.saturating_sub(rs.ride_start_ms()),
                pos.confirming_unique_wallets,
            ),
        };

    let closed = ClosedPosition {
        // ... all existing fields ...
        exit_reason: reason,
        exit_mode_str,
        ride_phase,
        ride_peak_mvsol,
        ride_hold_ms,
        ride_unique_wallets,
    };

    // ... existing close logic (log, callback, etc.) ...
}
```

---

## 1N) Tests — Add to #[cfg(test)] mod tests

Add these 5 tests. You'll need test helpers to construct `TradeEvent`s and tick positions. Adapt to actual constructors.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // Helper: create a minimal PositionConfig with defaults
    fn test_config() -> PositionConfig {
        PositionConfig {
            // ... existing test defaults ...
            ride_config: RideConfig::default(),
        }
    }

    // Helper: create a buy TradeEvent
    fn make_buy_event(sol_amount: u64, vsol_after: u64) -> TradeEvent {
        TradeEvent {
            is_buy: true,
            sol_amount,
            // set current_vsol / virtual_sol_reserves to vsol_after
            // ... fill other required fields with test defaults ...
        }
    }

    // Helper: create a sell TradeEvent
    fn make_sell_event(sol_amount: u64, vsol_after: u64) -> TradeEvent {
        TradeEvent {
            is_buy: false,
            sol_amount,
            // ... fill other required fields ...
        }
    }

    /// Test 1: Existing SCALP behavior is unchanged with new ExitMode enum.
    /// Open a position, feed a buy that doesn't qualify for RIDE, verify still Scalp.
    #[test]
    fn test_scalp_mode_unchanged() {
        let mut pm = PositionManager::new(test_config());
        let mint = [1u8; 32];
        let entry_event = make_buy_event(100_000_000, 30_000_000_000); // 0.1 SOL buy

        pm.open_position(&entry_event, 75.0, 1000, 20.0); // magnitude < 40

        // Feed one small buy — not enough to qualify
        let buy1 = make_buy_event(50_000_000, 30_100_000_000);
        pm.on_subsequent_trade(&mint, &buy1, 2000);

        let pos = pm.positions.get(&mint).expect("position should exist");
        assert!(
            matches!(pos.exit_mode, ExitMode::Scalp(_)),
            "Should remain in Scalp mode"
        );
    }

    /// Test 2: ride_qualified() returns correct true/false.
    #[test]
    fn test_ride_qualification() {
        // Construct a position that meets ALL criteria
        let mut pos = OpenPosition {
            // ... fill fields ...
            buys_since_entry: 3,
            confirming_buy_sol: 500_000_000,    // 0.5 SOL
            confirming_unique_wallets: 3,
            sells_during_hold: 0,
            current_vsol: 31_000_000_000,       // above entry + 1.5%
            entry_vsol: 30_000_000_000,
            magnitude_estimate: 55.0,
            exit_mode: ExitMode::Scalp(/* ... */),
            // ... other fields ...
        };

        assert!(ride_qualified(&pos), "Should qualify for RIDE");

        // Fail: too few buys
        pos.buys_since_entry = 1;
        assert!(!ride_qualified(&pos), "Should NOT qualify: too few buys");
        pos.buys_since_entry = 3;

        // Fail: sells during hold
        pos.sells_during_hold = 1;
        assert!(!ride_qualified(&pos), "Should NOT qualify: sells during hold");
        pos.sells_during_hold = 0;

        // Fail: magnitude too low
        pos.magnitude_estimate = 30.0;
        assert!(!ride_qualified(&pos), "Should NOT qualify: low magnitude");
        pos.magnitude_estimate = 55.0;

        // Fail: price hasn't moved enough
        pos.current_vsol = pos.entry_vsol + 1; // tiny gain
        assert!(!ride_qualified(&pos), "Should NOT qualify: insufficient price gain");
    }

    /// Test 3: Position transitions from SCALP to RIDE after qualifying buys.
    #[test]
    fn test_scalp_to_ride_transition() {
        let mut pm = PositionManager::new(test_config());
        let mint = [2u8; 32];
        // Entry at 30 SOL virtual reserves
        let entry_event = make_buy_event(200_000_000, 30_000_000_000);

        pm.open_position(&entry_event, 80.0, 1000, 60.0); // magnitude 60 >= 40

        // First qualifying buy: 0.2 SOL, price moves up
        let buy1 = make_buy_event(200_000_000, 30_600_000_000); // ~2% up
        pm.on_subsequent_trade(&mint, &buy1, 2000);

        // Still Scalp — only 1 buy so far (need >= 2)
        let pos = pm.positions.get(&mint).unwrap();
        assert!(matches!(pos.exit_mode, ExitMode::Scalp(_)));

        // Second qualifying buy: another 0.2 SOL, price moves up more
        let buy2 = make_buy_event(200_000_000, 31_000_000_000); // ~3.3% up
        pm.on_subsequent_trade(&mint, &buy2, 3000);

        let pos = pm.positions.get(&mint).unwrap();
        // Now: buys_since_entry >= 2, confirming_buy_sol = 0.4 SOL >= 0.3,
        // confirming_unique_wallets >= 2, sells = 0, price up ~3.3% > 1.5%,
        // magnitude 60 >= 40 → should be RIDE
        assert!(
            matches!(pos.exit_mode, ExitMode::Ride(_)),
            "Should have transitioned to Ride mode"
        );
    }

    /// Test 4: Low magnitude prevents RIDE transition even with qualifying buys.
    #[test]
    fn test_ride_no_transition_low_magnitude() {
        let mut pm = PositionManager::new(test_config());
        let mint = [3u8; 32];
        let entry_event = make_buy_event(200_000_000, 30_000_000_000);

        // magnitude_estimate = 30.0 (below 40 threshold)
        pm.open_position(&entry_event, 80.0, 1000, 30.0);

        // Feed 3 large qualifying buys with big price movement
        for i in 1..=3 {
            let buy = make_buy_event(200_000_000, 30_000_000_000 + (i * 500_000_000));
            pm.on_subsequent_trade(&mint, &buy, 1000 + i * 1000);
        }

        let pos = pm.positions.get(&mint).expect("position should exist");
        assert!(
            matches!(pos.exit_mode, ExitMode::Scalp(_)),
            "Should remain Scalp — magnitude too low for RIDE"
        );
    }

    /// Test 5: RIDE trailing stop fires when price drops from peak.
    #[test]
    fn test_ride_exit_trailing_stop() {
        let mut pm = PositionManager::new(test_config());
        let mint = [4u8; 32];
        let entry_event = make_buy_event(200_000_000, 30_000_000_000);

        pm.open_position(&entry_event, 80.0, 1000, 60.0);

        // Qualify for RIDE (2 big buys, price up)
        let buy1 = make_buy_event(200_000_000, 30_600_000_000);
        pm.on_subsequent_trade(&mint, &buy1, 2000);
        let buy2 = make_buy_event(200_000_000, 31_500_000_000); // ~5% up
        pm.on_subsequent_trade(&mint, &buy2, 3000);

        // Verify in RIDE mode
        assert!(matches!(
            pm.positions.get(&mint).unwrap().exit_mode,
            ExitMode::Ride(_)
        ));

        // Push price up further (pump continues)
        let buy3 = make_buy_event(100_000_000, 34_000_000_000); // ~13% up from entry
        pm.on_subsequent_trade(&mint, &buy3, 4000);

        // Now price crashes down — multiple sells
        // The trailing stop should fire when price drops enough from peak
        for i in 0..10 {
            let sell = make_sell_event(
                50_000_000,
                34_000_000_000 - ((i + 1) * 1_000_000_000), // dropping 1 SOL per sell
            );
            pm.on_subsequent_trade(&mint, &sell, 5000 + i * 500);
        }

        // Position should be closed with RideTrailingStop
        // (Exact drop needed depends on RideConfig trailing_pct — with defaults,
        //  a ~30% drop from peak should trigger it)
        assert!(
            pm.positions.get(&mint).is_none(),
            "Position should have been closed by trailing stop"
        );

        // Verify the closed position has the right exit reason
        // (Check your closed_positions vec/callback for ExitReason::RideTrailingStop)
    }
}
```

### Test Implementation Notes:

- **Adapt constructors:** The `OpenPosition` and `TradeEvent` structs may have many required fields. Fill them with sensible test defaults. Create builder helpers if needed.
- **Borrow patterns:** If `PositionManager` methods take `&mut self`, you cannot hold references to internal positions across mutable calls. Use temporary lets or re-fetch after each mutable operation.
- **ExitStateMachine in tests:** You'll need to construct a valid `ExitStateMachine` for the `ExitMode::Scalp(sm)` initial state. Use whatever constructor/default is available.
- **Verifying ClosedPosition:** If there's a `Vec<ClosedPosition>` or callback mechanism, assert on `exit_reason` and `exit_mode_str` fields in the closed position record for Test 5.

---

## 1O) Summary Checklist for Engineer 1

- [ ] Add `use crate::engine::ride_state::*` imports
- [ ] Add `ExitMode` enum
- [ ] Replace `exit_sm` field with `exit_mode` + 4 new tracking fields in `OpenPosition`
- [ ] Add 7 new `ExitReason` variants
- [ ] Add 5 new fields to `ClosedPosition`
- [ ] Add `lamports_to_mvsol()` helper
- [ ] Add `map_ride_exit_reason()` helper
- [ ] Add `ride_qualified()` function
- [ ] Add `ride_config` to `PositionConfig`
- [ ] Modify `open_position()` signature + body
- [ ] Rewrite `on_subsequent_trade()` with buy/sell RIDE logic
- [ ] Modify `on_tick()` with RIDE tick handling
- [ ] Modify `close_position_inner()` to populate RIDE metadata
- [ ] Fix ALL compiler errors from `exit_sm` → `exit_mode` migration
- [ ] Add 5 tests
- [ ] `cargo test` passes

---
---

# SECTION 2: Engineer 2 — hot_path.rs V2 Path Completion

**Target file:** `rust/pump-quant-core/src/engine/hot_path.rs`

Read this section top-to-bottom. This is a smaller change set but critical for wiring magnitude through the system.

---

## 2A) Pass magnitude_score to open_position() — V2 path

Find the V2 entry path code block. It calls `engine.evaluate()` which returns an `EntryDecision` (or similar struct). That struct has a `magnitude_score` field (f64, 0–100).

**Find this line (or equivalent):**
```rust
self.position_manager.open_position(trade, decision.entry_score, now);
```

**Replace with:**
```rust
self.position_manager.open_position(trade, decision.entry_score, now, decision.magnitude_score);
```

If `decision` uses a different field name for magnitude, find it. It's the 0–100 float that estimates pump magnitude. The field was added by the V2 entry engine work.

---

## 2B) Update the LEGACY entry path

Find the legacy (V1 / old scoring) entry path. It does NOT have a magnitude estimate.

**Find this line (or equivalent):**
```rust
self.position_manager.open_position(trade, score, now);
```

**Replace with:**
```rust
self.position_manager.open_position(trade, score, now, 0.0);
```

Passing `0.0` means legacy-scored positions will **never** qualify for RIDE (since `ride_qualified()` requires `magnitude_estimate >= 40.0`). This is intentional — legacy scoring doesn't have the signal quality to support RIDE mode.

---

## 2C) Add RiskManager integration

Add a field to the hot path struct (whatever it's called — `HotPath`, `TradeProcessor`, etc.):

```rust
/// Optional risk manager for V2 entry gating.
risk_manager: Option<crate::engine::risk_manager::RiskManager>,
```

Add a setter method:

```rust
/// Set the risk manager for V2 entry gating.
pub fn set_risk_manager(&mut self, rm: crate::engine::risk_manager::RiskManager) {
    self.risk_manager = Some(rm);
}
```

Initialize to `None` in the constructor.

In the V2 path, **before** calling `engine.evaluate()`, add this gate:

```rust
// Risk manager gate — skip entry evaluation if risk budget exhausted
if let Some(ref rm) = self.risk_manager {
    if !rm.allows_entry() {
        return; // or continue, depending on control flow
    }
}
```

**Note:** The `RiskManager` struct itself may not exist yet. If it doesn't:
1. Create a minimal stub at `rust/pump-quant-core/src/engine/risk_manager.rs`
2. Add `pub mod risk_manager;` to `engine/mod.rs`
3. Stub:

```rust
// rust/pump-quant-core/src/engine/risk_manager.rs

/// Risk manager stub — will be fleshed out in a future PR.
/// For now, always allows entry.
#[derive(Debug, Clone)]
pub struct RiskManager {
    _placeholder: (),
}

impl RiskManager {
    pub fn new() -> Self {
        Self { _placeholder: () }
    }

    /// Returns true if we're within risk budget and can open a new position.
    pub fn allows_entry(&self) -> bool {
        true // TODO: implement actual risk checks
    }
}
```

---

## 2D) Tests

Add to the hot_path test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that the V2 path correctly passes magnitude_score through
    /// to the position manager's open_position call.
    #[test]
    fn test_v2_path_passes_magnitude() {
        // Setup: create a HotPath with a mock/test position manager
        // Feed a trade that triggers V2 entry with a known magnitude_score
        // Verify the opened position has that magnitude_estimate

        // Implementation approach:
        // 1. Create HotPath with test config
        // 2. Set up a mock EntryDecision with magnitude_score = 72.5
        // 3. Process a trade that triggers entry
        // 4. Inspect the opened position:
        //    let pos = hot_path.position_manager.positions.values().next().unwrap();
        //    assert!((pos.magnitude_estimate - 72.5).abs() < f64::EPSILON);

        // NOTE: The exact setup depends on how HotPath is constructed and how
        // trades flow through. If HotPath doesn't expose internals, you may need
        // to check the position manager directly or use a test callback.

        todo!("Implement once HotPath test infrastructure is available");
    }
}
```

---

## 2E) Summary Checklist for Engineer 2

- [ ] Update V2 path `open_position()` call to pass `decision.magnitude_score`
- [ ] Update legacy path `open_position()` call to pass `0.0`
- [ ] Add `risk_manager: Option<RiskManager>` field
- [ ] Add `set_risk_manager()` method
- [ ] Add risk gate before `engine.evaluate()` in V2 path
- [ ] Create `risk_manager.rs` stub if it doesn't exist
- [ ] Add `pub mod risk_manager;` to `engine/mod.rs` if needed
- [ ] Add test
- [ ] `cargo test` passes (including positions.rs tests from Engineer 1)

---
---

# SECTION 3: Engineer 3 — paper_logger.rs RIDE Logging

**Target file:** `rust/pump-quant-core/src/persistence/paper_logger.rs`

**Also modifies:** `rust/pump-quant-core/src/engine/positions.rs` (ClosedPosition fields — coordinate with Engineer 1, who adds the struct fields; you add the logging)

Read this section top-to-bottom.

---

## 3A) Add RIDE exit reason strings

Find the `match` statement that converts `ExitReason` to a string for JSONL logging. It currently has arms for the existing SCALP exit reasons.

**Add these arms:**

```rust
ExitReason::RideTrailingStop  => "ride_trailing_stop",
ExitReason::RideHardFloor     => "ride_hard_floor",
ExitReason::RideWhaleExit     => "ride_whale_exit",
ExitReason::RideBuyGapTimeout => "ride_buy_gap_timeout",
ExitReason::RideSellCascade   => "ride_sell_cascade",
ExitReason::RideCreatorSell   => "ride_creator_sell",
ExitReason::RideMaxHold       => "ride_max_hold",
```

Place them after the existing arms, before the closing `}` of the match. If there's a `_ => "unknown"` wildcard, place them **before** it.

---

## 3B) Add RIDE phase name helper

Add this function in `paper_logger.rs` (or inline it):

```rust
/// Convert ride_phase u8 to a human-readable string.
fn ride_phase_name(phase: u8) -> &'static str {
    match phase {
        0 => "n/a",
        1 => "early",
        2 => "momentum",
        3 => "tighten",
        _ => "unknown",
    }
}
```

---

## 3C) Add RIDE fields to JSONL output

Find where the JSONL line is constructed for closed positions. It likely uses `serde_json::json!()` or manual string formatting.

**Add these fields to the JSON object:**

```rust
// Inside the json!() macro or equivalent:
"exitMode": closed.exit_mode_str,
"ridePhase": ride_phase_name(closed.ride_phase),
"ridePeakMvsol": closed.ride_peak_mvsol,
"rideHoldMs": closed.ride_hold_ms,
"rideUniqueWallets": closed.ride_unique_wallets,
```

If using manual string formatting (`format!()`), add:

```rust
format!(
    // ... existing fields ...
    r#","exitMode":"{}","ridePhase":"{}","ridePeakMvsol":{},"rideHoldMs":{},"rideUniqueWallets":{}"#,
    closed.exit_mode_str,
    ride_phase_name(closed.ride_phase),
    closed.ride_peak_mvsol,
    closed.ride_hold_ms,
    closed.ride_unique_wallets,
)
```

**Field semantics for downstream analysis:**

| Field | Type | Description |
|-------|------|-------------|
| `exitMode` | string | `"scalp"` or `"ride"` — which strategy was active at close |
| `ridePhase` | string | `"n/a"`, `"early"`, `"momentum"`, or `"tighten"` — RIDE phase at close |
| `ridePeakMvsol` | u32 | Highest milli-vSOL price seen during RIDE mode. 0 for scalp. |
| `rideHoldMs` | u64 | Duration in RIDE mode (ms). 0 for scalp. |
| `rideUniqueWallets` | u8 | Confirming unique wallets at close. 0 for scalp. |

---

## 3D) Coordinate with Engineer 1: ClosedPosition fields

Engineer 1 adds these fields to the `ClosedPosition` struct in `positions.rs`:

```rust
pub exit_mode_str: &'static str,  // "scalp" or "ride"
pub ride_phase: u8,                // 0=n/a, 1=early, 2=momentum, 3=tighten
pub ride_peak_mvsol: u32,         // 0 for scalp
pub ride_hold_ms: u64,            // 0 for scalp
pub ride_unique_wallets: u8,      // 0 for scalp
```

And populates them in `close_position_inner()`:

```rust
let (exit_mode_str, ride_phase, ride_peak_mvsol, ride_hold_ms, ride_unique_wallets) =
    match &pos.exit_mode {
        ExitMode::Scalp(_) => ("scalp", 0u8, 0u32, 0u64, 0u8),
        ExitMode::Ride(rs) => (
            "ride",
            rs.phase(),
            rs.peak_mvsol(),
            now_ms.saturating_sub(rs.ride_start_ms()),
            pos.confirming_unique_wallets,
        ),
    };
```

**Your job** (Engineer 3) is to READ these fields from `ClosedPosition` and WRITE them to JSONL. You do NOT modify `positions.rs` — that's Engineer 1's file.

If Engineer 1 hasn't merged yet and `ClosedPosition` doesn't have the fields, **add them yourself** to unblock your work, but note in a `// TODO: Engineer 1 also adds these — deduplicate at merge` comment.

---

## 3E) Example JSONL output

A SCALP exit line should look like (new fields at end):

```json
{"type":"close","mint":"...","entryScore":78.5,"exitReason":"scalp_take_profit","pnlPct":2.3,"holdMs":4500,"exitMode":"scalp","ridePhase":"n/a","ridePeakMvsol":0,"rideHoldMs":0,"rideUniqueWallets":0}
```

A RIDE exit line:

```json
{"type":"close","mint":"...","entryScore":82.1,"exitReason":"ride_trailing_stop","pnlPct":8.7,"holdMs":12300,"exitMode":"ride","ridePhase":"tighten","ridePeakMvsol":34500,"rideHoldMs":9200,"rideUniqueWallets":5}
```

---

## 3F) Summary Checklist for Engineer 3

- [ ] Add 7 RIDE exit reason string arms to the match
- [ ] Add `ride_phase_name()` helper
- [ ] Add 5 RIDE fields to JSONL close output
- [ ] Verify `ClosedPosition` has the required fields (coordinate with Engineer 1)
- [ ] Test with `cargo test` — at minimum verify the exit reason strings compile
- [ ] Manually verify JSONL output format with a debug print or test

---
---

# CROSS-CUTTING CONCERNS

## Build Order

The three engineers can work **in parallel** on separate branches, but merge order matters:

1. **Engineer 1 (positions.rs)** merges first — defines the types everyone depends on
2. **Engineer 2 (hot_path.rs)** merges second — depends on new `open_position()` signature
3. **Engineer 3 (paper_logger.rs)** merges last — depends on `ClosedPosition` fields from Engineer 1

If working in parallel, Engineers 2 and 3 should expect merge conflicts in `positions.rs` types and resolve by taking Engineer 1's version.

## Module Dependency Graph

```
hot_path.rs
  └─► positions.rs (open_position, on_subsequent_trade, on_tick)
        ├─► exit_machine.rs (ExitStateMachine — existing, unchanged)
        └─► ride_state.rs (RideState — new, from RIDE_SPEC_PART_B)
  └─► risk_manager.rs (new stub)

paper_logger.rs
  └─► positions.rs (ClosedPosition, ExitReason)
```

## Naming Conventions

- `mvsol` = milli-virtual-SOL = virtual SOL reserves / 1000 (stored as `u32`, millionths of SOL)
- `lamports` = 1e-9 SOL (the native Solana unit)
- Conversion: `lamports_to_mvsol(lamports) = (lamports + 500_000) / 1_000_000`
  - 1 mvsol = 0.001 SOL = 1_000_000 lamports
- `confirming_buy_sol` is stored in **lamports** (u64)
- `confirming_unique_wallets` uses `u8` — max 255, which is sufficient

## Error Handling

- No panics. Use `saturating_add` for counter increments.
- `close_position_inner` must handle the case where the position has already been removed (guard with `let Some(pos) = ... else { return }`).
- RIDE state machine methods return `RideAction::Hold` or `RideAction::Exit(reason)`. Never unwrap these — always pattern match.

## Performance Notes

- `lamports_to_mvsol()` is called on every trade event — it's a single division, no allocation. Fine.
- `ride_qualified()` is called on every buy event while in Scalp mode. It's 6 comparisons. Fine.
- The `ExitMode` enum is 2 variants. `match` compiles to a branch. No vtable overhead.
- `ClosedPosition` gains 18 bytes (1 + 1 + 4 + 8 + 1 + padding). Negligible.

---

**END OF RIDE_SPEC_PART_A**