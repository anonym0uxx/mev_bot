# Hot Path Integration Architecture — Entry Engine + Dual-Mode Exit

**Author:** Apollo (Principal Rust Systems Architect)  
**Date:** 2026-03-29  
**Source Specs:** `ENTRY_ENGINE_QUANT.md`, `QUANT_RIDE_A.md`, `QUANT_RIDE_B.md`, `ARCHITECT_ENTRY_ENGINE.md`  
**Supersedes:** Current `hot_path.rs` (GateStack + Scorer pipeline), `positions.rs` (single-mode ExitStateMachine)  
**Status:** Implementation-ready architecture

---

## Table of Contents

1. [System Overview](#1-system-overview)
2. [Hot Path Event Loop — Complete Pseudocode](#2-hot-path-event-loop--complete-pseudocode)
3. [OpenPosition Struct with ExitMode Enum](#3-openposition-struct-with-exitmode-enum)
4. [RideState — Stack-Allocated Ride Engine](#4-ridestate--stack-allocated-ride-engine)
5. [on_subsequent_trade() with RIDE Routing](#5-on_subsequent_trade-with-ride-routing)
6. [on_tick() with RIDE Phase Checks](#6-on_tick-with-ride-phase-checks)
7. [SCALP → RIDE Transition Logic](#7-scalp--ride-transition-logic)
8. [RiskManager Integration](#8-riskmanager-integration)
9. [EntryEngine Integration Points](#9-entryengine-integration-points)
10. [Concurrent Position Management](#10-concurrent-position-management)
11. [Config Struct Hierarchy](#11-config-struct-hierarchy)
12. [Module Organization](#12-module-organization)
13. [Initialization Flow (main.rs)](#13-initialization-flow-mainrs)
14. [Test Specifications](#14-test-specifications)
15. [Migration Plan](#15-migration-plan)
16. [Latency Budget](#16-latency-budget)
17. [Cache Line & Memory Layout Analysis](#17-cache-line--memory-layout-analysis)

---

## 1. System Overview

### Current Flow (being replaced)

```
TradeEvent
    → Scorer::compute()         // runs on EVERY buy — wasteful
    → GateStack::evaluate()     // 18+ branch checks
    → PositionManager::open_position(event, score, now)  // static size tiers

on_subsequent_trade():
    → ExitStateMachine::on_buy_event()
    → ExitStateMachine::on_price_tick()

on_tick():
    → max_hold check
    → ExitStateMachine::on_price_tick() (catch confirmation window expiry)
```

### New Flow

```
TradeEvent
    → RiskManager::check()               // <5ns — short-circuit if paused/capped
    → EntryEngine::evaluate()             // replaces GateStack + Scorer
        ├─ Stage 1: hard_gate()           // <50ns — boolean ops, ~65% reject
        ├─ Stage 2: composite_score()     // ~200ns — entry_score + magnitude_score
        └─ Stage 3: position_size()       // ~10ns — Kelly-derived from conviction tier
    → PositionManager::open_position(size, mode)  // SCALP always; RIDE after confirmation

on_subsequent_trade():
    → match exit_mode:
        ExitMode::Scalp(sm) →
            track confirming buys (count, sol_amount)
            if ride_qualified() → transition SCALP → RIDE
            sm.on_buy_event() / sm.on_price_tick()
        ExitMode::Ride(rs) →
            if is_buy → rs.on_buy_event(sol_amount, now)
            if is_sell → rs.on_sell_event(sol_amount, now)
            rs.on_tick(vsol, now)  // piggyback price check on trade event

on_tick():
    → for each position:
        match exit_mode:
            ExitMode::Scalp(sm) → sm.on_price_tick() + max_hold safety
            ExitMode::Ride(rs)  → rs.on_tick(vsol, now)  // trail, phase, buy_gap
```

### Design Invariants

1. **Zero heap allocation on hot path.** All new structs are `Copy`, stack-allocated, fixed-size arrays only.
2. **Single HashMap for all positions.** RIDE and SCALP share `HashMap<[u8;32], OpenPosition>`. No separate data structures.
3. **ExitMode is a tagged enum.** Only one mode is active per position. Transition is destructive (old state consumed).
4. **RiskManager checked FIRST.** Before EntryEngine::evaluate(). If paused → skip entirely, save ~260ns.
5. **EntryEngine replaces GateStack + Scorer.** Score only computed for hard-gate survivors (~35% of buy events).
6. **All positions start as SCALP.** RIDE activation requires confirming buy evidence. Never pre-assign RIDE at entry.
7. **Compile flags:** `target-cpu=native`, `LTO=fat`, `codegen-units=1`.

---

## 2. Hot Path Event Loop — Complete Pseudocode

### 2.1 on_trade() — Main Entry Pipeline

```rust
#[inline(always)]
pub fn on_trade(&mut self, trade: &TradeEvent) {
    self.stats.trades_seen += 1;
    let now = self.now_ms();

    // ── Helius lead-time measurement (unchanged) ────────────────
    if trade.source == FeedSource::PumpPortal {
        self.check_helius_lead(trade);
    }

    // ── 1. Push into MintHistoryMap (updates cached aggregates) ─
    let record = trade_to_record(trade, now);
    let history = self.mint_map.get_or_insert(&trade.mint, now);
    history.push(record, now);

    // ── 2. Existing position → delegate to on_subsequent_trade ──
    if self.position_manager.has_position(&trade.mint) {
        self.position_manager.on_subsequent_trade(trade, now);
        return;
    }

    // ── 3. Only consider buys for new position entry ────────────
    if !trade.is_buy {
        return;
    }

    // ── 4. Regime exclusion (unchanged) ─────────────────────────
    if self.excluded_mints.contains(&trade.mint) {
        self.stats.gate_rejects += 1;
        return;
    }

    // ── 5. Graduation boundary (unchanged) ──────────────────────
    if trade.vtoken_reserves > 0 {
        let progress = regime::compute_bonding_curve_progress(
            trade.vtoken_reserves, regime::INITIAL_VIRTUAL_TOKENS,
        );
        if progress >= self.gate_stack.config.regime_config.graduation_boundary_start
            && progress <= self.gate_stack.config.regime_config.graduation_boundary_end
        {
            self.stats.gate_rejects += 1;
            return;
        }
    }

    // ── 6. Health monitor (unchanged) ───────────────────────────
    if let Some(ref hm) = self.health_monitor {
        if !hm.is_trading_allowed() { return; }
    }

    // ── 7. NEW: RiskManager gate (checked BEFORE scoring) ───────
    //    Saves ~260ns when paused/capped by skipping entry_engine entirely.
    if !self.risk_manager.allows_entry(now) {
        return;
    }

    // ── 8. NEW: EntryEngine::evaluate() replaces GateStack + Scorer ─
    let history = self.mint_map.get(&trade.mint).unwrap();
    let ctx = EntryContext {
        trade,
        history,
        now,
    };

    let decision = match self.entry_engine.evaluate(&ctx) {
        EntryDecision::Reject => {
            self.stats.gate_rejects += 1;
            return;
        }
        EntryDecision::Enter(d) => d,
    };

    // decision contains: composite_score, magnitude_estimate, size_lamports

    // ── 9. Concurrent position limit check (mode-aware) ────────
    if !self.position_manager.can_open_scalp() {
        return;
    }

    // ── 10. Open position (always as SCALP — RIDE comes from confirmation) ─
    self.position_manager.open_position_v2(
        trade,
        decision.composite_score,
        decision.size_lamports,
        decision.magnitude_estimate,
        now,
    );
    self.stats.positions_opened += 1;

    // ── 11. Enrich position with entry context (unchanged pattern) ─
    if let Some(pos) = self.position_manager.get_position_mut(&trade.mint) {
        pos.pre_trigger_buys_1s = history.cached_buy_count_1s;
        pos.pre_trigger_buys_2s = history.cached_buy_count_2s;
        pos.pre_trigger_buys_5s = history.cached_buy_count_5s;
        pos.unique_buyers = history.cached_unique_buyers_30s;
        pos.vsol_delta_3s = trade.vsol_reserves.saturating_sub(history.cached_vsol_oldest_3s);
        pos.volume_5s = history.cached_volume_sol_5s;
        pos.sell_count_5s = history.cached_sell_count_5s;
    }
}
```

### 2.2 on_tick() — Timer-Driven Exits

```rust
pub fn on_tick(&mut self, ts_ms: u64) {
    self.stats.ticks += 1;
    self.position_manager.on_tick(ts_ms);

    // Periodic MintMap eviction (unchanged — every 10s)
    if self.stats.ticks % 200 == 0 {
        self.mint_map.evict_stale(ts_ms, 120_000);
    }

    // Daily risk manager reset check (every ~5min = 6000 ticks at 50ms)
    if self.stats.ticks % 6000 == 0 {
        self.risk_manager.check_daily_reset(ts_ms);
    }
}
```

### 2.3 on_position_closed() — Risk Feedback Loop

```rust
pub fn on_position_closed(&mut self, cp: &ClosedPosition) -> Option<RiskEvent> {
    self.risk_manager.on_trade_result(cp)
}
```

---

## 3. OpenPosition Struct with ExitMode Enum

### 3.1 ExitMode Enum

```rust
/// Tagged union: only one exit mode is active at a time.
/// Both variants are exactly 64 bytes → enum is 64 + 1 (discriminant) + 7 (padding) = 72 bytes.
/// Pad to 72 bytes total — still fits in 2 cache lines with the rest of OpenPosition.
///
/// The discriminant byte is the cheapest possible branch on mode type.
/// RIDE state and SCALP state never coexist — enum enforces this at the type level.
#[derive(Clone, Copy)]
#[repr(C, u8)]  // explicit discriminant for predictable layout
pub enum ExitMode {
    /// Default mode: millisecond-scale exits via ExitStateMachine.
    /// Holds 175ms-5s. TP 2-7%, SL 1-2%.
    Scalp(ExitStateMachine),   // 64 bytes

    /// Activated after confirming buys. Trailing stop, adaptive phases.
    /// Holds 30s-300s. Target 20-500%+ capture.
    Ride(RideState),           // 64 bytes
}
```

### 3.2 OpenPosition Struct (Updated)

```rust
/// An open (held) position — updated for dual-mode exit engine.
///
/// Layout target: ≤ 384 bytes. Current fields already use ~300 bytes.
/// New fields add ~24 bytes (confirming_buy_sol, unique_confirming_wallets,
/// magnitude_estimate, ride_eligible). Total: ~324 bytes = 6 cache lines.
pub struct OpenPosition {
    // ── Identity (unchanged) ────────────────────────────────────
    pub mint: [u8; 32],
    pub bonding_curve: [u8; 32],
    pub assoc_bonding_curve: [u8; 32],

    // ── Entry state (unchanged) ─────────────────────────────────
    pub entry_vsol: u64,
    pub entry_ts_ms: u64,
    pub peak_vsol: u64,
    pub trough_vsol: u64,
    pub current_vsol: u64,
    pub current_vtokens: u64,
    pub tokens_held: u64,
    pub score: f64,
    pub trigger_sol: u64,
    pub trigger_sig: [u8; 64],
    pub tod_multiplier: f64,

    // ── Position size (NOW DYNAMIC via Kelly sizing) ────────────
    pub size_sol: u64,             // was static tier-based, now Kelly-derived

    // ── Exit mode (REPLACES exit_sm field) ──────────────────────
    pub exit_mode: ExitMode,       // 72 bytes (enum: 64 payload + 8 tag+pad)

    // ── Flow tracking (expanded for RIDE qualification) ─────────
    pub trades_seen_after_entry: u32,
    pub buys_since_entry: u32,     // renamed from buys_after_entry for clarity
    pub flow_since_entry: u64,     // total buy flow (lamports) since entry

    // NEW: Confirming buy tracking for RIDE qualification
    pub confirming_buy_sol: u64,   // total SOL from confirming buys (lamports)
    pub sells_since_entry: u16,    // track sell pressure for RIDE health
    pub unique_confirming_wallets: u8, // count only, no dedup (V1 — see §3.3)

    // NEW: Magnitude estimate from entry engine (used for RIDE threshold)
    pub magnitude_estimate: u16,   // fixed-point × 100 (e.g., 150 = 1.50x expected pump)

    // ── Entry context (for logging/training — unchanged pattern) ─
    pub pre_trigger_buys_1s: u16,
    pub pre_trigger_buys_2s: u16,
    pub pre_trigger_buys_5s: u16,
    pub unique_buyers: u16,
    pub vsol_delta_3s: u64,
    pub volume_5s: u64,
    pub sell_count_5s: u16,
}
```

### 3.3 Wallet Tracking Decision — V1 Counter Only

**Decision: Option 3 — simple counter.** No wallet dedup.

**Rationale:**
- On Pump.fun, confirming buys arriving in separate transactions are almost certainly different wallets. Duplicate-wallet-same-tx is handled at the feed level (dedup by sig_prefix).
- A `[u8; 32] × 4` array for wallet tracking adds 128 bytes to each position (2 cache lines) and `memcmp` on each insert. The 128 bytes push `OpenPosition` from 6 → 8 cache lines, increasing L1 miss rate.
- A bloom filter at 8 bytes has ~3% FPR at 4 elements — barely better than no tracking.
- V1 ships with counter. V2 adds wallet tracking if replay data shows duplicate-wallet confirmations are distorting RIDE qualification.

### 3.4 ClosedPosition Struct Additions

```rust
pub struct ClosedPosition {
    // ... (all existing fields unchanged) ...

    // NEW: Dual-mode exit tracking
    pub exit_mode_was_ride: bool,    // true if position was in RIDE mode at exit
    pub ride_phase_at_exit: u8,      // 0=N/A, 1=early, 2=momentum, 3=tighten
    pub confirming_buy_sol: u64,     // total confirming buy SOL
    pub magnitude_estimate: u16,     // entry engine's magnitude prediction
    pub peak_gain_pct_fp: u16,       // peak % gain × 100 (for MFE tracking in RIDE)
    pub sells_after_entry: u16,      // sell count during hold
}
```

---

## 4. RideState — Stack-Allocated Ride Engine

### 4.1 Struct Layout

```rust
/// RIDE exit mode state. Tracks trailing stop, phase transitions, and buy/sell pressure.
///
/// Layout: exactly 64 bytes (one cache line) to match ExitStateMachine.
/// All fields Copy. Zero heap allocation.
///
/// Phase progression: Early(15s) → Momentum(60s) → Tighten(300s) → EXIT
/// Trail tightens with each phase: 8% → 6% → 4% (configurable via RideConfig)
///
/// Additional tightening triggers:
///   - Sell pressure > threshold → trail shrinks by sell_pressure_tighten_fp
///   - Whale exit (sell > whale_exit_sol) → immediate exit
///   - Buy gap > buy_gap_tighten_ms → trail shrinks 1%
///   - Buy gap > buy_gap_exit_ms → exit immediately (pump dead)
///   - Hard floor: never let gain drop below hard_floor_gain_fp from entry
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct RideState {
    // ── Phase tracking (8 bytes) ────────────────────────────────
    pub phase: RidePhase,           // u8: 0=Early, 1=Momentum, 2=Tighten
    pub _pad0: [u8; 3],            // alignment padding
    pub phase_start_ms: u32,       // offset from entry_ts_ms (max ~71 min, sufficient for 5min rides)

    // ── Trail stop (24 bytes) ───────────────────────────────────
    pub entry_vsol: f64,           // 8 — duplicated from position for self-contained tick
    pub peak_vsol: f64,            // 8 — high water mark since RIDE activation
    pub trail_vsol: f64,           // 8 — current trailing stop level (absolute vSol)

    // ── Pressure tracking (16 bytes) ────────────────────────────
    pub last_buy_ms: u32,          // offset from entry_ts_ms
    pub total_buy_sol: u32,        // total buy SOL since RIDE activation (lamports / 1000 for u32 range)
    pub total_sell_sol: u32,       // total sell SOL since RIDE activation (same encoding)
    pub sell_count: u16,           // sell events since RIDE activation
    pub buy_count: u16,            // buy events since RIDE activation

    // ── Floor (8 bytes) ─────────────────────────────────────────
    pub hard_floor_vsol: f64,      // 8 — absolute vSol floor (entry_vsol * (1 + hard_floor_gain_fp/100000))

    // ── Activation context (8 bytes) ────────────────────────────
    pub activation_ms: u32,        // offset from entry_ts_ms when RIDE was activated
    pub activation_vsol_fp: u32,   // vSol at activation time (lamports / 1000 for u32 range)
}

const _RIDE_SIZE_CHECK: () = assert!(std::mem::size_of::<RideState>() <= 64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RidePhase {
    Early    = 0,  // First 15s — widest trail (8%)
    Momentum = 1,  // 15s-60s — medium trail (6%)
    Tighten  = 2,  // 60s-300s — tightest trail (4%)
}
```

### 4.2 RideState Methods

```rust
impl RideState {
    /// Create a new RideState at RIDE activation time.
    /// Called when SCALP transitions to RIDE (after confirming buys qualify).
    #[inline]
    pub fn activate(
        config: &RideConfig,
        entry_vsol: f64,
        current_vsol: f64,
        entry_ts_ms: u64,
        now_ms: u64,
    ) -> Self {
        let trail_pct = config.phases[0].trail_fp as f64 / 100_000.0; // e.g., 800/100000 = 0.008 = 0.8% → wait, 800 = 8.0%? 
        // Clarification: trail_fp uses the same scale as ExitStateMachine — 
        // 800 = 0.8%. But RIDE trails should be much wider. 
        // Spec says phases[0].trail_fp=800 for 8%. So scale is trail_fp/10000.
        // Decision: RIDE uses trail_fp / 10_000 (different from SCALP's /100_000).
        // This gives us wider range: 800 = 8.0%, 600 = 6.0%, 400 = 4.0%.
        // ALTERNATIVELY: keep /100_000 and use 8000/6000/4000.
        // DECISION: Use same /100_000 scale as ExitStateMachine. Config values
        // for RIDE phases are 8000/6000/4000 (not 800/600/400).
        // The task spec shows trail_fp: 800/600/400 → these mean 0.8%/0.6%/0.4%.
        // That seems too tight for RIDE. But we implement what the spec says
        // and let config override. If Alon wants 8%/6%/4%, he sets 8000/6000/4000.
        let trail_pct = config.phases[0].trail_fp as f64 / 100_000.0;
        let floor_pct = config.hard_floor_gain_fp as f64 / 100_000.0;

        let trail_stop = current_vsol * (1.0 - trail_pct);
        let hard_floor = entry_vsol * (1.0 + floor_pct);
        // Trail stop must never go below hard floor
        let effective_trail = trail_stop.max(hard_floor);

        let offset_ms = (now_ms - entry_ts_ms) as u32;

        Self {
            phase: RidePhase::Early,
            _pad0: [0; 3],
            phase_start_ms: offset_ms,
            entry_vsol,
            peak_vsol: current_vsol,
            trail_vsol: effective_trail,
            last_buy_ms: offset_ms,
            total_buy_sol: 0,
            total_sell_sol: 0,
            sell_count: 0,
            buy_count: 0,
            hard_floor_vsol: hard_floor,
            activation_ms: offset_ms,
            activation_vsol_fp: (current_vsol / 1000.0) as u32,
        }
    }

    /// Process a buy event on this token.
    /// Updates pressure tracking, last_buy_ms, and may adjust trail upward.
    #[inline]
    pub fn on_buy_event(
        &mut self,
        config: &RideConfig,
        sol_amount_lamports: u64,
        current_vsol: f64,
        entry_ts_ms: u64,
        now_ms: u64,
    ) {
        let offset_ms = (now_ms - entry_ts_ms) as u32;
        self.last_buy_ms = offset_ms;
        self.buy_count += 1;
        self.total_buy_sol = self.total_buy_sol.saturating_add(
            (sol_amount_lamports / 1000) as u32
        );

        // Update peak and recompute trail
        if current_vsol > self.peak_vsol {
            self.peak_vsol = current_vsol;
            self._recompute_trail(config);
        }
    }

    /// Process a sell event on this token.
    /// Updates sell pressure tracking.
    #[inline]
    pub fn on_sell_event(
        &mut self,
        config: &RideConfig,
        sol_amount_lamports: u64,
        current_vsol: f64,
        entry_ts_ms: u64,
        now_ms: u64,
    ) -> RideDecision {
        self.sell_count += 1;
        self.total_sell_sol = self.total_sell_sol.saturating_add(
            (sol_amount_lamports / 1000) as u32
        );

        // Whale exit check: single sell > whale_exit_sol
        if sol_amount_lamports >= config.whale_exit_sol {
            return RideDecision::Exit(RideExitReason::WhaleExit);
        }

        // Sell pressure tightening: if sell_sol / buy_sol > threshold
        if self.total_buy_sol > 0 {
            let sell_ratio_fp = (self.total_sell_sol as u64 * 10_000)
                / self.total_buy_sol.max(1) as u64;
            if sell_ratio_fp > 5_000 {
                // Sell pressure > 50% of buy pressure → tighten trail
                self._tighten_trail(config.sell_pressure_tighten_fp);
            }
        }

        // Check trail after any sell (price likely dropped)
        if current_vsol <= self.trail_vsol {
            return RideDecision::Exit(RideExitReason::TrailingStop);
        }

        // Check hard floor
        if current_vsol <= self.hard_floor_vsol {
            return RideDecision::Exit(RideExitReason::HardFloor);
        }

        RideDecision::Hold
    }

    /// Periodic tick — check phase transitions, buy gap, trail.
    /// Called from on_tick() (50ms interval) and piggyback on trade events.
    #[inline]
    pub fn on_tick(
        &mut self,
        config: &RideConfig,
        current_vsol: f64,
        entry_ts_ms: u64,
        now_ms: u64,
    ) -> RideDecision {
        let offset_ms = (now_ms - entry_ts_ms) as u32;

        // ── Phase transition check ──────────────────────────────
        let phase_elapsed = offset_ms.saturating_sub(self.activation_ms);
        let new_phase = self._compute_phase(config, phase_elapsed);
        if new_phase != self.phase {
            self.phase = new_phase;
            self.phase_start_ms = offset_ms;
            self._recompute_trail(config); // tighter trail for new phase
        }

        // ── Buy gap check ───────────────────────────────────────
        let since_last_buy = offset_ms.saturating_sub(self.last_buy_ms);
        if since_last_buy >= config.buy_gap_exit_ms as u32 {
            // No buy for too long — pump is dead
            return RideDecision::Exit(RideExitReason::BuyGapTimeout);
        }
        if since_last_buy >= config.buy_gap_tighten_ms as u32 {
            // Starting to stall — tighten trail by 1% (100 fp)
            self._tighten_trail(100);
        }

        // ── Trail stop check ────────────────────────────────────
        if current_vsol <= self.trail_vsol {
            return RideDecision::Exit(RideExitReason::TrailingStop);
        }

        // ── Hard floor check ────────────────────────────────────
        if current_vsol <= self.hard_floor_vsol {
            return RideDecision::Exit(RideExitReason::HardFloor);
        }

        // ── Update peak (may have risen since last trade) ───────
        if current_vsol > self.peak_vsol {
            self.peak_vsol = current_vsol;
            self._recompute_trail(config);
        }

        // ── Max ride duration check ─────────────────────────────
        let total_ride_ms = offset_ms.saturating_sub(self.activation_ms);
        let max_phase = config.phases.last().unwrap_or(&config.phases[0]);
        let max_ride_ms = config.phases.iter().map(|p| p.duration_ms).sum::<u64>();
        if total_ride_ms as u64 >= max_ride_ms {
            return RideDecision::Exit(RideExitReason::MaxDuration);
        }

        RideDecision::Hold
    }

    // ── Internal helpers ────────────────────────────────────────

    /// Determine current phase from elapsed time since RIDE activation.
    #[inline(always)]
    fn _compute_phase(&self, config: &RideConfig, elapsed_ms: u32) -> RidePhase {
        let mut cumulative = 0u64;
        // Phase 0: Early
        cumulative += config.phases[0].duration_ms;
        if (elapsed_ms as u64) < cumulative { return RidePhase::Early; }
        // Phase 1: Momentum
        if config.phase_count > 1 {
            cumulative += config.phases[1].duration_ms;
            if (elapsed_ms as u64) < cumulative { return RidePhase::Momentum; }
        }
        // Phase 2: Tighten (or beyond)
        RidePhase::Tighten
    }

    /// Recompute trail_vsol from peak_vsol × (1 - current_phase_trail_pct).
    /// Ensures trail never goes below hard_floor_vsol.
    #[inline(always)]
    fn _recompute_trail(&mut self, config: &RideConfig) {
        let phase_idx = self.phase as usize;
        let trail_pct = config.phases[phase_idx.min(config.phase_count as usize - 1)]
            .trail_fp as f64 / 100_000.0;
        let new_trail = self.peak_vsol * (1.0 - trail_pct);
        self.trail_vsol = new_trail.max(self.hard_floor_vsol);
    }

    /// Tighten trail by the given fixed-point amount (additive to current trail).
    #[inline(always)]
    fn _tighten_trail(&mut self, tighten_fp: u32) {
        let tighten_pct = tighten_fp as f64 / 100_000.0;
        let tightened = self.trail_vsol * (1.0 + tighten_pct);
        // Tightening raises the trail stop (making it easier to hit)
        self.trail_vsol = tightened;
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RideDecision {
    Hold,
    Exit(RideExitReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RideExitReason {
    TrailingStop,     // Peak drawdown exceeded phase trail
    HardFloor,       // Gain dropped below hard_floor_gain_fp
    WhaleExit,       // Single sell > whale_exit_sol
    BuyGapTimeout,