// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'reducer' component (leaf 'rd_event_key').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_core::reducer::*;

#[test]
fn prop_key_source_independent() {
    let a = CanonEvent::test(100, 5, 0, /*source_seq*/ 1);
    let b = CanonEvent::test(100, 5, 0, /*source_seq*/ 999);
    assert_eq!(event_key(&a), event_key(&b));
    let c = CanonEvent::test(100, 6, 0, 1);
    assert!(event_key(&a) < event_key(&c));
}
