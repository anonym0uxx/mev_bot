// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'rank' component (leaf 'wr_lane_weights').
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
use pump_quant_watchlist::rank::*;

#[test]
fn wr_lane_weights_props() {
    let cs = pump_quant_watchlist::candidate::Lane::CreationSniper;
    let ec = pump_quant_watchlist::candidate::Lane::EarlyConfirmation;
    let gt = pump_quant_watchlist::candidate::Lane::GraduationTransition;
    let ams = pump_quant_watchlist::candidate::Lane::ActiveMarketScalp;

    // from_defaults seeds each lane's documented prior exactly.
    let mut w = LaneWeights::from_defaults();
    assert_eq!(w.get(cs), 8_000);
    assert_eq!(w.get(ec), 12_000);
    assert_eq!(w.get(gt), 11_000);
    assert_eq!(w.get(ams), WEIGHT_ONE);

    // set overrides exactly the targeted lane and get echoes it back.
    w.set(cs, WEIGHT_ONE);
    assert_eq!(w.get(cs), WEIGHT_ONE);
    // Every other lane is unaffected by the override.
    assert_eq!(w.get(ec), 12_000);
    assert_eq!(w.get(gt), 11_000);
    assert_eq!(w.get(ams), WEIGHT_ONE);

    // A second independent instance is unchanged (no shared state).
    let fresh = LaneWeights::from_defaults();
    assert_eq!(fresh.get(cs), 8_000);
}
