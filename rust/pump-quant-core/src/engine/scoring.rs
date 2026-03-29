//! Scoring helpers for the entry engine.
//! LUT builders, sigmoid/gaussian precomputation, weight structs.

/// Precomputed scoring weights (15 × f64 = 120 bytes)
#[repr(C)]
pub struct ScoringWeights {
    // Entry features (8)
    pub w_buy_burst: f64,
    pub w_volume: f64,
    pub w_curve: f64,
    pub w_concentration: f64,
    pub w_acceleration: f64,
    pub w_avg_size: f64,
    pub w_sell_absence: f64,
    pub w_recency: f64,
    // Magnitude features (7)
    pub w_mag_fill_rate: f64,
    pub w_mag_accel: f64,
    pub w_mag_wallet_quality: f64,
    pub w_mag_curve_remaining: f64,
    pub w_mag_volume_intensity: f64,
    pub w_mag_sell_vacuum: f64,
    pub w_mag_token_age: f64,
}

/// Precomputed reciprocals to avoid division on hot path
#[repr(C)]
pub struct Reciprocals {
    pub inv_volume_norm: f64,            // 1.0 / volume_norm_sol (default: 1/10 = 0.1)
    pub inv_max_recency_ms: f64,         // 1.0 / max_time_since_last_buy_ms (default: 1/500)
    pub inv_volume_intensity_norm: f64,   // 1.0 / volume_intensity_norm (default: 1/10)
    pub inv_avg_size_norm: f64,           // 1.0 / avg_size_norm_sol (default: 1/2)
    pub inv_sell_absence_mult: f64,       // 2.5 (the multiplier, precomputed)
    pub inv_magnitude_range: f64,         // 1.0 / (100.0 - min_magnitude_for_ride)
}

/// Build sigmoid LUT: lut[i] = 1 / (1 + exp(-steepness * (i - center)))
pub fn build_sigmoid_lut(size: usize, center: f64, steepness: f64) -> Vec<f64> {
    (0..size)
        .map(|i| {
            let x = i as f64;
            1.0 / (1.0 + (-steepness * (x - center)).exp())
        })
        .collect()
}

/// Build signed sigmoid LUT for acceleration: index = accel + offset
/// accel range: [-offset, size-offset-1]
pub fn build_signed_sigmoid_lut(
    size: usize,
    offset: usize,
    center: f64,
    steepness: f64,
) -> Vec<f64> {
    (0..size)
        .map(|i| {
            let x = i as f64 - offset as f64;
            1.0 / (1.0 + (-steepness * (x - center)).exp())
        })
        .collect()
}

/// Build Gaussian LUT: lut[i] = exp(-0.5 * ((i - mean) / sigma)^2)
pub fn build_gaussian_lut(size: usize, mean: f64, sigma: f64) -> Vec<f64> {
    (0..size)
        .map(|i| {
            let x = i as f64;
            let z = (x - mean) / sigma;
            (-0.5 * z * z).exp()
        })
        .collect()
}

/// Clamp f64 to [0, 1]
#[inline(always)]
pub fn clamp01(x: f64) -> f64 {
    if x < 0.0 {
        0.0
    } else if x > 1.0 {
        1.0
    } else {
        x
    }
}

/// Piecewise linear for buyer concentration:
/// peak at `peak` (score=1.0), ramp from `low` to `peak`, decay from `peak` to `max`, 0 above max
#[inline(always)]
pub fn concentration_score(unique_buyers: u16, peak: u16, max_cap: u16) -> f64 {
    if unique_buyers <= 5 {
        0.3 // Too few buyers
    } else if unique_buyers <= peak {
        // Ramp up: 0.3 at 5, 1.0 at peak
        0.3 + 0.7 * (unique_buyers - 5) as f64 / (peak - 5) as f64
    } else if unique_buyers <= max_cap {
        // Decay: 1.0 at peak, 0.0 at max_cap
        1.0 - (unique_buyers - peak) as f64 / (max_cap - peak) as f64
    } else {
        0.0
    }
}

/// Token age scoring: sweet spot 5-30s, penalty outside
#[inline(always)]
pub fn token_age_score(history_age_ms: u64) -> f64 {
    if history_age_ms < 5_000 {
        0.3 // Too new
    } else if history_age_ms <= 30_000 {
        1.0 // Sweet spot
    } else if history_age_ms <= 120_000 {
        // Linear decay from 1.0 to 0.3 over 30s-120s
        1.0 - 0.7 * (history_age_ms - 30_000) as f64 / 90_000.0
    } else {
        0.3 // Very old
    }
}

/// Sell vacuum scoring: 0 sells = 1.0, exponential decay
#[inline(always)]
pub fn sell_vacuum_score(sell_count_5s: u16) -> f64 {
    if sell_count_5s == 0 {
        1.0
    } else {
        0.6_f64.powi(sell_count_5s as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-9;

    // ── Sigmoid LUT ──────────────────────────────────────────────────

    #[test]
    fn sigmoid_lut_at_center_is_half() {
        let lut = build_sigmoid_lut(101, 50.0, 0.5);
        assert!((lut[50] - 0.5).abs() < EPS, "center should be 0.5, got {}", lut[50]);
    }

    #[test]
    fn sigmoid_lut_far_below_near_zero() {
        let lut = build_sigmoid_lut(101, 50.0, 0.5);
        assert!(lut[0] < 0.01, "far below center should be near 0, got {}", lut[0]);
    }

    #[test]
    fn sigmoid_lut_far_above_near_one() {
        let lut = build_sigmoid_lut(101, 50.0, 0.5);
        assert!(lut[100] > 0.99, "far above center should be near 1, got {}", lut[100]);
    }

    #[test]
    fn sigmoid_lut_monotonically_increasing() {
        let lut = build_sigmoid_lut(101, 50.0, 0.5);
        for i in 1..lut.len() {
            assert!(lut[i] >= lut[i - 1], "sigmoid should be monotonically increasing");
        }
    }

    // ── Signed Sigmoid LUT ──────────────────────────────────────────

    #[test]
    fn signed_sigmoid_center_at_offset() {
        // center=0 means the midpoint (0.5) is at index=offset
        let lut = build_signed_sigmoid_lut(201, 100, 0.0, 0.5);
        assert!(
            (lut[100] - 0.5).abs() < EPS,
            "signed sigmoid at offset should be 0.5, got {}",
            lut[100]
        );
    }

    #[test]
    fn signed_sigmoid_negative_near_zero() {
        let lut = build_signed_sigmoid_lut(201, 100, 0.0, 0.5);
        assert!(lut[0] < 0.01, "large negative accel should be near 0, got {}", lut[0]);
    }

    #[test]
    fn signed_sigmoid_positive_near_one() {
        let lut = build_signed_sigmoid_lut(201, 100, 0.0, 0.5);
        assert!(lut[200] > 0.99, "large positive accel should be near 1, got {}", lut[200]);
    }

    // ── Gaussian LUT ────────────────────────────────────────────────

    #[test]
    fn gaussian_lut_peaks_at_mean() {
        let lut = build_gaussian_lut(101, 50.0, 10.0);
        assert!(
            (lut[50] - 1.0).abs() < EPS,
            "gaussian should peak at 1.0 at the mean, got {}",
            lut[50]
        );
    }

    #[test]
    fn gaussian_lut_symmetric_decay() {
        let lut = build_gaussian_lut(101, 50.0, 10.0);
        // Values at equal distance from mean should be equal
        assert!(
            (lut[40] - lut[60]).abs() < EPS,
            "gaussian should be symmetric: lut[40]={} vs lut[60]={}",
            lut[40],
            lut[60]
        );
        assert!(
            (lut[30] - lut[70]).abs() < EPS,
            "gaussian should be symmetric: lut[30]={} vs lut[70]={}",
            lut[30],
            lut[70]
        );
    }

    #[test]
    fn gaussian_lut_decays_away_from_mean() {
        let lut = build_gaussian_lut(101, 50.0, 10.0);
        assert!(lut[50] > lut[40], "should decay away from mean");
        assert!(lut[40] > lut[30], "should decay further away");
        assert!(lut[30] > lut[20], "should decay further away");
        // Far from mean should be very small
        assert!(lut[0] < 0.01, "far from mean should be near 0, got {}", lut[0]);
    }

    // ── clamp01 ─────────────────────────────────────────────────────

    #[test]
    fn clamp01_within_range() {
        assert!((clamp01(0.5) - 0.5).abs() < EPS);
        assert!((clamp01(0.0) - 0.0).abs() < EPS);
        assert!((clamp01(1.0) - 1.0).abs() < EPS);
    }

    #[test]
    fn clamp01_below_zero() {
        assert!((clamp01(-0.5) - 0.0).abs() < EPS);
        assert!((clamp01(-100.0) - 0.0).abs() < EPS);
    }

    #[test]
    fn clamp01_above_one() {
        assert!((clamp01(1.5) - 1.0).abs() < EPS);
        assert!((clamp01(999.0) - 1.0).abs() < EPS);
    }

    // ── concentration_score ─────────────────────────────────────────

    #[test]
    fn concentration_score_few_buyers() {
        // 5 or fewer → 0.3
        assert!((concentration_score(0, 10, 30) - 0.3).abs() < EPS);
        assert!((concentration_score(3, 10, 30) - 0.3).abs() < EPS);
        assert!((concentration_score(5, 10, 30) - 0.3).abs() < EPS);
    }

    #[test]
    fn concentration_score_at_peak() {
        // At peak → 1.0
        assert!(
            (concentration_score(10, 10, 30) - 1.0).abs() < EPS,
            "at peak should be 1.0, got {}",
            concentration_score(10, 10, 30)
        );
    }

    #[test]
    fn concentration_score_ramp_up() {
        // Midpoint of ramp: buyers=7, peak=10 → 0.3 + 0.7*(2/5) = 0.58
        let score = concentration_score(7, 10, 30);
        let expected = 0.3 + 0.7 * 2.0 / 5.0;
        assert!(
            (score - expected).abs() < EPS,
            "ramp midpoint: expected {}, got {}",
            expected,
            score
        );
    }

    #[test]
    fn concentration_score_decay() {
        // At max_cap → 0.0
        assert!(
            (concentration_score(30, 10, 30) - 0.0).abs() < EPS,
            "at max_cap should be 0.0"
        );
        // Midpoint of decay: buyers=20, peak=10, max=30 → 1.0 - 10/20 = 0.5
        assert!(
            (concentration_score(20, 10, 30) - 0.5).abs() < EPS,
            "decay midpoint should be 0.5, got {}",
            concentration_score(20, 10, 30)
        );
    }

    #[test]
    fn concentration_score_above_max() {
        assert!((concentration_score(31, 10, 30) - 0.0).abs() < EPS);
        assert!((concentration_score(100, 10, 30) - 0.0).abs() < EPS);
    }

    // ── token_age_score ─────────────────────────────────────────────

    #[test]
    fn token_age_score_too_new() {
        // 1s = 1000ms → 0.3
        assert!(
            (token_age_score(1_000) - 0.3).abs() < EPS,
            "1s should be 0.3, got {}",
            token_age_score(1_000)
        );
        assert!((token_age_score(0) - 0.3).abs() < EPS);
        assert!((token_age_score(4_999) - 0.3).abs() < EPS);
    }

    #[test]
    fn token_age_score_sweet_spot() {
        // 10s = 10000ms → 1.0
        assert!(
            (token_age_score(10_000) - 1.0).abs() < EPS,
            "10s should be 1.0, got {}",
            token_age_score(10_000)
        );
        assert!((token_age_score(5_000) - 1.0).abs() < EPS);
        assert!((token_age_score(30_000) - 1.0).abs() < EPS);
    }

    #[test]
    fn token_age_score_decay() {
        // 60s = 60000ms → 1.0 - 0.7 * 30000/90000 = 1.0 - 0.2333... ≈ 0.7667
        let score = token_age_score(60_000);
        let expected = 1.0 - 0.7 * 30_000.0 / 90_000.0;
        assert!(
            (score - expected).abs() < EPS,
            "60s: expected {}, got {}",
            expected,
            score
        );
    }

    #[test]
    fn token_age_score_at_boundary() {
        // 120s → should be 0.3 (end of decay)
        let score = token_age_score(120_000);
        let expected = 1.0 - 0.7 * 90_000.0 / 90_000.0; // = 0.3
        assert!(
            (score - expected).abs() < EPS,
            "120s: expected {}, got {}",
            expected,
            score
        );
    }

    #[test]
    fn token_age_score_very_old() {
        assert!((token_age_score(120_001) - 0.3).abs() < EPS);
        assert!((token_age_score(999_999) - 0.3).abs() < EPS);
    }

    // ── sell_vacuum_score ───────────────────────────────────────────

    #[test]
    fn sell_vacuum_zero_sells() {
        assert!((sell_vacuum_score(0) - 1.0).abs() < EPS);
    }

    #[test]
    fn sell_vacuum_exponential_decay() {
        assert!((sell_vacuum_score(1) - 0.6).abs() < EPS);
        assert!((sell_vacuum_score(2) - 0.36).abs() < EPS);
        assert!((sell_vacuum_score(3) - 0.216).abs() < EPS);
    }

    // ── Struct layout sanity ────────────────────────────────────────

    #[test]
    fn scoring_weights_size() {
        assert_eq!(
            std::mem::size_of::<ScoringWeights>(),
            15 * 8,
            "ScoringWeights should be 15 × f64 = 120 bytes"
        );
    }

    #[test]
    fn reciprocals_size() {
        assert_eq!(
            std::mem::size_of::<Reciprocals>(),
            6 * 8,
            "Reciprocals should be 6 × f64 = 48 bytes"
        );
    }
}
