//! Leaf tests: fixed-point primitives (§22). Expectations computed by hand.

use pump_quant_social::fixedpoint::{
    clamp_bps, confidence_bps, decay_weight_bps, markout_bps, ratio_bps, weighted_mean_bps,
    BPS_SCALE,
};

#[test]
fn markout_basic_and_edges() {
    assert_eq!(markout_bps(100, 150), 5_000); // +50%
    assert_eq!(markout_bps(100, 50), -5_000); // -50%
    assert_eq!(markout_bps(100, 100), 0);
    assert_eq!(markout_bps(0, 100), 0); // divide-by-zero guard
    assert_eq!(markout_bps(100, 300), 20_000); // +200% is unclamped ground truth
}

#[test]
fn markout_monotonic_in_after() {
    // Property: for a fixed before, markout is non-decreasing in price_after.
    let before = 777u64;
    let mut prev = i64::MIN;
    for after in (0u64..=5000).step_by(37) {
        let m = markout_bps(before, after);
        assert!(m >= prev, "not monotonic at after={after}");
        prev = m;
    }
}

#[test]
fn decay_weight_known_points() {
    let hl = 1_000u64;
    assert_eq!(decay_weight_bps(0, hl), 10_000); // no age → full weight
    assert_eq!(decay_weight_bps(hl, hl), 5_000); // one half-life → half
    assert_eq!(decay_weight_bps(2 * hl, hl), 2_500); // two half-lives → quarter
                                                     // Half-way through the first half-life: base 10_000, reduce = 5_000*500/1000.
    assert_eq!(decay_weight_bps(500, hl), 7_500);
    assert_eq!(decay_weight_bps(1_000_000, hl), 0); // fully decayed (>63 hl)
    assert_eq!(decay_weight_bps(999, 0), 10_000); // hl==0 disables decay
}

#[test]
fn decay_weight_monotonic_nonincreasing() {
    // Property: weight never increases with age.
    let hl = 4_096u64;
    let mut prev = i64::MAX;
    for age in (0u64..=40_000).step_by(101) {
        let w = decay_weight_bps(age, hl);
        assert!(w <= prev, "decay increased at age={age}");
        assert!((0..=BPS_SCALE).contains(&w));
        prev = w;
    }
}

#[test]
fn confidence_curve() {
    assert_eq!(confidence_bps(0, 20), 0);
    assert_eq!(confidence_bps(20, 20), 5_000); // n == k → half
    assert_eq!(confidence_bps(60, 20), 7_500); // 10000*60/80
    assert_eq!(confidence_bps(1, 20), 476); // 10000/21 truncated
                                            // Property: monotonic non-decreasing in sample size.
    let mut prev = 0u16;
    for n in 0u32..500 {
        let c = confidence_bps(n, 30);
        assert!(c >= prev);
        assert!(c <= 10_000);
        prev = c;
    }
}

#[test]
fn weighted_mean_and_ratio() {
    assert_eq!(weighted_mean_bps(&[(100, 1), (300, 3)]), 250); // (100+900)/4
    assert_eq!(weighted_mean_bps(&[]), 0);
    assert_eq!(weighted_mean_bps(&[(500, 0), (900, 0)]), 0); // zero weight
    assert_eq!(ratio_bps(1, 4), 2_500);
    assert_eq!(ratio_bps(0, 9), 0);
    assert_eq!(ratio_bps(9, 0), 0);
    assert_eq!(clamp_bps(20_000), 10_000);
    assert_eq!(clamp_bps(-20_000), -10_000);
}

#[test]
fn weighted_mean_between_extremes() {
    // Property: a weighted mean lies within [min value, max value].
    let samples = [(100i64, 2i64), (900, 5), (400, 1), (-200, 3)];
    let m = weighted_mean_bps(&samples);
    let lo = samples.iter().map(|s| s.0).min().unwrap();
    let hi = samples.iter().map(|s| s.0).max().unwrap();
    assert!(lo <= m && m <= hi);
}
