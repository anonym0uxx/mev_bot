// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'scalp_position' component (leaf 'sp_redeploy_stop').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_strategy::scalp_position::*;

#[test]
fn prop_rate_comparator() {
    assert!(should_exit_on_rate(10, 100, 20, true, false)); // 10 < 80 -> exit
    assert!(!should_exit_on_rate(90, 100, 20, true, true)); // 90 >= 80 -> hold
    assert!(!should_exit_on_rate(-5, 0, 0, true, false) == false); // negative hold vs 0 redeploy -> exit
    assert!(should_exit_on_rate(0, 0, 0, false, true)); // stale -> baseline
    assert!(!should_exit_on_rate(0, 0, 0, false, false)); // stale -> baseline
}
