// engine/ride_state.rs — RIDE mode signal-driven exit engine v2.
//
// 128 bytes (2 cache lines). Zero heap. Zero f64. All integer arithmetic.
// Signal-driven 4-state machine replaces time-based 3-phase system.
//
// State machine: StrongPump ↔ Sustained ↔ Weakening → Exit
//   Transitions are bidirectional (can recover) except Exit (terminal).
//   Trail width computed dynamically every event from:
//     trail_bp = (base_trail × kelly_mult × phase_mult) >> 16
//
// References:
//   QUANT_UNIFIED_RIDE.md  — architecture spec
//   QUANT_HOLD_SIGNAL.md   — composite signal design
//   QUANT_KELLY_SL.md      — Kelly-derived dynamic trail
//   QUANT_PUMP_LIFECYCLE.md — pump phase detection

use crate::engine::signal_engine::{
    self, KellyConfig, LifecycleConfig, SignalWeights,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Max hold safety backstop — only fires if signals somehow never trigger exit.
pub const MAX_HOLD_RIDE_MS: u64 = 300_000;

/// Hard floor: any price below entry → immediate exit.
pub const HARD_FLOOR_ENABLED: bool = true;

/// Buy gap immediate exit threshold (ms).
pub const BUY_GAP_EXIT_MS: u16 = 10_000;

/// Sell cascade: N sells in SELL_CASCADE_WINDOW_MS → exit.
pub const SELL_CASCADE_COUNT: u8 = 3;
pub const SELL_CASCADE_WINDOW_MS: u16 = 3_000;

// Ring buffer sizes
pub const BUY_RING_LEN: usize = 8;
pub const SELL_RING_LEN: usize = 4;

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Signal-driven state. Replaces time-based RidePhase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SignalState {
    StrongPump = 0,
    Sustained  = 1,
    Weakening  = 2,
    Exit       = 3,
}

impl SignalState {
    #[inline(always)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StrongPump => "strong_pump",
            Self::Sustained  => "sustained",
            Self::Weakening  => "weakening",
            Self::Exit       => "exit",
        }
    }
}

// Keep old RidePhase for backward compat with ClosedPosition logging
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RidePhase {
    Early    = 0,
    Momentum = 1,
    Tighten  = 2,
}

impl RidePhase {
    #[inline(always)]
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Early,
            1 => Self::Momentum,
            _ => Self::Tighten,
        }
    }
}

/// Bitflags for emergency conditions.
pub mod ride_flags {
    pub const CREATOR_SELL:       u8 = 1 << 0;
    pub const EMERGENCY_EXIT:     u8 = 1 << 1;
    pub const SELL_CASCADE_SEEN:  u8 = 1 << 2;
    pub const WHALE_EXIT_SEEN:    u8 = 1 << 3;
}

/// Decision returned from on_tick / on_sell_event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RideDecision {
    Hold,
    Exit(RideExitReason),
}

/// Why RIDE exited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RideExitReason {
    TrailingStop,
    HardFloor,
    WhaleExit,
    BuyGapTimeout,
    SellCascade,
    CreatorSell,
    MaxHold,
    SignalExit,
}

// ---------------------------------------------------------------------------
// RideConfig — loaded from config.rs, passed by reference on hot path
// ---------------------------------------------------------------------------
// RideConfig is defined in config.rs. We re-export what we need via
// crate::engine::config::RideConfig. Here we define helper methods to
// extract signal sub-configs.

impl super::config::RideConfig {
    /// Extract SignalWeights for signal_engine calls.
    #[inline(always)]
    pub fn signal_weights(&self) -> SignalWeights {
        SignalWeights {
            w_buy_rate_1s: self.w_buy_rate_1s,
            w_buy_rate_5s: self.w_buy_rate_5s,
            w_sell_rate_5s: self.w_sell_rate_5s,
            w_vol_accel_shift: self.w_vol_accel_shift,
            w_buy_gap_divisor: self.w_buy_gap_divisor,
            w_sell_pressure_shift: self.w_sell_pressure_shift,
            w_pnl_shift: self.w_pnl_shift,
            w_time_since_peak_divisor: self.w_time_since_peak_divisor,
            w_unique_wallets: self.w_unique_wallets,
            w_confirm_vol_shift: self.w_confirm_vol_shift,
        }
    }

    /// Extract KellyConfig.
    #[inline(always)]
    pub fn kelly_config(&self) -> KellyConfig {
        KellyConfig {
            baseline_f_permille: self.kelly_baseline_f_permille,
            sqrt_lut: signal_engine::KELLY_SQRT_LUT,
        }
    }

    /// Extract LifecycleConfig.
    #[inline(always)]
    pub fn lifecycle_config(&self) -> LifecycleConfig {
        LifecycleConfig {
            accel_min_buys: self.lifecycle_accel_min_buys,
            accel_min_sol_msol: self.lifecycle_accel_min_sol_msol,
            momentum_min_buys: self.lifecycle_momentum_min_buys,
            momentum_min_sol_msol: self.lifecycle_momentum_min_sol_msol,
        }
    }
}

// Re-export RideConfig from config module for positions.rs compatibility
pub use super::config::RideConfig;

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

/// Convert lamports (1 SOL = 1_000_000_000) to milli-vSOL (1 SOL = 1000 mvsol).
#[inline(always)]
pub fn lamports_to_mvsol(lamports: u64) -> u32 {
    (lamports / 1_000_000) as u32
}

/// Convert milli-vSOL back to lamports.
#[inline(always)]
pub fn mvsol_to_lamports(mvsol: u32) -> u64 {
    mvsol as u64 * 1_000_000
}

/// Compute trail stop level: peak - (peak * trail_bp / 10000).
/// Trail can only ratchet UP (tighter = higher stop).
#[inline(always)]
pub fn compute_trail_stop(peak_mvsol: u32, trail_bp: u16) -> u32 {
    let drop = (peak_mvsol as u64 * trail_bp as u64) / 10_000;
    peak_mvsol.saturating_sub(drop as u32)
}

// ---------------------------------------------------------------------------
// RideState v2 — 128 bytes, 2 cache lines
// ---------------------------------------------------------------------------

/// Signal-driven RIDE exit state. 128 bytes exactly.
///
/// Cache line 0 (bytes 0-63): HOT — accessed every event.
/// Cache line 1 (bytes 64-127): WARM — ring buffers + bloom.
#[derive(Clone, Copy)]
#[repr(C, align(64))]
pub struct RideState {
    // ── Cache line 0: trail + timing + counters + signal ──────────

    // Trail state (16 bytes)
    pub peak_mvsol: u32,           // highest vSOL seen
    pub trail_stop_mvsol: u32,     // current trail stop (ratchets up)
    pub entry_mvsol: u32,          // vSOL at entry
    pub current_trail_bp: u16,     // active trail distance in vSOL bp
    pub state: SignalState,        // u8: signal-driven state
    pub flags: u8,                 // bitflags

    // Timing (16 bytes)
    pub ride_start_ms: u64,        // entry timestamp
    pub last_buy_ms: u64,          // last buy event timestamp

    // Counters (16 bytes)
    pub buys_after_entry: u16,
    pub sells_after_entry: u16,
    pub unique_wallets: u8,        // approx via bloom filter
    _pad0: [u8; 3],
    pub confirming_vol_msol: u32,  // cumulative buy volume in milli-SOL
    pub peak_pnl_bp: i16,         // best unrealized PnL in basis points
    pub peak_pnl_ms_rel: u16,     // when peak occurred (relative to entry, ms)

    // Signal composite (16 bytes)
    pub composite_score: u16,      // 0-1000
    pub kelly_trail_mult: u16,    // 8.8 fixed-point (256 = 1.0x)
    pub phase_trail_mult: u16,    // 8.8 fixed-point (256 = 1.0x)
    pub vol_accel_bp: i16,        // volume acceleration
    pub price_velocity: i32,      // EMA-smoothed vSOL delta/s
    pub peak_composite: u16,      // peak composite score seen
    pub entry_f_permille: u16,    // Kelly f* at entry (conviction prior for exit trail)

    // ── Cache line 1: ring buffers + bloom ────────────────────────

    // Buy ring: 8 entries × (u16 timestamp_rel + u16 amount_msol) = 32 bytes
    pub buy_ts_ring: [u16; BUY_RING_LEN],
    pub buy_sol_ring: [u16; BUY_RING_LEN],

    // Sell ring: 4 entries × (u16 timestamp_rel + u16 amount_msol) = 16 bytes
    pub sell_ts_ring: [u16; SELL_RING_LEN],
    pub sell_sol_ring: [u16; SELL_RING_LEN],

    // Ring indices + bloom + metadata (16 bytes)
    pub buy_ring_idx: u8,
    pub sell_ring_idx: u8,
    pub bloom_filter: [u8; 8],
    pub vol_recent_msol: u16,     // buy vol in [now-2s, now] for accel
    pub vol_prior_msol: u16,      // buy vol in [now-4s, now-2s] for accel

    // Legacy compat (2 bytes)
    pub phase: RidePhase,         // maps signal state for logging
    pub _pad2: u8,
}

// Compile-time size check
const _: () = assert!(core::mem::size_of::<RideState>() == 128);

impl core::fmt::Debug for RideState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RideState")
            .field("state", &self.state)
            .field("score", &self.composite_score)
            .field("trail_bp", &self.current_trail_bp)
            .field("peak_mvsol", &self.peak_mvsol)
            .field("buys", &self.buys_after_entry)
            .field("sells", &self.sells_after_entry)
            .field("wallets", &self.unique_wallets)
            .field("entry_f_permille", &self.entry_f_permille)
            .finish()
    }
}

impl RideState {
    /// Create a new RideState for a freshly opened position.
    #[inline(always)]
    pub fn new(
        entry_mvsol: u32,
        _current_mvsol: u32,  // kept for API compat
        now_ms: u64,
        entry_f_permille: u32, // Kelly f* conviction from entry engine
        config: &RideConfig,
    ) -> Self {
        let initial_trail = config.trail_strong_pump_bp;
        let trail_stop = compute_trail_stop(entry_mvsol, initial_trail);

        RideState {
            // Trail
            peak_mvsol: entry_mvsol,
            trail_stop_mvsol: trail_stop,
            entry_mvsol,
            current_trail_bp: initial_trail,
            state: SignalState::StrongPump,
            flags: 0,

            // Timing
            ride_start_ms: now_ms,
            last_buy_ms: now_ms,

            // Counters
            buys_after_entry: 0,
            sells_after_entry: 0,
            unique_wallets: 0,
            _pad0: [0; 3],
            confirming_vol_msol: 0,
            peak_pnl_bp: 0,
            peak_pnl_ms_rel: 0,

            // Signal
            composite_score: 500, // neutral start
            kelly_trail_mult: 256, // 1.0x
            phase_trail_mult: signal_engine::PHASE_IGNITION,
            vol_accel_bp: 0,
            price_velocity: 0,
            peak_composite: 500,
            entry_f_permille: entry_f_permille.min(u16::MAX as u32) as u16,

            // Ring buffers — timestamps sentinel to u16::MAX so they don't falsely count as "in window"
            buy_ts_ring: [u16::MAX; BUY_RING_LEN],
            buy_sol_ring: [0; BUY_RING_LEN],
            sell_ts_ring: [u16::MAX; SELL_RING_LEN],
            sell_sol_ring: [0; SELL_RING_LEN],
            buy_ring_idx: 0,
            sell_ring_idx: 0,
            bloom_filter: [0; 8],
            vol_recent_msol: 0,
            vol_prior_msol: 0,

            phase: RidePhase::Early,
            _pad2: 0,
        }
    }

    /// Peak composite score seen during this position's lifetime.
    #[inline(always)]
    pub fn peak_composite_score(&self) -> u16 {
        self.peak_composite
    }

    /// Time relative to entry, clamped to u16 max (65535ms ≈ 65s).
    #[inline(always)]
    fn rel_ms(&self, now_ms: u64) -> u16 {
        let delta = now_ms.saturating_sub(self.ride_start_ms);
        delta.min(u16::MAX as u64) as u16
    }

    /// Compute buy_gap_ms (time since last buy).
    #[inline(always)]
    fn buy_gap_ms(&self, now_ms: u64) -> u16 {
        let gap = now_ms.saturating_sub(self.last_buy_ms);
        gap.min(60_000) as u16
    }

    /// Compute unrealized PnL in basis points.
    #[inline(always)]
    fn unrealized_pnl_bp(&self, current_mvsol: u32) -> i16 {
        if self.entry_mvsol == 0 { return 0; }
        let delta = current_mvsol as i64 - self.entry_mvsol as i64;
        let bp = (delta * 10_000) / self.entry_mvsol as i64;
        bp.max(-5000).min(5000) as i16
    }

    // ─── Signal computation ──────────────────────────────────────

    /// Recompute all signals and update composite score + trail.
    /// Called after every buy/sell event processing.
    #[inline(always)]
    fn recompute_signals(&mut self, current_mvsol: u32, now_ms: u64, config: &RideConfig) {
        let now_rel = self.rel_ms(now_ms);

        // Feature extraction from ring buffers
        let buy_rate_1s = signal_engine::count_in_window(
            &self.buy_ts_ring, self.buy_ring_idx,
            BUY_RING_LEN as u8, now_rel, 1000,
        );
        let buy_rate_5s = signal_engine::count_in_window(
            &self.buy_ts_ring, self.buy_ring_idx,
            BUY_RING_LEN as u8, now_rel, 5000,
        );
        let sell_rate_5s = signal_engine::count_in_window(
            &self.sell_ts_ring, self.sell_ring_idx,
            SELL_RING_LEN as u8, now_rel, 5000,
        );

        let sell_pressure = signal_engine::sell_pressure_ratio(buy_rate_5s, sell_rate_5s);
        let buy_gap = self.buy_gap_ms(now_ms);
        let pnl_bp = self.unrealized_pnl_bp(current_mvsol);
        let time_since_peak = now_rel.saturating_sub(self.peak_pnl_ms_rel);

        // Update volume acceleration (cast u16→u32 for signal_engine)
        self.vol_accel_bp = signal_engine::volume_acceleration_bp(
            self.vol_recent_msol as u32, self.vol_prior_msol as u32,
        );

        // Update price velocity EMA
        let vsol_delta = current_mvsol as i32 - self.entry_mvsol as i32;
        let dt = now_rel.max(1);
        self.price_velocity = signal_engine::update_price_velocity_ema(
            self.price_velocity, vsol_delta, dt,
        );

        // Composite score
        let weights = config.signal_weights();
        self.composite_score = signal_engine::compute_composite_score(
            buy_rate_1s, buy_rate_5s, sell_rate_5s,
            self.vol_accel_bp, buy_gap, sell_pressure,
            pnl_bp, time_since_peak,
            self.unique_wallets, self.confirming_vol_msol,
            &weights,
        );

        // Track peak
        if self.composite_score > self.peak_composite {
            self.peak_composite = self.composite_score;
        }

        // Kelly multiplier
        let kelly_cfg = config.kelly_config();
        self.kelly_trail_mult = signal_engine::compute_kelly_multiplier(
            self.buys_after_entry, self.confirming_vol_msol,
            self.sells_after_entry, &kelly_cfg,
        );

        // Blend with entry conviction: if entry f* was high, start with wider trail
        // When entry_f_permille == 0 (not set), skip boost (neutral 1.0x)
        if self.entry_f_permille > 0 {
            let entry_boost = (self.entry_f_permille as u32 * 256) / 671; // 671 = baseline f_permille
            self.kelly_trail_mult = ((self.kelly_trail_mult as u32 * entry_boost) >> 8)
                .min(400).max(128) as u16;
        }

        // Lifecycle multiplier
        let lifecycle_cfg = config.lifecycle_config();
        self.phase_trail_mult = signal_engine::compute_lifecycle_multiplier(
            self.buys_after_entry, self.confirming_vol_msol,
            self.unique_wallets, buy_rate_1s, &lifecycle_cfg,
        );

        // State transition based on composite score
        let new_state = if self.composite_score >= config.signal_strong_threshold {
            SignalState::StrongPump
        } else if self.composite_score >= config.signal_sustained_threshold {
            SignalState::Sustained
        } else if self.composite_score >= config.signal_weakening_threshold {
            SignalState::Weakening
        } else {
            SignalState::Exit
        };
        self.state = new_state;

        // Map to legacy phase for logging compat
        self.phase = match new_state {
            SignalState::StrongPump => RidePhase::Early,
            SignalState::Sustained  => RidePhase::Momentum,
            SignalState::Weakening | SignalState::Exit => RidePhase::Tighten,
        };

        // Dynamic trail computation
        let base_trail = match self.state {
            SignalState::StrongPump => config.trail_strong_pump_bp,
            SignalState::Sustained  => config.trail_sustained_bp,
            SignalState::Weakening  => config.trail_weakening_bp,
            SignalState::Exit       => 0, // will trigger exit in on_tick
        };

        // trail_bp = (base × kelly_mult × phase_mult) >> 16
        let trail = (base_trail as u32)
            .wrapping_mul(self.kelly_trail_mult as u32)
            .wrapping_mul(self.phase_trail_mult as u32)
            >> 16;
        let trail = trail.max(config.kelly_min_trail_bp as u32)
                         .min(config.kelly_max_trail_bp as u32);
        self.current_trail_bp = trail as u16;

        // Update peak PnL tracking
        if pnl_bp > self.peak_pnl_bp {
            self.peak_pnl_bp = pnl_bp;
            self.peak_pnl_ms_rel = now_rel;
        }
    }

    // ─── Buy event processing ────────────────────────────────────

    /// Process a buy event. Updates ring buffers, bloom filter, counters.
    #[inline(always)]
    pub fn on_buy_event(&mut self, sol_amount_mvsol: u32, now_ms: u64, wallet_hash: u64) {
        self.buys_after_entry = self.buys_after_entry.saturating_add(1);
        self.last_buy_ms = now_ms;

        // Accumulate confirming volume (milli-SOL)
        let amount_msol = sol_amount_mvsol.min(u16::MAX as u32) as u16;
        self.confirming_vol_msol = self.confirming_vol_msol.saturating_add(sol_amount_mvsol);

        // Update volume windows for acceleration calc
        // Simple approach: accumulate into vol_recent; rotation happens in recompute_signals
        self.vol_recent_msol = self.vol_recent_msol.saturating_add(amount_msol);

        // Write to buy ring buffer
        let now_rel = self.rel_ms(now_ms);
        let idx = (self.buy_ring_idx as usize) % BUY_RING_LEN;
        self.buy_ts_ring[idx] = now_rel;
        self.buy_sol_ring[idx] = amount_msol;
        self.buy_ring_idx = self.buy_ring_idx.wrapping_add(1);

        // Bloom filter for unique wallets
        signal_engine::bloom_insert(&mut self.bloom_filter, wallet_hash);
        self.unique_wallets = signal_engine::bloom_count(&self.bloom_filter);
    }

    // ─── Sell event processing ───────────────────────────────────

    /// Process a sell event. Returns Some(reason) if should exit immediately.
    #[inline(always)]
    pub fn on_sell_event(
        &mut self,
        sol_amount_mvsol: u32,
        now_ms: u64,
        config: &RideConfig,
    ) -> Option<RideExitReason> {
        self.sells_after_entry = self.sells_after_entry.saturating_add(1);

        let amount_msol = sol_amount_mvsol.min(u16::MAX as u32) as u16;
        let now_rel = self.rel_ms(now_ms);

        // Write to sell ring
        let idx = (self.sell_ring_idx as usize) % SELL_RING_LEN;
        self.sell_ts_ring[idx] = now_rel;
        self.sell_sol_ring[idx] = amount_msol;
        self.sell_ring_idx = self.sell_ring_idx.wrapping_add(1);

        // ── Emergency checks (override everything) ──

        // Creator sell flag
        if self.flags & ride_flags::CREATOR_SELL != 0 {
            return Some(RideExitReason::CreatorSell);
        }

        // Whale exit: single sell > threshold
        let whale_threshold_msol = (config.whale_exit_lamports / 1_000_000) as u32;
        if sol_amount_mvsol > whale_threshold_msol {
            self.flags |= ride_flags::WHALE_EXIT_SEEN;
            return Some(RideExitReason::WhaleExit);
        }

        // Sell cascade: check if N sells in window
        let cascade_count = signal_engine::count_in_window(
            &self.sell_ts_ring, self.sell_ring_idx,
            SELL_RING_LEN as u8, now_rel, SELL_CASCADE_WINDOW_MS,
        );
        if cascade_count >= SELL_CASCADE_COUNT {
            self.flags |= ride_flags::SELL_CASCADE_SEEN;
            return Some(RideExitReason::SellCascade);
        }

        None
    }

    // ─── Tick processing (called after buy or sell processing) ───

    /// Main tick: recompute signals, check trail stop, check emergency exits.
    /// Called after on_buy_event or on_sell_event, and periodically.
    #[inline(always)]
    pub fn on_tick(
        &mut self,
        current_mvsol: u32,
        now_ms: u64,
        config: &RideConfig,
    ) -> RideDecision {
        // ── Emergency overrides (highest priority) ──

        // Creator sell
        if self.flags & ride_flags::CREATOR_SELL != 0 {
            return RideDecision::Exit(RideExitReason::CreatorSell);
        }

        // Hard floor: price below entry
        if HARD_FLOOR_ENABLED && current_mvsol < self.entry_mvsol {
            return RideDecision::Exit(RideExitReason::HardFloor);
        }

        // Max hold safety backstop
        if now_ms.saturating_sub(self.ride_start_ms) >= config.max_hold_ms.max(MAX_HOLD_RIDE_MS) {
            return RideDecision::Exit(RideExitReason::MaxHold);
        }

        // Buy gap timeout: no buy in > threshold
        let gap = self.buy_gap_ms(now_ms);
        if gap >= BUY_GAP_EXIT_MS {
            return RideDecision::Exit(RideExitReason::BuyGapTimeout);
        }

        // ── Recompute signals ──
        self.recompute_signals(current_mvsol, now_ms, config);

        // ── Signal-driven exit ──
        if self.state == SignalState::Exit {
            return RideDecision::Exit(RideExitReason::SignalExit);
        }

        // ── Update peak and trail stop ──
        if current_mvsol > self.peak_mvsol {
            self.peak_mvsol = current_mvsol;
        }

        // Recompute trail stop from new peak and dynamic trail
        let new_stop = compute_trail_stop(self.peak_mvsol, self.current_trail_bp);
        // Trail stop can only ratchet UP (protect more profit)
        if new_stop > self.trail_stop_mvsol {
            self.trail_stop_mvsol = new_stop;
        }

        // ── Check trailing stop ──
        if current_mvsol <= self.trail_stop_mvsol {
            return RideDecision::Exit(RideExitReason::TrailingStop);
        }

        RideDecision::Hold
    }

    /// Flag creator sell for immediate exit on next tick.
    #[inline(always)]
    pub fn mark_creator_sell(&mut self) {
        self.flags |= ride_flags::CREATOR_SELL;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> super::super::config::RideConfig {
        super::super::config::RideConfig {
            min_confirming_buys: 2,
            min_confirming_lamports: 500_000_000,
            min_gain_vsol_fp: 10200,
            max_curve_pct_x100: 8000,
            early_trail_bp: 408,
            momentum_trail_bp: 305,
            tighten_trail_bp: 202,
            emergency_trail_bp: 101,
            early_to_momentum_ms: 15_000,
            momentum_to_tighten_ms: 60_000,
            max_hold_ms: 300_000,
            gain_momentum_vsol_fp: 10724,
            gain_tighten_vsol_fp: 12247,
            hard_floor_vsol_fp: 9800,
            whale_exit_lamports: 2_000_000_000,
            buy_gap_tighten_ms: 5_000,
            buy_gap_exit_ms: 10_000,
            sell_cascade_count: 3,
            sell_pressure_tighten_bp: 100,
            // Signal v2 fields
            signal_strong_threshold: 700,
            signal_sustained_threshold: 400,
            signal_weakening_threshold: 200,
            w_buy_rate_1s: 24,
            w_buy_rate_5s: 16,
            w_sell_rate_5s: -20,
            w_vol_accel_shift: 6,
            w_buy_gap_divisor: 150,
            w_sell_pressure_shift: 2,
            w_pnl_shift: 3,
            w_time_since_peak_divisor: 200,
            w_unique_wallets: 14,
            w_confirm_vol_shift: 8,
            kelly_baseline_f_permille: 671,
            kelly_min_trail_bp: 50,
            kelly_max_trail_bp: 800,
            lifecycle_accel_min_buys: 5,
            lifecycle_accel_min_sol_msol: 2000,
            lifecycle_momentum_min_buys: 15,
            lifecycle_momentum_min_sol_msol: 10000,
            trail_strong_pump_bp: 500,
            trail_sustained_bp: 350,
            trail_weakening_bp: 200,
        }
    }

    #[test]
    fn test_struct_size() {
        assert_eq!(core::mem::size_of::<RideState>(), 128);
    }

    #[test]
    fn test_new_state_is_strong_pump() {
        let cfg = test_config();
        let rs = RideState::new(30_000, 30_000, 1000, 0, &cfg);
        assert_eq!(rs.state, SignalState::StrongPump);
        assert_eq!(rs.entry_mvsol, 30_000);
        assert_eq!(rs.peak_mvsol, 30_000);
        assert_eq!(rs.buys_after_entry, 0);
        assert_eq!(rs.sells_after_entry, 0);
        assert_eq!(rs.composite_score, 500);
    }

    #[test]
    fn test_hard_floor_exit() {
        let cfg = test_config();
        let mut rs = RideState::new(30_000, 30_000, 1000, 0, &cfg);
        // Price drops below entry → hard floor exit
        let decision = rs.on_tick(29_999, 1100, &cfg);
        assert_eq!(decision, RideDecision::Exit(RideExitReason::HardFloor));
    }

    #[test]
    fn test_hold_on_price_above_entry() {
        let cfg = test_config();
        let mut rs = RideState::new(30_000, 30_000, 1000, 0, &cfg);
        // Feed a buy to update last_buy_ms
        rs.on_buy_event(500, 1050, 0x12345678);
        // Price above entry → hold
        let decision = rs.on_tick(31_000, 1100, &cfg);
        assert_eq!(decision, RideDecision::Hold);
    }

    #[test]
    fn test_buy_gap_timeout() {
        let cfg = test_config();
        let mut rs = RideState::new(30_000, 30_000, 1000, 0, &cfg);
        rs.on_buy_event(500, 1000, 0xAABB);
        // 11 seconds with no buy → gap timeout
        let decision = rs.on_tick(30_500, 12_000, &cfg);
        assert_eq!(decision, RideDecision::Exit(RideExitReason::BuyGapTimeout));
    }

    #[test]
    fn test_creator_sell_immediate_exit() {
        let cfg = test_config();
        let mut rs = RideState::new(30_000, 30_000, 1000, 0, &cfg);
        rs.on_buy_event(500, 1050, 0x1111);
        rs.mark_creator_sell();
        let decision = rs.on_tick(31_000, 1100, &cfg);
        assert_eq!(decision, RideDecision::Exit(RideExitReason::CreatorSell));
    }

    #[test]
    fn test_whale_exit() {
        let cfg = test_config();
        let mut rs = RideState::new(30_000, 30_000, 1000, 0, &cfg);
        // Sell > 2 SOL (2000 mvsol) → whale exit
        let result = rs.on_sell_event(2_500, 1100, &cfg);
        assert_eq!(result, Some(RideExitReason::WhaleExit));
    }

    #[test]
    fn test_bloom_unique_wallets() {
        let cfg = test_config();
        let mut rs = RideState::new(30_000, 30_000, 1000, 0, &cfg);
        rs.on_buy_event(100, 1010, 0xAAAA_0000_0000_0001);
        rs.on_buy_event(100, 1020, 0xBBBB_0000_0000_0002);
        rs.on_buy_event(100, 1030, 0xCCCC_0000_0000_0003);
        // Should detect approximately 3 unique wallets
        assert!(rs.unique_wallets >= 2);
    }

    #[test]
    fn test_trail_ratchets_up() {
        let cfg = test_config();
        let mut rs = RideState::new(30_000, 30_000, 1000, 0, &cfg);
        rs.on_buy_event(500, 1010, 0x1111);

        // Price goes up → trail stop ratchets up
        let _ = rs.on_tick(32_000, 1050, &cfg);
        let stop1 = rs.trail_stop_mvsol;

        rs.on_buy_event(500, 1060, 0x2222);
        let _ = rs.on_tick(33_000, 1070, &cfg);
        let stop2 = rs.trail_stop_mvsol;

        assert!(stop2 >= stop1, "Trail stop should only go up: {} >= {}", stop2, stop1);
    }

    #[test]
    fn test_compute_trail_stop_math() {
        // peak=30000, trail=500bp → drop = 30000*500/10000 = 1500
        let stop = compute_trail_stop(30_000, 500);
        assert_eq!(stop, 28_500);
    }

    #[test]
    fn test_lamports_mvsol_roundtrip() {
        let lamports: u64 = 5_000_000_000; // 5 SOL
        let mvsol = lamports_to_mvsol(lamports);
        assert_eq!(mvsol, 5_000);
        let back = mvsol_to_lamports(mvsol);
        assert_eq!(back, lamports);
    }
}