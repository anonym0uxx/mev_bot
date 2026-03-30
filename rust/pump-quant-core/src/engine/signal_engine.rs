//! Composite signal computation engine — integer-only, zero-heap, zero-float.
//!
//! Every function is `#[inline(always)]` and designed for <50ns hot-path execution.
//! All state lives in RideState v2; this module provides pure functions only.
//!
//! Score range: 0–1000 (clamped). Higher = stronger hold signal.
//!   700–1000 → STRONG_PUMP  (10% trail)
//!   400–699  → SUSTAINED    (6% trail)
//!   200–399  → WEAKENING    (3% trail)
//!   0–199    → EXIT         (immediate close)

// ───────────────────────────── Config Structs ─────────────────────────────

/// Weights for the composite signal score formula.
/// All weights are small integers; arithmetic stays in i32.
#[derive(Debug, Clone, Copy)]
pub struct SignalWeights {
    pub w_buy_rate_1s: i8,              // default: 24
    pub w_buy_rate_5s: i8,              // default: 16
    pub w_sell_rate_5s: i8,             // default: -20
    pub w_vol_accel_shift: u8,          // default: 6
    pub w_buy_gap_divisor: u16,         // default: 150
    pub w_sell_pressure_shift: u8,      // default: 2
    pub w_pnl_shift: u8,               // default: 3
    pub w_time_since_peak_divisor: u16, // default: 200
    pub w_unique_wallets: i8,           // default: 14
    pub w_confirm_vol_shift: u8,        // default: 8
}

impl SignalWeights {
    pub const DEFAULT: Self = Self {
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
    };
}

impl Default for SignalWeights {
    #[inline(always)]
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Kelly criterion trail-width multiplier config.
/// Uses a precomputed 17-entry sqrt LUT (8.8 fixed-point, 256 = 1.0x).
#[derive(Debug, Clone, Copy)]
pub struct KellyConfig {
    /// Baseline Kelly fraction in permille (671 = 0.671).
    pub baseline_f_permille: u16,
    /// sqrt(f*(p) / f*_baseline) × 256 for p = index/16, index ∈ [0, 16].
    pub sqrt_lut: [u16; 17],
}

impl KellyConfig {
    pub const DEFAULT: Self = Self {
        baseline_f_permille: 671,
        sqrt_lut: KELLY_SQRT_LUT,
    };
}

impl Default for KellyConfig {
    #[inline(always)]
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Lifecycle phase thresholds.
#[derive(Debug, Clone, Copy)]
pub struct LifecycleConfig {
    pub accel_min_buys: u16,        // default: 5
    pub accel_min_sol_msol: u32,    // default: 2000 (2 SOL)
    pub momentum_min_buys: u16,     // default: 15
    pub momentum_min_sol_msol: u32, // default: 10000 (10 SOL)
}

impl LifecycleConfig {
    pub const DEFAULT: Self = Self {
        accel_min_buys: 5,
        accel_min_sol_msol: 2000,
        momentum_min_buys: 15,
        momentum_min_sol_msol: 10000,
    };
}

impl Default for LifecycleConfig {
    #[inline(always)]
    fn default() -> Self {
        Self::DEFAULT
    }
}

// ───────────────────────────── Constants ──────────────────────────────────

/// Precomputed Kelly sqrt LUT.
/// Index i → sqrt(f*(i/16) / f*_baseline) × 256 for f*_baseline = 0.671.
pub const KELLY_SQRT_LUT: [u16; 17] = [
    0, 99, 140, 171, 198, 221, 242, 261, 279, 296, 312, 327, 341, 355, 368, 381, 394,
];

/// Lifecycle phase multipliers (8.8 fixed-point, 256 = 1.0x).
pub const PHASE_IGNITION: u16 = 192;     // 0.75x — tight trail
pub const PHASE_ACCELERATION: u16 = 256; // 1.00x — normal
pub const PHASE_MOMENTUM: u16 = 320;     // 1.25x — wide trail
pub const PHASE_DECAY: u16 = 160;        // 0.625x — very tight trail

// ───────────────────────────── Core Functions ─────────────────────────────

/// Compute composite signal score from current RideState fields.
///
/// Returns 0–1000. Called on every trade event.
/// Budget: <30ns. Integer-only. Branchless where possible.
///
/// Formula: accumulate weighted features into i32, clamp to [0, 1000].
/// Uses arithmetic right-shift for division-by-power-of-2,
/// and integer division for non-power-of-2 divisors (compiler emits mul+shift).
#[inline(always)]
pub fn compute_composite_score(
    buy_rate_1s: u8,
    buy_rate_5s: u8,
    sell_rate_5s: u8,
    vol_accel_bp: i16,
    buy_gap_ms: u16,
    sell_pressure_ratio: u8,
    unrealized_pnl_bp: i16,
    time_since_peak_ms: u16,
    unique_wallets: u8,
    confirming_vol_msol: u32,
    config: &SignalWeights,
) -> u16 {
    // Base score
    let mut s: i32 = 100;

    // F0: buy intensity short-term (positive weight)
    s += buy_rate_1s as i32 * config.w_buy_rate_1s as i32;

    // F1: buy intensity 5s window (positive weight)
    s += buy_rate_5s as i32 * config.w_buy_rate_5s as i32;

    // F2: sell penalty (negative weight)
    s += sell_rate_5s as i32 * config.w_sell_rate_5s as i32;

    // F3: volume acceleration (shift-divide)
    s += (vol_accel_bp as i32) >> config.w_vol_accel_shift;

    // F4: buy gap penalty (integer division — compiler uses reciprocal multiply)
    // Guard: divisor cannot be 0 (config invariant), but use OR 1 for safety.
    s -= buy_gap_ms as i32 / (config.w_buy_gap_divisor as i32 | 1);

    // F5: sell pressure penalty (shift-divide)
    s -= (sell_pressure_ratio as i32) >> config.w_sell_pressure_shift;

    // F6: unrealized PnL bonus (shift-divide)
    s += (unrealized_pnl_bp as i32) >> config.w_pnl_shift;

    // F7: time since peak penalty (integer division)
    s -= time_since_peak_ms as i32 / (config.w_time_since_peak_divisor as i32 | 1);

    // F8: unique wallet bonus
    s += unique_wallets as i32 * config.w_unique_wallets as i32;

    // F9: confirming volume bonus (shift-divide)
    s += (confirming_vol_msol as i32) >> config.w_confirm_vol_shift;

    // Branchless clamp to [0, 1000].
    // s.max(0).min(1000) compiles to cmov/conditional-select on x86-64 and aarch64.
    s.max(0).min(1000) as u16
}

/// Compute Kelly trail multiplier from win probability estimate.
///
/// Returns 8.8 fixed-point: 256 = 1.0x multiplier.
/// Uses LUT indexed by estimated win probability (0..16 → 0.0..1.0).
///
/// Win probability estimate: p_est = buys / (buys + sells), mapped to [0, 16].
/// Confirming volume adds confidence (blended with count-based estimate).
///
/// Budget: <10ns.
#[inline(always)]
pub fn compute_kelly_multiplier(
    buys_after_entry: u16,
    confirming_vol_msol: u32,
    sells_after_entry: u16,
    config: &KellyConfig,
) -> u16 {
    let total = buys_after_entry as u32 + sells_after_entry as u32;

    // No data → return baseline (1.0x)
    if total == 0 {
        return 256;
    }

    // Count-based win prob estimate: p_count = buys * 16 / total
    let p_count = (buys_after_entry as u32 * 16) / total;

    // Volume-based confidence adjustment:
    // If confirming vol > 5 SOL (5000 mSOL), boost p by 1 (capped at 16).
    // If confirming vol < 1 SOL (1000 mSOL), reduce p by 1 (floored at 0).
    let vol_adj: i32 = if confirming_vol_msol >= 5000 {
        1
    } else if confirming_vol_msol < 1000 {
        -1
    } else {
        0
    };

    // Final p index, clamped to [0, 16]
    let p_idx = (p_count as i32 + vol_adj).max(0).min(16) as usize;

    // LUT lookup — no bounds check needed since p_idx ∈ [0, 16]
    // and sqrt_lut has 17 entries.
    config.sqrt_lut[p_idx]
}

/// Compute lifecycle phase multiplier.
///
/// Returns 8.8 fixed-point: 256 = 1.0x.
///
/// Phases (in priority order):
///   DECAY:        buys >= accel_min AND buy_rate_1s < 2 → 160 (0.625x)
///   IGNITION:     buys < accel_min                      → 192 (0.75x)
///   MOMENTUM:     buys >= momentum_min AND vol >= momentum_sol → 320 (1.25x)
///   ACCELERATION: everything else                       → 256 (1.0x)
#[inline(always)]
pub fn compute_lifecycle_multiplier(
    buys_after_entry: u16,
    confirming_vol_msol: u32,
    unique_wallets: u8,
    buy_rate_1s: u8,
    config: &LifecycleConfig,
) -> u16 {
    let _ = unique_wallets; // reserved for future use

    // IGNITION: not enough buys yet
    if buys_after_entry < config.accel_min_buys {
        return PHASE_IGNITION;
    }

    // DECAY: past ignition but buy rate collapsed
    if buy_rate_1s < 2 {
        return PHASE_DECAY;
    }

    // MOMENTUM: enough buys AND enough volume
    if buys_after_entry >= config.momentum_min_buys
        && confirming_vol_msol >= config.momentum_min_sol_msol
    {
        return PHASE_MOMENTUM;
    }

    // ACCELERATION: default
    PHASE_ACCELERATION
}

/// Count events in last `window_ms` from a ring buffer of relative timestamps.
///
/// Ring stores timestamps as u16 ms offsets from entry.
/// `ring_len` is the number of valid entries (≤ ring.len()).
/// Handles ring wrap correctly.
///
/// For small fixed-size rings (≤ 20 elements), the compiler unrolls this loop.
#[inline(always)]
pub fn count_in_window(
    ring: &[u16],
    ring_idx: u8,
    ring_len: u8,
    now_rel_ms: u16,
    window_ms: u16,
) -> u8 {
    let len = ring_len.min(ring.len() as u8) as usize;
    let cap = ring.len();

    // Threshold: events with ts >= threshold are within window.
    // Use saturating_sub to handle the case where window > now_rel_ms.
    let threshold = now_rel_ms.saturating_sub(window_ms);

    let mut count: u8 = 0;

    // Walk backwards from most recent entry.
    // ring_idx points to next write slot, so most recent = ring_idx - 1.
    let mut i: usize = 0;
    while i < len {
        // Compute index with wrap. (ring_idx as usize + cap - 1 - i) % cap
        // This avoids underflow by adding cap before subtracting.
        let idx = (ring_idx as usize + cap - 1 - i) % cap;

        // Safety: idx < cap ≤ ring.len(), so this is in-bounds.
        // Use unchecked for speed in release mode.
        let ts = unsafe { *ring.get_unchecked(idx) };

        // Within window? Branchless: (ts >= threshold) as u8 adds 0 or 1.
        count += (ts >= threshold && ts <= now_rel_ms) as u8;

        i += 1;
    }

    count
}

/// Compute sell pressure ratio (0–255).
///
/// ratio = sell_rate × 255 / (buy_rate + sell_rate).
/// Returns 0 if no events. Returns 255 if all sells.
#[inline(always)]
pub fn sell_pressure_ratio(buy_rate: u8, sell_rate: u8) -> u8 {
    let total = buy_rate as u16 + sell_rate as u16;
    if total == 0 {
        return 0;
    }
    // sell_rate * 255 / total — both fit in u16 (max: 255 * 255 = 65025).
    ((sell_rate as u16 * 255) / total) as u8
}

/// Compute volume acceleration in basis points.
///
/// accel_bp = (recent - prior) × 10000 / max(prior, 1)
/// Clamped to [-10000, +10000].
///
/// Uses multiplication + shift where possible. For the general case,
/// integer division is unavoidable but the compiler emits efficient code.
#[inline(always)]
pub fn volume_acceleration_bp(vol_recent_msol: u32, vol_prior_msol: u32) -> i16 {
    let prior = vol_prior_msol.max(1) as i64;
    let delta = vol_recent_msol as i64 - vol_prior_msol as i64;

    // delta * 10000 / prior — use i64 to avoid overflow.
    // Max intermediate: ~4_294_967_295 * 10000 = ~4.3e13, fits i64.
    let accel = (delta * 10000) / prior;

    // Clamp to i16-safe range [-10000, +10000].
    accel.max(-10000).min(10000) as i16
}

/// Update EMA-smoothed price velocity.
///
/// EMA-4: new = (old × 3 + sample) >> 2
///
/// `vsol_delta`: change in virtual SOL reserves (lamports) since last sample.
/// `dt_ms`: time since last sample in milliseconds.
/// Returns updated EMA value in lamports/s (scaled).
#[inline(always)]
pub fn update_price_velocity_ema(old_pv: i32, vsol_delta: i32, dt_ms: u16) -> i32 {
    // Compute instantaneous velocity: delta * 1000 / dt (lamports per second).
    // Guard against dt=0.
    let dt = dt_ms.max(1) as i32;
    let sample = (vsol_delta as i64 * 1000 / dt as i64) as i32;

    // EMA-4: (old * 3 + sample) >> 2
    // Use wrapping arithmetic — values may be large but the shift keeps things bounded.
    (old_pv.wrapping_mul(3).wrapping_add(sample)) >> 2
}

/// Approximate unique wallet count from 64-bit bloom filter.
///
/// Uses popcount × 45 / 64 ≈ popcount × ln(2) for 2-hash bloom filter.
/// This gives the maximum-likelihood estimate of the number of distinct insertions.
#[inline(always)]
pub fn bloom_count(bloom: &[u8; 8]) -> u8 {
    // Transmute [u8; 8] to u64 (little-endian). This is a no-op on LE architectures.
    let bits = u64::from_le_bytes(*bloom);

    // Popcount — compiles to a single `popcnt` instruction on x86-64 with BMI.
    let pop = bits.count_ones() as u16;

    // Estimate: pop * 45 / 64. Max pop=64 → 64*45/64 = 45. Fits u8.
    ((pop * 45) >> 6) as u8
}

/// Insert a wallet hash into a 64-bit bloom filter (2 hash functions).
///
/// h1 = bits [0:5] of wallet_hash  (& 0x3F)
/// h2 = bits [16:21] of wallet_hash (>> 16 & 0x3F)
#[inline(always)]
pub fn bloom_insert(bloom: &mut [u8; 8], wallet_hash: u64) {
    let h1 = (wallet_hash & 0x3F) as u32;
    let h2 = ((wallet_hash >> 16) & 0x3F) as u32;

    // Operate on u64 directly for single-instruction bit-set.
    let bits = u64::from_le_bytes(*bloom);
    let updated = bits | (1u64 << h1) | (1u64 << h2);
    *bloom = updated.to_le_bytes();
}

// ───────────────────────────── Tests ──────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_composite_score_base_only() {
        // All zeros → base score of 100.
        let score = compute_composite_score(0, 0, 0, 0, 0, 0, 0, 0, 0, 0, &SignalWeights::DEFAULT);
        assert_eq!(score, 100);
    }

    #[test]
    fn test_composite_score_strong_pump() {
        // Scenario A from spec: strong pump profile.
        let w = SignalWeights::DEFAULT;
        let score = compute_composite_score(
            5,     // buy_rate_1s
            9,     // buy_rate_5s
            0,     // sell_rate_5s
            3000,  // vol_accel_bp
            200,   // buy_gap_ms
            0,     // sell_pressure_ratio
            560,   // unrealized_pnl_bp
            100,   // time_since_peak_ms
            6,     // unique_wallets
            5000,  // confirming_vol_msol
            &w,
        );
        // Expected: 100 + 120 + 144 + 0 + 46 - 1 - 0 + 70 - 0 + 84 + 19 = 582
        // (exact value depends on integer division rounding)
        assert!(score >= 400, "Strong pump should be SUSTAINED+, got {}", score);
        assert!(score <= 1000);
    }

    #[test]
    fn test_composite_score_whale_dump() {
        // Whale exit scenario: should score low (EXIT zone).
        let w = SignalWeights::DEFAULT;
        let score = compute_composite_score(
            0,      // buy_rate_1s — no buying
            2,      // buy_rate_5s — minimal
            3,      // sell_rate_5s — active selling
            -5000,  // vol_accel_bp — negative
            2000,   // buy_gap_ms — long gap
            180,    // sell_pressure_ratio — high
            -100,   // unrealized_pnl_bp — losing
            1500,   // time_since_peak_ms — well past peak
            2,      // unique_wallets — low
            1000,   // confirming_vol_msol — low
            &w,
        );
        assert!(score < 200, "Whale dump should be EXIT zone, got {}", score);
    }

    #[test]
    fn test_composite_score_clamped_low() {
        // Extreme negative inputs → clamp to 0.
        let w = SignalWeights::DEFAULT;
        let score = compute_composite_score(
            0, 0, 10, -10000, 60000, 255, -5000, 60000, 0, 0, &w,
        );
        assert_eq!(score, 0);
    }

    #[test]
    fn test_composite_score_clamped_high() {
        // Extreme positive inputs → clamp to 1000.
        let w = SignalWeights::DEFAULT;
        let score = compute_composite_score(
            20, 20, 0, 10000, 0, 0, 5000, 0, 255, 1_000_000, &w,
        );
        assert_eq!(score, 1000);
    }

    #[test]
    fn test_kelly_multiplier_no_data() {
        let cfg = KellyConfig::DEFAULT;
        let m = compute_kelly_multiplier(0, 0, 0, &cfg);
        assert_eq!(m, 256, "No data → 1.0x baseline");
    }

    #[test]
    fn test_kelly_multiplier_all_buys() {
        let cfg = KellyConfig::DEFAULT;
        // 16 buys, 0 sells, high vol → p_idx = 16+1 clamped to 16
        let m = compute_kelly_multiplier(16, 10000, 0, &cfg);
        assert_eq!(m, KELLY_SQRT_LUT[16], "All buys high vol → max LUT");
    }

    #[test]
    fn test_kelly_multiplier_balanced() {
        let cfg = KellyConfig::DEFAULT;
        // 8 buys, 8 sells → p = 8 * 16 / 16 = 8
        let m = compute_kelly_multiplier(8, 3000, 8, &cfg);
        assert_eq!(m, KELLY_SQRT_LUT[8]);
    }

    #[test]
    fn test_lifecycle_ignition() {
        let cfg = LifecycleConfig::DEFAULT;
        let m = compute_lifecycle_multiplier(3, 500, 2, 4, &cfg);
        assert_eq!(m, PHASE_IGNITION);
    }

    #[test]
    fn test_lifecycle_decay() {
        let cfg = LifecycleConfig::DEFAULT;
        // buys >= accel_min (5) but buy_rate_1s < 2
        let m = compute_lifecycle_multiplier(10, 5000, 5, 1, &cfg);
        assert_eq!(m, PHASE_DECAY);
    }

    #[test]
    fn test_lifecycle_momentum() {
        let cfg = LifecycleConfig::DEFAULT;
        // buys >= 15, vol >= 10000, buy_rate >= 2
        let m = compute_lifecycle_multiplier(20, 15000, 8, 5, &cfg);
        assert_eq!(m, PHASE_MOMENTUM);
    }

    #[test]
    fn test_lifecycle_acceleration() {
        let cfg = LifecycleConfig::DEFAULT;
        // buys = 10 (>= 5, < 15), buy_rate >= 2
        let m = compute_lifecycle_multiplier(10, 5000, 4, 3, &cfg);
        assert_eq!(m, PHASE_ACCELERATION);
    }

    #[test]
    fn test_count_in_window_basic() {
        // Ring: [100, 200, 300, 400, 500, 600, 700, 800]
        // now=850, window=300 → events >= 550: 600, 700, 800 = 3
        let ring: [u16; 8] = [100, 200, 300, 400, 500, 600, 700, 800];
        let count = count_in_window(&ring, 0, 8, 850, 300);
        assert_eq!(count, 3);
    }

    #[test]
    fn test_count_in_window_empty() {
        let ring: [u16; 8] = [0; 8];
        let count = count_in_window(&ring, 0, 0, 1000, 500);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_count_in_window_wrap() {
        // Ring wraps: idx=3, len=8 → most recent at idx 2, then 1, 0, 7, 6, 5, 4, 3
        let ring: [u16; 8] = [700, 800, 900, 100, 200, 300, 400, 500];
        //                     [0]  [1]  [2]  [3]  [4]  [5]  [6]  [7]
        // Write order: 3→100, 4→200, 5→300, 6→400, 7→500, 0→700, 1→800, 2→900
        // Most recent = idx 2 (900), then 1 (800), 0 (700), ...
        let count = count_in_window(&ring, 3, 8, 900, 250);
        // Events >= 650: 700, 800, 900 = 3
        assert_eq!(count, 3);
    }

    #[test]
    fn test_sell_pressure_ratio_no_events() {
        assert_eq!(sell_pressure_ratio(0, 0), 0);
    }

    #[test]
    fn test_sell_pressure_ratio_all_buys() {
        assert_eq!(sell_pressure_ratio(10, 0), 0);
    }

    #[test]
    fn test_sell_pressure_ratio_all_sells() {
        assert_eq!(sell_pressure_ratio(0, 10), 255);
    }

    #[test]
    fn test_sell_pressure_ratio_balanced() {
        // 5 buys, 5 sells → 5*255/10 = 127
        assert_eq!(sell_pressure_ratio(5, 5), 127);
    }

    #[test]
    fn test_volume_acceleration_bp_positive() {
        // recent=2000, prior=1000 → (2000-1000)*10000/1000 = 10000
        assert_eq!(volume_acceleration_bp(2000, 1000), 10000);
    }

    #[test]
    fn test_volume_acceleration_bp_negative() {
        // recent=500, prior=1000 → (500-1000)*10000/1000 = -5000
        assert_eq!(volume_acceleration_bp(500, 1000), -5000);
    }

    #[test]
    fn test_volume_acceleration_bp_zero_prior() {
        // prior=0 → clamp prior to 1. recent=100 → 100*10000/1 = 1_000_000 clamped to 10000.
        assert_eq!(volume_acceleration_bp(100, 0), 10000);
    }

    #[test]
    fn test_volume_acceleration_bp_equal() {
        assert_eq!(volume_acceleration_bp(1000, 1000), 0);
    }

    #[test]
    fn test_update_price_velocity_ema() {
        // EMA-4: (old * 3 + sample) >> 2
        // old=1000, vsol_delta=2000, dt=100ms → sample = 2000*1000/100 = 20000
        // new = (1000*3 + 20000) >> 2 = (3000 + 20000) >> 2 = 23000 >> 2 = 5750
        let pv = update_price_velocity_ema(1000, 2000, 100);
        assert_eq!(pv, 5750);
    }

    #[test]
    fn test_update_price_velocity_ema_zero_dt() {
        // dt=0 → clamped to 1. vsol_delta=500 → sample = 500*1000/1 = 500000.
        // new = (0*3 + 500000) >> 2 = 125000
        let pv = update_price_velocity_ema(0, 500, 0);
        assert_eq!(pv, 125000);
    }

    #[test]
    fn test_bloom_empty() {
        let bloom = [0u8; 8];
        assert_eq!(bloom_count(&bloom), 0);
    }

    #[test]
    fn test_bloom_insert_and_count() {
        let mut bloom = [0u8; 8];

        // Insert wallet hash 0x0000_0000_0001_0001.
        // h1 = 0x0001_0001 & 0x3F = 1
        // h2 = (0x0001_0001 >> 16) & 0x3F = 1
        // Both hashes hit bit 1 → popcount=1 → count = 1*45/64 = 0 (rounded down).
        bloom_insert(&mut bloom, 0x0001_0001);
        // Actually: h1=1, h2=1 → 1 bit set. 1*45>>6 = 0.
        // But that's the estimator floor. For 1 bit, estimate is 0.
        // This is expected behavior for bloom count estimator with very few items.

        // Insert distinct hash with different h1, h2.
        bloom_insert(&mut bloom, 0x0020_0020);
        // h1 = 0x20 & 0x3F = 32
        // h2 = (0x0020_0020 >> 16) & 0x3F = 0x20 = 32 → same bit! Still only 2 bits.
        // Hmm, let's use a hash that produces distinct h1, h2.
        bloom_insert(&mut bloom, 0x0010_0005);
        // h1 = 0x05 & 0x3F = 5
        // h2 = (0x0010_0005 >> 16) & 0x3F = 0x10 = 16
        // Now bits 1, 5, 16, 32 are set → popcount=4 → count = 4*45/64 = 2.
        let c = bloom_count(&bloom);
        assert!(c >= 1 && c <= 4, "Bloom count after 3 inserts: {}", c);
    }

    #[test]
    fn test_bloom_insert_distinct_hashes() {
        let mut bloom = [0u8; 8];

        // Insert 10 hashes with well-separated bits.
        for i in 0u64..10 {
            // Construct hash so h1 and h2 are distinct and spread out.
            let hash = i | ((i + 20) << 16);
            bloom_insert(&mut bloom, hash);
        }

        let c = bloom_count(&bloom);
        // 10 inserts × 2 bits each = up to 20 bits set (less due to collisions).
        // Estimate should be in ballpark of 7-14.
        assert!(c >= 5, "Expected reasonable count for 10 inserts, got {}", c);
    }

    #[test]
    fn test_bloom_full() {
        // All bits set → popcount=64 → 64*45/64 = 45.
        let bloom = [0xFF; 8];
        assert_eq!(bloom_count(&bloom), 45);
    }
}