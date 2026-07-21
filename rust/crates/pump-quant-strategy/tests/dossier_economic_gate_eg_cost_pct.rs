// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'economic_gate' component (leaf 'eg_cost_pct').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports)]
use pump_quant_strategy::economic_gate::*;

#[test]
fn prop_cost_is_u_shaped() {
    let c = ImpactCurve::linear_test(1_000);
    let small = round_trip_cost_bps(10_000, 160, 200, &c).unwrap();
    let mid = round_trip_cost_bps(50_000, 160, 200, &c).unwrap();
    let large = round_trip_cost_bps(500_000, 160, 200, &c).unwrap();
    assert!(
        mid < small,
        "cost falls from tiny size (fixed cost amortizes)"
    );
    assert!(large > mid, "cost rises at large size (impact dominates)");
    assert_eq!(round_trip_cost_bps(0, 160, 200, &c), None);
}
