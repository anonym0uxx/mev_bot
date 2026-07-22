// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'economic_gate' component (leaf 'eg_effective_fixed').
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
fn prop_effective_fixed_multiplier() {
    assert_eq!(effective_fixed_lamports(100, 0), Some(100));
    // 26.8% failure -> multiplier ~1.366 -> ~136
    let e = effective_fixed_lamports(100, 2680).unwrap();
    assert!(e >= 136 && e <= 137);
    // monotone: higher failure rate -> higher effective fixed
    assert!(
        effective_fixed_lamports(100, 5000).unwrap() > effective_fixed_lamports(100, 2680).unwrap()
    );
    assert_eq!(effective_fixed_lamports(100, 10_000), None);
}
