// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'economic_gate' component (leaf 'eg_min_viable_size').
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
use pump_quant_strategy::economic_gate::*;

#[test]
fn prop_min_viable_refuses_when_fixed_dominates() {
    let c = ImpactCurve::linear_test(1_000);
    // generous move: 400 bps expected, fixed 160 lamports, protocol 200, margin 50
    let x = min_viable_size(400, 160, 200, 50, &c, 10_000_000).unwrap();
    // at x, cost+margin must be covered; just below x it must NOT be
    let cost_at = round_trip_cost_bps(x, 160, 200, &c).unwrap();
    assert!(400 >= cost_at + 50);
    // a move smaller than protocol+margin can never clear at any size -> None
    assert_eq!(min_viable_size(210, 160, 200, 50, &c, 10_000_000), None);
}
