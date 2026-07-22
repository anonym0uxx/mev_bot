// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'hazard' component (leaf 'hz_observe_bounded').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(
    unused_imports,
    dead_code,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_simulator::hazard::*;

#[test]
fn hz_observe_bounded_enforces_capacity_and_accumulates() {
    let mut h = PartialPooledHazard::new(1_000, 3, 2);
    assert!(h.observe(1, 1, 10).is_ok());
    assert!(h.observe(2, 1, 10).is_ok());
    assert_eq!(h.phase_count(), 2);
    // A third NEW phase exceeds the cap: explicit error, no silent drop.
    assert_eq!(h.observe(3, 1, 10), Err(HazardError::PhaseCapacityExceeded));
    assert_eq!(h.phase_count(), 2);
    // Accumulating into an existing phase is allowed even when full.
    assert!(h.observe(1, 2, 5).is_ok());
    let e = h.estimate(1);
    assert_eq!(e.events, 3);
    assert_eq!(e.trials, 15);
    // max_phases is clamped to >= 1.
    let mut h2 = PartialPooledHazard::new(0, 1, 0);
    assert!(h2.observe(7, 0, 0).is_ok());
    assert_eq!(h2.observe(8, 0, 0), Err(HazardError::PhaseCapacityExceeded));
    assert_eq!(h2.phase_count(), 1);
}
