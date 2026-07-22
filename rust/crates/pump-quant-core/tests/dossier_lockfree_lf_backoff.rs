// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'lockfree' component (leaf 'lf_backoff').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_core::lockfree::*;

#[test]
fn prop_backoff_hot_never_parks() {
    let mut b = Backoff::new();
    for _ in 0..1_000_000 {
        assert!(matches!(backoff_step(&mut b, true), Waited::Spun));
    }
    b.reset();
    let mut saw_yield = false;
    let mut saw_park = false;
    for _ in 0..100_000 {
        match backoff_step(&mut b, false) {
            Waited::Yielded => saw_yield = true,
            Waited::Parked => {
                saw_park = true;
                break;
            }
            Waited::Spun => {}
        }
    }
    assert!(saw_yield && saw_park); // escalation reaches park off the hot window
}
