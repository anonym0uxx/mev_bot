// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'replay' component (leaf 'rp_parity_assert').
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
use pump_quant_core::replay::*;

#[test]
fn prop_parity_first_divergence() {
    let evs: Vec<_> = (0..50).map(|i| CanonEvent::test(i, 0, 0, i)).collect();
    let mut st = WorldState::new();
    let mut cps = vec![];
    for (i, e) in evs.iter().enumerate() {
        st = apply_world(&st, e);
        if i % 10 == 9 {
            cps.push((i, state_hash(&st)));
        }
    }
    assert!(matches!(replay_assert(&evs, &cps), ReplayVerdict::Match));
    let mut bad = cps.clone();
    bad[2].1[0] ^= 0xFF;
    match replay_assert(&evs, &bad) {
        ReplayVerdict::Diverged { at_event, .. } => assert_eq!(at_event, bad[2].0),
        _ => panic!("must diverge at the third checkpoint"),
    }
}
