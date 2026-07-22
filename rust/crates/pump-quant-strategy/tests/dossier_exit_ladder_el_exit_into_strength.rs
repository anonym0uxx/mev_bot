// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'exit_ladder' component (leaf 'el_exit_into_strength').
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
use pump_quant_strategy::exit_ladder::*;

#[test]
fn prop_fires_only_on_authentic_climax_in_profit() {
    let climax = BurstState::test(BurstPhase::Climax, Dir::Buy);
    assert!(exit_into_strength_fires(&climax, true, 9_000, 5_000));
    assert!(!exit_into_strength_fires(&climax, false, 9_000, 5_000)); // not in profit
    assert!(!exit_into_strength_fires(&climax, true, 1_000, 5_000)); // fabricated burst
    let onset = BurstState::test(BurstPhase::Onset, Dir::Buy);
    assert!(!exit_into_strength_fires(&onset, true, 9_000, 5_000));
}
