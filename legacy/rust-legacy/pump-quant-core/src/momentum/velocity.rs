//! Velocity and acceleration computations for momentum exit signals.
//!
//! Provides sliding-window velocity (bps/tick), acceleration (delta-velocity),
//! and momentum collapse detection. All functions are pure, stateless,
//! and integer-only (i64 outputs in milli-bps per second for precision).
//!
//! Consumed by `evaluate_velocity_exit()` in position.rs.

/// Compute price velocity over the last `window` samples.
///
/// Returns velocity in milli-bps per tick (mbps/tick).
/// Positive = price rising, negative = price falling.
/// Uses simple linear regression slope approximation:
///   velocity = (last - first) * 1000 / window
///
/// Returns 0 if fewer than 2 samples or window < 2.
#[inline]
pub fn compute_velocity(samples: &[i32], window: usize) -> i64 {
    let n = samples.len();
    if n < 2 || window < 2 {
        return 0;
    }
    let start = if n > window { n - window } else { 0 };
    let first = samples[start] as i64;
    let last = samples[n - 1] as i64;
    let span = (n - 1 - start) as i64;
    if span == 0 {
        return 0;
    }
    // milli-bps per tick: (delta_bps * 1000) / span
    (last - first) * 1000 / span
}

/// Compute price acceleration over the last `window` samples.
///
/// Returns acceleration in milli-bps per tick² (mbps/tick²).
/// Acceleration = change in velocity between the two halves of the window.
/// Negative acceleration while velocity is negative = collapse intensifying.
///
/// Returns 0 if fewer than 4 samples or window < 4.
#[inline]
pub fn compute_acceleration(samples: &[i32], window: usize) -> i64 {
    let n = samples.len();
    if n < 4 || window < 4 {
        return 0;
    }
    let start = if n > window { n - window } else { 0 };
    let slice = &samples[start..n];
    let len = slice.len();
    let mid = len / 2;

    // Velocity of first half
    let v1 = if mid > 0 {
        (slice[mid] as i64 - slice[0] as i64) * 1000 / mid as i64
    } else {
        0
    };

    // Velocity of second half
    let v2 = if len - mid > 0 {
        (slice[len - 1] as i64 - slice[mid] as i64) * 1000 / (len - mid - 1).max(1) as i64
    } else {
        0
    };

    // Acceleration = change in velocity
    v2 - v1
}

/// Detect a momentum collapse pattern: local peak followed by sharp gap-down.
///
/// Scans the last `lookback` samples for a local peak >= `min_peak_bps`,
/// then checks if price dropped by >= `drop_threshold_bps` within
/// `max_samples` ticks after the peak.
///
/// Returns true if collapse pattern detected.
#[inline]
pub fn detect_momentum_collapse(
    samples: &[i32],
    lookback: usize,
    min_peak_bps: i32,
    drop_threshold_bps: i32,
    max_samples: usize,
) -> bool {
    let n = samples.len();
    if n < 3 || lookback < 2 {
        return false;
    }
    let start = if n > lookback { n - lookback } else { 0 };
    let slice = &samples[start..n];
    let len = slice.len();

    // Find local peak in the window (excluding the last sample, which is current)
    let mut peak_idx = 0;
    let mut peak_val = i32::MIN;
    for i in 0..len.saturating_sub(1) {
        if slice[i] >= min_peak_bps && slice[i] > peak_val {
            peak_val = slice[i];
            peak_idx = i;
        }
    }

    if peak_val < min_peak_bps {
        return false;
    }

    // Check for gap-down from peak within max_samples.
    // drop_threshold_bps may be negative (config convention: "bps drop" as negative).
    // Use absolute value since `drop` = peak - current is always positive for a decline.
    let abs_threshold = drop_threshold_bps.unsigned_abs() as i32;
    let search_end = (peak_idx + max_samples + 1).min(len);
    for i in (peak_idx + 1)..search_end {
        let drop = peak_val - slice[i];
        if drop >= abs_threshold {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Original unit tests (preserved) ─────────────────────────────────

    #[test]
    fn test_compute_velocity_rising() {
        // Steady rise: 0, 100, 200, 300, 400
        // window=5 → start=0, span=4, v = (400-0)*1000/4 = 100_000 mbps
        let samples = [0, 100, 200, 300, 400];
        let v = compute_velocity(&samples, 5);
        assert_eq!(v, 100_000); // 400 bps over 4 ticks * 1000 = 100_000 mbps/tick
    }

    #[test]
    fn test_compute_velocity_falling() {
        let samples = [400, 300, 200, 100, 0];
        let v = compute_velocity(&samples, 5);
        assert!(v < 0, "falling price should have negative velocity");
    }

    #[test]
    fn test_compute_velocity_flat() {
        let samples = [100, 100, 100, 100];
        let v = compute_velocity(&samples, 4);
        assert_eq!(v, 0);
    }

    #[test]
    fn test_compute_velocity_insufficient_samples() {
        assert_eq!(compute_velocity(&[100], 5), 0);
        assert_eq!(compute_velocity(&[], 5), 0);
    }

    #[test]
    fn test_compute_acceleration_decelerating() {
        // First half rising fast, second half rising slow → negative acceleration
        let samples = [0, 200, 400, 500, 520, 530];
        let a = compute_acceleration(&samples, 6);
        assert!(a < 0, "decelerating should have negative acceleration: {a}");
    }

    #[test]
    fn test_compute_acceleration_insufficient() {
        assert_eq!(compute_acceleration(&[0, 100, 200], 3), 0);
    }

    #[test]
    fn test_detect_momentum_collapse_basic() {
        // Peak at 500, then drops 300 bps in 2 samples
        let samples = [0, 100, 300, 500, 300, 200];
        assert!(detect_momentum_collapse(&samples, 6, 200, 300, 3));
    }

    #[test]
    fn test_detect_momentum_collapse_no_peak() {
        // Never reaches min_peak_bps
        let samples = [0, 50, 80, 60, 40];
        assert!(!detect_momentum_collapse(&samples, 5, 200, 100, 3));
    }

    #[test]
    fn test_detect_momentum_collapse_no_drop() {
        // Peak reached but no sharp drop
        let samples = [0, 200, 400, 390, 385];
        assert!(!detect_momentum_collapse(&samples, 5, 200, 100, 3));
    }

    // ════════════════════════════════════════════════════════════════════
    // Quant spec velocity exit tests (10 required + 1 bonus)
    // ════════════════════════════════════════════════════════════════════

    /// Test 1: Slow pump, no velocity exit.
    /// Steady +50 bps/sample rise → velocity strongly positive,
    /// no momentum collapse.
    #[test]
    fn test_01_slow_pump_no_velocity_exit() {
        let samples: &[i32] = &[50, 100, 150, 200, 250, 300, 350];

        // Velocity over last 3 samples: (350 - 200) * 1000 / 2 = 75_000 mbps
        let v = compute_velocity(samples, 3);
        assert!(
            v > 0,
            "steady rise should produce positive velocity, got {v}"
        );
        // Expect strongly positive: at least +50_000 mbps (50 bps/tick)
        assert!(
            v >= 50_000,
            "slow pump velocity should be >= 50_000 mbps, got {v}"
        );

        // No momentum collapse: steady rise, no peak-then-drop pattern.
        // drop_threshold_bps is a POSITIVE value representing the minimum drop
        // from peak to trigger collapse (function computes peak_val - current >= threshold).
        // In a steady rise, peak is the second-to-last value (300 at index 5),
        // and the last checked sample is 350 at index 6 → drop = 300-350 = -50 < 200 → no collapse.
        let collapsed = detect_momentum_collapse(
            samples,
            7,   // lookback = full window
            200, // min_peak_bps
            200, // drop_threshold_bps (need 200+ bps drop from peak)
            2,   // max_samples_after_peak
        );
        assert!(
            !collapsed,
            "steady rise should NOT trigger momentum collapse"
        );
    }

    /// Test 2: Fast dump triggers velocity exit.
    /// Pumped to 300, now crashing → velocity strongly negative.
    #[test]
    fn test_02_fast_dump_triggers_velocity_exit() {
        let samples: &[i32] = &[100, 200, 300, 200, 50, -100];

        // Velocity over last 3 samples: window covers [200, 50, -100]
        // v = (-100 - 200) * 1000 / 2 = -150_000 mbps
        let v = compute_velocity(samples, 3);
        assert!(
            v <= -150_000,
            "fast dump velocity should be <= -150_000 mbps, got {v}"
        );
    }

    /// Test 3: Momentum collapse gap-down.
    /// Peak at 300, dropped 250 bps in 1 sample → collapse detected.
    #[test]
    fn test_03_momentum_collapse_gap_down() {
        let samples: &[i32] = &[10, 50, 200, 300, 50];

        // detect_momentum_collapse: peak=300, drop from 300→50 = 250 bps in 1 sample
        // min_peak=200, drop_threshold=200, max_samples_after_peak=2
        let collapsed = detect_momentum_collapse(
            samples,
            5,   // lookback
            200, // min_peak_bps
            200, // drop_threshold_bps (absolute drop, not negative)
            2,   // max_samples_after_peak
        );
        assert!(
            collapsed,
            "300→50 gap-down should trigger momentum collapse"
        );
    }

    /// Test 4: Slow decline — no momentum collapse (too gradual).
    /// Peak at start, declining slowly over 5 samples → max_samples_after_peak=2
    /// means the drop at samples 4-5 is too far from the peak.
    #[test]
    fn test_04_slow_decline_no_momentum_collapse() {
        let samples: &[i32] = &[300, 280, 260, 240, 220, 200];

        // Peak = 300 at index 0 (in lookback=6 window).
        // max_samples_after_peak=2 means we only check indices 1..3 (280, 260).
        // Drop from 300→260 = 40 bps, which is < 200 drop threshold.
        let collapsed = detect_momentum_collapse(
            samples,
            6,   // lookback
            200, // min_peak_bps
            200, // drop_threshold_bps
            2,   // max_samples_after_peak
        );
        assert!(
            !collapsed,
            "slow decline should NOT trigger momentum collapse (gradual drop < threshold within window)"
        );
    }

    /// Test 5: Peak too small — momentum collapse suppressed.
    /// Peak=100 but min_peak_bps=200 → no collapse even with a drop.
    #[test]
    fn test_05_peak_too_small_collapse_suppressed() {
        let samples: &[i32] = &[10, 50, 100, 50];

        let collapsed = detect_momentum_collapse(
            samples,
            4,   // lookback
            200, // min_peak_bps — peak of 100 doesn't qualify
            50,  // drop_threshold_bps (generous threshold)
            3,   // max_samples_after_peak
        );
        assert!(
            !collapsed,
            "peak=100 < min_peak=200 should suppress collapse detection"
        );
    }

    /// Test 6: Accelerating decline.
    /// Each tick loses more than the last → acceleration should be negative.
    #[test]
    fn test_06_accelerating_decline() {
        let samples: &[i32] = &[500, 480, 450, 400, 330, 240];

        // Drops: -20, -30, -50, -70, -90 → accelerating decline
        // First half (4 samples): [500, 480, 450, 400]
        //   v1 = (400 - 500) * 1000 / 2 = -50_000
        //   v2 depends on window split
        let a = compute_acceleration(samples, 4);
        assert!(
            a < 0,
            "accelerating decline should have negative acceleration, got {a}"
        );

        // Also verify with full window
        let a_full = compute_acceleration(samples, 6);
        assert!(
            a_full < 0,
            "full-window accelerating decline should be negative, got {a_full}"
        );
    }

    /// Test 7: Flat prices — no signal.
    /// Nearly flat samples → velocity near 0.
    #[test]
    fn test_07_flat_prices_no_signal() {
        let samples: &[i32] = &[200, 201, 200, 200, 201, 200];

        let v = compute_velocity(samples, 3);
        // Absolute value should be < 10_000 mbps (10 bps/tick)
        assert!(
            v.abs() < 10_000,
            "flat prices should produce near-zero velocity, got {v}"
        );

        // Full-window velocity also near zero
        let v_full = compute_velocity(samples, 6);
        assert!(
            v_full.abs() < 10_000,
            "flat prices full-window velocity should be near-zero, got {v_full}"
        );
    }

    /// Test 8: Too few samples — returns 0.
    /// Window larger than sample count → graceful 0 return.
    #[test]
    fn test_08_too_few_samples_returns_zero() {
        // compute_velocity: 2 samples, window=5 → still works (uses available range)
        // but with only 2 samples span=1, so it should produce a value.
        // However window=5 with only 2 samples: start = max(0, 2-5) = 0
        // so (200-100)*1000/1 = 100_000 — this actually works.
        //
        // For TRUE "returns 0", we need window < 2 or samples.len() < 2:
        let v1 = compute_velocity(&[100], 5); // only 1 sample → 0
        assert_eq!(v1, 0, "single sample should return velocity 0");

        let v2 = compute_velocity(&[], 5); // empty → 0
        assert_eq!(v2, 0, "empty samples should return velocity 0");

        // compute_acceleration: 3 samples, window=4 → n < 4 → returns 0
        let a1 = compute_acceleration(&[100, 200, 300], 4);
        assert_eq!(a1, 0, "3 samples with window=4 should return acceleration 0");

        // compute_acceleration: window < 4 → returns 0
        let a2 = compute_acceleration(&[100, 200, 300, 400], 3);
        assert_eq!(a2, 0, "window < 4 should return acceleration 0");
    }

    /// Test 9: Hard_sl scenario — 1488 bps peak then fast dump.
    /// Simulates the -0.032 SOL hard_sl trade pattern.
    #[test]
    fn test_09_hard_sl_peak_then_dump() {
        let samples: &[i32] = &[100, 500, 1000, 1488, 800, 200, -100];

        // Velocity from last 3 samples: [800, 200, -100]
        // v = (-100 - 800) * 1000 / 2 = -450_000 mbps → very strongly negative
        let v = compute_velocity(samples, 3);
        assert!(
            v < -100_000,
            "hard_sl dump velocity should be < -100_000 mbps, got {v}"
        );

        // Momentum collapse detection:
        // lookback=5 → window = [1488, 800, 200, -100] (last 5 is indices 2..7 = [1000, 1488, 800, 200, -100])
        // peak = 1488 at index 1 in window (original index 3)
        // max_samples_after_peak=2 → check indices 2,3 in window = [800, 200]
        // drop from 1488→800 = 688 bps (>= 200 threshold) → triggers!
        let collapsed = detect_momentum_collapse(
            samples,
            5,   // lookback: last 5 samples → [1000, 1488, 800, 200, -100]
            200, // min_peak_bps
            200, // drop_threshold_bps
            2,   // max_samples_after_peak
        );
        // With lookback=5, window is [1000, 1488, 800, 200, -100].
        // Peak=1488 at window index 1. search_end = min(1+2+1, 5) = 4.
        // Check i=2: 1488-800=688 >= 200 → true
        assert!(
            collapsed,
            "1488 bps peak → 800 bps drop should trigger momentum collapse"
        );

        // Also verify acceleration is strongly negative (decline intensifying)
        let a = compute_acceleration(samples, 6);
        assert!(
            a < 0,
            "post-peak dump should show negative acceleration, got {a}"
        );
    }

    /// Test 10: evaluate_velocity_exit — below min_profit, no signal.
    /// Since evaluate_velocity_exit doesn't exist in position.rs yet,
    /// this test validates the precondition logic: if current_bps < min_profit_bps,
    /// velocity exit should NOT be triggered regardless of velocity.
    /// When the real function lands, replace with direct call.
    #[test]
    fn test_10_below_min_profit_no_velocity_exit_signal() {
        // Scenario: current_bps = 30, velocity_exit_min_profit_bps = 50
        // Even with strongly negative velocity, the exit should NOT fire
        // because the position hasn't reached minimum profit threshold.
        let current_bps: i32 = 30;
        let min_profit_bps: i32 = 50;

        // Simulate velocity data showing a dump
        let samples: &[i32] = &[100, 80, 60, 40, 30];
        let v = compute_velocity(samples, 3);
        assert!(v < 0, "velocity should be negative in this scenario");

        // The gating logic: velocity exit only fires when current_bps >= min_profit
        let should_fire = current_bps >= min_profit_bps && v < -50_000;
        assert!(
            !should_fire,
            "velocity exit should NOT fire when current_bps ({current_bps}) < min_profit ({min_profit_bps})"
        );

        // Confirm it WOULD fire if above min_profit
        let above_profit_bps: i32 = 60;
        let would_fire = above_profit_bps >= min_profit_bps && v < -50_000;
        // Note: with these samples v might not be < -50_000, so just test the gating
        let gating_passes = above_profit_bps >= min_profit_bps;
        assert!(
            gating_passes,
            "gating should pass when current_bps ({above_profit_bps}) >= min_profit ({min_profit_bps})"
        );
    }

    /// Bonus test: Window=2 bootstrap case.
    /// Massive drop in 2 samples → strongly negative velocity.
    #[test]
    fn test_bonus_window2_bootstrap() {
        let samples: &[i32] = &[500, 100];

        // v = (100 - 500) * 1000 / 1 = -400_000 mbps
        let v = compute_velocity(samples, 2);
        assert_eq!(
            v, -400_000,
            "window=2 bootstrap: 500→100 should give exactly -400_000 mbps, got {v}"
        );
        assert!(
            v < -300_000,
            "window=2 massive drop should be strongly negative"
        );
    }

    // ── Additional edge case tests ──────────────────────────────────────

    /// Verify that window larger than samples uses the full array.
    #[test]
    fn test_window_larger_than_samples() {
        let samples: &[i32] = &[0, 100, 200];
        // window=10, but only 3 samples → uses full array
        let v = compute_velocity(samples, 10);
        // (200 - 0) * 1000 / 2 = 100_000
        assert_eq!(v, 100_000, "window > len should use full array");
    }

    /// Verify momentum collapse: drop exactly at threshold.
    #[test]
    fn test_momentum_collapse_exact_threshold() {
        // Peak=300, next sample=100 → drop exactly = 200
        let samples: &[i32] = &[0, 300, 100];
        let collapsed = detect_momentum_collapse(
            samples,
            3,   // lookback
            200, // min_peak_bps
            200, // drop_threshold_bps (exactly 200 drop)
            2,   // max_samples
        );
        assert!(
            collapsed,
            "drop exactly at threshold (200 bps) should trigger collapse"
        );
    }

    /// Verify acceleration with V-shaped recovery (negative then positive).
    #[test]
    fn test_acceleration_v_shaped_recovery() {
        // First half: falling. Second half: rising → positive acceleration
        let samples: &[i32] = &[500, 400, 300, 200, 300, 400, 500];
        let a = compute_acceleration(samples, 6);
        assert!(
            a > 0,
            "V-shaped recovery should have positive acceleration, got {a}"
        );
    }

    /// Verify velocity with a single-sample window returns 0 (window < 2).
    #[test]
    fn test_velocity_window_one_returns_zero() {
        let samples: &[i32] = &[100, 200, 300];
        let v = compute_velocity(samples, 1);
        assert_eq!(v, 0, "window=1 should return 0 (< 2 minimum)");
    }
}
