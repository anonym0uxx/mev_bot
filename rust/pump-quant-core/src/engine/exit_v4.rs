//! Dynamic Exit Framework v4 — Integer-only urgency-based exit engine.
//!
//! Computes a composite urgency score U (0–10000) from four signal components:
//!   1. Bayesian Kelly edge decay (u_kelly)
//!   2. Momentum divergence (u_momentum)
//!   3. Volatility-adaptive trail (u_vol_trail)
//!   4. Liquidity slippage (u_liquidity)
//!
//! When U crosses thresholds, exit decisions fire (partial, majority, full).
//! All arithmetic is integer-only (u16/u32/u64). Zero f32/f64. Zero heap.
//!
//! Structs: MomentumDivergence (16B), VolatilityEstimator (16B), UrgencyState (8B).
//! Total new state: 40 bytes in cache line 2 of RideState v4.

use super::kelly_sizing::DEFAULT_ROUND_TRIP_FEE_BP;

// ---------------------------------------------------------------------------
// MomentumDivergence — 16 bytes
// ---------------------------------------------------------------------------

/// Momentum divergence state. 16 bytes.
///
/// Tracks buy/sell ratio in two event-count windows using a packed
/// 20-bit ring buffer. Detects when short-term buying fades while
/// medium-term momentum still looks strong (classic divergence).
///
/// Window sizes:
///   - Short: last 5 events (~0.5–2s during active pump)
///   - Medium: last 20 events (~2–10s)
#[derive(Clone, Copy)]
#[repr(C)]
pub struct MomentumDivergence {
    /// Ring buffer: last 20 events. Bit set = buy, clear = sell.
    /// Packed into 20 bits of a u32 (12 bits spare).
    event_ring: u32,       // 4 bytes

    /// Ring write index (0–19). Wraps modulo 20.
    ring_idx: u8,          // 1 byte

    /// Count of events written (saturates at 20).
    ring_count: u8,        // 1 byte

    /// Last computed short-window buy count (0–5). Cached for combiner.
    short_buys: u8,        // 1 byte

    /// Last computed medium-window buy count (0–20). Cached for combiner.
    medium_buys: u8,       // 1 byte

    /// Volume-weighted buy pressure: sum of last 8 buy sizes in mSOL.
    /// Decayed by shifting to prior every 8 events.
    buy_vol_recent: u16,   // 2 bytes

    /// Volume-weighted buy pressure: prior 8-event window.
    buy_vol_prior: u16,    // 2 bytes

    /// Sell volume in last 8 events (mSOL). For sell acceleration detection.
    sell_vol_recent: u16,  // 2 bytes

    _pad: [u8; 2],        // 2 bytes → total 16 bytes
}

const _: () = assert!(core::mem::size_of::<MomentumDivergence>() == 16);

impl MomentumDivergence {
    #[inline(always)]
    pub fn new() -> Self {
        Self {
            event_ring: 0,
            ring_idx: 0,
            ring_count: 0,
            short_buys: 0,
            medium_buys: 0,
            buy_vol_recent: 0,
            buy_vol_prior: 0,
            sell_vol_recent: 0,
            _pad: [0; 2],
        }
    }

    /// Record a trade event (buy or sell).
    /// Called from on_buy_event / on_sell_event in RideState.
    /// `sol_msol`: trade size in milli-SOL (1 SOL = 1000 mSOL).
    #[inline(always)]
    pub fn record_event(&mut self, is_buy: bool, sol_msol: u16) {
        let idx = self.ring_idx as u32;

        // Write to ring: set or clear bit at position idx
        if is_buy {
            self.event_ring |= 1 << idx;
        } else {
            self.event_ring &= !(1 << idx);
        }

        // Advance ring
        self.ring_idx = if self.ring_idx >= 19 { 0 } else { self.ring_idx + 1 };
        self.ring_count = self.ring_count.saturating_add(1).min(20);

        // Volume tracking (8-event decay cycle)
        if is_buy {
            self.buy_vol_recent = self.buy_vol_recent.saturating_add(sol_msol);
        } else {
            self.sell_vol_recent = self.sell_vol_recent.saturating_add(sol_msol);
        }

        // Every 8 events: shift recent → prior, reset recent
        if self.ring_count % 8 == 0 && self.ring_count > 0 {
            self.buy_vol_prior = self.buy_vol_recent;
            self.buy_vol_recent = 0;
            self.sell_vol_recent = 0;
        }

        // Recompute cached buy counts
        self.recompute_counts();
    }

    /// Recompute short (5) and medium (20) window buy counts from ring.
    /// Uses popcount on masked portions of the packed ring. ~3ns.
    #[inline(always)]
    fn recompute_counts(&mut self) {
        let n = self.ring_count.min(20) as u32;

        // Medium window: all events in ring (up to 20 bits)
        let medium_mask = if n >= 20 { 0x000F_FFFF } else { (1u32 << n) - 1 };
        // When ring wraps, all 20 bits are valid. We need to mask correctly.
        // The ring stores bits 0..19; the ACTIVE bits depend on ring_count.
        // For simplicity (and since count wraps ring), we use all populated bits.
        self.medium_buys = (self.event_ring & medium_mask).count_ones() as u8;

        // Short window: last 5 events
        let short_n = n.min(5);
        let mut short_mask = 0u32;
        for i in 0..short_n {
            let bit_pos = (self.ring_idx as u32 + 20 - 1 - i) % 20;
            short_mask |= 1 << bit_pos;
        }
        self.short_buys = (self.event_ring & short_mask).count_ones() as u8;
    }

    /// Compute momentum divergence urgency (u16, 0–10000).
    ///
    /// Divergence = short-window buy rate is LOWER than medium-window buy rate.
    /// Also detects sell acceleration and volume decline.
    #[inline(always)]
    pub fn urgency(&self) -> u16 {
        let n = self.ring_count;
        if n < 5 { return 0; } // insufficient data

        // Short-window buy rate × 1000 (0–1000)
        let short_rate = self.short_buys as u32 * 1000 / 5;

        // Medium-window buy rate × 1000 (0–1000)
        let med_count = n.min(20) as u32;
        let med_rate = self.medium_buys as u32 * 1000 / med_count;

        // Divergence: how much worse is short vs medium?
        let divergence_urgency = if short_rate >= med_rate {
            0u32
        } else {
            let gap = med_rate - short_rate;
            // Scale: gap of 500 → 5000 urgency. Linear, capped at 8000.
            (gap * 10).min(8000)
        };

        // Volume divergence: buy volume declining.
        // Only compare when we've had at least 16 events total (both windows populated).
        let vol_urgency = if n >= 16 && self.buy_vol_prior > 0 && self.buy_vol_recent < self.buy_vol_prior {
            let decline = self.buy_vol_prior - self.buy_vol_recent;
            let pct = decline as u32 * 100 / self.buy_vol_prior as u32;
            // 50% decline → 2000 urgency, 100% decline → 4000
            (pct * 40).min(4000)
        } else {
            0u32
        };

        // Sell acceleration: sell volume exceeding buy volume
        let sell_urgency = if self.sell_vol_recent > self.buy_vol_recent.saturating_add(500) {
            let excess = (self.sell_vol_recent - self.buy_vol_recent) as u32;
            // 1 SOL excess → 2000, 3 SOL excess → 6000
            (excess * 2).min(6000)
        } else {
            0u32
        };

        // Composite: max of the three signals (not sum — avoid double-count)
        divergence_urgency.max(vol_urgency).max(sell_urgency).min(10000) as u16
    }
}

// ---------------------------------------------------------------------------
// VolatilityEstimator — 16 bytes
// ---------------------------------------------------------------------------

/// Volatility estimator for adaptive trail width. 16 bytes.
///
/// Tracks variance of vSOL deltas across recent trade events using
/// an exponential moving average of absolute deltas.
/// Output: vol_bp_x100 = estimated volatility in basis points × 100.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct VolatilityEstimator {
    /// Sum of absolute vSOL deltas (mvsol units), EMA-8 decayed.
    abs_delta_sum: u32,     // 0-3

    /// Sum of squared deltas / 256 (scaled to prevent overflow).
    sq_delta_sum_shr8: u32, // 4-7

    /// Previous vSOL reading (mvsol). For computing deltas.
    prev_mvsol: u32,        // 8-11

    /// Cached volatility output × 100 (basis points × 100).
    pub vol_bp_x100: u16,   // 12-13

    /// Number of deltas recorded (saturates at 8).
    count: u8,              // 14

    _pad: u8,               // 15 → total 16 bytes
}

const _: () = assert!(core::mem::size_of::<VolatilityEstimator>() == 16);

impl VolatilityEstimator {
    #[inline(always)]
    pub fn new(entry_mvsol: u32) -> Self {
        Self {
            abs_delta_sum: 0,
            sq_delta_sum_shr8: 0,
            prev_mvsol: entry_mvsol,
            count: 0,
            vol_bp_x100: 0,
            _pad: 0,
        }
    }

    /// Record a new vSOL observation. Computes delta from previous.
    /// Called on every trade event from on_tick or event handlers.
    #[inline(always)]
    pub fn record(&mut self, current_mvsol: u32) {
        if self.prev_mvsol == 0 {
            self.prev_mvsol = current_mvsol;
            return;
        }

        // Absolute delta
        let delta = if current_mvsol >= self.prev_mvsol {
            current_mvsol - self.prev_mvsol
        } else {
            self.prev_mvsol - current_mvsol
        };

        self.prev_mvsol = current_mvsol;

        // Exponential decay of accumulators: multiply by 7/8 then add new sample
        // Half-life ≈ 5.2 events
        if self.count >= 8 {
            self.abs_delta_sum = (self.abs_delta_sum * 7 + 4) >> 3;
            self.sq_delta_sum_shr8 = (self.sq_delta_sum_shr8 * 7 + 4) >> 3;
        }

        self.abs_delta_sum = self.abs_delta_sum.saturating_add(delta);
        self.sq_delta_sum_shr8 = self.sq_delta_sum_shr8
            .saturating_add((delta as u64 * delta as u64 >> 8) as u32);
        self.count = self.count.saturating_add(1);

        self.recompute_vol();
    }

    /// Recompute cached volatility in bp × 100.
    /// Uses mean absolute delta as a stddev proxy.
    #[inline(always)]
    fn recompute_vol(&mut self) {
        let n = self.count.min(8) as u32;
        if n == 0 || self.prev_mvsol == 0 { return; }

        let mean_abs = self.abs_delta_sum / n;

        // vol_bp_x100 = mean_abs × 1_000_000 / prev_mvsol
        let vol = if self.prev_mvsol > 0 {
            (mean_abs as u64 * 1_000_000 / self.prev_mvsol as u64) as u32
        } else {
            0
        };

        self.vol_bp_x100 = vol.min(u16::MAX as u32) as u16;
    }

    /// Compute trail width multiplier from volatility.
    /// Returns a multiplier × 256 (fixed-point).
    /// 256 = 1.0× (no adjustment). Range: [192, 640] = [0.75×, 2.5×].
    #[inline(always)]
    pub fn trail_multiplier_x256(&self) -> u16 {
        let vol = self.vol_bp_x100 as u32;
        if vol == 0 { return 256; }

        // Linear: multiplier = 256 × vol / baseline
        const BASELINE: u32 = 300;
        let mult = (256 * vol + BASELINE / 2) / BASELINE;

        mult.clamp(192, 640) as u16
    }
}

// ---------------------------------------------------------------------------
// UrgencyState — 8 bytes
// ---------------------------------------------------------------------------

/// Exit urgency state. 8 bytes. Stored in RideState extension.
///
/// Tracks the monotonic urgency floor and partial exit history.
/// Once urgency crosses a threshold and a partial exit fires,
/// the floor ratchets up — the position can only exit MORE, never
/// re-accumulate.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct UrgencyState {
    /// Monotonic urgency floor (ratchets up, never down).
    pub urgency_floor: u16,       // 0-1

    /// Remaining position in permille (1000 = 100%).
    pub remaining_permille: u16,  // 2-3

    /// Last computed composite urgency (for logging/API).
    pub last_urgency: u16,        // 4-5

    /// Number of partial exits executed (0–3).
    pub partial_count: u8,        // 6

    _pad: u8,                     // 7 → total 8 bytes
}

const _: () = assert!(core::mem::size_of::<UrgencyState>() == 8);

impl UrgencyState {
    #[inline(always)]
    pub fn new() -> Self {
        Self {
            urgency_floor: 0,
            remaining_permille: 1000,
            partial_count: 0,
            last_urgency: 0,
            _pad: 0,
        }
    }

    /// Apply urgency floor and return the effective urgency.
    #[inline(always)]
    pub fn effective_urgency(&self, computed: u16) -> u16 {
        computed.max(self.urgency_floor)
    }

    /// Execute exit decision based on effective urgency.
    /// Returns the fraction to exit.
    ///
    /// Thresholds:
    ///   U < 3000          → HOLD
    ///   3000 ≤ U < 5000   → TIGHTEN (no exit, tighten trail)
    ///   5000 ≤ U < 7000   → PARTIAL EXIT: sell 350‰ of REMAINING
    ///   7000 ≤ U < 9000   → MAJORITY EXIT: sell 600‰ of remaining
    ///   U ≥ 9000          → FULL EXIT: sell everything
    #[inline(always)]
    pub fn decide(&mut self, effective_u: u16) -> ExitFraction {
        self.last_urgency = effective_u;

        if effective_u >= 9000 {
            // Full exit
            let frac = self.remaining_permille;
            self.remaining_permille = 0;
            self.urgency_floor = 10000;
            return ExitFraction::Exit(frac);
        }

        if effective_u >= 7000 && self.remaining_permille > 0 {
            // Majority exit: 60% of remaining
            let sell = (self.remaining_permille as u32 * 600 / 1000) as u16;
            let sell = sell.max(1).min(self.remaining_permille);
            self.remaining_permille -= sell;
            self.partial_count = self.partial_count.saturating_add(1);
            self.urgency_floor = (effective_u as u32 * 7 / 8) as u16;
            return ExitFraction::Partial(sell);
        }

        if effective_u >= 5000 && self.remaining_permille > 0 && self.partial_count < 2 {
            // Partial exit: 35% of remaining (limited to 2 partials before majority)
            let sell = (self.remaining_permille as u32 * 350 / 1000) as u16;
            let sell = sell.max(1).min(self.remaining_permille);
            self.remaining_permille -= sell;
            self.partial_count = self.partial_count.saturating_add(1);
            self.urgency_floor = (effective_u as u32 * 7 / 8) as u16;
            return ExitFraction::Partial(sell);
        }

        if effective_u >= 3000 {
            return ExitFraction::Tighten;
        }

        ExitFraction::Hold
    }
}

/// Exit fraction decision returned by UrgencyState::decide().
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitFraction {
    /// Do nothing. Edge is intact.
    Hold,
    /// Tighten trail stop (reduce width). No position change.
    Tighten,
    /// Sell this many permille of the ORIGINAL position.
    Partial(u16),
    /// Sell this many permille (should equal remaining — full exit).
    Exit(u16),
}

// ---------------------------------------------------------------------------
// Urgency component functions — all #[inline(always)]
// ---------------------------------------------------------------------------

/// Map f̂* to urgency component u_kelly (u16, 0–10000).
///
/// f̂* > 0.70 × f_entry → 0 (strong edge, no urgency)
/// f̂* = 0.35 × f_entry → 3000 (weakening)
/// f̂* = 0               → 7000 (edge gone)
/// f̂* < 0               → 10000 (negative EV — GET OUT)
#[inline(always)]
pub fn u_kelly(f_hat: i16, f_entry: u16) -> u16 {
    if f_entry == 0 { return 10000; }
    let fe = f_entry as i32;

    // Thresholds (same as SignalState boundaries)
    let strong = (fe * 179) >> 8;   // ~0.70 × f_entry
    let sustain = (fe * 90) >> 8;   // ~0.35 × f_entry
    let f = f_hat as i32;

    if f >= strong {
        0
    } else if f >= sustain {
        // Linear: strong→0, sustain→3000
        let range = strong - sustain;
        if range == 0 { return 1500; }
        ((strong - f) as u32 * 3000 / range as u32) as u16
    } else if f > 0 {
        // Linear: sustain→3000, zero→7000
        if sustain == 0 { return 5000; }
        (3000 + (sustain - f) as u32 * 4000 / sustain as u32) as u16
    } else {
        // f̂ ≤ 0: linear 7000→10000 as f goes from 0 to -f_entry
        let neg_depth = (-f).min(fe) as u32;
        (7000 + neg_depth * 3000 / fe as u32).min(10000) as u16
    }
}

/// Compute volatility-trail urgency based on price proximity to adaptive trail stop.
///
/// Returns u16 urgency (0–10000).
/// Price > stop × 1.05 (500bp margin) → 0
/// Price = stop × 1.02 (200bp margin) → 3000
/// Price = stop                        → 7000
/// Price < stop                        → 10000
#[inline(always)]
pub fn u_vol_trail(current_mvsol: u32, adaptive_trail_stop: u32) -> u16 {
    if adaptive_trail_stop == 0 { return 0; }

    if current_mvsol <= adaptive_trail_stop {
        return 10000; // Below stop → full exit
    }

    // Distance above stop in basis points
    let margin_bp = ((current_mvsol - adaptive_trail_stop) as u64 * 10000
        / adaptive_trail_stop as u64) as u32;

    if margin_bp >= 500 {
        0
    } else if margin_bp >= 200 {
        // 500→0, 200→3000
        ((500 - margin_bp) * 3000 / 300) as u16
    } else {
        // 200→3000, 0→7000
        (3000 + (200 - margin_bp) * 4000 / 200) as u16
    }
}

/// Compute slippage urgency from position size vs available liquidity.
///
/// Returns u16 urgency (0–10000).
/// Slippage > 8% → max urgency.
#[inline(always)]
pub fn u_liquidity(position_size_lamports: u64, liquidity_lamports: u64) -> u16 {
    if liquidity_lamports == 0 { return 10000; }

    // Slippage estimate (basis points) = position_size × 10000 / liquidity
    let slippage_bp = (position_size_lamports as u128 * 10000 / liquidity_lamports as u128) as u32;

    if slippage_bp <= 100 {
        0
    } else if slippage_bp <= 300 {
        ((slippage_bp - 100) * 3000 / 200) as u16
    } else if slippage_bp <= 800 {
        (3000 + (slippage_bp - 300) * 5000 / 500) as u16
    } else {
        10000
    }
}

/// Compute composite exit urgency from four component signals.
///
/// All inputs are u16 in range [0, 10000].
/// Weights sum to 256 (fixed-point × 256 for integer multiply).
/// Output: u16 in [0, 10000].
///
/// Weight allocation:
///   Kelly/Bayesian: 115/256 ≈ 45%
///   Momentum:        77/256 ≈ 30%
///   Vol trail:        38/256 ≈ 15%
///   Liquidity:        26/256 ≈ 10%
#[inline(always)]
pub fn composite_urgency(
    uk: u16,
    um: u16,
    uv: u16,
    ul: u16,
) -> u16 {
    // Override conditions (any one → max urgency)
    if uk >= 9000 || ul >= 9000 {
        return 10000;
    }
    // Universal agreement: all signals alarmed
    if uk >= 5000 && um >= 5000 && uv >= 5000 && ul >= 5000 {
        return 10000;
    }

    // Weighted sum: weights sum to 256
    const W_KELLY: u32 = 115;     // 45%
    const W_MOMENTUM: u32 = 77;   // 30%
    const W_VOL: u32 = 38;        // 15%
    const W_LIQ: u32 = 26;        // 10%
    // 115 + 77 + 38 + 26 = 256 ✓

    let weighted = W_KELLY * uk as u32
        + W_MOMENTUM * um as u32
        + W_VOL * uv as u32
        + W_LIQ * ul as u32;

    // Divide by 256 (right shift)
    let composite = weighted >> 8;

    composite.min(10000) as u16
}

/// Compute adaptive trail stop from peak and vol-adjusted trail width.
/// `peak_mvsol`: highest price seen
/// `base_trail_bp`: base trail width from SignalState
/// `vol_mult_x256`: volatility multiplier × 256 from VolatilityEstimator
#[inline(always)]
pub fn compute_adaptive_trail_stop(peak_mvsol: u32, base_trail_bp: u16, vol_mult_x256: u16) -> u32 {
    // Adjusted trail = base_trail_bp × vol_mult / 256
    let adaptive_trail_bp = (base_trail_bp as u32 * vol_mult_x256 as u32) >> 8;
    // Clamp to sane range (10bp - 2000bp)
    let trail_bp = adaptive_trail_bp.clamp(10, 2000);
    // trail_stop = peak × (10000 - trail_bp) / 10000
    let stop = peak_mvsol as u64 * (10_000u64 - trail_bp as u64) / 10_000u64;
    stop as u32
}

// ---------------------------------------------------------------------------
// ExitV4Config
// ---------------------------------------------------------------------------

/// Configuration for the V4 exit engine. All tunable parameters.
/// Stored in RideConfig extension; defaults provide shadow mode.
#[derive(Debug, Clone, Copy)]
pub struct ExitV4Config {
    /// Master toggle. false = shadow mode (compute+log, don't act).
    pub enabled: bool,

    // Urgency weights (sum to 256)
    pub w_kelly: u8,           // default: 115
    pub w_momentum: u8,        // default: 77
    pub w_vol: u8,             // default: 38
    pub w_liq: u8,             // default: 26

    // Urgency thresholds (u16, 0–10000)
    pub threshold_tighten: u16,      // default: 3000
    pub threshold_partial: u16,      // default: 5000
    pub threshold_majority: u16,     // default: 7000
    pub threshold_full_exit: u16,    // default: 9000

    // Partial exit fractions (permille)
    pub partial_sell_permille: u16,   // default: 350 (35%)
    pub majority_sell_permille: u16,  // default: 600 (60%)

    // Volatility estimator
    pub vol_baseline_bp_x100: u16,    // default: 300 (3bp median)
    pub vol_trail_min_mult_x256: u16, // default: 192 (0.75×)
    pub vol_trail_max_mult_x256: u16, // default: 640 (2.5×)

    // Monotonic floor ratchet factor × 8 (7 = 7/8 = 87.5%)
    pub floor_ratchet_numer: u8,      // default: 7

    // Max partial exits before forcing majority
    pub max_partials: u8,             // default: 2
}

impl Default for ExitV4Config {
    fn default() -> Self {
        Self {
            enabled: false,  // shadow mode by default
            w_kelly: 115,
            w_momentum: 77,
            w_vol: 38,
            w_liq: 26,
            threshold_tighten: 3000,
            threshold_partial: 5000,
            threshold_majority: 7000,
            threshold_full_exit: 9000,
            partial_sell_permille: 350,
            majority_sell_permille: 600,
            vol_baseline_bp_x100: 300,
            vol_trail_min_mult_x256: 192,
            vol_trail_max_mult_x256: 640,
            floor_ratchet_numer: 7,
            max_partials: 2,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── Size assertions ────────────────────────────────────────────

    #[test]
    fn test_momentum_divergence_size() {
        assert_eq!(core::mem::size_of::<MomentumDivergence>(), 16);
    }

    #[test]
    fn test_volatility_estimator_size() {
        assert_eq!(core::mem::size_of::<VolatilityEstimator>(), 16);
    }

    #[test]
    fn test_urgency_state_size() {
        assert_eq!(core::mem::size_of::<UrgencyState>(), 8);
    }

    // ── MomentumDivergence tests ───────────────────────────────────

    #[test]
    fn test_momentum_new_returns_zero_urgency() {
        let m = MomentumDivergence::new();
        assert_eq!(m.urgency(), 0);
    }

    #[test]
    fn test_momentum_all_buys_no_divergence() {
        let mut m = MomentumDivergence::new();
        for _ in 0..10 {
            m.record_event(true, 500);
        }
        // All buys: short_buys=5/5=1.0, medium_buys=10/10=1.0. No divergence.
        assert_eq!(m.urgency(), 0);
    }

    #[test]
    fn test_momentum_divergence_detects_fading_buys() {
        let mut m = MomentumDivergence::new();
        // First 15 buys (medium window looks great)
        for _ in 0..15 {
            m.record_event(true, 500);
        }
        // Then 5 sells (short window now all sells)
        for _ in 0..5 {
            m.record_event(false, 500);
        }
        // Short = 0/5 buys → rate 0. Medium has 15 buys, 5 sells → 15/20 = 750.
        // Divergence: 750 - 0 = 750 gap → urgency = 750 * 10 = 7500.
        let u = m.urgency();
        assert!(u >= 5000, "Expected high divergence urgency, got {u}");
    }

    #[test]
    fn test_momentum_sell_acceleration() {
        let mut m = MomentumDivergence::new();
        // Few buys then heavy sells
        for _ in 0..3 {
            m.record_event(true, 200);
        }
        for _ in 0..7 {
            m.record_event(false, 2000); // Big sells
        }
        let u = m.urgency();
        assert!(u > 0, "Expected urgency from sell acceleration, got {u}");
    }

    // ── VolatilityEstimator tests ──────────────────────────────────

    #[test]
    fn test_vol_new_returns_zero() {
        let v = VolatilityEstimator::new(1000);
        assert_eq!(v.vol_bp_x100, 0);
    }

    #[test]
    fn test_vol_tracks_variance() {
        let mut v = VolatilityEstimator::new(1000);
        // Stable price: small deltas
        for i in 0..10 {
            v.record(1000 + (i % 2)); // ±1 mvsol
        }
        let vol_stable = v.vol_bp_x100;

        let mut v2 = VolatilityEstimator::new(1000);
        // Volatile price: large deltas
        for i in 0..10 {
            let price = if i % 2 == 0 { 1050 } else { 950 };
            v2.record(price);
        }
        let vol_volatile = v2.vol_bp_x100;

        assert!(vol_volatile > vol_stable,
            "Volatile should have higher vol: {vol_volatile} vs {vol_stable}");
    }

    #[test]
    fn test_vol_trail_multiplier_default() {
        let v = VolatilityEstimator::new(1000);
        // No data → multiplier = 256 (1.0×)
        assert_eq!(v.trail_multiplier_x256(), 256);
    }

    // ── UrgencyState tests ─────────────────────────────────────────

    #[test]
    fn test_urgency_hold_when_low() {
        let mut u = UrgencyState::new();
        assert_eq!(u.decide(2000), ExitFraction::Hold);
        assert_eq!(u.remaining_permille, 1000);
    }

    #[test]
    fn test_urgency_tighten() {
        let mut u = UrgencyState::new();
        assert_eq!(u.decide(3500), ExitFraction::Tighten);
        assert_eq!(u.remaining_permille, 1000);
    }

    #[test]
    fn test_urgency_partial_exit() {
        let mut u = UrgencyState::new();
        let result = u.decide(5500);
        match result {
            ExitFraction::Partial(sell) => {
                // 35% of 1000 = 350
                assert_eq!(sell, 350);
                assert_eq!(u.remaining_permille, 650);
                assert_eq!(u.partial_count, 1);
            }
            other => panic!("Expected Partial, got {:?}", other),
        }
    }

    #[test]
    fn test_urgency_majority_exit() {
        let mut u = UrgencyState::new();
        let result = u.decide(7500);
        match result {
            ExitFraction::Partial(sell) => {
                // 60% of 1000 = 600
                assert_eq!(sell, 600);
                assert_eq!(u.remaining_permille, 400);
            }
            other => panic!("Expected Partial at majority threshold, got {:?}", other),
        }
    }

    #[test]
    fn test_urgency_full_exit() {
        let mut u = UrgencyState::new();
        let result = u.decide(9500);
        match result {
            ExitFraction::Exit(sell) => {
                assert_eq!(sell, 1000);
                assert_eq!(u.remaining_permille, 0);
            }
            other => panic!("Expected Exit, got {:?}", other),
        }
    }

    #[test]
    fn test_urgency_floor_ratchets_up() {
        let mut u = UrgencyState::new();
        // First partial at 5500
        u.decide(5500);
        assert!(u.urgency_floor > 0);
        let floor1 = u.urgency_floor;

        // Urgency drops to 2000, but floor keeps it above
        let effective = u.effective_urgency(2000);
        assert!(effective >= floor1, "Floor should ratchet up, got effective={effective} floor={floor1}");
    }

    #[test]
    fn test_urgency_max_partials() {
        let mut u = UrgencyState::new();
        // 2 partials at partial threshold
        u.decide(5500);
        assert_eq!(u.partial_count, 1);
        u.decide(5500);
        assert_eq!(u.partial_count, 2);
        // 3rd time: should not partial again (max_partials=2), should tighten instead
        let result = u.decide(5500);
        assert_eq!(result, ExitFraction::Tighten);
    }

    // ── u_kelly tests ──────────────────────────────────────────────

    #[test]
    fn test_u_kelly_strong_edge() {
        // f_hat = 200, f_entry = 250 → strong edge
        let u = u_kelly(200, 250);
        // strong threshold = 250*179/256 ≈ 174
        // 200 > 174 → urgency = 0
        assert_eq!(u, 0, "Strong edge should produce 0 urgency, got {u}");
    }

    #[test]
    fn test_u_kelly_weakening() {
        // f_hat = 50, f_entry = 250
        let u = u_kelly(50, 250);
        // sustain = 250*90/256 ≈ 87. 50 < 87 but > 0 → range [3000, 7000]
        assert!(u >= 3000 && u <= 7000, "Weakening should be 3000–7000, got {u}");
    }

    #[test]
    fn test_u_kelly_negative_max_urgency() {
        // f_hat = -100, f_entry = 250
        let u = u_kelly(-100, 250);
        assert!(u >= 7000, "Negative f_hat should be high urgency, got {u}");
    }

    #[test]
    fn test_u_kelly_zero_entry() {
        assert_eq!(u_kelly(50, 0), 10000);
    }

    // ── u_vol_trail tests ──────────────────────────────────────────

    #[test]
    fn test_u_vol_trail_far_from_stop() {
        // Price 10% above stop
        let u = u_vol_trail(1100, 1000);
        assert_eq!(u, 0, "Far from stop = 0 urgency, got {u}");
    }

    #[test]
    fn test_u_vol_trail_at_stop() {
        let u = u_vol_trail(1000, 1000);
        assert_eq!(u, 10000, "At stop = full urgency");
    }

    #[test]
    fn test_u_vol_trail_below_stop() {
        let u = u_vol_trail(990, 1000);
        assert_eq!(u, 10000, "Below stop = full urgency");
    }

    #[test]
    fn test_u_vol_trail_near_stop() {
        // 1% above stop (100bp)
        let u = u_vol_trail(1010, 1000);
        assert!(u >= 5000 && u <= 8000, "Near stop should be high urgency, got {u}");
    }

    // ── u_liquidity tests ──────────────────────────────────────────

    #[test]
    fn test_u_liquidity_deep_pool() {
        // 0.1 SOL position in 100 SOL pool → 10bp slippage
        let u = u_liquidity(100_000_000, 100_000_000_000);
        assert_eq!(u, 0);
    }

    #[test]
    fn test_u_liquidity_shallow_pool() {
        // 1 SOL position in 2 SOL pool → 5000bp slippage
        let u = u_liquidity(1_000_000_000, 2_000_000_000);
        assert_eq!(u, 10000);
    }

    #[test]
    fn test_u_liquidity_zero() {
        assert_eq!(u_liquidity(100, 0), 10000);
    }

    // ── composite_urgency tests ────────────────────────────────────

    #[test]
    fn test_composite_all_zero() {
        assert_eq!(composite_urgency(0, 0, 0, 0), 0);
    }

    #[test]
    fn test_composite_kelly_override() {
        // Kelly at 9000+ → immediate override to 10000
        assert_eq!(composite_urgency(9500, 0, 0, 0), 10000);
    }

    #[test]
    fn test_composite_universal_agreement() {
        // All signals >= 5000 → 10000
        assert_eq!(composite_urgency(5000, 5000, 5000, 5000), 10000);
    }

    #[test]
    fn test_composite_weighted_sum() {
        // Only kelly at 5000 → 5000 × 115 / 256 ≈ 2246
        let u = composite_urgency(5000, 0, 0, 0);
        assert!(u >= 2200 && u <= 2300, "Expected ~2246, got {u}");
    }

    #[test]
    fn test_composite_weights_sum_to_256() {
        // All at 10000 → override → 10000 (but let's verify the math works)
        // If kelly < 9000 and liq < 9000 and not all >= 5000, it does weighted sum
        let u = composite_urgency(4000, 4000, 4000, 4000);
        // 4000 × 256 / 256 = 4000 (all equal and weights sum to 256)
        assert_eq!(u, 4000);
    }

    // ── compute_adaptive_trail_stop tests ──────────────────────────

    #[test]
    fn test_adaptive_trail_stop_default_vol() {
        // Peak 1000 mvsol, base trail 500bp, multiplier 256 (1.0×)
        let stop = compute_adaptive_trail_stop(1000, 500, 256);
        // trail = 500bp → stop = 1000 × (10000-500)/10000 = 950
        assert_eq!(stop, 950);
    }

    #[test]
    fn test_adaptive_trail_stop_high_vol() {
        // Peak 1000, base 500bp, vol multiplier 512 (2.0×)
        let stop = compute_adaptive_trail_stop(1000, 500, 512);
        // trail = 500 × 512 / 256 = 1000bp → stop = 1000 × 9000/10000 = 900
        assert_eq!(stop, 900);
    }

    #[test]
    fn test_adaptive_trail_stop_low_vol() {
        // Peak 1000, base 500bp, vol multiplier 192 (0.75×)
        let stop = compute_adaptive_trail_stop(1000, 500, 192);
        // trail = 500 × 192 / 256 = 375bp → stop = 1000 × 9625/10000 = 962
        assert_eq!(stop, 962);
    }

    // ── ExitV4Config tests ─────────────────────────────────────────

    #[test]
    fn test_exit_v4_config_default_weights_sum() {
        let cfg = ExitV4Config::default();
        let sum = cfg.w_kelly as u16 + cfg.w_momentum as u16 + cfg.w_vol as u16 + cfg.w_liq as u16;
        assert_eq!(sum, 256, "Weights must sum to 256, got {sum}");
    }

    #[test]
    fn test_exit_v4_config_default_disabled() {
        let cfg = ExitV4Config::default();
        assert!(!cfg.enabled, "Default should be shadow mode (disabled)");
    }
}
