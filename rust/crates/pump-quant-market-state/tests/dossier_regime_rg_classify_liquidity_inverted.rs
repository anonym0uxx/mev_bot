// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'regime' component (leaf 'rg_classify_liquidity_inverted').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_market_state::regime::*;

#[test]
fn rg_classify_liquidity_inverted() {
    let th = RegimeThresholds::default();
    let mk = |idx: u64| {
        classify(
            &RegimeObservation {
                liquidity_index: Some(idx),
                ..RegimeObservation::default()
            },
            &th,
        )
        .liquidity_regime
    };
    assert_eq!(mk(50), Some(RegimeLevel::High));
    assert_eq!(mk(100), Some(RegimeLevel::Elevated));
    assert_eq!(mk(1000), Some(RegimeLevel::Normal));
    assert_eq!(mk(10000), Some(RegimeLevel::Low));
    assert_eq!(
        classify(&RegimeObservation::default(), &th).liquidity_regime,
        None
    );
    assert_eq!(classify(&RegimeObservation::default(), &th).version, 0);
}
