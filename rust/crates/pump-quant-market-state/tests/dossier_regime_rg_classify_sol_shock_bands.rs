// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'regime' component (leaf 'rg_classify_sol_shock_bands').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_market_state::regime::*;

#[test]
fn rg_classify_sol_shock_bands() {
    let th = RegimeThresholds::default();
    let mk = |bps: i64| {
        classify(
            &RegimeObservation {
                sol_price_change_bps: Some(bps),
                ..RegimeObservation::default()
            },
            &th,
        )
        .sol_price_shock
    };
    assert_eq!(mk(-1000), Some(Skew::StrongDown));
    assert_eq!(mk(-999), Some(Skew::Down));
    assert_eq!(mk(-300), Some(Skew::Down));
    assert_eq!(mk(-299), Some(Skew::Neutral));
    assert_eq!(mk(0), Some(Skew::Neutral));
    assert_eq!(mk(299), Some(Skew::Neutral));
    assert_eq!(mk(300), Some(Skew::Up));
    assert_eq!(mk(999), Some(Skew::Up));
    assert_eq!(mk(1000), Some(Skew::StrongUp));
    assert_eq!(
        classify(&RegimeObservation::default(), &th).sol_price_shock,
        None
    );
}
