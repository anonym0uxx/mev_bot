// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'fixedpoint' component (leaf 'fp_mul_div').
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
use pump_quant_core::fixedpoint::*;

#[test]
fn prop_mul_div_matches_reference() {
    assert_eq!(mul_div_u128(6, 7, 3), Some(14));
    assert_eq!(mul_div_u128(10, 10, 0), None);
    assert_eq!(mul_div_u128(u128::MAX, 2, 1), None); // overflow -> None
    assert_eq!(mul_div_u128(100, 0, 5), Some(0));
}
