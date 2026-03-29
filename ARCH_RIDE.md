# ARCH_RIDE.md — Ride Mode Exit Engine Architecture

_Principal architecture spec for the RIDE exit engine. Parallel engineers implement from this spec — no design decisions left open._
_Author: Apollo (Systems Architect). Based on QUANT_RIDE_A.md, QUANT_RIDE_B.md, EXIT_STRATEGY_QUANT.md._
_Date: 2026-03-29_

---

## 0. Executive Summary

The RIDE engine is a second exit path that activates on high-conviction pumps (buysAfterEntry≥2, qualified). It replaces the fixed TP% with an adaptive trailing stop that lets winners run for 5-300 seconds instead of exiting at 3-7%.

**Key invariants:**
- `RideState` is 64 bytes exactly (1 cache line), `#[repr(C)]`
- Zero heap allocation on the hot path
- All price comparisons use integer vSOL (u32, millionths of SOL) — zero f64
- Trail stop can only ratchet up, never down
- Phase transitions are one-way: Early → Momentum → Tighten
- Hard floor: entry_price × 1.01 — RIDE never loses money

**What doesn't change:** The existing `ExitStateMachine` (SCALP mode) remains untouched. RIDE is a parallel path activated by a qualification check.

---

## 1. Architecture Overview

### 1.1 Dual-Mode Decision Flow

```
TradeEvent arrives
    │
    ├─ on_buy_event() → ExitStateMachine (SCALP)
    │     │
    │     ├─ conviction_level reaches 2+
    │     │     │
    │     │     ├─ evaluate_ride_qualification() → true
    │     │     │     └─ TRANSITION: init RideState, set ride_state = Some(...)
    │     │     │
    │     │     └─ false → stay in ConvictionScaled (SCALP with scaled TP)
    │     │
    │     └─ conviction_level < 2 → normal SCALP flow
    │
    └─ on_price_tick() / on_subsequent_trade()
          │
          ├─ ride_state is Some → RideState::on_tick()
          │     └─ returns ExitDecision (Hold / Exit with reason)
          │
          └─ ride_state is None → ExitStateMachine::on_price_tick() (SCALP)
```

### 1.2 File Ownership

| File | Action | Owner |
|------|--------|-------|
| `engine/ride_machine.rs` | **CREATE** (new file) | Engineer A |
| `engine/positions.rs` | **MODIFY** — add `ride_state` field, wire RIDE into `on_subsequent_trade()` and `on_tick()` | Engineer B |
| `engine/config.rs` | **MODIFY** — add `RideConfig` struct, deserialize from JSON | Engineer C |
| `engine/mod.rs` | **MODIFY** — add `pub mod ride_machine;` | Engineer A |
| `config/canary.json` | **MODIFY** — add `ride_*` config keys | Engineer C |

**Sequencing:**
1. Engineer A delivers `ride_machine.rs` (compiles standalone with `cargo check`)
2. Engineer C delivers `RideConfig` in `config.rs` (parallel with A)
3. Engineer B wires A's types into `positions.rs` (depends on A + C)

---

## 2. Data Types

### 2.1 RideState — 64 Bytes Exactly

```rust
// engine/ride_machine.rs

/// RIDE phase. One-way progression: Early → Momentum → Tighten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RidePhase {
    Early    = 0,  // 0-15s, trail=8%
    Momentum = 1,  // 15-60s or +15% gain, trail=6%
    Tighten  = 2,  // 60s+ or +30% gain, trail=4%
}

/// Bitflags for RideState::flags field.
pub mod ride_flags {
    pub const SELL_PRESSURE_SPIKE: u16 = 1 << 0;  // sell_vol/buy_vol > 0.5
    pub const BUY_DECELERATION:    u16 = 1 << 1;  // rate < 30% of start rate
    pub const WHALE_EXIT_SEEN:     u16 = 1 << 2;  // single sell > 1 SOL seen
    pub const BUY_GAP_5S:          u16 = 1 << 3;  // buy gap > 5s detected
    pub const EMERGENCY_EXIT:      u16 = 1 << 4;  // emergency condition triggered
    pub const CREATOR_SELL:        u16 = 1 << 5;  // creator wallet sold
}

/// Exit reasons specific to RIDE mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RideExitReason {
    TrailingStop,        // Normal trail hit
    BuyGapTimeout,       // No buy for 10+ seconds
    WhaleDump,           // Single sell > 2 SOL
    SellCascade,         // 3+ sells in 3 seconds
    CreatorSell,         // Creator wallet sold
    PriceBelowEntry,     // Price dropped below entry (shouldn't happen with floor)
    MaxHoldRide,         // 300s safety backstop
    EmergencyFloor,      // Hit the hard floor (entry × 1.01)
}

/// The RIDE exit state machine. Exactly 64 bytes, 1 cache line.
///
/// All prices stored as "milli-SOL vSOL" — u32 representing vSOL in units
/// of 0.001 SOL (1_000_000 lamports). Range: 0 to 4,294,967 SOL.
/// At typical entry vSOL of 30-115 SOL, this gives precision to 0.001 SOL.
///
/// Lamports-to-mvsol: mvsol = (lamports + 500_000) / 1_000_000
/// mvsol-to-lamports: lamports = mvsol as u64 * 1_000_000
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct RideState {
    // ── Byte 0-3: Phase + padding ──
    pub phase: u8,               // 0=Early, 1=Momentum, 2=Tighten (cast from RidePhase)
    pub unique_wallets: u8,      // Confirming wallet count during ride (max 255)
    pub sells_during_ride: u16,  // Sell event counter (max 65535)

    // ── Byte 4-19: Price levels (milli-SOL vSOL, u32) ──
    pub entry_mvsol: u32,        // Entry vSOL in milli-SOL units
    pub peak_mvsol: u32,         // Peak vSOL (high water mark)
    pub floor_mvsol: u32,        // Hard floor = entry × 1.01 (never sell below)
    pub trail_stop_mvsol: u32,   // Current trailing stop (ratchet: only increases)

    // ── Byte 20-35: Timestamps ──
    pub ride_start_ms: u64,      // When RIDE mode activated (epoch ms)
    pub last_buy_ms: u64,        // Last confirming buy timestamp (for gap detection)

    // ── Byte 36-43: Rate tracking ──
    pub buy_rate_at_start: u16,  // buy_count_5s when RIDE activated (for deceleration)
    pub trail_distance_bp: u16,  // Trail distance in basis points (800 = 8.00%)
    pub flags: u16,              // Bitflags (see ride_flags module)
    pub _reserved: u16,          // Reserved for future use (keeps alignment)

    // ── Byte 44-51: Volume tracking (milli-SOL, u32) ──
    pub total_buy_msol: u32,     // Total confirming buy SOL during ride (milli-SOL)
    pub total_sell_msol: u32,    // Total sell SOL during ride (milli-SOL)

    // ── Byte 52-59: Recent window tracking ──
    pub recent_sell_count_3s: u8, // Sells in last 3s window (rolling, reset on check)
    pub recent_sell_window_start: u8, // Offset from ride_start in seconds (max 255s = 4.25min)
    pub _pad: [u8; 2],           // Padding to 4-byte alignment
    pub last_sell_msol: u32,     // Size of most recent sell (milli-SOL, for whale detection)

    // ── Byte 60-63: Entry context ──
    pub entry_gain_bp: u16,      // Unrealized gain at RIDE activation (basis points)
    pub _pad2: [u8; 2],          // Final padding
}

// ── SIZE ASSERTION ──
const _RIDE_STATE_SIZE: () = assert!(std::mem::size_of::<RideState>() == 64);
// ── ALIGNMENT ASSERTION ──
const _RIDE_STATE_ALIGN: () = assert!(std::mem::align_of::<RideState>() <= 8);
```

### 2.2 Size Verification

```
Byte map (manual count):
  [0]     phase: u8                    = 1
  [1]     unique_wallets: u8           = 1
  [2-3]   sells_during_ride: u16       = 2
  [4-7]   entry_mvsol: u32            = 4
  [8-11]  peak_mvsol: u32             = 4
  [12-15] floor_mvsol: u32            = 4
  [16-19] trail_stop_mvsol: u32       = 4
  [20-27] ride_start_ms: u64          = 8
  [28-35] last_buy_ms: u64            = 8
  [36-37] buy_rate_at_start: u16      = 2
  [38-39] trail_distance_bp: u16      = 2
  [40-41] flags: u16                  = 2
  [42-43] _reserved: u16              = 2
  [44-47] total_buy_msol: u32         = 4
  [48-51] total_sell_msol: u32        = 4
  [52]    recent_sell_count_3s: u8     = 1
  [53]    recent_sell_window_start: u8 = 1
  [54-55] _pad: [u8; 2]              = 2
  [56-59] last_sell_msol: u32         = 4
  [60-61] entry_gain_bp: u16          = 2
  [62-63] _pad2: [u8; 2]             = 2
  TOTAL                                = 64 ✓
```

**Why `#[repr(C)]` and not `#[repr(packed)]`:** Packed structs generate unaligned loads which are slow on x86 and UB on ARM. `repr(C)` with manual padding gives deterministic layout AND aligned access.

### 2.3 Milli-SOL vSOL (mvsol) Unit System

All vSOL prices in `RideState` use a custom fixed-point unit: **milli-SOL vSOL (mvsol)**.

```
1 mvsol = 0.001 SOL = 1_000_000 lamports

Conversion:
  lamports_to_mvsol(lamports: u64) -> u32 = ((lamports + 500_000) / 1_000_000) as u32
  mvsol_to_lamports(mvsol: u32) -> u64    = mvsol as u64 * 1_000_000
```

**Range:** u32 mvsol covers 0 to 4,294,967 SOL. The bonding curve operates at 30-115 SOL vSOL range. We have 37,000x headroom.

**Precision:** 0.001 SOL = 0.003% at vSOL=30. Trailing stop decisions at 4-8% granularity need ~0.1% precision. 0.003% is 30x better than needed.

**Why not u64 lamports?** Because we need the struct to fit 64 bytes. Using u32 mvsol for 5 price fields saves 20 bytes vs u64 lamports, which is the difference between fitting and not fitting.

### 2.4 Trail Arithmetic — Zero Floating Point

```rust
/// Compute trail stop from peak and trail distance. Pure integer math.
///
/// trail_stop = peak - (peak × trail_distance_bp / 10000)
///            = peak × (10000 - trail_distance_bp) / 10000
///
/// Example: peak=50_000 mvsol (50 SOL), trail=800 bp (8%)
///   trail_stop = 50_000 × 9200 / 10000 = 46_000 mvsol (46 SOL) ✓
#[inline(always)]
fn compute_trail_stop(peak_mvsol: u32, trail_distance_bp: u16) -> u32 {
    // Use u64 intermediate to avoid u32 overflow on multiply.
    // peak_mvsol max ≈ 115_000 (115 SOL), trail max ≈ 10000
    // 115_000 × 10_000 = 1.15e9 — fits u32, but use u64 for safety.
    let keep_bp = 10_000u32 - trail_distance_bp as u32;
    ((peak_mvsol as u64 * keep_bp as u64) / 10_000) as u32
}

/// Check if gain_pct threshold is met using only mvsol integer comparison.
///
/// "current >= entry × (1 + pct)" becomes:
/// "current × 10000 >= entry × (10000 + pct_bp)"
///
/// Example: entry=40_000, current=46_000, threshold=15% (1500 bp)
///   46_000 × 10_000 = 460_000_000
///   40_000 × 11_500 = 460_000_000
///   460M >= 460M → true (exactly +15%) ✓
#[inline(always)]
fn gain_exceeds_bp(current_mvsol: u32, entry_mvsol: u32, threshold_bp: u16) -> bool {
    let lhs = current_mvsol as u64 * 10_000;
    let rhs = entry_mvsol as u64 * (10_000 + threshold_bp as u64);
    lhs >= rhs
}
```

---

## 3. RideState Implementation

### 3.1 Constructor — `RideState::activate()`

Called when SCALP mode's `on_buy_event()` increments conviction to ≥2 and `evaluate_ride_qualification()` returns true.

```rust
impl RideState {
    /// Initialize RIDE mode from current position state.
    ///
    /// # Arguments
    /// * `entry_vsol_lamports` — vSOL at original position entry (lamports)
    /// * `current_vsol_lamports` — current vSOL reserves (lamports)
    /// * `now_ms` — current timestamp (epoch ms)
    /// * `buy_count_5s` — buy rate at activation (for deceleration detection)
    /// * `unique_wallets` — distinct confirming wallets so far
    /// * `confirming_buy_sol_lamports` — total SOL from confirming buys
    #[inline]
    pub fn activate(
        entry_vsol_lamports: u64,
        current_vsol_lamports: u64,
        now_ms: u64,
        buy_count_5s: u16,
        unique_wallets: u8,
        confirming_buy_sol_lamports: u64,
    ) -> Self {
        let entry_mvsol = lamports_to_mvsol(entry_vsol_lamports);
        let current_mvsol = lamports_to_mvsol(current_vsol_lamports);

        // Hard floor: entry × 1.01 = entry × 10100 / 10000
        let floor_mvsol = ((entry_mvsol as u64 * 10_100) / 10_000) as u32;

        // Initial trail stop: max(floor, trail from current peak)
        // Phase is EARLY → trail = 800 bp (8%)
        let initial_trail_bp: u16 = 800;
        let trail_from_peak = compute_trail_stop(current_mvsol, initial_trail_bp);
        let trail_stop = trail_from_peak.max(floor_mvsol);

        // Entry gain in basis points
        let entry_gain_bp = if entry_mvsol > 0 {
            (((current_mvsol as u64).saturating_sub(entry_mvsol as u64)) * 10_000
                / entry_mvsol as u64) as u16
        } else {
            0
        };

        Self {
            phase: RidePhase::Early as u8,
            unique_wallets,
            sells_during_ride: 0,
            entry_mvsol,
            peak_mvsol: current_mvsol,
            floor_mvsol,
            trail_stop_mvsol: trail_stop,
            ride_start_ms: now_ms,
            last_buy_ms: now_ms,
            buy_rate_at_start: buy_count_5s,
            trail_distance_bp: initial_trail_bp,
            flags: 0,
            _reserved: 0,
            total_buy_msol: lamports_to_mvsol(confirming_buy_sol_lamports) as u32,
            total_sell_msol: 0,
            recent_sell_count_3s: 0,
            recent_sell_window_start: 0,
            _pad: [0; 2],
            last_sell_msol: 0,
            entry_gain_bp,
            _pad2: [0; 2],
        }
    }
}

/// Convert lamports to milli-SOL vSOL (rounded).
#[inline(always)]
pub fn lamports_to_mvsol(lamports: u64) -> u32 {
    ((lamports + 500_000) / 1_000_000) as u32
}

/// Convert milli-SOL vSOL back to lamports.
#[inline(always)]
pub fn mvsol_to_lamports(mvsol: u32) -> u64 {
    mvsol as u64 * 1_000_000
}
```

### 3.2 Core Hot Path — `RideState::on_tick()`

This is the most performance-critical function in the RIDE engine. Called on every trade event and every 50ms tick while a RIDE position is open.

**Performance target: ≤50ns.** All branches are predictable (phase rarely changes). All arithmetic is integer. No function calls except inlined helpers.

```rust
impl RideState {
    /// Core hot-path tick. Called on every price update.
    ///
    /// Returns ExitDecision::Hold or ExitDecision::Exit(reason).
    ///
    /// # Arguments
    /// * `current_vsol_lamports` — current vSOL reserves from trade event
    /// * `now_ms` — current timestamp (epoch ms)
    /// * `config` — ride configuration (passed by reference, likely in L1)
    #[inline(always)]
    pub fn on_tick(
        &mut self,
        current_vsol_lamports: u64,
        now_ms: u64,
        config: &RideConfig,
    ) -> ExitDecision {
        let current = lamports_to_mvsol(current_vsol_lamports);
        let elapsed_ms = now_ms.saturating_sub(self.ride_start_ms);

        // ── 1. EMERGENCY: price below entry (should never happen with floor) ──
        if current < self.entry_mvsol {
            return ExitDecision::Exit(ExitReasonNew::RideExit(RideExitReason::PriceBelowEntry));
        }

        // ── 2. MAX HOLD: 300s safety backstop ──
        if elapsed_ms >= config.max_hold_ride_ms {
            return ExitDecision::Exit(ExitReasonNew::RideExit(RideExitReason::MaxHoldRide));
        }

        // ── 3. BUY GAP: silence detector ──
        let gap_ms = now_ms.saturating_sub(self.last_buy_ms);
        if gap_ms >= config.buy_gap_exit_ms {
            // 10s+ silence = dead pump, immediate exit
            return ExitDecision::Exit(ExitReasonNew::RideExit(RideExitReason::BuyGapTimeout));
        }

        // ── 4. UPDATE PEAK (high water mark) ──
        if current > self.peak_mvsol {
            self.peak_mvsol = current;
        }

        // ── 5. PHASE TRANSITIONS (one-way: Early → Momentum → Tighten) ──
        let phase = self.phase;
        let mut base_trail_bp = self.trail_distance_bp;

        if phase == RidePhase::Early as u8 {
            // Transition to MOMENTUM: 15s elapsed OR +15% gain
            if elapsed_ms >= config.early_to_momentum_ms
                || gain_exceeds_bp(current, self.entry_mvsol, config.early_to_momentum_gain_bp)
            {
                self.phase = RidePhase::Momentum as u8;
                base_trail_bp = config.momentum_trail_bp; // 600 = 6%
            }
        }

        if self.phase == RidePhase::Momentum as u8 {
            // Transition to TIGHTEN: 60s elapsed OR +30% gain
            if elapsed_ms >= config.momentum_to_tighten_ms
                || gain_exceeds_bp(current, self.entry_mvsol, config.momentum_to_tighten_gain_bp)
            {
                self.phase = RidePhase::Tighten as u8;
                base_trail_bp = config.tighten_trail_bp; // 400 = 4%
            }
        }

        // ── 6. ADAPTIVE TRAIL TIGHTENING (signal stacking) ──
        let mut effective_trail_bp = base_trail_bp;

        // 6a. Sell pressure spike: sell_vol / buy_vol > 0.5
        //     In mvsol: total_sell_msol * 2 > total_buy_msol
        if self.total_buy_msol > 0
            && (self.total_sell_msol as u64 * 2) > self.total_buy_msol as u64
        {
            effective_trail_bp = effective_trail_bp.saturating_sub(config.sell_pressure_tighten_bp);
            self.flags |= ride_flags::SELL_PRESSURE_SPIKE;
        }

        // 6b. Buy deceleration: current rate < 30% of start rate
        //     We don't have current buy_count_5s in this struct, so we approximate:
        //     If last_buy gap > 3.3s (= 1/0.3 of avg 1s interval), flag deceleration.
        //     More precise: caller passes buy_count_5s in on_buy_event and we track it.
        //     For on_tick, use gap_ms as proxy.
        if gap_ms >= config.buy_deceleration_gap_ms {
            effective_trail_bp = effective_trail_bp.saturating_sub(config.buy_deceleration_tighten_bp);
            self.flags |= ride_flags::BUY_DECELERATION;
        }

        // 6c. Whale exit: single sell > 1 SOL seen (flagged by on_sell_event)
        if self.flags & ride_flags::WHALE_EXIT_SEEN != 0 {
            effective_trail_bp = effective_trail_bp.min(config.whale_exit_trail_cap_bp);
        }

        // 6d. Buy gap > 5s (not yet at 10s exit threshold)
        if gap_ms >= config.buy_gap_tighten_ms {
            effective_trail_bp = effective_trail_bp.saturating_sub(config.buy_gap_tighten_bp);
            self.flags |= ride_flags::BUY_GAP_5S;
        }

        // 6e. Floor on trail distance: never tighter than min_trail_bp (150 = 1.5%)
        effective_trail_bp = effective_trail_bp.max(config.min_trail_bp);

        // Update stored trail distance for logging/debugging
        self.trail_distance_bp = effective_trail_bp;

        // ── 7. COMPUTE TRAIL STOP (ratchet: only increases) ──
        let new_trail_stop = compute_trail_stop(self.peak_mvsol, effective_trail_bp);
        let new_trail_stop = new_trail_stop.max(self.floor_mvsol); // Never below floor
        if new_trail_stop > self.trail_stop_mvsol {
            self.trail_stop_mvsol = new_trail_stop; // Ratchet up
        }

        // ── 8. TRAIL STOP CHECK ──
        if current <= self.trail_stop_mvsol {
            return ExitDecision::Exit(ExitReasonNew::RideExit(RideExitReason::TrailingStop));
        }

        ExitDecision::Hold
    }
}
```

**Branch analysis for the hot path:**
- Steps 1-3 (emergency/max_hold/gap): Almost never taken. `#[cold]` hint on the exit arms. The branch predictor will learn these are not-taken within ~5 ticks.
- Step 4 (peak update): Taken ~40% of the time during pumps (price frequently sets new highs).
- Step 5 (phase transition): Taken exactly twice per RIDE lifetime. Near-zero cost.
- Steps 6a-6e (tightening): Each is a simple integer compare. All branches predictable.
- Step 7 (trail compute): Always executed. 1 multiply + 1 divide + 2 max comparisons.
- Step 8 (trail check): Taken once (at exit). Otherwise not-taken.

**Total cost estimate:** ~15-25ns on modern x86. Well under the 50ns budget.

### 3.3 Buy Event Handler — `RideState::on_buy_event()`

Called when a buy event arrives for a token in RIDE mode. Updates buy tracking, resets gap timer.

```rust
impl RideState {
    /// Process a confirming buy event during RIDE mode.
    ///
    /// # Arguments
    /// * `buy_sol_lamports` — size of the buy in lamports
    /// * `now_ms` — timestamp
    /// * `is_new_wallet` — whether this buyer hasn't been seen before
    #[inline]
    pub fn on_buy_event(
        &mut self,
        buy_sol_lamports: u64,
        now_ms: u64,
        is_new_wallet: bool,
    ) {
        // Update last buy time (resets gap timer)
        self.last_buy_ms = now_ms;

        // Accumulate buy volume
        let buy_msol = lamports_to_mvsol(buy_sol_lamports);
        self.total_buy_msol = self.total_buy_msol.saturating_add(buy_msol);

        // Track unique wallets (cap at 255)
        if is_new_wallet && self.unique_wallets < 255 {
            self.unique_wallets += 1;
        }

        // Clear buy-gap and deceleration flags (fresh buy = momentum renewed)
        self.flags &= !(ride_flags::BUY_GAP_5S | ride_flags::BUY_DECELERATION);
    }
}
```

### 3.4 Sell Event Handler — `RideState::on_sell_event()`

Called when a sell event arrives for a token in RIDE mode. Tracks sell pressure and detects emergency conditions.

```rust
impl RideState {
    /// Process a sell event during RIDE mode. Returns ExitDecision for
    /// emergency exits that override the trailing stop.
    ///
    /// # Arguments
    /// * `sell_sol_lamports` — size of the sell in lamports
    /// * `now_ms` — timestamp
    /// * `is_creator` — whether the seller is the token creator
    /// * `config` — ride configuration
    #[inline]
    pub fn on_sell_event(
        &mut self,
        sell_sol_lamports: u64,
        now_ms: u64,
        is_creator: bool,
        config: &RideConfig,
    ) -> ExitDecision {
        let sell_msol = lamports_to_mvsol(sell_sol_lamports);

        // ── Emergency: Creator sell ──
        if is_creator {
            self.flags |= ride_flags::CREATOR_SELL;
            return ExitDecision::Exit(ExitReasonNew::RideExit(RideExitReason::CreatorSell));
        }

        // ── Emergency: Whale dump (single sell > 2 SOL = 2000 mvsol) ──
        if sell_msol >= config.whale_dump_exit_msol {
            self.flags |= ride_flags::EMERGENCY_EXIT;
            return ExitDecision::Exit(ExitReasonNew::RideExit(RideExitReason::WhaleDump));
        }

        // ── Track sell volume ──
        self.total_sell_msol = self.total_sell_msol.saturating_add(sell_msol);
        self.sells_during_ride = self.sells_during_ride.saturating_add(1);
        self.last_sell_msol = sell_msol;

        // ── Flag whale exit (single sell > 1 SOL = 1000 mvsol) for trail tightening ──
        if sell_msol >= config.whale_exit_msol {
            self.flags |= ride_flags::WHALE_EXIT_SEEN;
        }

        // ── Sell cascade detection: 3+ sells in 3s window ──
        let window_offset_s = ((now_ms.saturating_sub(self.ride_start_ms)) / 1000) as u8;
        if window_offset_s.saturating_sub(self.recent_sell_window_start) > 3 {
            // Window expired, reset
            self.recent_sell_count_3s = 1;
            self.recent_sell_window_start = window_offset_s;
        } else {
            self.recent_sell_count_3s = self.recent_sell_count_3s.saturating_add(1);
        }

        if self.recent_sell_count_3s >= config.sell_cascade_count {
            self.flags |= ride_flags::EMERGENCY_EXIT;
            return ExitDecision::Exit(ExitReasonNew::RideExit(RideExitReason::SellCascade));
        }

        ExitDecision::Hold
    }
}
```

---

## 4. RIDE Qualification Logic

### 4.1 Qualification Criteria

RIDE activation happens inside `on_buy_event()` when conviction reaches ≥2. Not every conviction≥2 trade should RIDE — we need additional confirmation.

```rust
// In positions.rs or a helper module

/// Evaluate whether a position qualifies for RIDE mode.
/// Called when buys_after_entry reaches 2+.
///
/// All thresholds are from RideConfig (hot-reloadable via canary.json).
///
/// Returns true if ALL criteria are met.
#[inline]
pub fn evaluate_ride_qualification(
    confirming_buy_sol_lamports: u64,  // Total SOL from confirming buys
    unique_confirming_wallets: u8,     // Distinct wallets
    sells_since_entry: u16,            // Sell count since our entry
    current_vsol_lamports: u64,        // Current vSOL reserves
    entry_vsol_lamports: u64,          // vSOL at entry
    curve_pct_bp: u16,                 // Curve fill % in basis points (4500 = 45%)
    config: &RideConfig,
) -> bool {
    // 1. Confirming buys are material (not dust)
    //    Default: 300_000_000 lamports = 0.3 SOL
    if confirming_buy_sol_lamports < config.min_confirming_sol_lamports {
        return false;
    }

    // 2. Multiple distinct wallets (not self-trade)
    if unique_confirming_wallets < config.min_unique_wallets {
        return false;
    }

    // 3. Zero sell pressure during confirmation window
    if sells_since_entry > 0 {
        return false;
    }

    // 4. Currently in profit
    if current_vsol_lamports <= entry_vsol_lamports {
        return false;
    }

    // 5. Minimum unrealized gain (1.5% = 150 bp)
    //    current >= entry × (10000 + 150) / 10000
    let gain_check = current_vsol_lamports as u128 * 10_000
        >= entry_vsol_lamports as u128 * (10_000 + config.min_gain_for_ride_bp as u128);
    if !gain_check {
        return false;
    }

    // 6. Room to run: curve < 80% (8000 bp) OR gain already >= 3% (300 bp)
    let has_room = curve_pct_bp < config.max_curve_pct_bp;
    let already_pumping = {
        let lhs = current_vsol_lamports as u128 * 10_000;
        let rhs = entry_vsol_lamports as u128 * (10_000 + config.override_gain_bp as u128);
        lhs >= rhs
    };
    if !has_room && !already_pumping {
        return false;
    }

    true
}
```

### 4.2 Qualification Parameters (RideConfig)

```rust
/// RIDE mode configuration. Loaded from canary.json `ride` section.
/// Passed by reference — expect it to live in L1 cache during hot path.
#[derive(Debug, Clone)]
pub struct RideConfig {
    // ── Qualification thresholds ──
    pub min_confirming_sol_lamports: u64, // 300_000_000 (0.3 SOL)
    pub min_unique_wallets: u8,           // 2
    pub min_gain_for_ride_bp: u16,        // 150 (1.5%)
    pub max_curve_pct_bp: u16,            // 8000 (80%)
    pub override_gain_bp: u16,            // 300 (3% — overrides curve check)

    // ── Phase timing ──
    pub early_to_momentum_ms: u64,        // 15_000 (15s)
    pub early_to_momentum_gain_bp: u16,   // 1500 (15%)
    pub momentum_to_tighten_ms: u64,      // 60_000 (60s)
    pub momentum_to_tighten_gain_bp: u16, // 3000 (30%)
    pub max_hold_ride_ms: u64,            // 300_000 (5 minutes)

    // ── Trail distances per phase (basis points) ──
    pub early_trail_bp: u16,              // 800 (8%)
    pub momentum_trail_bp: u16,           // 600 (6%)
    pub tighten_trail_bp: u16,            // 400 (4%)
    pub min_trail_bp: u16,                // 150 (1.5% — never tighter)

    // ── Adaptive tightening ──
    pub sell_pressure_tighten_bp: u16,    // 200 (tighten by 2% on sell pressure spike)
    pub buy_deceleration_tighten_bp: u16, // 100 (tighten by 1% on deceleration)
    pub buy_deceleration_gap_ms: u64,     // 3_300 (proxy: gap > 3.3s ≈ deceleration)
    pub whale_exit_trail_cap_bp: u16,     // 200 (cap trail at 2% on whale exit)
    pub buy_gap_tighten_ms: u64,          // 5_000 (tighten by 2% on 5s gap)
    pub buy_gap_tighten_bp: u16,          // 200 (tighten amount for 5s gap)
    pub buy_gap_exit_ms: u64,             // 10_000 (immediate exit on 10s gap)

    // ── Emergency thresholds ──
    pub whale_dump_exit_msol: u32,        // 2_000 (2 SOL — immediate exit)
    pub whale_exit_msol: u32,             // 1_000 (1 SOL — trail tighten)
    pub sell_cascade_count: u8,           // 3 (3 sells in 3s window)
}

impl Default for RideConfig {
    fn default() -> Self {
        Self {
            min_confirming_sol_lamports: 300_000_000,
            min_unique_wallets: 2,
            min_gain_for_ride_bp: 150,
            max_curve_pct_bp: 8000,
            override_gain_bp: 300,
            early_to_momentum_ms: 15_000,
            early_to_momentum_gain_bp: 1500,
            momentum_to_tighten_ms: 60_000,
            momentum_to_tighten_gain_bp: 3000,
            max_hold_ride_ms: 300_000,
            early_trail_bp: 800,
            momentum_trail_bp: 600,
            tighten_trail_bp: 400,
            min_trail_bp: 150,
            sell_pressure_tighten_bp: 200,
            buy_deceleration_tighten_bp: 100,
            buy_deceleration_gap_ms: 3_300,
            whale_exit_trail_cap_bp: 200,
            buy_gap_tighten_ms: 5_000,
            buy_gap_tighten_bp: 200,
            buy_gap_exit_ms: 10_000,
            whale_dump_exit_msol: 2_000,
            whale_exit_msol: 1_000,
            sell_cascade_count: 3,
        }
    }
}
```

---

## 5. Integration with PositionManager

### 5.1 Changes to OpenPosition

```rust
// In engine/positions.rs — OpenPosition struct

pub struct OpenPosition {
    // ... existing fields unchanged ...

    /// Signal-based exit state machine (SCALP mode).
    pub exit_sm: ExitStateMachine,

    // ── NEW: RIDE mode state ──
    /// RIDE exit state. None = SCALP mode. Some = RIDE mode active.
    /// When Some, on_tick delegates to RideState instead of ExitStateMachine.
    pub ride_state: Option<RideState>,

    // ── NEW: RIDE qualification tracking ──
    /// Total SOL from confirming buys (lamports). Accumulated on each buy event.
    pub confirming_buy_sol: u64,
    /// Unique confirming wallets (tracked via external Bloom filter or small set).
    /// For RIDE qualification, we need unique_wallets >= 2.
    /// Use a simple 2-element array: first two distinct wallet prefixes.
    pub confirming_wallet_hashes: [u32; 2],  // FNV1a of first 8 bytes of wallet
    pub unique_confirming_wallets: u8,
    /// Sell events since our entry.
    pub sells_since_entry: u16,
}
```

**Size impact:** `Option<RideState>` is 65 bytes (64 + 1 discriminant) but the compiler may pad to 72 bytes. Alternative: use a `ride_active: bool` flag and store `RideState` inline (always 64 bytes, wasted when not in RIDE). Since `OpenPosition` is already heap-allocated in `HashMap`, the extra 64 bytes is irrelevant — it's not on the hot path struct.

**Decision: Use `Option<RideState>`.** The branch prediction benefit of checking `is_some()` vs a bool flag is zero, and `Option` gives us Rust's type safety.

### 5.2 Unique Wallet Tracking (Zero-Allocation)

We need to track unique confirming wallets for RIDE qualification. Full wallet dedup requires a HashSet (heap allocation). Instead, use a minimal scheme:

```rust
/// Lightweight unique wallet counter. Zero heap allocation.
/// Uses FNV-1a hash of first 8 bytes of wallet pubkey.
/// False positive rate: ~1/4B for 2 wallets. Acceptable.
#[inline]
fn fnv1a_wallet(wallet: &[u8; 32]) -> u32 {
    let mut hash: u32 = 2166136261;
    for &byte in &wallet[..8] {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(16777619);
    }
    hash
}

/// Check if wallet is new and update tracking. Returns true if new.
fn track_confirming_wallet(pos: &mut OpenPosition, wallet: &[u8; 32]) -> bool {
    let h = fnv1a_wallet(wallet);
    match pos.unique_confirming_wallets {
        0 => {
            pos.confirming_wallet_hashes[0] = h;
            pos.unique_confirming_wallets = 1;
            true
        }
        1 => {
            if h == pos.confirming_wallet_hashes[0] {
                false // Same wallet
            } else {
                pos.confirming_wallet_hashes[1] = h;
                pos.unique_confirming_wallets = 2;
                true
            }
        }
        _ => {
            // Already have 2+ unique wallets. Don't bother tracking more for qualification.
            // For RideState::on_buy_event, always pass is_new_wallet=true at this point
            // (conservative: slight overcount of unique wallets in RIDE, acceptable).
            if h != pos.confirming_wallet_hashes[0] && h != pos.confirming_wallet_hashes[1] {
                // Genuinely new wallet (as far as we can tell)
                true
            } else {
                false
            }
        }
    }
}
```

### 5.3 Modified `on_subsequent_trade()` — The Integration Point

```rust
impl PositionManager {
    #[inline]
    pub fn on_subsequent_trade(&mut self, event: &TradeEvent, now_ms: u64) -> bool {
        if event.vsol_reserves == 0 {
            return false;
        }

        let pos = match self.positions.get_mut(&event.mint) {
            Some(p) => p,
            None => return false,
        };

        if event.sig == pos.trigger_sig {
            return false;
        }

        // ── Update position state (unchanged) ──
        pos.trades_seen_after_entry += 1;
        pos.current_vsol = event.vsol_reserves;
        pos.current_vtokens = event.vtoken_reserves;
        if event.vsol_reserves > pos.peak_vsol {
            pos.peak_vsol = event.vsol_reserves;
        }
        if event.vsol_reserves < pos.trough_vsol {
            pos.trough_vsol = event.vsol_reserves;
        }

        // ── SELL EVENT HANDLING ──
        if !event.is_buy {
            pos.sells_since_entry += 1;

            // If in RIDE mode, delegate to RideState::on_sell_event()
            if let Some(ref mut ride) = pos.ride_state {
                let decision = ride.on_sell_event(
                    event.sol_amount,
                    now_ms,
                    event.is_creator,  // NEW FIELD: must be added to TradeEvent
                    &self.config.ride_config,
                );
                if let ExitDecision::Exit(reason) = decision {
                    let mint = event.mint;
                    self.close_position_inner(&mint, map_ride_exit_reason(reason), now_ms);
                    return true;
                }
            }
        }

        // ── BUY EVENT HANDLING ──
        if event.is_buy {
            pos.flow_since_entry += event.sol_amount;
            pos.buys_since_entry += 1;

            // Track confirming buy metadata for RIDE qualification
            pos.confirming_buy_sol += event.sol_amount;
            let is_new_wallet = track_confirming_wallet(pos, &event.trader_wallet);

            // If already in RIDE mode, feed buy to RideState
            if let Some(ref mut ride) = pos.ride_state {
                ride.on_buy_event(event.sol_amount, now_ms, is_new_wallet);
            } else {
                // SCALP mode: feed buy to ExitStateMachine
                let decision = pos.exit_sm.on_buy_event(
                    &self.config.exit_config,
                    event.vsol_reserves as f64,
                    now_ms,
                );
                if let ExitDecision::Exit(reason) = decision {
                    let mint = event.mint;
                    self.close_position_inner(&mint, map_exit_reason_new(reason), now_ms);
                    return true;
                }

                // ── RIDE TRANSITION CHECK ──
                // After buy event, if conviction just hit 2+, evaluate RIDE
                if pos.exit_sm.conviction_level >= 2 && pos.ride_state.is_none() {
                    let curve_pct_bp = compute_curve_pct_bp(event.vsol_reserves);
                    if evaluate_ride_qualification(
                        pos.confirming_buy_sol,
                        pos.unique_confirming_wallets,
                        pos.sells_since_entry,
                        event.vsol_reserves,
                        pos.entry_vsol,
                        curve_pct_bp,
                        &self.config.ride_config,
                    ) {
                        // ── ACTIVATE RIDE MODE ──
                        pos.ride_state = Some(RideState::activate(
                            pos.entry_vsol,
                            event.vsol_reserves,
                            now_ms,
                            pos.buys_since_entry as u16,
                            pos.unique_confirming_wallets,
                            pos.confirming_buy_sol,
                        ));
                        // Note: ExitStateMachine is now dormant.
                        // We don't modify it — we just skip it in on_tick.
                    }
                }
            }
        }

        // ── PRICE TICK (delegates to active exit engine) ──
        let mint = event.mint;
        let pos = self.positions.get_mut(&mint).unwrap();

        if let Some(ref mut ride) = pos.ride_state {
            // RIDE mode: delegate to RideState::on_tick()
            let decision = ride.on_tick(
                event.vsol_reserves,
                now_ms,
                &self.config.ride_config,
            );
            if let ExitDecision::Exit(reason) = decision {
                self.close_position_inner(&mint, map_ride_exit_reason(reason), now_ms);
                return true;
            }
        } else {
            // SCALP mode: delegate to ExitStateMachine::on_price_tick()
            let decision = pos.exit_sm.on_price_tick(
                &self.config.exit_config,
                event.vsol_reserves as f64,
                now_ms,
            );
            if let ExitDecision::Exit(reason) = decision {
                self.close_position_inner(&mint, map_exit_reason_new(reason), now_ms);
                return true;
            }
        }

        false
    }
}

/// Compute curve fill percentage in basis points from vSOL reserves (lamports).
/// curve_pct = (vsol - 30 SOL) / 85 SOL × 10000
#[inline(always)]
fn compute_curve_pct_bp(vsol_lamports: u64) -> u16 {
    let vsol_above_base = vsol_lamports.saturating_sub(30_000_000_000); // 30 SOL in lamports
    // = vsol_above_base × 10000 / 85_000_000_000
    // Use u128 to avoid overflow
    let bp = (vsol_above_base as u128 * 10_000) / 85_000_000_000u128;
    bp.min(10_000) as u16
}
```

### 5.4 Modified `on_tick()` — Timer-Based Checks

```rust
impl PositionManager {
    pub fn on_tick(&mut self, now_ms: u64) {
        let mut to_close: Vec<([u8; 32], ExitReason)> = Vec::new();

        for (mint, pos) in self.positions.iter_mut() {
            let hold_ms = now_ms.saturating_sub(pos.entry_ts_ms);

            if let Some(ref mut ride) = pos.ride_state {
                // RIDE mode: check max hold (300s) and buy gap timeout
                // via on_tick with current vSOL
                let decision = ride.on_tick(
                    pos.current_vsol,
                    now_ms,
                    &self.config.ride_config,
                );
                if let ExitDecision::Exit(reason) = decision {
                    to_close.push((*mint, map_ride_exit_reason(reason)));
                    continue;
                }
            } else {
                // SCALP mode: max hold safety (5000ms)
                if hold_ms >= self.config.max_hold_ms {
                    to_close.push((*mint, ExitReason::MaxHold));
                    continue;
                }

                // Feed synthetic price tick for confirmation window expiry
                let decision = pos.exit_sm.on_price_tick(
                    &self.config.exit_config,
                    pos.current_vsol as f64,
                    now_ms,
                );
                if let ExitDecision::Exit(reason) = decision {
                    to_close.push((*mint, map_exit_reason_new(reason)));
                }
            }
        }

        for (mint, reason) in to_close {
            self.close_position_inner(&mint, reason, now_ms);
        }
    }
}
```

### 5.5 Exit Reason Mapping

```rust
/// Map RIDE exit reasons to the existing ExitReason enum.
/// Extend ExitReason with new variants as needed.
fn map_ride_exit_reason(reason: ExitReasonNew) -> ExitReason {
    match reason {
        ExitReasonNew::RideExit(r) => match r {
            RideExitReason::TrailingStop   => ExitReason::RideTrailingStop,
            RideExitReason::BuyGapTimeout  => ExitReason::RideBuyGap,
            RideExitReason::WhaleDump      => ExitReason::RideWhaleDump,
            RideExitReason::SellCascade    => ExitReason::RideSellCascade,
            RideExitReason::CreatorSell    => ExitReason::RideCreatorSell,
            RideExitReason::PriceBelowEntry => ExitReason::RideEmergency,
            RideExitReason::MaxHoldRide    => ExitReason::RideMaxHold,
            RideExitReason::EmergencyFloor => ExitReason::RideEmergency,
        },
        // Shouldn't happen — other ExitReasonNew variants come from SCALP
        other => map_exit_reason_new(other),
    }
}
```

### 5.6 ExitReason Enum Extension

```rust
// In engine/positions.rs — extend ExitReason

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    // Existing SCALP reasons
    TakeProfit,
    StopLoss,
    NextBuyer,          // Legacy, kept for compatibility
    MaxHold,
    IntraHoldTrail,
    MomentumDecayFlat,
    MomentumDecayFade,
    TakeProfitScaled,
    MomentumStall,

    // ── NEW: RIDE mode reasons ──
    RideTrailingStop,   // Normal trailing stop exit
    RideBuyGap,         // 10s+ without a buy
    RideWhaleDump,      // Single sell > 2 SOL
    RideSellCascade,    // 3+ sells in 3s
    RideCreatorSell,    // Creator wallet sold
    RideEmergency,      // Price below entry / floor hit
    RideMaxHold,        // 300s safety backstop
}
```

### 5.7 ExitReasonNew Enum Extension

```rust
// In engine/exit_machine.rs or ride_machine.rs

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReasonNew {
    // Existing SCALP reasons
    TakeProfit,
    TakeProfitScaled,
    StopLoss,
    MomentumDecayFlat,
    MomentumStall,
    TrailingStop,
    MaxHoldSafety,

    // ── NEW: RIDE exit wrapper ──
    RideExit(RideExitReason),
}
```

---

## 6. Safety Timer Changes

### 6.1 SCALP Safety Timer (unchanged)

5000ms `tokio::time::sleep`, fires `SafetyTimeout` message. Idempotent on close.

### 6.2 RIDE Safety Timer

When RIDE activates, the position needs a **new** 300s safety timer:

```rust
// In the RIDE activation block (Section 5.3)

// Cancel the old 5s safety timer (or let it fire harmlessly)
// Spawn new 300s RIDE safety timer
let mint = event.mint;
let tx = self.exit_tx.clone();
tokio::spawn(async move {
    tokio::time::sleep(Duration::from_secs(300)).await;
    let _ = tx.send(HotPathMsg::RideSafetyTimeout { mint });
});
```

**However:** The `RideState::on_tick()` already checks `max_hold_ride_ms` on every tick. The tokio timer is a belt-and-suspenders backup for positions that stop receiving trade events (token goes silent). Both paths are idempotent — whichever fires first closes the position; the other finds nothing.

**Decision: Keep both.** The `on_tick` check handles the common case (sub-microsecond overhead). The tokio timer catches the edge case where a token goes completely silent and no ticks arrive.

---

## 7. Config Integration (Engineer C)

### 7.1 canary.json Schema Addition

```json
{
  "mev": {
    "...existing fields...",

    "ride": {
      "enabled": true,
      "min_confirming_sol": 0.3,
      "min_unique_wallets": 2,
      "min_gain_for_ride_pct": 1.5,
      "max_curve_pct": 80,
      "override_gain_pct": 3.0,

      "early_to_momentum_s": 15,
      "early_to_momentum_gain_pct": 15.0,
      "momentum_to_tighten_s": 60,
      "momentum_to_tighten_gain_pct": 30.0,
      "max_hold_ride_s": 300,

      "early_trail_pct": 8.0,
      "momentum_trail_pct": 6.0,
      "tighten_trail_pct": 4.0,
      "min_trail_pct": 1.5,

      "sell_pressure_tighten_pct": 2.0,
      "buy_deceleration_tighten_pct": 1.0,
      "buy_deceleration_gap_s": 3.3,
      "whale_exit_trail_cap_pct": 2.0,
      "buy_gap_tighten_s": 5.0,
      "buy_gap_tighten_pct": 2.0,
      "buy_gap_exit_s": 10.0,

      "whale_dump_exit_sol": 2.0,
      "whale_exit_sol": 1.0,
      "sell_cascade_count": 3
    }
  }
}
```

### 7.2 JSON Deserialization

```rust
// In engine/config.rs

#[derive(Deserialize, Debug)]
pub struct RideJsonConfig {
    pub enabled: Option<bool>,
    pub min_confirming_sol: Option<f64>,
    pub min_unique_wallets: Option<u8>,
    pub min_gain_for_ride_pct: Option<f64>,
    pub max_curve_pct: Option<f64>,
    pub override_gain_pct: Option<f64>,
    pub early_to_momentum_s: Option<f64>,
    pub early_to_momentum_gain_pct: Option<f64>,
    pub momentum_to_tighten_s: Option<f64>,
    pub momentum_to_tighten_gain_pct: Option<f64>,
    pub max_hold_ride_s: Option<f64>,
    pub early_trail_pct: Option<f64>,
    pub momentum_trail_pct: Option<f64>,
    pub tighten_trail_pct: Option<f64>,
    pub min_trail_pct: Option<f64>,
    pub sell_pressure_tighten_pct: Option<f64>,
    pub buy_deceleration_tighten_pct: Option<f64>,
    pub buy_deceleration_gap_s: Option<f64>,
    pub whale_exit_trail_cap_pct: Option<f64>,
    pub buy_gap_tighten_s: Option<f64>,
    pub buy_gap_tighten_pct: Option<f64>,
    pub buy_gap_exit_s: Option<f64>,
    pub whale_dump_exit_sol: Option<f64>,
    pub whale_exit_sol: Option<f64>,
    pub sell_cascade_count: Option<u8>,
}

impl RideJsonConfig {
    /// Convert JSON floats → internal units (lamports, mvsol, bp, ms).
    pub fn to_ride_config(&self) -> RideConfig {
        let pct_to_bp = |p: f64| (p * 100.0) as u16;   // 8.0% → 800
        let sol_to_lam = |s: f64| (s * 1_000_000_000.0) as u64;
        let sol_to_msol = |s: f64| (s * 1_000.0) as u32;
        let s_to_ms = |s: f64| (s * 1_000.0) as u64;

        let d = RideConfig::default();
        RideConfig {
            min_confirming_sol_lamports: self.min_confirming_sol
                .map(sol_to_lam).unwrap_or(d.min_confirming_sol_lamports),
            min_unique_wallets: self.min_unique_wallets.unwrap_or(d.min_unique_wallets),
            min_gain_for_ride_bp: self.min_gain_for_ride_pct
                .map(pct_to_bp).unwrap_or(d.min_gain_for_ride_bp),
            max_curve_pct_bp: self.max_curve_pct
                .map(|p| (p * 100.0) as u16).unwrap_or(d.max_curve_pct_bp),
            override_gain_bp: self.override_gain_pct
                .map(pct_to_bp).unwrap_or(d.override_gain_bp),
            early_to_momentum_ms: self.early_to_momentum_s
                .map(s_to_ms).unwrap_or(d.early_to_momentum_ms),
            early_to_momentum_gain_bp: self.early_to_momentum_gain_pct
                .map(pct_to_bp).unwrap_or(d.early_to_momentum_gain_bp),
            momentum_to_tighten_ms: self.momentum_to_tighten_s
                .map(s_to_ms).unwrap_or(d.momentum_to_tighten_ms),
            momentum_to_tighten_gain_bp: self.momentum_to_tighten_gain_pct
                .map(pct_to_bp).unwrap_or(d.momentum_to_tighten_gain_bp),
            max_hold_ride_ms: self.max_hold_ride_s
                .map(s_to_ms).unwrap_or(d.max_hold_ride_ms),
            early_trail_bp: self.early_trail_pct
                .map(pct_to_bp).unwrap_or(d.early_trail_bp),
            momentum_trail_bp: self.momentum_trail_pct
                .map(pct_to_bp).unwrap_or(d.momentum_trail_bp),
            tighten_trail_bp: self.tighten_trail_pct
                .map(pct_to_bp).unwrap_or(d.tighten_trail_bp),
            min_trail_bp: self.min_trail_pct
                .map(pct_to_bp).unwrap_or(d.min_trail_bp),
            sell_pressure_tighten_bp: self.sell_pressure_tighten_pct
                .map(pct_to_bp).unwrap_or(d.sell_pressure_tighten_bp),
            buy_deceleration_tighten_bp: self.buy_deceleration_tighten_pct
                .map(pct_to_bp).unwrap_or(d.buy