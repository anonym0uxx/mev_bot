// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'evaluator_stats' component (leaf 'ev_markout').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports)]
use pump_quant_evaluator::evaluator_stats::*;

#[test]
fn prop_markout_sign_adjustment() {
    let f =
        |side, fill: u64, later: u64| FillRow::test(FillClass::ScalpEntry, side, fill, later, 30);
    let rows = vec![f(Side::Buy, 1_000, 1_100), f(Side::Sell, 1_000, 900)];
    let m = markouts(&rows, &[30]);
    assert_eq!(m[0].n, 2);
    assert_eq!(m[0].median_bps, 1_000); // both favorable: +10% == +1000 bps
}
