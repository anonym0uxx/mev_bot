// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'driver' component (leaf 'break_set_none_is_empty_identity').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_replay::driver::*;

#[test]
fn break_set_none_is_empty_identity() {
    let all = [
        BreakCondition::OnMint,
        BreakCondition::OnDecision,
        BreakCondition::OnEntry,
        BreakCondition::OnExit,
    ];
    let empty = BreakSet::none();
    assert!(empty.is_empty());
    assert_eq!(empty, BreakSet::default());
    for &c in &all {
        assert!(!empty.contains(c));
    }
    for &c in &all {
        assert!(!BreakSet::none().with(c).is_empty());
    }
    assert_eq!(BreakSet::none(), BreakSet::none());
}
