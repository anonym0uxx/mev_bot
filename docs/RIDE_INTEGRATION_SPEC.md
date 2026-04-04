# RIDE Integration Spec — 5 Engineer Build Plan

**Date:** 2026-03-29
**Status:** Implementation-ready
**Constraint:** Zero heap allocation on hot path. All 261 existing tests must pass. `#[inline(always)]` on hot-path functions, `#[cold]` on error/rejection paths.

---

## Engineer 1: positions.rs — ExitMode + RIDE Routing (MOST CRITICAL)

### Target file
`rust/pump-quant-core/src/engine/positions.rs`

### Action
MODIFY

### Dependencies
Add these imports at the top of the file (alongside existing imports):
```rust
use super::ride_state::{RideState, RideConfig as RideStateConfig, RideDecision, RideExitReason, RidePhase, lamports_to_mvsol};
use super::entry_engine::EntryAction;
```

### Specification

#### A. Add `ExitMode` enum (before `OpenPosition` struct)

```rust
/// Exit strategy mode. SCALP uses the existing signal-based ExitStateMachine.
/// RIDE uses the integer vSOL trailing-stop RideState engine.
/// Both stored inline — no heap allocation.
pub enum ExitMode {
    Scalp(crate::engine::exit_machine::ExitStateMachine),
    Ride(RideState),
}
```

#### B. Add RIDE variants to `ExitReason`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    TakeProfit,
    StopLoss,
    NextBuyer,
    MaxHold,
    IntraHoldTrail,
    MomentumDecayFlat,
    MomentumDecayFade,
    TakeProfitScaled,
    MomentumStall,
    // ── RIDE mode exit reasons ──
    RideTrailingStop,
    RideHardFloor,
    RideWhaleExit,
    RideBuyGapTimeout,
    RideSellCascade,
    RideCreatorSell,
    RideMaxHold,
}
```

#### C. Modify `OpenPosition` struct

Replace `pub exit_sm: crate::engine::exit_machine::ExitStateMachine,` with:

```rust
    /// Exit strategy: SCALP (ExitStateMachine) or RIDE (RideState).
    pub exit_mode: ExitMode,
    /// Magnitude estimate from EntryEngine (0-100). >= 40 = RIDE candidate.
    pub magnitude_estimate: f64,
    /// Accumulated confirming buy SOL since entry (lamports).
    pub confirming_buy_sol: u64,
    /// Unique wallets counter (simple increment, accepts overcount).
    pub confirming_unique_wallets: u8,
    /// Sell events during SCALP hold. Must be 0 for RIDE qualification.
    pub sells_during_hold: u16,
```

All other fields remain unchanged.

#### D. Add RIDE fields to `ClosedPosition`

After the `tod_multiplier` field, add:

```rust
    /// True if position was in RIDE mode at exit.
    pub is_ride: bool,
    /// RIDE phase at exit (0=Early, 1=Momentum, 2=Tighten). 0 for SCALP.
    pub ride_phase: u8,
    /// RIDE peak vSOL in mvsol. 0 for SCALP.
    pub ride_peak_mvsol: u32,
    /// RIDE trail stop mvsol at exit. 0 for SCALP.
    pub ride_trail_stop_mvsol: u32,
    /// ms from RIDE activation to exit. 0 for SCALP.
    pub ride_hold_ms: u64,
    /// Unique wallets during RIDE. 0 for SCALP.
    pub ride_unique_wallets: u8,
    /// Magnitude estimate from entry engine. 0.0 for legacy.
    pub magnitude_estimate: f64,
```

#### E. Add `ride_config` to `PositionConfig`

After the `exit_config` field in `PositionConfig`:

```rust
    /// RIDE mode configuration. When Some, RIDE transitions are enabled.
    pub ride_config: Option<RideStateConfig>,
```

(Note: `RideStateConfig` is the `RideConfig` from `ride_state.rs`, imported as `RideStateConfig` to avoid name collision with `config.rs::RideConfig`.)

#### F. Add helper functions (after `map_exit_reason_new`)

```rust
/// Check if a SCALP position qualifies for RIDE transition.
/// All thresholds are hard-coded (zero-config, zero-alloc).
#[inline(always)]
fn ride_qualified(pos: &OpenPosition) -> bool {
    if pos.magnitude_estimate < 40.0 { return false; }
    if pos.buys_since_entry < 2 { return false; }
    if pos.sells_during_hold > 0 { return false; }
    if pos.confirming_buy_sol < 300_000_000 { return false; }
    if pos.confirming_unique_wallets < 2 { return false; }
    // 1.5% gain: current × 10000 > entry × 10150
    let current_fp = pos.current_vsol as u128 * 10_000;
    let threshold_fp = pos.entry_vsol as u128 * 10_150;
    current_fp > threshold_fp
}

#[inline(always)]
fn map_ride_exit_reason(r: RideExitReason) -> ExitReason {
    match r {
        RideExitReason::TrailingStop => ExitReason::RideTrailingStop,
        RideExitReason::HardFloor => ExitReason::RideHardFloor,
        RideExitReason::WhaleExit => ExitReason::RideWhaleExit,
        RideExitReason::BuyGapTimeout => ExitReason::RideBuyGapTimeout,
        RideExitReason::SellCascade => ExitReason::RideSellCascade,
        RideExitReason::CreatorSell => ExitReason::RideCreatorSell,
        RideExitReason::MaxHold => ExitReason::RideMaxHold,
    }
}
```

#### G. Modify `open_position()` signature and body

New signature:
```rust
pub fn open_position(
    &mut self,
    event: &TradeEvent,
    score: f64,
    now_ms: u64,
    magnitude_estimate: f64,
    _entry_action: Option<EntryAction>,
) {
```

Body changes (position initialization):
- Change `exit_sm` initialization to wrap in `ExitMode::Scalp(...)`:
  ```rust
  let exit_sm = crate::engine::exit_machine::ExitStateMachine::on_entry(
      &self.config.exit_config, trigger_sol, event.vsol_reserves as f64, now_ms,
  );
  ```
- In the `OpenPosition` struct literal, replace `exit_sm,` with:
  ```rust
  exit_mode: ExitMode::Scalp(exit_sm),
  magnitude_estimate,
  confirming_buy_sol: 0,
  confirming_unique_wallets: 0,
  sells_during_hold: 0,
  ```

#### H. Replace `on_subsequent_trade()` entirely

```rust
#[inline(always)]
pub fn on_subsequent_trade(&mut self, event: &TradeEvent, now_ms: u64) -> bool {
    if event.vsol_reserves == 0 { return false; }
    let pos = match self.positions.get_mut(&event.mint) {
        Some(p) => p, None => return false,
    };
    if event.sig == pos.trigger_sig { return false; }

    // Update shared state
    pos.trades_seen_after_entry += 1;
    pos.current_vsol = event.vsol_reserves;
    pos.current_vtokens = event.vtoken_reserves;
    if event.vsol_reserves > pos.peak_vsol { pos.peak_vsol = event.vsol_reserves; }
    if event.vsol_reserves < pos.trough_vsol { pos.trough_vsol = event.vsol_reserves; }
    if event.is_buy {
        pos.flow_since_entry += event.sol_amount;
        pos.buys_since_entry += 1;
    }

    let mint = event.mint;
    let is_ride = matches!(self.positions.get(&mint).unwrap().exit_mode, ExitMode::Ride(_));

    if is_ride {
        self.on_subsequent_trade_ride(&mint, event, now_ms)
    } else {
        self.on_subsequent_trade_scalp(&mint, event, now_ms)
    }
}
```

#### I. Add `on_subsequent_trade_scalp()` (new private method)

```rust
#[inline(always)]
fn on_subsequent_trade_scalp(&mut self, mint: &[u8; 32], event: &TradeEvent, now_ms: u64) -> bool {
    let pos = self.positions.get_mut(mint).unwrap();

    if event.is_buy {
        pos.confirming_buy_sol += event.sol_amount;
        pos.confirming_unique_wallets = pos.confirming_unique_wallets.saturating_add(1);

        // Check RIDE qualification
        if self.config.ride_config.is_some() && ride_qualified(pos) {
            let entry_mvsol = lamports_to_mvsol(pos.entry_vsol);
            let current_mvsol = lamports_to_mvsol(pos.current_vsol);
            let buy_rate_5s = pos.buys_since_entry.min(u16::MAX as u32) as u16;
            let ride_config = self.config.ride_config.as_ref().unwrap();
            let rs = RideState::new(entry_mvsol, current_mvsol, now_ms, buy_rate_5s, ride_config);
            let pos = self.positions.get_mut(mint).unwrap();
            pos.exit_mode = ExitMode::Ride(rs);
            // Immediate tick
            if let ExitMode::Ride(ref mut rs) = self.positions.get_mut(mint).unwrap().exit_mode {
                let ride_config = self.config.ride_config.as_ref().unwrap();
                let d = rs.on_tick(current_mvsol, now_ms, ride_config);
                if let RideDecision::Exit(reason) = d {
                    self.close_position_inner(mint, map_ride_exit_reason(reason), now_ms);
                    return true;
                }
            }
            return false;
        }

        // Feed buy to SCALP ExitStateMachine
        if let ExitMode::Scalp(ref mut sm) = self.positions.get_mut(mint).unwrap().exit_mode {
            let d = sm.on_buy_event(&self.config.exit_config, event.vsol_reserves as f64, now_ms);
            if let crate::engine::exit_machine::ExitDecision::Exit(reason) = d {
                self.close_position_inner(mint, map_exit_reason_new(reason), now_ms);
                return true;
            }
        }
    } else {
        pos.sells_during_hold += 1;
    }

    // Feed price tick to SCALP ExitStateMachine
    if let ExitMode::Scalp(ref mut sm) = self.positions.get_mut(mint).unwrap().exit_mode {
        let d = sm.on_price_tick(&self.config.exit_config, event.vsol_reserves as f64, now_ms);
        if let crate::engine::exit_machine::ExitDecision::Exit(reason) = d {
            self.close_position_inner(mint, map_exit_reason_new(reason), now_ms);
            return true;
        }
    }
    false
}
```

#### J. Add `on_subsequent_trade_ride()` (new private method)

```rust
#[inline(always)]
fn on_subsequent_trade_ride(&mut self, mint: &[u8; 32], event: &TradeEvent, now_ms: u64) -> bool {
    let current_mvsol = lamports_to_mvsol(event.vsol_reserves);

    // Process buy/sell event
    if let ExitMode::Ride(ref mut rs) = self.positions.get_mut(mint).unwrap().exit_mode {
        if event.is_buy {
            rs.on_buy_event(lamports_to_mvsol(event.sol_amount), now_ms);
        } else {
            let ride_config = self.config.ride_config.as_ref().unwrap();
            if let Some(reason) = rs.on_sell_event(lamports_to_mvsol(event.sol_amount), now_ms, ride_config) {
                self.close_position_inner(mint, map_ride_exit_reason(reason), now_ms);
                return true;
            }
        }
    }

    // Run on_tick for trail stop / phase transitions
    if let ExitMode::Ride(ref mut rs) = self.positions.get_mut(mint).unwrap().exit_mode {
        let ride_config = self.config.ride_config.as_ref().unwrap();
        let d = rs.on_tick(current_mvsol, now_ms, ride_config);
        if let RideDecision::Exit(reason) = d {
            self.close_position_inner(mint, map_ride_exit_reason(reason), now_ms);
            return true;
        }
    }
    false
}
```

#### K. Replace `on_tick()`

```rust
pub fn on_tick(&mut self, now_ms: u64) {
    let mut to_close: Vec<([u8; 32], ExitReason)> = Vec::new();

    // Max hold safety for SCALP positions only
    for (mint, pos) in self.positions.iter() {
        if matches!(pos.exit_mode, ExitMode::Scalp(_)) {
            if now_ms.saturating_sub(pos.entry_ts_ms) >= self.config.max_hold_ms {
                to_close.push((*mint, ExitReason::MaxHold));
            }
        }
    }
    for (mint, reason) in to_close {
        self.close_position_inner(&mint, reason, now_ms);
    }

    // SCALP: ExitStateMachine tick
    let exit_config = &self.config.exit_config;
    let mut sm_closes: Vec<([u8; 32], ExitReason)> = Vec::new();
    for (mint, pos) in self.positions.iter_mut() {
        if let ExitMode::Scalp(ref mut sm) = pos.exit_mode {
            let d = sm.on_price_tick(exit_config, pos.current_vsol as f64, now_ms);
            if let crate::engine::exit_machine::ExitDecision::Exit(reason) = d {
                sm_closes.push((*mint, map_exit_reason_new(reason)));
            }
        }
    }
    for (mint, reason) in sm_closes {
        self.close_position_inner(&mint, reason, now_ms);
    }

    // RIDE: RideState tick
    if let Some(ref ride_config) = self.config.ride_config {
        let mut ride_closes: Vec<([u8; 32], ExitReason)> = Vec::new();
        for (mint, pos) in self.positions.iter_mut() {
            if let ExitMode::Ride(ref mut rs) = pos.exit_mode {
                let mvsol = lamports_to_mvsol(pos.current_vsol);
                let d = rs.on_tick(mvsol, now_ms, ride_config);
                if let RideDecision::Exit(reason) = d {
                    ride_closes.push((*mint, map_ride_exit_reason(reason)));
                }
            }
        }
        for (mint, reason) in ride_closes {
            self.close_position_inner(&mint, reason, now_ms);
        }
    }
}
```

#### L. Replace `close_position_inner()`

In the `ClosedPosition` construction, extract RIDE state before building:

```rust
fn close_position_inner(&mut self, mint: &[u8; 32], reason: ExitReason, now_ms: u64) {
    let pos = match self.positions.remove(mint) { Some(p) => p, None => return };
    let hold_ms = now_ms.saturating_sub(pos.entry_ts_ms);
    let exit_vsol = pos.current_vsol;
    let gross_pnl_sol = if pos.entry_vsol > 0 {
        let delta = exit_vsol as i128 - pos.entry_vsol as i128;
        (delta * pos.size_sol as i128 / pos.entry_vsol as i128) as i64
    } else { 0 };
    let pump_fees = pos.size_sol * 2 / 100;
    let jito_fees = self.config.jito_tip_lamports * 2;
    let total_fees = pump_fees + jito_fees;
    let net_pnl_sol = gross_pnl_sol - total_fees as i64;

    let (is_ride, ride_phase, ride_peak_mvsol, ride_trail_stop_mvsol, ride_hold_ms, ride_unique_wallets) =
        match &pos.exit_mode {
            ExitMode::Ride(rs) => (true, rs.phase, rs.peak_mvsol, rs.trail_stop_mvsol,
                now_ms.saturating_sub(rs.ride_start_ms), rs.unique_wallets),
            ExitMode::Scalp(_) => (false, 0, 0, 0, 0, 0),
        };

    let closed = ClosedPosition {
        mint: pos.mint, entry_vsol: pos.entry_vsol, exit_vsol,
        entry_ts_ms: pos.entry_ts_ms, exit_ts_ms: now_ms, hold_ms,
        size_sol: pos.size_sol, gross_pnl_sol, net_pnl_sol, fees_sol: total_fees,
        exit_reason: reason, score: pos.score, tokens_held: pos.tokens_held,
        current_vtokens: pos.current_vtokens, current_vsol: pos.current_vsol,
        bonding_curve: pos.bonding_curve, assoc_bonding_curve: pos.assoc_bonding_curve,
        peak_vsol: pos.peak_vsol, trough_vsol: pos.trough_vsol, trigger_sol: pos.trigger_sol,
        trades_after_entry: pos.trades_seen_after_entry, buys_after_entry: pos.buys_since_entry,
        flow_after_entry: pos.flow_since_entry,
        pre_trigger_buys_1s: pos.pre_trigger_buys_1s, pre_trigger_buys_2s: pos.pre_trigger_buys_2s,
        pre_trigger_buys_5s: pos.pre_trigger_buys_5s, unique_buyers: pos.unique_buyers,
        vsol_delta_3s: pos.vsol_delta_3s, volume_5s: pos.volume_5s, sell_count_5s: pos.sell_count_5s,
        tod_multiplier: pos.tod_multiplier,
        is_ride, ride_phase, ride_peak_mvsol, ride_trail_stop_mvsol,
        ride_hold_ms, ride_unique_wallets, magnitude_estimate: pos.magnitude_estimate,
    };
    let _ = self.closed_tx.try_send(closed);
}
```

#### M. Update existing tests

1. In `test_config()`, add `ride_config: None,` to the `PositionConfig` struct literal.

2. **Every** call to `pm.open_position(&event, 0.85, 1000)` becomes:
   ```rust
   pm.open_position(&event, 0.85, 1000, 0.0, None);
   ```
   This applies to these test functions: `test_open_position`, `test_skip_trigger_event`, `test_skip_zero_reserves`, `test_max_hold_exit`, `test_stop_loss_exit`, `test_take_profit_exit`, `test_momentum_decay_flat`, `test_momentum_decay_fade`, `test_close_all`, `test_max_concurrent_positions`, `test_pnl_calculation`, `test_nb_exit_requires_min_hold_and_trades`.

### Tests

Add these tests to the existing `mod tests`:

```rust
#[test]
fn test_ride_qualified_rejects_low_magnitude() {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut pm = PositionManager::new(test_config(), tx);
    let mint = [0xAAu8; 32];
    let sig = [0xBBu8; 64];
    let event = make_trade_event(mint, sig, 50_000_000, 30_000_000_000, 1_000_000_000_000_000, true);
    pm.open_position(&event, 0.85, 1000, 30.0, Some(EntryAction::Scalp)); // magnitude < 40
    let pos = pm.get_position_mut(&mint).unwrap();
    pos.buys_since_entry = 5;
    pos.confirming_buy_sol = 500_000_000;
    pos.confirming_unique_wallets = 3;
    pos.current_vsol = (30_000_000_000f64 * 1.02) as u64;
    assert!(!ride_qualified(pos)); // magnitude 30 < 40 threshold
}

#[test]
fn test_ride_qualified_rejects_sells_during_hold() {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut pm = PositionManager::new(test_config(), tx);
    let mint = [0xAAu8; 32];
    let sig = [0xBBu8; 64];
    let event = make_trade_event(mint, sig, 50_000_000, 30_000_000_000, 1_000_000_000_000_000, true);
    pm.open_position(&event, 0.85, 1000, 60.0, Some(EntryAction::Ride));
    let pos = pm.get_position_mut(&mint).unwrap();
    pos.buys_since_entry = 3;
    pos.confirming_buy_sol = 500_000_000;
    pos.confirming_unique_wallets = 3;
    pos.sells_during_hold = 1; // disqualifies
    pos.current_vsol = (30_000_000_000f64 * 1.02) as u64;
    assert!(!ride_qualified(pos));
}

#[test]
fn test_ride_qualified_accepts_full_qualification() {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut config = test_config();
    config.ride_config = Some(RideStateConfig::default());
    let mut pm = PositionManager::new(config, tx);
    let mint = [0xAAu8; 32];
    let sig = [0xBBu8; 64];
    let event = make_trade_event(mint, sig, 50_000_000, 30_000_000_000, 1_000_000_000_000_000, true);
    pm.open_position(&event, 0.85, 1000, 60.0, Some(EntryAction::Ride));
    let pos = pm.get_position_mut(&mint).unwrap();
    pos.buys_since_entry = 3;
    pos.confirming_buy_sol = 500_000_000;
    pos.confirming_unique_wallets = 3;
    pos.sells_during_hold = 0;
    pos.current_vsol = (30_000_000_000f64 * 1.02) as u64; // +2% > 1.5% threshold
    assert!(ride_qualified(pos));
}

#[test]
fn test_scalp_to_ride_transition() {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut config = test_config();
    config.ride_config = Some(RideStateConfig::default());
    config.max_hold_ms = 300_000; // don't interfere
    let mut pm = PositionManager::new(config, tx);
    let mint = [0xAAu8; 32];
    let sig = [0xBBu8; 64];
    let entry_vsol = 30_000_000_000u64;
    let event = make_trade_event(mint, sig, 50_000_000, entry_vsol, 1_000_000_000_000_000, true);
    pm.open_position(&event, 0.85, 1000, 60.0, Some(EntryAction::Ride));

    // Starts as SCALP
    assert!(matches!(pm.get_position_mut(&mint).unwrap().exit_mode, ExitMode::Scalp(_)));

    // Feed confirming buys to qualify for RIDE
    let up_vsol = (entry_vsol as f64 * 1.02) as u64;
    for i in 0..3u8 {
        let buy = make_trade_event(mint, [0xC0 + i; 64], 200_000_000, up_vsol, 1_000_000_000_000_000, true);
        pm.on_subsequent_trade(&buy, 1100 + i as u64 * 10);
    }

    // Should have transitioned to RIDE
    if let Some(pos) = pm.get_position_mut(&mint) {
        assert!(matches!(pos.exit_mode, ExitMode::Ride(_)), "Expected RIDE mode after qualifying buys");
    }
}

#[test]
fn test_ride_closed_position_fields() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut config = test_config();
    config.ride_config = Some(RideStateConfig::default());
    config.max_hold_ms = 300_000;
    let mut pm = PositionManager::new(config, tx);
    let mint = [0xAAu8; 32];
    let sig = [0xBBu8; 64];
    let entry_vsol = 30_000_000_000u64;
    let event = make_trade_event(mint, sig, 50_000_000, entry_vsol, 1_000_000_000_000_000, true);
    pm.open_position(&event, 0.85, 1000, 60.0, Some(EntryAction::Ride));

    // Force into RIDE and then force close
    let up_vsol = (entry_vsol as f64 * 1.02) as u64;
    for i in 0..3u8 {
        let buy = make_trade_event(mint, [0xC0 + i; 64], 200_000_000, up_vsol, 1_000_000_000_000_000, true);
        pm.on_subsequent_trade(&buy, 1100 + i as u64 * 10);
    }

    pm.force_close(&mint, ExitReason::RideMaxHold, 2000);
    let cp = rx.try_recv().unwrap();
    assert!(cp.is_ride);
    assert_eq!(cp.exit_reason, ExitReason::RideMaxHold);
    assert!(cp.ride_peak_mvsol > 0);
    assert!((cp.magnitude_estimate - 60.0).abs() < 0.001);
}

#[test]
fn test_scalp_closed_position_ride_fields_zero() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut pm = PositionManager::new(test_config(), tx);
    let mint = [0xAAu8; 32];
    let sig = [0xBBu8; 64];
    let event = make_trade_event(mint, sig, 50_000_000, 30_000_000_000, 1_000_000_000_000_000, true);
    pm.open_position(&event, 0.85, 1000, 0.0, None);
    pm.force_close(&mint, ExitReason::MaxHold, 2000);
    let cp = rx.try_recv().unwrap();
    assert!(!cp.is_ride);
    assert_eq!(cp.ride_phase, 0);
    assert_eq!(cp.ride_peak_mvsol, 0);
    assert_eq!(cp.ride_hold_ms, 0);
}
```

---

## Engineer 2: hot_path.rs — V2 Path Completion + RiskManager

### Target file
`rust/pump-quant-core/src/engine/hot_path.rs`

### Action
MODIFY

### Dependencies
Add this import at the top (alongside existing imports):
```rust
use super::risk_manager::RiskManager;
```

### Specification

#### A. Add `risk_manager` field to `HotPath` struct

After the `entry_engine: Option<EntryEngine>,` field:

```rust
    /// V2 risk manager (replaces inline daily_loss + circuit breaker when Some).
    risk_manager: Option<RiskManager>,
```

#### B. Initialize in `HotPath::new()`

In the `Self { ... }` block, after `entry_engine: None,`:

```rust
    risk_manager: None,
```

#### C. Add `set_risk_manager()` method

After `set_entry_engine()`:

```rust
    /// Set the V2 risk manager. When set, V2 entry path uses RiskManager
    /// instead of the inline daily_loss + circuit breaker checks.
    pub fn set_risk_manager(&mut self, rm: RiskManager) {
        self.risk_manager = Some(rm);
    }
```

#### D. Modify V2 entry engine path in `on_trade()`

Find the V2 entry engine block (starts with `if let Some(ref engine) = self.entry_engine {`).

Replace the block from `EntryAction::Scalp | EntryAction::Ride =>` up to `return;` with:

```rust
                EntryAction::Scalp | EntryAction::Ride => {
                    self.stats.gates_passed += 1;

                    // Determine if this is a RIDE-intent entry
                    let is_ride_intent = matches!(decision.action, EntryAction::Ride);

                    // V2 risk manager gate (replaces inline safety checks)
                    if let Some(ref rm) = self.risk_manager {
                        if !rm.allows_entry(now, is_ride_intent) {
                            return;
                        }
                    } else {
                        // Fallback: inline safety checks (legacy)
                        self.check_and_reset_daily_loss(now);
                        if self.daily_loss_lamports as u64 >= self.daily_loss_cap_lamports {
                            return;
                        }
                        if now < self.stop_pause_until_ms {