// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'scalp_position' component (leaf 'sp_min_hold').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_strategy::scalp_position::*;

#[test]
fn prop_exemptions_absolute() {
    for c in [
        ExitClass::Emergency,
        ExitClass::SellabilityFailure,
        ExitClass::RiskLimit,
        ExitClass::CircuitBreaker,
    ] {
        assert!(!min_hold_blocks_exit(Lane::Scalp, 0, u64::MAX, c));
    }
    assert!(min_hold_blocks_exit(Lane::Scalp, 10, 20, ExitClass::Normal));
    assert!(!min_hold_blocks_exit(
        Lane::Scalp,
        20,
        20,
        ExitClass::Normal
    ));
    assert!(!min_hold_blocks_exit(Lane::Scalp, 5, 0, ExitClass::Normal)); // zero min-hold legal
}
