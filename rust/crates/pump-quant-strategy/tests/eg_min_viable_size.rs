#![allow(
    unused_imports,
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
