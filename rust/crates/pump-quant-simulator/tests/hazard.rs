//! Leaf tests for `hazard`: partial-pooled, phase-separated hazard estimator (§48).

use pump_quant_simulator::hazard::{HazardError, PartialPooledHazard};

#[test]
fn global_rate_and_shrinkage_hand_computed() {
    // prior 2000 bps, pooling strength k=10.
    let mut h = PartialPooledHazard::new(2_000, 10, 8);
    h.observe(1, 1, 2).unwrap(); // phase 1: 1/2
    h.observe(2, 8, 10).unwrap(); // phase 2: 8/10

    // global = (1+8)*10000/(2+10... no) -> total events 9, total trials 12 -> 7500.
    assert_eq!(h.global_bps(), 7_500);

    // phase 1: (1*10000 + 10*7500)/(2+10) = 85000/12 = 7083 (floor).
    let e1 = h.estimate(1);
    assert_eq!(e1.hazard_bps, 7_083);
    assert_eq!(e1.trials, 2);
    assert_eq!(e1.events, 1);

    // phase 2: (8*10000 + 10*7500)/(10+10) = 155000/20 = 7750.
    assert_eq!(h.estimate(2).hazard_bps, 7_750);

    // Unseen phase collapses to the global rate (fully pooled).
    assert_eq!(h.estimate(99).hazard_bps, 7_500);

    assert_eq!(h.phase_count(), 2);
}

#[test]
fn no_data_falls_back_to_prior() {
    let h = PartialPooledHazard::new(3_000, 10, 4);
    assert_eq!(h.global_bps(), 3_000);
    // With no trials, every phase estimate equals the prior.
    assert_eq!(h.estimate(1).hazard_bps, 3_000);
}

#[test]
fn zero_pooling_with_no_phase_data_uses_global() {
    // k = 0 and an unseen phase would give 0/0; the estimator falls back to global.
    let h = PartialPooledHazard::new(4_000, 0, 4);
    assert_eq!(h.estimate(7).hazard_bps, 4_000);
}

#[test]
fn observations_accumulate_within_a_phase() {
    let mut h = PartialPooledHazard::new(0, 5, 4);
    h.observe(1, 1, 2).unwrap();
    h.observe(1, 2, 3).unwrap(); // phase 1 now 3/5
    let e = h.estimate(1);
    assert_eq!(e.events, 3);
    assert_eq!(e.trials, 5);
    // global = 3*10000/5 = 6000 ; estimate = (3*10000 + 5*6000)/(5+5) = 60000/10 = 6000.
    assert_eq!(h.global_bps(), 6_000);
    assert_eq!(e.hazard_bps, 6_000);
}

#[test]
fn phase_capacity_is_bounded() {
    let mut h = PartialPooledHazard::new(1_000, 3, 2);
    h.observe(1, 1, 10).unwrap();
    h.observe(2, 1, 10).unwrap();
    // A third *new* phase exceeds the bound.
    assert_eq!(h.observe(3, 1, 10), Err(HazardError::PhaseCapacityExceeded));
    // But accumulating into an existing phase is always allowed.
    assert!(h.observe(1, 1, 5).is_ok());
    assert_eq!(h.phase_count(), 2);
}

#[test]
fn estimate_all_is_sorted_and_complete() {
    let mut h = PartialPooledHazard::new(1_000, 4, 8);
    h.observe(5, 2, 4).unwrap();
    h.observe(1, 0, 4).unwrap();
    h.observe(3, 4, 4).unwrap();
    let all = h.estimate_all();
    let ids: Vec<u16> = all.iter().map(|e| e.phase_id).collect();
    assert_eq!(ids, vec![1, 3, 5], "deterministic ascending phase order");
    assert_eq!(all.len(), 3);
}
