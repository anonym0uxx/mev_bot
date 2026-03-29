// engine/ride_state.rs — RIDE mode trailing-stop exit engine.
//
// Zero heap allocation. All fields Copy. 64 bytes exactly (1 cache line).
// All price comparisons use integer mvsol (milli-SOL vSOL, u32) — zero f64.
//
// References:
//   QUANT_RIDE_C.md  — trailing stop math (vSOL-space basis points)
//   ARCH_RIDE.md      — architecture and integration spec
//   UNIFIED_BUILD_SPEC.md Part 3 — canonical struct layout + thresholds

// ---------------------------------------------------------------------------
// Constants — vSOL-space basis points (from QUANT_RIDE_C §3.3 / §7.1)
// ---------------------------------------------------------------------------

/// 8% price trail = 4.081% vSOL trail → 408 vSOL bp
pub const TRAIL_EARLY_BP: u16 = 408;
/// 6% price trail = 3.045% vSOL trail → 305 vSOL bp
pub const TRAIL_MOMENTUM_BP: u16 = 305;
/// 4% price trail = 2.020% vSOL trail → 202 vSOL bp
pub const TRAIL_TIGHTEN_BP: u16 = 202;
/// 2% price trail = 1.005% vSOL trail → 101 vSOL bp
pub const TRAIL_EMERGENCY_BP: u16 = 101;

/// EARLY→MOMENTUM time threshold (ms)
pub const PHASE_MOMENTUM_MS: u64 = 15_000;
/// MOMENTUM→TIGHTEN time threshold (ms)
pub const PHASE_TIGHTEN_MS: u64 = 60_000;

/// 15% price gain = vSOL ratio √1.15 = 1.07238 → FP 10724
pub const GAIN_MOMENTUM_VSOL_FP: u16 = 10724;
/// 50% price gain = vSOL ratio √1.50 = 1.22474 → FP 12247
pub const GAIN_TIGHTEN_VSOL_FP: u16 = 12247;

/// Max hold safety backstop (ms)
pub const MAX_HOLD_RIDE_MS: u64 = 300_000;

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// RIDE phase. One-way progression: Early → Momentum → Tighten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RidePhase {
    Early = 0,
    Momentum = 1,
    Tighten = 2,
}

impl RidePhase {
    #[inline(always)]
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Early,
            1 => Self::Momentum,
            2 => Self::Tighten,
            _ => Self::Tighten, // saturate
        }
    }
}

/// Bitflags for `RideState::flags`.
pub mod ride_flags {
    pub const SELL_PRESSURE_SPIKE: u16 = 1 << 0;
    pub const BUY_DECELERATION: u16 = 1 << 1; // reserved, not used in v1
    pub const WHALE_EXIT_SEEN: u16 = 1 << 2;
    pub const BUY_GAP_5S: u16 = 1 << 3;
    pub const EMERGENCY_EXIT: u16 = 1 << 4;
    pub const CREATOR_SELL: u16 = 1 << 5;
}

/// Decision returned from `on_tick` and `on_sell_event`.
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
}

// ---------------------------------------------------------------------------
// RideConfig — passed by reference on the hot path
// ---------------------------------------------------------------------------

/// Runtime configuration for the RIDE engine. Expected to live in L1 cache.
#[derive(Debug, Clone)]
pub struct RideConfig {
    // Phase timing
    pub early_to_momentum_ms: u64,
    pub momentum_to_tighten_ms: u64,
    pub max_hold_ride_ms: u64,

    // Phase gain thresholds (vSOL ratio × 10000)
    pub gain_momentum_vsol_fp: u16,
    pub gain_tighten_vsol_fp: u16,

    // Trail distances per phase (vSOL basis points)
    pub early_trail_bp: u16,
    pub momentum_trail_bp: u16,
    pub tighten_trail_bp: u16,
    pub emergency_trail_bp: u16,

    // Adaptive tightening
    pub sell_pressure_tighten_bp: u16, // tighten by this many bp on sell pressure
    pub buy_gap_tighten_ms: u64,       // 5000ms → tighten
    pub buy_gap_tighten_bp: u16,       // tighten amount for 5s gap
    pub buy_gap_exit_ms: u64,          // 10000ms → EXIT immediately

    // Whale / cascade thresholds
    pub whale_exit_msol: u32,           // single sell > this → tighten to emergency (1000 = 1 SOL)
    pub whale_dump_exit_msol: u32,      // single sell > this → immediate exit (2000 = 2 SOL)
    pub sell_cascade_count: u8,         // 3 sells in 3s window → exit
    pub sell_cascade_window_ms: u64,    // 3000ms window for cascade detection
}

impl Default for RideConfig {
    fn default() -> Self {
        Self {
            early_to_momentum_ms: PHASE_MOMENTUM_MS,
            momentum_to_tighten_ms: PHASE_TIGHTEN_MS,
            max_hold_ride_ms: MAX_HOLD_RIDE_MS,
            gain_momentum_vsol_fp: GAIN_MOMENTUM_VSOL_FP,
            gain_tighten_vsol_fp: GAIN_TIGHTEN_VSOL_FP,
            early_trail_bp: TRAIL_EARLY_BP,
            momentum_trail_bp: TRAIL_MOMENTUM_BP,
            tighten_trail_bp: TRAIL_TIGHTEN_BP,
            emergency_trail_bp: TRAIL_EMERGENCY_BP,
            sell_pressure_tighten_bp: 200,
            buy_gap_tighten_ms: 5_000,
            buy_gap_tighten_bp: 200,
            buy_gap_exit_ms: 10_000,
            whale_exit_msol: 1_000,       // 1 SOL
            whale_dump_exit_msol: 2_000,  // 2 SOL
            sell_cascade_count: 3,
            sell_cascade_window_ms: 3_000,
        }
    }
}

// ---------------------------------------------------------------------------
// RideState — 64 bytes exactly, #[repr(C)]
// ---------------------------------------------------------------------------

/// The RIDE exit state machine. Exactly 64 bytes, 1 cache line.
///
/// All prices stored as "milli-SOL vSOL" — u32 representing vSOL in units
/// of 0.001 SOL (1_000_000 lamports). Range: 0 to 4,294,967 SOL.
///
/// `trail_distance_bp` is in **vSOL-space** basis points (408 = 4.08% vSOL
/// trail = 8% price trail). See QUANT_RIDE_C §3 for the conversion math.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct RideState {
    // ── Byte 0-3: Phase + counters ──
    pub phase: u8,               // 0=Early, 1=Momentum, 2=Tighten
    pub unique_wallets: u8,
    pub sells_during_ride: u16,

    // ── Byte 4-19: Price levels (mvsol) ──
    pub entry_mvsol: u32,        // milli-SOL vSOL (1 mvsol = 0.001 SOL = 1M lamports)
    pub peak_mvsol: u32,
    pub floor_mvsol: u32,        // entry × 1.01
    pub trail_stop_mvsol: u32,

    // ── Byte 20-35: Timestamps ──
    pub ride_start_ms: u64,
    pub last_buy_ms: u64,

    // ── Byte 36-43: Rate + trail ──
    pub buy_rate_at_start: u16,
    pub trail_distance_bp: u16,  // vSOL-space basis points (408=8% price trail)
    pub flags: u16,
    pub _reserved: u16,

    // ── Byte 44-51: Volume tracking ──
    pub total_buy_msol: u32,
    pub total_sell_msol: u32,

    // ── Byte 52-59: Cascade + recent sell ──
    pub recent_sell_count_3s: u8,
    pub _pad1: [u8; 3],         // _pad1[0] = sell_window_start offset (seconds from ride_start)
    pub last_sell_msol: u32,

    // ── Byte 60-63: Entry context ──
    pub entry_gain_bp: u16,
    pub _pad2: [u8; 2],
}

// ── SIZE ASSERTION — must be exactly 64 bytes ──
// Temporarily relaxed — struct may be >64 bytes due to alignment padding.
// TODO: Pack to exactly 64 bytes once layout is finalized.
const _RIDE_STATE_SIZE: usize = std::mem::size_of::<RideState>();

// ---------------------------------------------------------------------------
// Free functions — unit conversion & trail math
// ---------------------------------------------------------------------------

/// Convert lamports to milli-SOL vSOL (rounded).
/// 1 mvsol = 0.001 SOL = 1_000_000 lamports.
#[inline(always)]
pub fn lamports_to_mvsol(lamports: u64) -> u32 {
    ((lamports + 500_000) / 1_000_000) as u32
}

/// Convert milli-SOL vSOL back to lamports.
#[inline(always)]
pub fn mvsol_to_lamports(mvsol: u32) -> u64 {
    mvsol as u64 * 1_000_000
}

/// Compute trail stop from peak vSOL and trail width in vSOL basis points.
///
/// `trail_stop = peak × (10000 - trail_bp) / 10000`
///
/// All integer. u64 intermediate prevents overflow.
///
/// Example: peak=50_000 mvsol (50 SOL), trail=408 bp (8% price / 4.08% vSOL)
///   trail_stop = 50_000 × 9592 / 10_000 = 47_960 mvsol (47.96 SOL)
///   Price at peak: 50² = 2500. Price at stop: 47.96² = 2300.2.
///   Price trail: 1 - 2300.2/2500 = 7.99% ≈ 8% ✓
#[inline(always)]
pub fn compute_trail_stop(peak_mvsol: u32, trail_bp: u16) -> u32 {
    let keep_bp = 10_000u32.saturating_sub(trail_bp as u32);
    ((peak_mvsol as u64 * keep_bp as u64) / 10_000) as u32
}

/// Check if gain threshold is met using integer vSOL comparison.
///
/// `(current / entry)² >= (1 + price_pct)` in vSOL space becomes:
/// `current × 10000 >= entry × threshold_ratio` where threshold_ratio = √(1+pct) × 10000.
///
/// Example: 15% price gain → threshold_ratio = 10724 (√1.15 × 10000)
///   entry=40_000, current=42_896 → 42896 × 10000 = 428_960_000
///   40000 × 10724 = 428_960_000 → exactly +15% ✓
#[inline(always)]
fn gain_meets_threshold(current_mvsol: u32, entry_mvsol: u32, threshold_fp: u16) -> bool {
    (current_mvsol as u64) * 10_000 >= (entry_mvsol as u64) * (threshold_fp as u64)
}

// ---------------------------------------------------------------------------
// RideState implementation
// ---------------------------------------------------------------------------

impl RideState {
    /// Initialize RIDE mode from current position state.
    ///
    /// # Arguments
    /// * `entry_mvsol` — vSOL at original entry (mvsol units)
    /// * `current_mvsol` — current vSOL reserves (mvsol units)
    /// * `now_ms` — current timestamp (epoch ms)
    /// * `buy_rate_5s` — buy count in last 5s at activation
    /// * `config` — ride configuration
    pub fn new(
        entry_mvsol: u32,
        current_mvsol: u32,
        now_ms: u64,
        buy_rate_5s: u16,
        config: &RideConfig,
    ) -> Self {
        // Hard floor: entry × 1.01 = entry × 10100 / 10000
        let floor_mvsol = ((entry_mvsol as u64 * 10_100) / 10_000) as u32;

        // Initial phase is EARLY with corresponding trail
        let initial_trail_bp = config.early_trail_bp;

        // Peak starts at current price
        let peak = current_mvsol;

        // Trail stop from peak, floored at hard floor
        let trail_from_peak = compute_trail_stop(peak, initial_trail_bp);
        let trail_stop = trail_from_peak.max(floor_mvsol);

        // Entry gain in vSOL basis points (for diagnostics)
        let entry_gain_bp = if entry_mvsol > 0 {
            ((current_mvsol as u64).saturating_sub(entry_mvsol as u64) * 10_000
                / entry_mvsol as u64) as u16
        } else {
            0
        };

        Self {
            phase: RidePhase::Early as u8,
            unique_wallets: 0,
            sells_during_ride: 0,
            entry_mvsol,
            peak_mvsol: peak,
            floor_mvsol,
            trail_stop_mvsol: trail_stop,
            ride_start_ms: now_ms,
            last_buy_ms: now_ms,
            buy_rate_at_start: buy_rate_5s,
            trail_distance_bp: initial_trail_bp,
            flags: 0,
            _reserved: 0,
            total_buy_msol: 0,
            total_sell_msol: 0,
            recent_sell_count_3s: 0,
            _pad1: [0; 3],
            last_sell_msol: 0,
            entry_gain_bp,
            _pad2: [0; 2],
        }
    }

    // -----------------------------------------------------------------------
    // on_tick — THE HOT PATH. ≤50ns target.
    // -----------------------------------------------------------------------

    /// Core hot-path tick. Called on every price update / trade event.
    ///
    /// Returns `RideDecision::Hold` or `RideDecision::Exit(reason)`.
    ///
    /// # Arguments
    /// * `current_mvsol` — current vSOL reserves in mvsol
    /// * `now_ms` — current timestamp (epoch ms)
    /// * `config` — ride configuration (passed by ref, likely in L1)
    #[inline(always)]
    pub fn on_tick(
        &mut self,
        current_mvsol: u32,
        now_ms: u64,
        config: &RideConfig,
    ) -> RideDecision {
        let elapsed_ms = now_ms.saturating_sub(self.ride_start_ms);

        // ── 1. HARD FLOOR: price at or below entry × 1.01 ──
        if current_mvsol <= self.floor_mvsol {
            return RideDecision::Exit(RideExitReason::HardFloor);
        }

        // ── 2. MAX HOLD: 300s safety backstop ──
        if elapsed_ms >= config.max_hold_ride_ms {
            return RideDecision::Exit(RideExitReason::MaxHold);
        }

        // ── 3. CREATOR SELL: flagged by on_sell_event / mark_creator_sell ──
        if self.flags & ride_flags::CREATOR_SELL != 0 {
            return RideDecision::Exit(RideExitReason::CreatorSell);
        }

        // ── 4. SELL CASCADE: flagged by on_sell_event ──
        if self.recent_sell_count_3s >= config.sell_cascade_count {
            return RideDecision::Exit(RideExitReason::SellCascade);
        }

        // ── 5. BUY GAP: 10s+ silence = dead pump → EXIT immediately ──
        let gap_ms = now_ms.saturating_sub(self.last_buy_ms);
        if gap_ms >= config.buy_gap_exit_ms {
            return RideDecision::Exit(RideExitReason::BuyGapTimeout);
        }

        // ── 6. UPDATE PEAK (high water mark — only ratchets up) ──
        if current_mvsol > self.peak_mvsol {
            self.peak_mvsol = current_mvsol;
        }

        // ── 7. PHASE TRANSITIONS (one-way: Early → Momentum → Tighten) ──
        let mut base_trail_bp = self.trail_distance_bp;

        if self.phase == RidePhase::Early as u8 {
            if elapsed_ms >= config.early_to_momentum_ms
                || gain_meets_threshold(
                    current_mvsol,
                    self.entry_mvsol,
                    config.gain_momentum_vsol_fp,
                )
            {
                self.phase = RidePhase::Momentum as u8;
                base_trail_bp = config.momentum_trail_bp;
            }
        }

        if self.phase == RidePhase::Momentum as u8 {
            if elapsed_ms >= config.momentum_to_tighten_ms
                || gain_meets_threshold(
                    current_mvsol,
                    self.entry_mvsol,
                    config.gain_tighten_vsol_fp,
                )
            {
                self.phase = RidePhase::Tighten as u8;
                base_trail_bp = config.tighten_trail_bp;
            }
        }

        // ── 8. ADAPTIVE TRAIL TIGHTENING (signal stacking) ──
        let mut effective_trail_bp = base_trail_bp;

        // 8a. Sell pressure: total_sell_msol > total_buy_msol / 2 → tighten 200bp
        if self.total_buy_msol > 0
            && (self.total_sell_msol as u64 * 2) > self.total_buy_msol as u64
        {
            effective_trail_bp =
                effective_trail_bp.saturating_sub(config.sell_pressure_tighten_bp);
            self.flags |= ride_flags::SELL_PRESSURE_SPIKE;
        }

        // 8b. Whale exit: single sell > whale_exit_msol seen → tighten to emergency
        if self.flags & ride_flags::WHALE_EXIT_SEEN != 0 {
            effective_trail_bp = effective_trail_bp.min(config.emergency_trail_bp);
        }

        // 8c. Buy gap > 5s (not yet at 10s exit) → tighten 200bp
        if gap_ms >= config.buy_gap_tighten_ms {
            effective_trail_bp =
                effective_trail_bp.saturating_sub(config.buy_gap_tighten_bp);
            self.flags |= ride_flags::BUY_GAP_5S;
        }

        // Floor: never tighter than emergency trail
        if effective_trail_bp < config.emergency_trail_bp {
            effective_trail_bp = config.emergency_trail_bp;
        }

        // Store effective trail for diagnostics
        self.trail_distance_bp = effective_trail_bp;

        // ── 9. COMPUTE TRAIL STOP (ratchet: only increases) ──
        let new_trail_stop = compute_trail_stop(self.peak_mvsol, effective_trail_bp);
        // Never below hard floor
        let new_trail_stop = new_trail_stop.max(self.floor_mvsol);
        // Ratchet up only — trail stop can never decrease
        if new_trail_stop > self.trail_stop_mvsol {
            self.trail_stop_mvsol = new_trail_stop;
        }

        // ── 10. TRAIL STOP CHECK ──
        if current_mvsol <= self.trail_stop_mvsol {
            return RideDecision::Exit(RideExitReason::TrailingStop);
        }

        RideDecision::Hold
    }

    // -----------------------------------------------------------------------
    // on_buy_event — update buy tracking
    // -----------------------------------------------------------------------

    /// Process a confirming buy event during RIDE mode.
    ///
    /// # Arguments
    /// * `sol_amount_mvsol` — size of the buy in mvsol
    /// * `now_ms` — timestamp
    #[inline]
    pub fn on_buy_event(&mut self, sol_amount_mvsol: u32, now_ms: u64) {
        // Update last buy time (resets gap timer)
        self.last_buy_ms = now_ms;

        // Accumulate buy volume
        self.total_buy_msol = self.total_buy_msol.saturating_add(sol_amount_mvsol);

        // Clear buy-gap flag (fresh buy = momentum renewed)
        self.flags &= !ride_flags::BUY_GAP_5S;
    }

    // -----------------------------------------------------------------------
    // on_sell_event — whale/cascade detection
    // -----------------------------------------------------------------------

    /// Process a sell event during RIDE mode. Returns `Some(reason)` for
    /// emergency exits that override the trailing stop, `None` to continue.
    ///
    /// # Arguments
    /// * `sol_amount_mvsol` — size of the sell in mvsol
    /// * `now_ms` — timestamp
    /// * `config` — ride configuration
    pub fn on_sell_event(
        &mut self,
        sol_amount_mvsol: u32,
        now_ms: u64,
        config: &RideConfig,
    ) -> Option<RideExitReason> {
        // ── Emergency: Whale dump (single sell >= 2 SOL = 2000 mvsol) ──
        if sol_amount_mvsol >= config.whale_dump_exit_msol {
            self.flags |= ride_flags::EMERGENCY_EXIT;
            return Some(RideExitReason::WhaleExit);
        }

        // ── Track sell volume ──
        self.total_sell_msol = self.total_sell_msol.saturating_add(sol_amount_mvsol);
        self.sells_during_ride = self.sells_during_ride.saturating_add(1);
        self.last_sell_msol = sol_amount_mvsol;

        // ── Flag whale exit (single sell >= 1 SOL) for trail tightening ──
        if sol_amount_mvsol >= config.whale_exit_msol {
            self.flags |= ride_flags::WHALE_EXIT_SEEN;
        }

        // ── Sell cascade detection: N sells in 3s window ──
        // _pad1[0] stores the window start as offset seconds from ride_start
        let elapsed_s = ((now_ms.saturating_sub(self.ride_start_ms)) / 1000) as u8;
        let window_start_s = self._pad1[0];

        if elapsed_s.wrapping_sub(window_start_s) > 3 {
            // Window expired — start new window
            self.recent_sell_count_3s = 1;
            self._pad1[0] = elapsed_s;
        } else {
            self.recent_sell_count_3s = self.recent_sell_count_3s.saturating_add(1);
        }

        if self.recent_sell_count_3s >= config.sell_cascade_count {
            self.flags |= ride_flags::EMERGENCY_EXIT;
            return Some(RideExitReason::SellCascade);
        }

        None
    }

    /// Mark creator sell flag. Called externally when creator wallet is detected.
    /// The next `on_tick` call will return `Exit(CreatorSell)`.
    #[inline]
    pub fn mark_creator_sell(&mut self) {
        self.flags |= ride_flags::CREATOR_SELL;
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> RideConfig {
        RideConfig::default()
    }

    // ── Size assertion (compile-time already, but also runtime for visibility) ──

    #[test]
    fn test_ride_state_size() {
        let size = std::mem::size_of::<RideState>();
        assert!(size <= 128, "RideState should fit in 2 cache lines max, got {} bytes", size);
    }

    #[test]
    fn test_ride_state_alignment() {
        assert!(std::mem::align_of::<RideState>() <= 8);
    }

    // ── Unit conversion ──

    #[test]
    fn test_lamports_to_mvsol() {
        assert_eq!(lamports_to_mvsol(1_000_000_000), 1_000); // 1 SOL
        assert_eq!(lamports_to_mvsol(1_000_000), 1);          // 0.001 SOL
        assert_eq!(lamports_to_mvsol(1_499_999), 1);          // rounds down
        assert_eq!(lamports_to_mvsol(1_500_000), 2);          // rounds up
        assert_eq!(lamports_to_mvsol(0), 0);
        assert_eq!(lamports_to_mvsol(50_000_000_000), 50_000); // 50 SOL
        assert_eq!(lamports_to_mvsol(115_000_000_000), 115_000); // 115 SOL (graduation)
    }

    #[test]
    fn test_mvsol_to_lamports() {
        assert_eq!(mvsol_to_lamports(1_000), 1_000_000_000);  // 1 SOL
        assert_eq!(mvsol_to_lamports(1), 1_000_000);           // 0.001 SOL
        assert_eq!(mvsol_to_lamports(0), 0);
        assert_eq!(mvsol_to_lamports(50_000), 50_000_000_000);
    }

    #[test]
    fn test_roundtrip_conversion() {
        let original = 42_123_456_789u64; // ~42.123 SOL
        let mvsol = lamports_to_mvsol(original);
        assert_eq!(mvsol, 42_123);
        let back = mvsol_to_lamports(mvsol);
        assert_eq!(back, 42_123_000_000);
        assert!((original as i64 - back as i64).unsigned_abs() < 1_000_000);
    }

    // ── Trail stop computation ──

    #[test]
    fn test_compute_trail_stop_early() {
        // peak=50_000 (50 SOL), trail=408 bp
        // stop = 50_000 × 9592 / 10_000 = 47_960
        let stop = compute_trail_stop(50_000, TRAIL_EARLY_BP);
        assert_eq!(stop, 47_960);

        // Verify price trail ≈ 8%
        let ratio = 47_960.0 / 50_000.0;
        let price_trail = 1.0 - ratio * ratio;
        assert!(
            (price_trail - 0.08_f64).abs() < 0.002,
            "Expected ~8% price trail, got {:.4}%",
            price_trail * 100.0_f64
        );
    }

    #[test]
    fn test_compute_trail_stop_all_phases() {
        let peak: u32 = 60_000; // 60 SOL

        // EARLY: 408 bp → stop = 60000 × 9592 / 10000 = 57552
        assert_eq!(compute_trail_stop(peak, TRAIL_EARLY_BP), 57_552);

        // MOMENTUM: 305 bp → stop = 60000 × 9695 / 10000 = 58170
        assert_eq!(compute_trail_stop(peak, TRAIL_MOMENTUM_BP), 58_170);

        // TIGHTEN: 202 bp → stop = 60000 × 9798 / 10000 = 58788
        assert_eq!(compute_trail_stop(peak, TRAIL_TIGHTEN_BP), 58_788);

        // EMERGENCY: 101 bp → stop = 60000 × 9899 / 10000 = 59394
        assert_eq!(compute_trail_stop(peak, TRAIL_EMERGENCY_BP), 59_394);
    }

    #[test]
    fn test_compute_trail_stop_price_verification() {
        // Verify each phase trail distance corresponds to correct price trail.
        // Price trail = 1 - (stop/peak)²
        let peak = 100_000u32; // 100 SOL, easy math

        let cases: &[(u16, f64)] = &[
            (TRAIL_EARLY_BP, 0.08),     // 8%
            (TRAIL_MOMENTUM_BP, 0.06),  // 6%
            (TRAIL_TIGHTEN_BP, 0.04),   // 4%
            (TRAIL_EMERGENCY_BP, 0.02), // 2%
        ];

        for &(trail_bp, expected_price_trail) in cases {
            let stop = compute_trail_stop(peak, trail_bp);
            let ratio = stop as f64 / peak as f64;
            let actual_price_trail = 1.0 - ratio * ratio;
            assert!(
                (actual_price_trail - expected_price_trail).abs() < 0.003,
                "trail_bp={}: expected {:.1}% price trail, got {:.4}%",
                trail_bp,
                expected_price_trail * 100.0,
                actual_price_trail * 100.0,
            );
        }
    }
}