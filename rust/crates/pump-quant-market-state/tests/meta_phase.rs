//! §21.4 meta lifecycle phase / decay detection: happy path, small-n refusal,
//! point-in-time safety, boundary/monotonicity, and bounded-capacity churn.

use pump_quant_market_state::common::Completeness;
use pump_quant_market_state::meta_phase::{
    classify_phase, MetaPhase, MetaPhaseThresholds, MetaPhaseTracker, MetaSample, MetaSampleWrite,
    META_PHASE_MIN_FALLING_MEASURES, META_PHASE_MIN_SAMPLES, META_PHASE_SERIES_CAP,
};

const CAT: u64 = 42;

fn th() -> MetaPhaseThresholds {
    MetaPhaseThresholds {
        max_categories: 4,
        ..MetaPhaseThresholds::DEFAULT
    }
}

fn sample(slot: u64, participation: u64, attention: u64, realized_outcome_bps: i64) -> MetaSample {
    MetaSample {
        slot,
        participation,
        attention,
        realized_outcome_bps,
    }
}

fn tracker(samples: &[MetaSample]) -> MetaPhaseTracker {
    let mut t = MetaPhaseTracker::new(th());
    for s in samples {
        assert_eq!(t.record(CAT, *s), MetaSampleWrite::Recorded);
    }
    t
}

// ---------------------------------------------------------------------------
// Happy path — Decaying becomes reachable
// ---------------------------------------------------------------------------

#[test]
fn a_meta_rolling_over_from_its_peak_is_decaying() {
    // Participation 100 -> peak 120 -> 60 (50% off peak); attention likewise;
    // realized outcome 800 -> peak 900 -> -400.
    let t = tracker(&[
        sample(1, 100, 1_000, 500),
        sample(2, 120, 1_200, 900),
        sample(3, 90, 900, 100),
        sample(4, 60, 600, -400),
    ]);
    let e = t.estimate_as_of(CAT, 4).expect("four samples");
    assert_eq!(e.phase, Some(MetaPhase::Decaying));
    assert_eq!(e.falling_measures, 3);
    assert_eq!(e.participation_decline_bps, 5_000);
    assert_eq!(e.attention_decline_bps, 5_000);
    assert_eq!(e.outcome_drop_bps, 1_300);
    assert_eq!(e.peak_participation_slot, 2);
    assert_eq!(e.peak_attention_slot, 2);
    assert_eq!(e.latest_slot, 4);
}

#[test]
fn broad_and_rising_is_hot_broad_and_flat_is_saturated() {
    let hot = tracker(&[
        sample(1, 30, 1_000, 100),
        sample(2, 40, 1_500, 200),
        sample(3, 50, 2_000, 300),
    ]);
    assert_eq!(hot.phase_as_of(CAT, 3), Some(MetaPhase::Hot));

    let saturated = tracker(&[
        sample(1, 30, 1_000, 100),
        sample(2, 40, 1_010, 100),
        sample(3, 50, 1_000, 100),
    ]);
    assert_eq!(saturated.phase_as_of(CAT, 3), Some(MetaPhase::Saturated));
}

#[test]
fn narrow_and_rising_is_emerging() {
    let t = tracker(&[
        sample(1, 2, 100, 0),
        sample(2, 4, 300, 50),
        sample(3, 6, 900, 100),
    ]);
    let e = t.estimate_as_of(CAT, 3).expect("estimate");
    assert_eq!(e.phase, Some(MetaPhase::Emerging));
    assert_eq!(e.attention_change_bps, 80_000);
    assert_eq!(e.falling_measures, 0);
}

#[test]
fn decay_outranks_broad_participation() {
    // Still broad in absolute terms (60 participants) but rolled over hard: the
    // ordinal phase must be Decaying, not Hot/Saturated.
    let t = tracker(&[
        sample(1, 200, 5_000, 900),
        sample(2, 150, 3_000, 400),
        sample(3, 60, 1_000, -200),
    ]);
    let e = t.estimate_as_of(CAT, 3).expect("estimate");
    assert_eq!(e.phase, Some(MetaPhase::Decaying));
    assert!(e.latest_participation >= th().broad_participation);
}

#[test]
fn phase_ordinals_are_stable_and_round_trip() {
    for (p, o) in [
        (MetaPhase::Emerging, 0u8),
        (MetaPhase::Hot, 1),
        (MetaPhase::Saturated, 2),
        (MetaPhase::Decaying, 3),
    ] {
        assert_eq!(p.ordinal(), o);
        assert_eq!(MetaPhase::from_ordinal(o), Some(p));
    }
    assert_eq!(MetaPhase::from_ordinal(4), None);
}

// ---------------------------------------------------------------------------
// Small-n / fail-closed refusal
// ---------------------------------------------------------------------------

#[test]
fn small_n_yields_no_estimate_at_all() {
    assert_eq!(META_PHASE_MIN_SAMPLES, 3);
    let t = MetaPhaseTracker::new(th());
    assert!(t.estimate_as_of(CAT, u64::MAX).is_none(), "untracked");

    let one = tracker(&[sample(1, 100, 1_000, 0)]);
    assert!(one.estimate_as_of(CAT, u64::MAX).is_none());

    let two = tracker(&[sample(1, 100, 1_000, 0), sample(2, 50, 500, -500)]);
    assert!(
        two.estimate_as_of(CAT, u64::MAX).is_none(),
        "a peak-and-decline trajectory needs three points"
    );

    let three = tracker(&[
        sample(1, 100, 1_000, 0),
        sample(2, 120, 1_200, 100),
        sample(3, 50, 500, -500),
    ]);
    assert!(three.estimate_as_of(CAT, u64::MAX).is_some());
}

#[test]
fn an_unnameable_state_carries_no_phase() {
    // Narrow, flat, not falling: the measures name no lifecycle position, so the
    // estimate exists (inspectable) but its phase is None (§6.4).
    let t = tracker(&[
        sample(1, 3, 1_000, 0),
        sample(2, 3, 1_000, 0),
        sample(3, 3, 1_000, 0),
    ]);
    let e = t.estimate_as_of(CAT, 3).expect("estimate exists");
    assert_eq!(e.phase, None, "quiet-and-flat is not 'emerging'");
    assert_eq!(e.falling_measures, 0);
    assert_eq!(e.attention_change_bps, 0);
    assert_eq!(t.phase_as_of(CAT, 3), None);
}

#[test]
fn a_decline_under_the_gate_is_not_decay() {
    // 10% off peak on both magnitudes: below the 25% named threshold.
    let t = tracker(&[
        sample(1, 90, 900, 0),
        sample(2, 100, 1_000, 0),
        sample(3, 90, 900, 0),
    ]);
    let e = t.estimate_as_of(CAT, 3).expect("estimate");
    assert_eq!(e.participation_decline_bps, 1_000);
    assert_eq!(e.falling_measures, 0);
    assert_ne!(e.phase, Some(MetaPhase::Decaying));
}

#[test]
fn one_falling_measure_is_below_the_quorum() {
    assert_eq!(META_PHASE_MIN_FALLING_MEASURES, 2);
    // Only attention rolls over; participation and outcome hold.
    let t = tracker(&[
        sample(1, 40, 1_000, 100),
        sample(2, 40, 2_000, 100),
        sample(3, 40, 500, 100),
    ]);
    let e = t.estimate_as_of(CAT, 3).expect("estimate");
    assert_eq!(e.falling_measures, 1);
    assert_ne!(e.phase, Some(MetaPhase::Decaying));
}

#[test]
fn invalid_thresholds_refuse_entirely() {
    let window = [
        sample(1, 100, 1_000, 500),
        sample(2, 120, 1_200, 900),
        sample(3, 40, 400, -400),
    ];
    let good = MetaPhaseThresholds::DEFAULT;
    assert!(classify_phase(&window, &good, Completeness::Complete).is_some());

    for bad in [
        MetaPhaseThresholds {
            min_samples: 2,
            ..good
        },
        MetaPhaseThresholds {
            min_falling_measures: 0,
            ..good
        },
        MetaPhaseThresholds {
            attention_flat_band_bps: -1,
            ..good
        },
        MetaPhaseThresholds { window: 2, ..good },
    ] {
        assert!(!bad.is_valid());
        assert!(classify_phase(&window, &bad, Completeness::Complete).is_none());
    }
}

// ---------------------------------------------------------------------------
// §20 point-in-time safety
// ---------------------------------------------------------------------------

#[test]
fn a_later_sample_cannot_influence_an_earlier_estimate() {
    let mut t = tracker(&[
        sample(1, 10, 100, 0),
        sample(2, 20, 300, 100),
        sample(3, 30, 900, 200),
    ]);
    let before = t.estimate_as_of(CAT, 3).expect("estimate at slot 3");
    assert_eq!(before.phase, Some(MetaPhase::Hot));

    // The meta collapses at slot 4.
    assert_eq!(
        t.record(CAT, sample(4, 1, 10, -900)),
        MetaSampleWrite::Recorded
    );
    let after = t.estimate_as_of(CAT, 3).expect("estimate at slot 3 still");
    assert_eq!(
        before, after,
        "a slot-4 collapse leaked into the slot-3 view"
    );
    assert_eq!(after.latest_slot, 3);

    // And at slot 4 the collapse is visible.
    assert_eq!(t.phase_as_of(CAT, 4), Some(MetaPhase::Decaying));
}

#[test]
fn every_sample_used_is_at_or_before_the_cutoff() {
    let t = tracker(&[
        sample(10, 10, 100, 0),
        sample(20, 20, 300, 100),
        sample(30, 30, 900, 200),
        sample(40, 5, 50, -500),
    ]);
    for cutoff in [30u64, 35, 40, 1_000] {
        let e = t.estimate_as_of(CAT, cutoff).expect("enough history");
        assert!(e.latest_slot <= cutoff);
        assert!(e.peak_participation_slot <= cutoff);
        assert!(e.peak_attention_slot <= cutoff);
    }
    // Below the sample floor at the cutoff: no estimate.
    assert!(t.estimate_as_of(CAT, 29).is_none());
    assert!(t.estimate_as_of(CAT, 9).is_none());
}

#[test]
fn backwards_slots_are_refused_and_do_not_mutate_the_series() {
    let mut t = tracker(&[sample(10, 10, 100, 0), sample(20, 20, 200, 0)]);
    assert_eq!(
        t.record(CAT, sample(9, 999, 999, 999)),
        MetaSampleWrite::NonMonotonic
    );
    assert_eq!(t.samples(CAT).expect("tracked").len(), 2);
    // Equal slots are a legitimate restatement of the same instant.
    assert_eq!(
        t.record(CAT, sample(20, 21, 210, 0)),
        MetaSampleWrite::Recorded
    );
    assert_eq!(t.samples(CAT).expect("tracked").len(), 3);
}

// ---------------------------------------------------------------------------
// Boundary / monotonicity
// ---------------------------------------------------------------------------

#[test]
fn decline_gate_boundary_is_inclusive() {
    // Exactly 25% off peak on participation and attention = the threshold.
    let at = tracker(&[
        sample(1, 100, 1_000, 0),
        sample(2, 100, 1_000, 0),
        sample(3, 75, 750, 0),
    ]);
    let e = at.estimate_as_of(CAT, 3).expect("estimate");
    assert_eq!(e.participation_decline_bps, 2_500);
    assert_eq!(e.falling_measures, 2);
    assert_eq!(e.phase, Some(MetaPhase::Decaying));

    // One unit less of decline drops below the gate on both measures.
    let below = tracker(&[
        sample(1, 100, 1_000, 0),
        sample(2, 100, 1_000, 0),
        sample(3, 76, 760, 0),
    ]);
    let e = below.estimate_as_of(CAT, 3).expect("estimate");
    assert_eq!(e.falling_measures, 0);
    assert_ne!(e.phase, Some(MetaPhase::Decaying));
}

#[test]
fn a_plateau_at_the_peak_is_not_falling() {
    // The maximum is taken at its LATEST occurrence, so a flat top does not read
    // as a roll-over.
    let t = tracker(&[
        sample(1, 100, 1_000, 100),
        sample(2, 100, 1_000, 100),
        sample(3, 100, 1_000, 100),
    ]);
    let e = t.estimate_as_of(CAT, 3).expect("estimate");
    assert_eq!(e.falling_measures, 0);
    assert_eq!(e.participation_decline_bps, 0);
    assert_eq!(e.phase, Some(MetaPhase::Saturated));
}

#[test]
fn deeper_declines_never_reduce_the_falling_count() {
    let mut prev = 0u8;
    for latest in [100u64, 90, 75, 50, 10, 0] {
        let t = tracker(&[
            sample(1, 100, 1_000, 0),
            sample(2, 100, 1_000, 0),
            sample(3, latest, latest * 10, 0),
        ]);
        let e = t.estimate_as_of(CAT, 3).expect("estimate");
        assert!(
            e.falling_measures >= prev,
            "falling count regressed at latest={latest}"
        );
        prev = e.falling_measures;
    }
    assert_eq!(prev, 2);
}

#[test]
fn saturated_inputs_do_not_panic_or_wrap() {
    let t = tracker(&[
        sample(0, u64::MAX, u64::MAX, i64::MAX),
        sample(1, 0, 0, i64::MIN),
        sample(2, u64::MAX, u64::MAX, i64::MAX),
        sample(3, 0, 0, i64::MIN),
    ]);
    let e = t.estimate_as_of(CAT, u64::MAX).expect("estimate");
    assert_eq!(e.phase, Some(MetaPhase::Decaying));
    assert_eq!(e.participation_decline_bps, 10_000);
    assert_eq!(e.outcome_drop_bps, u64::MAX);
}

#[test]
fn attention_rising_from_zero_saturates_rather_than_dividing_by_zero() {
    let t = tracker(&[
        sample(1, 2, 0, 0),
        sample(2, 3, 500, 0),
        sample(3, 4, 900, 0),
    ]);
    let e = t.estimate_as_of(CAT, 3).expect("estimate");
    assert_eq!(e.attention_change_bps, i64::MAX);
    assert_eq!(e.phase, Some(MetaPhase::Emerging));
}

// ---------------------------------------------------------------------------
// Bounded capacity / churn
// ---------------------------------------------------------------------------

#[test]
fn series_ring_is_bounded_and_marks_the_peak_a_lower_bound() {
    let mut t = MetaPhaseTracker::new(th());
    for i in 0..(META_PHASE_SERIES_CAP as u64 * 3) {
        assert_eq!(
            t.record(CAT, sample(i, 10 + i, 100 + i, 0)),
            MetaSampleWrite::Recorded
        );
    }
    assert_eq!(
        t.samples(CAT).expect("tracked").len(),
        META_PHASE_SERIES_CAP
    );
    assert_eq!(t.dropped_samples(CAT), META_PHASE_SERIES_CAP as u64 * 2);
    let e = t
        .estimate_as_of(CAT, META_PHASE_SERIES_CAP as u64 * 3)
        .expect("estimate");
    assert_eq!(
        e.completeness,
        Completeness::Incomplete,
        "an evicted past makes the in-window peak a lower bound"
    );
    // The evicted past is genuinely gone: an as_of query into it refuses.
    assert!(t.estimate_as_of(CAT, 1).is_none());
}

#[test]
fn category_capacity_is_bounded_and_refuses_rather_than_displacing() {
    let mut t = MetaPhaseTracker::new(MetaPhaseThresholds {
        max_categories: 2,
        ..MetaPhaseThresholds::DEFAULT
    });
    assert_eq!(
        t.record(1, sample(1, 10, 100, 0)),
        MetaSampleWrite::Recorded
    );
    assert_eq!(
        t.record(2, sample(1, 10, 100, 0)),
        MetaSampleWrite::Recorded
    );
    assert_eq!(
        t.record(3, sample(1, 10, 100, 0)),
        MetaSampleWrite::AtCapacity
    );
    assert_eq!(t.len(), 2);
    assert_eq!(t.completeness(), Completeness::Incomplete);
    // Tracked categories keep full fidelity.
    assert_eq!(
        t.record(1, sample(2, 20, 200, 0)),
        MetaSampleWrite::Recorded
    );
    assert_eq!(t.samples(1).expect("tracked").len(), 2);
    assert!(t.samples(3).is_none());
}

#[test]
fn churn_never_exceeds_either_bound() {
    let mut t = MetaPhaseTracker::new(MetaPhaseThresholds {
        max_categories: 4,
        ..MetaPhaseThresholds::DEFAULT
    });
    for i in 0..1_000u64 {
        t.record(i % 8, sample(i, i % 50, i % 500, 0));
        assert!(t.len() <= 4, "category bound breached at {i}");
        for c in 0..8u64 {
            if let Some(s) = t.samples(c) {
                assert!(s.len() <= META_PHASE_SERIES_CAP, "series bound breached");
            }
        }
    }
    assert_eq!(t.len(), 4);
}

#[test]
fn categories_are_independent() {
    let mut t = MetaPhaseTracker::new(th());
    for (cat, rising) in [(1u64, true), (2, false)] {
        for i in 1..=3u64 {
            let level = if rising { i * 400 } else { 2_000 / i };
            let part = if rising { i * 20 } else { 100 / i };
            t.record(cat, sample(i, part, level, 0));
        }
    }
    assert_eq!(t.phase_as_of(1, 3), Some(MetaPhase::Hot));
    assert_eq!(t.phase_as_of(2, 3), Some(MetaPhase::Decaying));
    assert!(t.estimate_as_of(3, 3).is_none());
}
