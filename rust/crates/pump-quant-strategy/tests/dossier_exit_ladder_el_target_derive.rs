// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'exit_ladder' component (leaf 'el_target_derive').
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
use pump_quant_strategy::exit_ladder::*;

#[test]
fn prop_target_above_floor_or_inadmissible() {
    assert_eq!(derive_target_bps(200, 100, Some(500)), Some(300));
    assert_eq!(derive_target_bps(200, 100, Some(250)), None); // MFE can't pay the floor
    assert_eq!(derive_target_bps(200, 100, None), Some(300));
    assert_eq!(derive_target_bps(u32::MAX, 1, None), None); // overflow
}
