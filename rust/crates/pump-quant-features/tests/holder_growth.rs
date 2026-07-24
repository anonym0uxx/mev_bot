//! §70.1 holder-growth acceleration: happy path, small-n refusal, point-in-time
//! safety, boundary/monotonicity, and bounded-capacity churn.

use pump_quant_features::holder_growth::{
    HolderGrowthConfig, HolderGrowthTracker, HolderSample, HolderSeries, HOLDER_GROWTH_NORM_NS,
    HOLDER_MIN_INTERVAL_NS, HOLDER_MIN_SAMPLES_FOR_ACCEL, HOLDER_SERIES_CAP,
};
use pump_quant_features::types::FeatureError;

/// One second in nanoseconds — the default minimum comparison spacing.
const SEC: u64 = 1_000_000_000;
/// One minute in nanoseconds — the default normalization basis.
const MIN: u64 = 60 * SEC;

fn s(ts_ns: u64, holder_count: u64) -> HolderSample {
    HolderSample {
        ts_ns,
        holder_count,
    }
}

/// Build a series from `(ts, holders)` pairs, asserting every push is accepted.
fn series(samples: &[(u64, u64)]) -> HolderSeries {
    let mut sr = HolderSeries::new();
    for (ts, h) in samples {
        sr.push(s(*ts, *h)).expect("monotonic push accepted");
    }
    sr
}

// ---------------------------------------------------------------------------
// Happy path
// ---------------------------------------------------------------------------

#[test]
fn accelerating_holder_growth_is_positive() {
    // 100 -> 110 over one minute (+1000 bps/min), then 110 -> 143 over one
    // minute (+3000 bps/min). Acceleration = 3000 - 1000 = +2000 bps/min.
    let sr = series(&[(0, 100), (MIN, 110), (2 * MIN, 143)]);
    let e = sr
        .estimate_as_of(2 * MIN, &HolderGrowthConfig::DEFAULT)
        .expect("three well-spaced samples yield an estimate");
    assert_eq!(e.prior_growth_bps, 1_000);
    assert_eq!(e.growth_bps, 3_000);
    assert_eq!(e.accel_bps, 2_000);
    assert_eq!(e.span_ns(), 2 * MIN);
}

#[test]
fn decelerating_holder_growth_is_negative() {
    // +3000 bps/min then +1000 bps/min: growth is still positive but the
    // second derivative — the leading indicator — has already turned negative.
    let sr = series(&[(0, 100), (MIN, 130), (2 * MIN, 143)]);
    let e = sr
        .estimate_as_of(2 * MIN, &HolderGrowthConfig::DEFAULT)
        .expect("estimate available");
    assert!(e.growth_bps > 0, "rate still positive: {}", e.growth_bps);
    assert_eq!(e.prior_growth_bps, 3_000);
    assert_eq!(e.growth_bps, 1_000);
    assert_eq!(e.accel_bps, -2_000);
}

#[test]
fn constant_rate_measures_exactly_zero_acceleration() {
    // Measured-flat is a real reading of 0 — and it is reachable only through
    // `Some`, which is exactly why "no data" must never also be 0 (§6.4).
    let sr = series(&[(0, 100), (MIN, 110), (2 * MIN, 121)]);
    let e = sr
        .estimate_as_of(2 * MIN, &HolderGrowthConfig::DEFAULT)
        .expect("estimate available");
    assert_eq!(e.growth_bps, 1_000);
    assert_eq!(e.prior_growth_bps, 1_000);
    assert_eq!(e.accel_bps, 0);
}

#[test]
fn irregular_spacing_is_time_normalized() {
    // +10% over 60 s and +10% over 30 s are NOT the same rate. Normalizing to a
    // per-minute basis makes the second interval read 2000 bps/min.
    let sr = series(&[(0, 100), (MIN, 110), (MIN + 30 * SEC, 121)]);
    let e = sr
        .estimate_as_of(MIN + 30 * SEC, &HolderGrowthConfig::DEFAULT)
        .expect("estimate available");
    assert_eq!(e.prior_growth_bps, 1_000);
    assert_eq!(e.growth_bps, 2_000);
    assert_eq!(e.accel_bps, 1_000);
}

// ---------------------------------------------------------------------------
// Small-n / fail-closed refusal
// ---------------------------------------------------------------------------

#[test]
fn small_n_refuses_rather_than_returning_zero() {
    assert_eq!(HOLDER_MIN_SAMPLES_FOR_ACCEL, 3);
    let empty = HolderSeries::new();
    assert!(empty
        .estimate_as_of(u64::MAX, &HolderGrowthConfig::DEFAULT)
        .is_none());

    let one = series(&[(0, 100)]);
    assert!(one
        .estimate_as_of(u64::MAX, &HolderGrowthConfig::DEFAULT)
        .is_none());

    // Two samples: a growth RATE exists, an acceleration does not. Must refuse.
    let two = series(&[(0, 100), (MIN, 110)]);
    assert!(
        two.estimate_as_of(u64::MAX, &HolderGrowthConfig::DEFAULT)
            .is_none(),
        "a second difference needs three points"
    );

    // The third sample unlocks it — proving the refusal was the sample gate.
    let three = series(&[(0, 100), (MIN, 110), (2 * MIN, 121)]);
    assert!(three
        .estimate_as_of(u64::MAX, &HolderGrowthConfig::DEFAULT)
        .is_some());
}

#[test]
fn sub_interval_sampling_is_refused() {
    // Three samples 1 ms apart: enough points, not enough spacing.
    let sr = series(&[(0, 100), (1_000_000, 101), (2_000_000, 103)]);
    assert!(sr
        .estimate_as_of(2_000_000, &HolderGrowthConfig::DEFAULT)
        .is_none());
    // Exactly at the minimum spacing it is admitted (boundary is inclusive).
    let ok = series(&[
        (0, 100),
        (HOLDER_MIN_INTERVAL_NS, 101),
        (2 * HOLDER_MIN_INTERVAL_NS, 103),
    ]);
    assert!(ok
        .estimate_as_of(2 * HOLDER_MIN_INTERVAL_NS, &HolderGrowthConfig::DEFAULT)
        .is_some());
}

#[test]
fn stale_gap_is_refused() {
    let cfg = HolderGrowthConfig {
        min_interval_ns: SEC,
        max_interval_ns: 10 * SEC,
        norm_ns: HOLDER_GROWTH_NORM_NS,
    };
    // Middle gap is 11 s > the 10 s staleness ceiling.
    let stale = series(&[(0, 100), (11 * SEC, 110), (12 * SEC, 120)]);
    assert!(stale.estimate_as_of(12 * SEC, &cfg).is_none());
    // Same shape inside the ceiling resolves.
    let fresh = series(&[(0, 100), (9 * SEC, 110), (10 * SEC, 120)]);
    assert!(fresh.estimate_as_of(10 * SEC, &cfg).is_some());
}

#[test]
fn zero_base_holder_count_is_undefined_not_infinite() {
    // A relative growth rate off a zero base has no value; refuse it.
    let sr = series(&[(0, 0), (MIN, 10), (2 * MIN, 20)]);
    assert!(sr
        .estimate_as_of(2 * MIN, &HolderGrowthConfig::DEFAULT)
        .is_none());
}

#[test]
fn invalid_config_refuses() {
    let bad_norm = HolderGrowthConfig {
        norm_ns: 0,
        ..HolderGrowthConfig::DEFAULT
    };
    assert!(!bad_norm.is_valid());
    let sr = series(&[(0, 100), (MIN, 110), (2 * MIN, 121)]);
    assert!(sr.estimate_as_of(2 * MIN, &bad_norm).is_none());

    let inverted = HolderGrowthConfig {
        min_interval_ns: 10 * SEC,
        max_interval_ns: SEC,
        norm_ns: MIN,
    };
    assert!(!inverted.is_valid());
    assert!(sr.estimate_as_of(2 * MIN, &inverted).is_none());
}

// ---------------------------------------------------------------------------
// §20 point-in-time safety
// ---------------------------------------------------------------------------

#[test]
fn a_later_sample_cannot_influence_an_earlier_estimate() {
    let mut sr = series(&[(0, 100), (MIN, 110), (2 * MIN, 121)]);
    let before = sr
        .estimate_as_of(2 * MIN, &HolderGrowthConfig::DEFAULT)
        .expect("estimate at 2m");

    // A violent future observation arrives.
    sr.push(s(3 * MIN, 100_000)).expect("monotonic");
    let after = sr
        .estimate_as_of(2 * MIN, &HolderGrowthConfig::DEFAULT)
        .expect("estimate at 2m still available");

    assert_eq!(before, after, "future sample leaked into a past estimate");
    assert!(before.newest.ts_ns <= 2 * MIN);

    // And the cutoff genuinely moves the answer when it is allowed to.
    let now = sr
        .estimate_as_of(3 * MIN, &HolderGrowthConfig::DEFAULT)
        .expect("estimate at 3m");
    assert_ne!(now.accel_bps, before.accel_bps);
    assert_eq!(now.newest.ts_ns, 3 * MIN);
}

#[test]
fn every_selected_sample_is_at_or_before_the_cutoff() {
    let sr = series(&[
        (0, 100),
        (MIN, 110),
        (2 * MIN, 121),
        (3 * MIN, 200),
        (4 * MIN, 400),
    ]);
    for cutoff in [2 * MIN, 2 * MIN + 1, 3 * MIN, 4 * MIN, 10 * MIN] {
        let e = sr
            .estimate_as_of(cutoff, &HolderGrowthConfig::DEFAULT)
            .expect("enough history");
        assert!(e.newest.ts_ns <= cutoff);
        assert!(e.mid.ts_ns <= cutoff);
        assert!(e.oldest.ts_ns <= cutoff);
        assert!(e.oldest.ts_ns < e.mid.ts_ns && e.mid.ts_ns < e.newest.ts_ns);
    }
}

#[test]
fn cutoff_before_the_third_sample_refuses() {
    let sr = series(&[(0, 100), (MIN, 110), (2 * MIN, 121)]);
    // Only two samples are knowable at 1m — refuse, do not extrapolate.
    assert!(sr
        .estimate_as_of(MIN, &HolderGrowthConfig::DEFAULT)
        .is_none());
    assert!(sr
        .estimate_as_of(2 * MIN - 1, &HolderGrowthConfig::DEFAULT)
        .is_none());
    assert!(sr
        .estimate_as_of(2 * MIN, &HolderGrowthConfig::DEFAULT)
        .is_some());
}

#[test]
fn backwards_information_time_is_rejected() {
    let mut sr = HolderSeries::new();
    sr.push(s(10 * SEC, 100)).expect("first");
    let err = sr
        .push(s(9 * SEC, 100))
        .expect_err("backwards push must fail");
    assert_eq!(
        err,
        FeatureError::NonMonotonicTimestamp {
            previous_ns: 10 * SEC,
            offending_ns: 9 * SEC,
        }
    );
    assert_eq!(sr.len(), 1, "a rejected push must not mutate the series");
}

// ---------------------------------------------------------------------------
// Boundary / monotonicity
// ---------------------------------------------------------------------------

#[test]
fn acceleration_is_monotone_in_the_terminal_holder_count() {
    // Holding the first interval fixed, a larger final holder count can never
    // yield a smaller acceleration.
    let mut prev: Option<i64> = None;
    for final_holders in [110u64, 115, 121, 150, 400] {
        let sr = series(&[(0, 100), (MIN, 110), (2 * MIN, final_holders)]);
        let e = sr
            .estimate_as_of(2 * MIN, &HolderGrowthConfig::DEFAULT)
            .expect("estimate available");
        if let Some(p) = prev {
            assert!(
                e.accel_bps >= p,
                "accel {} < previous {} at final={final_holders}",
                e.accel_bps,
                p
            );
        }
        prev = Some(e.accel_bps);
    }
}

#[test]
fn shrinking_holder_base_reports_negative_rates() {
    let sr = series(&[(0, 200), (MIN, 180), (2 * MIN, 90)]);
    let e = sr
        .estimate_as_of(2 * MIN, &HolderGrowthConfig::DEFAULT)
        .expect("estimate available");
    assert_eq!(e.prior_growth_bps, -1_000);
    assert_eq!(e.growth_bps, -5_000);
    assert_eq!(e.accel_bps, -4_000);
}

#[test]
fn saturated_inputs_do_not_panic() {
    let sr = series(&[
        (0, 1),
        (HOLDER_MIN_INTERVAL_NS, u64::MAX),
        (2 * HOLDER_MIN_INTERVAL_NS, 1),
    ]);
    // Whatever it decides, it must decide it without panicking or wrapping.
    let _ = sr.estimate_as_of(u64::MAX, &HolderGrowthConfig::DEFAULT);

    let far = series(&[
        (u64::MAX - 2 * HOLDER_MIN_INTERVAL_NS, 10),
        (u64::MAX - HOLDER_MIN_INTERVAL_NS, 20),
        (u64::MAX, 40),
    ]);
    let e = far.estimate_as_of(u64::MAX, &HolderGrowthConfig::DEFAULT);
    assert!(e.is_some());
}

// ---------------------------------------------------------------------------
// Bounded capacity / churn
// ---------------------------------------------------------------------------

#[test]
fn series_ring_is_capacity_bounded_and_evicts_oldest() {
    let mut sr = HolderSeries::new();
    for i in 0..(HOLDER_SERIES_CAP as u64 * 4) {
        sr.push(s(i * SEC, 100 + i)).expect("monotonic");
    }
    assert_eq!(sr.len(), HOLDER_SERIES_CAP);
    assert_eq!(sr.capacity(), HOLDER_SERIES_CAP);
    assert_eq!(sr.dropped(), HOLDER_SERIES_CAP as u64 * 3);
    // The newest sample survived; the evicted past is genuinely gone, so an
    // as_of query into it refuses rather than answering from survivors.
    let newest = sr.at_rev(0).expect("newest present");
    assert_eq!(newest.ts_ns, (HOLDER_SERIES_CAP as u64 * 4 - 1) * SEC);
    assert!(sr
        .estimate_as_of(SEC, &HolderGrowthConfig::DEFAULT)
        .is_none());
    assert!(sr
        .estimate_as_of(newest.ts_ns, &HolderGrowthConfig::DEFAULT)
        .is_some());
}

#[test]
fn tracker_is_capacity_bounded_and_evicts_least_recently_updated() {
    let mut t = HolderGrowthTracker::with_capacity(2);
    // Mint 1 last updated at 1s, mint 2 at 5s.
    t.push(1, s(SEC, 100)).expect("push");
    t.push(2, s(5 * SEC, 100)).expect("push");
    assert_eq!(t.len(), 2);

    // Mint 3 arrives: mint 1 (oldest last_ts) must be the victim.
    t.push(3, s(6 * SEC, 100)).expect("push");
    assert_eq!(t.len(), 2);
    assert_eq!(t.evictions(), 1);
    assert!(t.series(1).is_none(), "least-recently-updated mint evicted");
    assert!(t.series(2).is_some());
    assert!(t.series(3).is_some());
}

#[test]
fn tracker_churn_never_exceeds_capacity() {
    let mut t = HolderGrowthTracker::with_capacity(8);
    for i in 0..500u64 {
        t.push(i, s(i * SEC, 100 + i)).expect("push");
        assert!(t.len() <= 8, "capacity breached at {i}");
    }
    assert_eq!(t.len(), 8);
    assert_eq!(t.evictions(), 500 - 8);
}

#[test]
fn tracker_keeps_per_mint_series_independent_and_answers_point_in_time() {
    let mut t = HolderGrowthTracker::with_capacity(4);
    for (mint, base) in [(7u64, 100u64), (9, 1_000)] {
        t.push(mint, s(0, base)).expect("push");
        t.push(mint, s(MIN, base + base / 10)).expect("push");
        t.push(
            mint,
            s(2 * MIN, base + base / 10 + (base + base / 10) * 3 / 10),
        )
        .expect("push");
    }
    let a = t
        .estimate_as_of(7, 2 * MIN, &HolderGrowthConfig::DEFAULT)
        .expect("mint 7 estimate");
    let b = t
        .estimate_as_of(9, 2 * MIN, &HolderGrowthConfig::DEFAULT)
        .expect("mint 9 estimate");
    assert_eq!(a.accel_bps, b.accel_bps, "same shape, same acceleration");
    assert!(t
        .estimate_as_of(11, 2 * MIN, &HolderGrowthConfig::DEFAULT)
        .is_none());
    // Point-in-time through the tracker too.
    assert!(t
        .estimate_as_of(7, MIN, &HolderGrowthConfig::DEFAULT)
        .is_none());
}

#[test]
fn tracker_rejects_backwards_time_per_mint() {
    let mut t = HolderGrowthTracker::with_capacity(4);
    t.push(1, s(10 * SEC, 100)).expect("push");
    assert!(t.push(1, s(9 * SEC, 100)).is_err());
    // A different mint is unaffected by mint 1's clock.
    assert!(t.push(2, s(SEC, 100)).is_ok());
}
