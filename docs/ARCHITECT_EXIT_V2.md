# ARCHITECT_EXIT_V2.md — Signal-Based Exit State Machine

_Architecture spec for parallel Rust engineers. Based on EXIT_STRATEGY_QUANT.md._
_Author: Apollo. All engineers implement from this spec — no design decisions left open._

---

## 1. Architecture Overview

Replace all ms-timer-based exits with an event-driven `ExitStateMachine` struct embedded inline in the existing `Position` struct. The machine transitions on buy events and price ticks, not on tokio sleep loops.

**Design invariants:**
- Zero heap allocation per tick — all fields are `Copy`, no `Vec`, no `Box`
- Struct size ≤ 64 bytes (one cache line)
- `on_buy_event()` and `on_price_tick()` ≤ 100ns each
- One tokio safety timer per position (5000ms), cancelled on exit
- All other exits are event-triggered — no polling

**What changes vs current code:**
| Old | New |
|-----|-----|
| `max_hold_ms=1500` as primary exit | 5000ms safety-net only (`MaxHoldSafety`) |
| `momentum_decay_check_ms` loop | 200ms confirmation window, event-driven |
| `next_buyer` exit (anti-pattern) | Triggering buy → CONFIRMED transition instead |
| `intra_hold_trailing_stop` (always on) | Trailing stop only at conviction≥2 |
| Flat TP/SL regardless of confirmation | Split: unconfirmed (tight) / confirmed (wide) |
| Fixed TP regardless of buysAfterEntry | TP scales 1.0×/1.4×/1.8×/2.2× by conviction |

---

## 2. File Ownership — Zero Merge Conflicts

**Three engineers, zero shared files.**

| Engineer | Files Owned | Action |
|----------|------------|--------|
| A | `engine/exit_machine.rs` | CREATE (new file) |
| B | `engine/positions.rs` | MODIFY ONLY |
| C | `engine/config.rs` + `config/canary.json` | MODIFY ONLY |

Engineer A delivers `exit_machine.rs` first. Engineers B and C work in parallel after A's interface is locked. B depends on A's types; C is fully independent.

Sequencing:
1. Engineer A: implement `exit_machine.rs`, confirm it compiles standalone
2. Engineers B + C: parallel, both depend on A's output
3. Final: single `cargo build` integrates all three

---

## 3. Engineer A — `engine/exit_machine.rs` (NEW FILE)

Engineer A creates this file from scratch. No other file is touched.

### Structs and Enums

```rust
use std::time::Instant;

/// Conviction level: how many confirming buys arrived after entry.
/// Stored as u8 (0-4). Level 0 = unconfirmed. Level 4+ clamped.
pub type ConvictionLevel = u8;

/// Exit state machine state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitState {
    /// No confirming buy yet. Position may be dead.
    Unconfirmed,
    /// At least 1 confirming buy. Momentum confirmed.
    Confirmed,
    /// 2+ confirming buys. TP scaled up by conviction multiplier.
    ConvictionScaled { level: ConvictionLevel },
}

/// Result of a state machine tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExitDecision {
    Hold,
    Exit(ExitReasonNew),
}

/// New exit reasons (replaces parts of old ExitReason enum).
/// Engineer B maps these back to the existing ExitReason enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReasonNew {
    TakeProfit,
    TakeProfitScaled,        // TP hit at conviction-scaled level
    StopLoss,
    MomentumDecayFlat,       // No confirming buy by confirmation_window_ms
    MomentumStall,           // Confirmed but stalled: no buy + price fading
    TrailingStop,            // Conviction>=2 trailing stop triggered
    MaxHoldSafety,           // 5000ms safety backstop
}

/// TP/SL tier — now split into unconfirmed and confirmed levels.
/// trigger_max_lamports: if trigger_sol <= this, use this tier.
#[derive(Debug, Clone, Copy)]
pub struct TpSlTierV2 {
    pub trigger_max_lamports: u64,
    /// Unconfirmed state TP (tighter — grab fast gains before confirmation)
    pub unconfirmed_tp_fp: u32,  // fixed-point: actual = value / 100_000 (e.g. 2000 = 2.0%)
    /// Unconfirmed state SL (tighter — cut fast on dead positions)
    pub unconfirmed_sl_fp: u32,
    /// Confirmed state TP (base, buysAfter=1)
    pub confirmed_tp_fp: u32,
    /// Confirmed state SL
    pub confirmed_sl_fp: u32,
}

/// Full exit config — passed by reference to ExitStateMachine::on_entry().
#[derive(Debug, Clone)]
pub struct ExitConfig {
    /// How long to wait for confirming buy before declaring position dead (ms).
    /// Recommended: 200. Data: p50 of TP hold = 175ms.
    pub confirmation_window_ms: u64,

    /// Signal-based stall: no new buy for this long + price fading → exit (CONFIRMED state).
    pub stall_no_buy_ms: u64,           // recommended: 500
    /// Price fade threshold for stall: if price < peak * (1 - fade_pct) → stall condition met.
    pub stall_fade_fp: u32,             // fixed-point /100_000, recommended: 1000 = 1.0%

    /// Same stall params for CONVICTION_SCALED state (more generous).
    pub stall_conviction_no_buy_ms: u64, // recommended: 800
    pub stall_conviction_fade_fp: u32,   // recommended: 1500 = 1.5%

    /// Safety net timer (ms). All positions exit by this time at the latest.
    pub max_hold_safety_ms: u64,         // recommended: 5000

    /// Conviction TP multipliers indexed by conviction level (0-4).
    /// Level 0: 1.0 (= 100), Level 1: 1.0 (= 100), Level 2: 1.4 (= 140),
    /// Level 3: 1.8 (= 180), Level 4+: 2.2 (= 220).
    /// Stored as u16 fixed-point /100.
    pub conviction_tp_multipliers: [u16; 5], // [100, 100, 140, 180, 220]

    /// Trailing stop: minimum conviction level to activate.
    pub trail_min_conviction: u8,         // recommended: 2

    /// Trailing stop activation: what % of the base TP must be reached first.
    /// e.g. 60 means "activate trail when price >= entry + 60% of base_tp".
    pub trail_activation_pct_of_base_tp: u8, // recommended: 60

    /// Trailing stop distance from high water mark (fixed-point /100_000).
    pub trail_distance_fp: u32,           // recommended: 1500 = 1.5%

    /// TP/SL tiers (up to 8, checked in order, first match used).
    pub tp_sl_tiers: [TpSlTierV2; 8],
    pub tp_sl_tier_count: u8,
}

/// The state machine. Embedded inline in Position struct (no heap).
/// Size target: ≤ 64 bytes.
#[derive(Debug, Clone, Copy)]
pub struct ExitStateMachine {
    // State
    pub state: ExitState,                // 1 byte (enum)
    pub conviction_level: u8,            // 0-4

    // Prices (f64 — required for accuracy)
    pub entry_price_vsol: f64,           // vSol at entry (lamports as f64)
    pub peak_price_vsol: f64,            // high water mark for trailing stop
    pub trail_stop_price: f64,           // trailing stop level (0 = inactive)

    // Computed TP/SL levels (lamports, derived from entry_price * pct)
    pub current_tp_vsol: f64,           // entry_price * (1 + tp_pct), updated on conviction
    pub current_sl_vsol: f64,           // entry_price * (1 - sl_pct), fixed

    // Base TP (for conviction scaling — keep to compute scaled levels)
    pub base_confirmed_tp_fp: u32,      // from tier, fixed-point /100_000

    // Timing (ms since epoch as u64 — smaller than Instant on most targets)
    pub entry_time_ms: u64,
    pub last_buy_time_ms: u64,          // 0 = no buy yet
    pub confirmed_at_ms: u64,           // 0 = not confirmed

    // Flags
    pub trail_active: bool,
    pub _pad: [u8; 3],                  // padding for alignment
}
// Static assertion: ExitStateMachine must fit in 64 bytes.
// Add this after the struct:
// const _: () = assert!(std::mem::size_of::<ExitStateMachine>() <= 64);
```

**Size check:** 2 (state+conviction) + 5×8 (f64s) + 3×8 (u64s) + 1×4 (u32) + 4 (bool+pad) = 2 + 40 + 24 + 4 + 4 = 74 bytes. Drop `confirmed_at_ms` (derivable from entry+window) → 66 bytes. Drop `trail_stop_price` if trail computed from `peak_price_vsol` inline → 58 bytes ✓. **Engineer A: eliminate `confirmed_at_ms` and `trail_stop_price` — compute them on the fly from `peak_price_vsol` and `entry_time_ms`. See implementation notes.**

### Method Implementations

#### `ExitStateMachine::on_entry()`

```rust
pub fn on_entry(config: &ExitConfig, trigger_lamports: u64, entry_vsol: f64, now_ms: u64) -> Self {
    // Find tier
    let tier = find_tier(&config.tp_sl_tiers[..config.tp_sl_tier_count as usize], trigger_lamports);

    // Initial TP/SL: unconfirmed (tighter)
    let tp_pct = tier.unconfirmed_tp_fp as f64 / 100_000.0;
    let sl_pct = tier.unconfirmed_sl_fp as f64 / 100_000.0;

    Self {
        state: ExitState::Unconfirmed,
        conviction_level: 0,
        entry_price_vsol: entry_vsol,
        peak_price_vsol: entry_vsol,
        trail_stop_price: 0.0,  // or remove field per above
        current_tp_vsol: entry_vsol * (1.0 + tp_pct),
        current_sl_vsol: entry_vsol * (1.0 - sl_pct),
        base_confirmed_tp_fp: tier.confirmed_tp_fp,
        entry_time_ms: now_ms,
        last_buy_time_ms: 0,
        confirmed_at_ms: 0,  // or remove
        trail_active: false,
        _pad: [0; 3],
    }
}
```

#### `ExitStateMachine::on_buy_event()`

Called on every buy event for the position's token. Must be ≤ 100ns.

```rust
pub fn on_buy_event(&mut self, config: &ExitConfig, now_ms: u64) -> ExitDecision {
    self.last_buy_time_ms = now_ms;

    match self.state {
        ExitState::Unconfirmed => {
            // First confirming buy → transition to CONFIRMED
            // (price check done in on_price_tick — don't duplicate here)
            self.conviction_level = 1;
            self.state = ExitState::Confirmed;
            self.confirmed_at_ms = now_ms;
            // Upgrade TP/SL to confirmed levels
            // (recompute from base_confirmed_tp_fp)
            self._apply_conviction_tp(config, 1);
            ExitDecision::Hold
        }
        ExitState::Confirmed | ExitState::ConvictionScaled { .. } => {
            // Increment conviction
            let new_level = (self.conviction_level + 1).min(4);
            if new_level != self.conviction_level {
                self.conviction_level = new_level;
                if new_level >= 2 {
                    self.state = ExitState::ConvictionScaled { level: new_level };
                    self._apply_conviction_tp(config, new_level);
                }
            }
            ExitDecision::Hold
        }
    }
}

// Internal: recompute current_tp_vsol from base_confirmed_tp_fp × multiplier
fn _apply_conviction_tp(&mut self, config: &ExitConfig, level: u8) {
    let level_idx = level.min(4) as usize;
    let multiplier = config.conviction_tp_multipliers[level_idx] as f64 / 100.0;
    let base_tp_pct = self.base_confirmed_tp_fp as f64 / 100_000.0;
    let scaled_tp_pct = base_tp_pct * multiplier;
    self.current_tp_vsol = self.entry_price_vsol * (1.0 + scaled_tp_pct);
    // SL stays fixed (confirmed SL was set at transition from Unconfirmed)
}
```

#### `ExitStateMachine::on_price_tick()`

Called on every price update (vSol change). Must be ≤ 100ns.

```rust
pub fn on_price_tick(&mut self, config: &ExitConfig, current_vsol: f64, now_ms: u64) -> ExitDecision {
    // Update high water mark
    if current_vsol > self.peak_price_vsol {
        self.peak_price_vsol = current_vsol;
    }

    // 1. SL check (always, any state)
    if current_vsol <= self.current_sl_vsol {
        return ExitDecision::Exit(ExitReasonNew::StopLoss);
    }

    // 2. TP check
    if current_vsol >= self.current_tp_vsol {
        return ExitDecision::Exit(match self.state {
            ExitState::ConvictionScaled { .. } => ExitReasonNew::TakeProfitScaled,
            _ => ExitReasonNew::TakeProfit,
        });
    }

    match self.state {
        ExitState::Unconfirmed => {
            // 3. Confirmation window expired with no buy?
            let elapsed = now_ms.saturating_sub(self.entry_time_ms);
            if elapsed >= config.confirmation_window_ms && self.last_buy_time_ms == 0 {
                return ExitDecision::Exit(ExitReasonNew::MomentumDecayFlat);
            }
        }
        ExitState::Confirmed => {
            // 4. Momentum stall check
            if self.last_buy_time_ms > 0 {
                let since_last_buy = now_ms.saturating_sub(self.last_buy_time_ms);
                if since_last_buy >= config.stall_no_buy_ms {
                    let fade_threshold = self.peak_price_vsol
                        * (1.0 - config.stall_fade_fp as f64 / 100_000.0);
                    if current_vsol < fade_threshold {
                        return ExitDecision::Exit(ExitReasonNew::MomentumStall);
                    }
                }
            }
        }
        ExitState::ConvictionScaled { level } => {
            // 5. Conviction stall (more generous)
            if self.last_buy_time_ms > 0 {
                let since_last_buy = now_ms.saturating_sub(self.last_buy_time_ms);
                if since_last_buy >= config.stall_conviction_no_buy_ms {
                    let fade_threshold = self.peak_price_vsol
                        * (1.0 - config.stall_conviction_fade_fp as f64 / 100_000.0);
                    if current_vsol < fade_threshold {
                        return ExitDecision::Exit(ExitReasonNew::MomentumStall);
                    }
                }
            }

            // 6. Trailing stop (conviction >= trail_min_conviction)
            if level >= config.trail_min_conviction {
                let base_tp_pct = self.base_confirmed_tp_fp as f64 / 100_000.0;
                let activation_pct = base_tp_pct
                    * config.trail_activation_pct_of_base_tp as f64 / 100.0;
                let activation_price = self.entry_price_vsol * (1.0 + activation_pct);

                if current_vsol >= activation_price {
                    let trail_pct = config.trail_distance_fp as f64 / 100_000.0;
                    let trail_stop = self.peak_price_vsol * (1.0 - trail_pct);
                    if current_vsol <= trail_stop {
                        return ExitDecision::Exit(ExitReasonNew::TrailingStop);
                    }
                }
            }
        }
    }

    ExitDecision::Hold
}
```

#### `ExitStateMachine::on_safety_timeout()`

```rust
pub fn on_safety_timeout(&self) -> ExitReasonNew {
    ExitReasonNew::MaxHoldSafety
}
```

#### Helper

```rust
fn find_tier(tiers: &[TpSlTierV2], trigger_lamports: u64) -> &TpSlTierV2 {
    tiers.iter()
        .find(|t| trigger_lamports <= t.trigger_max_lamports)
        .unwrap_or(tiers.last().unwrap())
}
```

### Engineer A Tests (5 required)

```rust
#[cfg(test)]
mod tests {
    // Test 1: on_buy_event transitions Unconfirmed → Confirmed
    // Verify: state changes, conviction_level=1, TP upgraded to confirmed level

    // Test 2: Confirmation window expiry kills dead position
    // Setup: entry at t=0, no buys, call on_price_tick at t=201ms
    // Verify: returns Exit(MomentumDecayFlat)

    // Test 3: Conviction scaling — TP increases per buy
    // Setup: enter, fire 3 buy events, verify TP = base * 1.8×

    // Test 4: Trailing stop activates at conviction >= 2
    // Setup: conviction=2, price rises to activation threshold, then drops
    // Verify: Exit(TrailingStop) returned

    // Test 5: Safety timeout always exits
    // Setup: any state, call on_safety_timeout()
    // Verify: returns MaxHoldSafety
}
```

---

## 4. Engineer B — `engine/positions.rs` (MODIFY ONLY)

Engineer B modifies `positions.rs` only. Does NOT touch `exit_machine.rs` (reads it as a dependency).

### Changes to `Position` struct

Add `exit_sm: ExitStateMachine` field. Remove fields that are now in the state machine:
- Keep: `buys_since_entry` (still needed for JSONL logging)
- Remove as primary exit drivers: `intra_hold_trailing_stop_pct`, `intra_hold_trailing_stop_min_mfe_pct`

### Changes to `PositionConfig`

**Remove these fields** (replaced by `ExitConfig`):
```rust
// REMOVE:
pub max_hold_ms: u64,                        // → ExitConfig::max_hold_safety_ms
pub momentum_decay_check_ms: u64,            // → ExitConfig::confirmation_window_ms
pub momentum_decay_min_mfe_pct: f64,         // → absorbed into state machine
pub momentum_decay_max_drawdown_pct: f64,    // → ExitConfig::stall_fade_fp
pub intra_hold_trailing_stop_pct: f64,       // → ExitConfig::trail_distance_fp
pub intra_hold_trailing_stop_min_mfe_pct: f64, // → ExitConfig::trail_activation_pct_of_base_tp
pub next_buyer_profit_exit_pct: f64,         // → REMOVED (next_buyer eliminated)
pub next_buyer_aggregate_flow_ratio: f64,    // → REMOVED
pub next_buyer_count_threshold: u32,         // → REMOVED
pub next_buyer_single_buy_ratio: f64,        // → REMOVED
pub tp_tiers: Vec<TpSlTier>,                 // → replaced by ExitConfig::tp_sl_tiers
```

**Add:**
```rust
pub exit_config: ExitConfig,
```

### Changes to `PositionManager::on_trade()`

The existing `on_trade()` handles buy events for open positions. Wire exit machine:

```rust
// In the section where we process trades for open positions:
if let Some(pos) = self.positions.get_mut(&event.mint) {
    // Existing: increment buys_since_entry
    if event.is_buy {
        pos.buys_since_entry += 1;

        // NEW: feed buy event to exit state machine
        let now_ms = /* current time as u64 ms */;
        let decision = pos.exit_sm.on_buy_event(&self.config.exit_config, now_ms);
        if let ExitDecision::Exit(reason) = decision {
            let exit_reason = map_exit_reason(reason);
            self.close_position_inner(&event.mint, exit_reason, now_ms);
            return;
        }
    }

    // Existing: feed price update
    // NEW: feed price tick to exit state machine
    let decision = pos.exit_sm.on_price_tick(
        &self.config.exit_config,
        event.vsol as f64,
        now_ms,
    );
    if let ExitDecision::Exit(reason) = decision {
        let exit_reason = map_exit_reason(reason);
        self.close_position_inner(&event.mint, exit_reason, now_ms);
        return;
    }

    // REMOVE: all existing next_buyer logic
    // REMOVE: all existing momentum_decay timer logic
    // REMOVE: intra_hold_trailing_stop logic
    // KEEP: explicit TP/SL from existing code (now redundant — state machine handles — but keep as double-check, remove later)
}
```

### Safety Timer

The existing codebase uses `tokio::time::sleep` spawned in `hot_path.rs` for `max_hold`. Engineer B must:

1. When opening a position, spawn:
```rust
let mint = event.mint;
let tx = self.exit_tx.clone(); // existing channel to hot_path
tokio::spawn(async move {
    tokio::time::sleep(Duration::from_millis(5000)).await;
    let _ = tx.send(HotPathMsg::SafetyTimeout { mint });
});
```

2. Add `SafetyTimeout { mint: [u8; 32] }` variant to `HotPathMsg` enum.

3. In `hot_path.rs` message handler: call `position_manager.force_close(mint, ExitReason::MaxHold, now_ms)`.

4. **Cancellation:** The current code doesn't cancel timers on early exit (positions close before 5000ms). This is fine — the safety timeout will fire, try to close an already-closed position, and find nothing. The `force_close` call should be idempotent (check if position exists first).

### `map_exit_reason()` helper

```rust
fn map_exit_reason(r: ExitReasonNew) -> ExitReason {
    match r {
        ExitReasonNew::TakeProfit => ExitReason::TakeProfit,
        ExitReasonNew::TakeProfitScaled => ExitReason::TakeProfit,  // same bucket for now
        ExitReasonNew::StopLoss => ExitReason::StopLoss,
        ExitReasonNew::MomentumDecayFlat => ExitReason::MomentumDecayFlat,
        ExitReasonNew::MomentumStall => ExitReason::MomentumDecayFade,
        ExitReasonNew::TrailingStop => ExitReason::IntraHoldTrail,
        ExitReasonNew::MaxHoldSafety => ExitReason::MaxHold,
    }
}
```

### Position Open — Initialize State Machine

In `open_position()` (wherever a new `Position` is constructed):
```rust
let exit_sm = ExitStateMachine::on_entry(
    &config.exit_config,
    event.sol_amount,        // trigger lamports
    event.vsol as f64,       // entry vsol
    now_ms,
);
// Assign to Position struct
```

### Engineer B Tests (5 required)

```rust
// Test 1: Buy event routed to exit_sm, Unconfirmed → Confirmed transition wired correctly
// Test 2: Price tick routes to exit_sm, SL fires correctly
// Test 3: next_buyer logic is GONE — no NextBuyer exits in on_trade()
// Test 4: Safety timeout fires MaxHoldSafety after 5000ms (use tokio::time::advance)
// Test 5: Safety timeout on already-closed position is idempotent (no panic)
```

---

## 5. Engineer C — `engine/config.rs` + `config/canary.json` (MODIFY ONLY)

Engineer C modifies config deserialization only. Does NOT touch `positions.rs` or `exit_machine.rs`.

### `MevJsonConfig` additions

```rust
// In MevJsonConfig (the JSON-deserialized struct):

// Exit state machine config
pub confirmation_window_ms: Option<u64>,
pub stall_no_buy_ms: Option<u64>,
pub stall_fade_pct: Option<f64>,
pub stall_conviction_no_buy_ms: Option<u64>,
pub stall_conviction_fade_pct: Option<f64>,
pub max_hold_safety_ms: Option<u64>,
pub trail_min_conviction: Option<u8>,
pub trail_activation_pct_of_base_tp: Option<u8>,
pub trail_distance_pct: Option<f64>,

// New TP/SL tiers (replaces existing tp_tiers with unconfirmed/confirmed split)
// JSON format: see canary.json below
pub tp_sl_tiers_v2: Option<Vec<TpSlTierJsonV2>>,

// Deprecated — keep for backward compat but ignore in favor of tp_sl_tiers_v2
// pub tp_tiers: Option<Vec<...>>,  // keep parsing but don't use if tp_sl_tiers_v2 present
```

```rust
#[derive(Deserialize)]
pub struct TpSlTierJsonV2 {
    pub trigger_max_sol: f64,
    pub unconfirmed_tp_pct: f64,
    pub unconfirmed_sl_pct: f64,
    pub confirmed_tp_pct: f64,
    pub confirmed_sl_pct: f64,
}
```

### `load_config()` — ExitConfig builder

```rust
fn build_exit_config(mev: &MevJsonConfig) -> ExitConfig {
    let tiers_v2: Vec<TpSlTierV2> = if let Some(tiers) = &mev.tp_sl_tiers_v2 {
        tiers.iter().map(|t| TpSlTierV2 {
            trigger_max_lamports: (t.trigger_max_sol * 1_000_000_000.0) as u64,
            unconfirmed_tp_fp: (t.unconfirmed_tp_pct * 100_000.0) as u32,
            unconfirmed_sl_fp: (t.unconfirmed_sl_pct * 100_000.0) as u32,
            confirmed_tp_fp: (t.confirmed_tp_pct * 100_000.0) as u32,
            confirmed_sl_fp: (t.confirmed_sl_pct * 100_000.0) as u32,
        }).collect()
    } else {
        // Fallback: convert old tp_tiers (unconfirmed = confirmed for backward compat)
        build_tiers_from_legacy(mev)
    };

    let mut arr = [TpSlTierV2::default(); 8];
    let count = tiers_v2.len().min(8);
    arr[..count].copy_from_slice(&tiers_v2[..count]);

    ExitConfig {
        confirmation_window_ms: mev.confirmation_window_ms.unwrap_or(200),
        stall_no_buy_ms: mev.stall_no_buy_ms.unwrap_or(500),
        stall_fade_fp: (mev.stall_fade_pct.unwrap_or(0.01) * 100_000.0) as u32,
        stall_conviction_no_buy_ms: mev.stall_conviction_no_buy_ms.unwrap_or(800),
        stall_conviction_fade_fp: (mev.stall_conviction_fade_pct.unwrap_or(0.015) * 100_000.0) as u32,
        max_hold_safety_ms: mev.max_hold_safety_ms.unwrap_or(5000),
        conviction_tp_multipliers: [100, 100, 140, 180, 220],
        trail_min_conviction: mev.trail_min_conviction.unwrap_or(2),
        trail_activation_pct_of_base_tp: mev.trail_activation_pct_of_base_tp.unwrap_or(60),
        trail_distance_fp: (mev.trail_distance_pct.unwrap_or(0.015) * 100_000.0) as u32,
        tp_sl_tiers: arr,
        tp_sl_tier_count: count as u8,
    }
}
```

### `config/canary.json` changes

Remove deprecated fields, add new ones:

```json
// REMOVE from canary.json:
"max_hold_ms": 1500,
"momentum_decay_check_ms": ...,
"momentum_decay_min_mfe_pct": ...,
"momentum_decay_max_drawdown_pct": ...,
"intra_hold_trailing_stop_pct": ...,
"intra_hold_trailing_stop_min_mfe_pct": ...,
"next_buyer_exit": ...,
"next_buyer_aggregate_flow_ratio": ...,
"next_buyer_count_threshold": ...,
"next_buyer_single_buy_ratio": ...,
"next_buyer_profit_exit_pct": ...,
"tp_tiers": [...],  // replaced by tp_sl_tiers_v2

// ADD to canary.json mev section:
"confirmation_window_ms": 200,
"stall_no_buy_ms": 500,
"stall_fade_pct": 0.01,
"stall_conviction_no_buy_ms": 800,
"stall_conviction_fade_pct": 0.015,
"max_hold_safety_ms": 5000,
"trail_min_conviction": 2,
"trail_activation_pct_of_base_tp": 60,
"trail_distance_pct": 0.015,
"tp_sl_tiers_v2": [
  {
    "trigger_max_sol": 0.6,
    "unconfirmed_tp_pct": 0.020, "unconfirmed_sl_pct": 0.010,
    "confirmed_tp_pct": 0.030,   "confirmed_sl_pct": 0.015
  },
  {
    "trigger_max_sol": 0.8,
    "unconfirmed_tp_pct": 0.025, "unconfirmed_sl_pct": 0.010,
    "confirmed_tp_pct": 0.040,   "confirmed_sl_pct": 0.015
  },
  {
    "trigger_max_sol": 1.5,
    "unconfirmed_tp_pct": 0.030, "unconfirmed_sl_pct": 0.012,
    "confirmed_tp_pct": 0.045,   "confirmed_sl_pct": 0.015
  },
  {
    "trigger_max_sol": 5.0,
    "unconfirmed_tp_pct": 0.050, "unconfirmed_sl_pct": 0.012,
    "confirmed_tp_pct": 0.070,   "confirmed_sl_pct": 0.015
  }
]
```

### Engineer C Tests (5 required)

```rust
// Test 1: tp_sl_tiers_v2 deserializes correctly from JSON
// Test 2: Missing optional fields use correct defaults
// Test 3: conviction_tp_multipliers always = [100,100,140,180,220] (not configurable per-JSON)
// Test 4: Backward compat — old tp_tiers JSON still loads (mapped to confirmed fields)
// Test 5: Deprecated fields (max_hold_ms, next_buyer_*) don't cause parse errors if present
```

---

## 6. ExitReason Enum Changes

**In `engine/positions.rs`** — keep all existing variants (for JSONL backward compat), add two:

```rust
pub enum ExitReason {
    TakeProfit,          // KEEP
    StopLoss,            // KEEP
    NextBuyer,           // KEEP (for JSONL history) — but no longer generated by new code
    MaxHold,             // KEEP → now maps from MaxHoldSafety (5000ms safety)
    IntraHoldTrail,      // KEEP → now maps from TrailingStop
    MomentumDecayFlat,   // KEEP
    MomentumDecayFade,   // KEEP → now maps from MomentumStall
    // NEW:
    TakeProfitScaled,    // Conviction-scaled TP exit (buysAfter >= 2)
    MomentumStall,       // Signal-based stall: no buy + price fading
}
```

Gate index assignments for new variants (in `hot_path.rs` `gate_reject_index()`):
- `TakeProfitScaled` → index 25
- `MomentumStall` → index 26

---

## 7. Integration Sequence

```
Day 1:
  Engineer A writes exit_machine.rs, gets it to compile standalone (no positions.rs changes)
  Engineer C writes config.rs changes + canary.json (fully independent)

Day 1 (after A compiles):
  Engineer B wires positions.rs — add exit_sm field, remove old timer/next_buyer logic, add buy-event routing

Final:
  Single cargo build — all three changes integrated
  cargo test — all 15 new tests + 187 existing pass
  Restart daemon on release binary
```

---

## 8. Expected PnL Impact (from quant analysis)

| Component | Estimated Δ SOL | Source |
|-----------|----------------|--------|
| max_hold recovery (396 missed TPs) | +2.54 | 96.5% had zeroBuysAfter, extend to 5000ms |
| Tighter unconfirmed SL (1.0% vs 1.5%) | +1.37 | 97.6% of SL exits had zeroBuysAfter |
| Faster flat exits (200ms window) | +0.45 | Reclassify slow-confirms as Confirmed |
| Conviction TP scaling (1.4x-2.2x) | +1.89 | buysAfter=2+, WR=92.7%, let winners run |
| next_buyer elimination → hold to TP | +1.50 | next_buyer: 0.000418 SOL vs TP: 0.00736 SOL |
| Trailing stop (capture beyond TP) | +0.50 | conviction≥2 activates trail |
| Momentum stall (signal vs timer) | +0.30 | more precise stall detection |
| **Total** | **+8.55 SOL** | over 5,729 historical trades |

System remains net-negative post-implementation. Remaining gap requires Jito execution (atomic entry, zero adverse fills) to close.

---

_Spec complete. Engineers A/B/C implement in parallel from this document._
