// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'scalp_position' component (leaf 'sp_landing_eval').
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
use pump_quant_strategy::scalp_position::*;

#[test]
fn prop_landing_is_adverse() {
    let p = 1_000_000u64;
    let buy = expected_landing(p, Side::Buy, 50, 30).unwrap();
    let sell = expected_landing(p, Side::Sell, 50, 30).unwrap();
    assert!(buy >= p && sell <= p);
    assert_eq!(expected_landing(p, Side::Buy, 0, 0).unwrap(), p);
    assert!(
        expected_landing(u64::MAX, Side::Buy, 10_000, 10_000).is_none()
            || expected_landing(u64::MAX, Side::Buy, 10_000, 10_000).unwrap() >= u64::MAX / 2
    );
}
