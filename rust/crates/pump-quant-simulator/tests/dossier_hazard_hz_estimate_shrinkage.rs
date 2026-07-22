// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'hazard' component (leaf 'hz_estimate_shrinkage').
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
fn hz_estimate_partial_pool_shrinkage() {
    let mut h = PartialPooledHazard::new(2_000, 10, 8);
    h.observe(1, 1, 2).unwrap();
    h.observe(2, 8, 10).unwrap();
    // global = 7500
    // phase 1: (1*10000 + 10*7500)/(2+10) = 85000/12 = 7083 (floor)
    let e1 = h.estimate(1);
    assert_eq!(e1.hazard_bps, 7_083);
    assert_eq!(e1.events, 1);
    assert_eq!(e1.trials, 2);
    assert_eq!(e1.phase_id, 1);
    // phase 2: (8*10000 + 10*7500)/(10+10) = 155000/20 = 7750
    assert_eq!(h.estimate(2).hazard_bps, 7_750);
    // Unseen phase collapses to the global rate with zero counts.
    let u = h.estimate(99);
    assert_eq!(u.hazard_bps, 7_500);
    assert_eq!(u.events, 0);
    assert_eq!(u.trials, 0);
    // k = 0 with an unseen phase is 0/0 -> falls back to global (= prior here).
    let z = PartialPooledHazard::new(4_000, 0, 4);
    assert_eq!(z.estimate(7).hazard_bps, 4_000);
}
