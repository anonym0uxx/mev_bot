// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'regime' component (leaf 'rg_classify_ratio_components').
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
use pump_quant_market_state::regime::*;

#[test]
fn rg_classify_ratio_components() {
    let th = RegimeThresholds::default();
    let obs = RegimeObservation {
        launches: 100,
        graduations: 20,
        ..RegimeObservation::default()
    };
    assert_eq!(classify(&obs, &th).graduation_rate, Some(RegimeLevel::High));
    let obs0 = RegimeObservation {
        launches: 0,
        graduations: 5,
        ..RegimeObservation::default()
    };
    assert_eq!(classify(&obs0, &th).graduation_rate, None);
    let obsr = RegimeObservation {
        route_attempts: 10,
        route_failures: 3,
        ..RegimeObservation::default()
    };
    assert_eq!(
        classify(&obsr, &th).route_degradation,
        Some(RegimeLevel::Elevated)
    );
    let obsr0 = RegimeObservation {
        route_attempts: 0,
        route_failures: 0,
        ..RegimeObservation::default()
    };
    assert_eq!(classify(&obsr0, &th).route_degradation, None);
    let obsrug = RegimeObservation {
        rugs: 100,
        live_markets: Some(1000),
        ..RegimeObservation::default()
    };
    assert_eq!(
        classify(&obsrug, &th).rug_collapse_rate,
        Some(RegimeLevel::Normal)
    );
    let obsrug_none = RegimeObservation {
        rugs: 100,
        live_markets: None,
        ..RegimeObservation::default()
    };
    assert_eq!(classify(&obsrug_none, &th).rug_collapse_rate, None);
    let obsrug0 = RegimeObservation {
        rugs: 5,
        live_markets: Some(0),
        ..RegimeObservation::default()
    };
    assert_eq!(classify(&obsrug0, &th).rug_collapse_rate, None);
}
