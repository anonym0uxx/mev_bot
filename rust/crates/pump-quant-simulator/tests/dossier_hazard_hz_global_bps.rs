// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'hazard' component (leaf 'hz_global_bps').
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
fn hz_global_bps_pooled_rate_and_prior_fallback() {
    // No trials -> prior rate.
    let empty = PartialPooledHazard::new(2_500, 10, 8);
    assert_eq!(empty.global_bps(), 2_500);
    // Prior is clamped to BPS_ONE at construction.
    let clamped = PartialPooledHazard::new(50_000, 10, 8);
    assert_eq!(clamped.global_bps(), 10_000);
    // Pooled rate = total_events * 10000 / total_trials.
    let mut h = PartialPooledHazard::new(0, 10, 8);
    h.observe(1, 1, 2).unwrap();
    h.observe(2, 8, 10).unwrap();
    assert_eq!(h.global_bps(), 7_500); // 9*10000/12
                                       // Events exceeding trials cap the rate at BPS_ONE.
    let mut hi = PartialPooledHazard::new(0, 10, 8);
    hi.observe(1, 20, 10).unwrap();
    assert_eq!(hi.global_bps(), 10_000);
}
