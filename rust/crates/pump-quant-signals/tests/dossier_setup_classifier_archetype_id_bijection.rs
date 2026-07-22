// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'setup_classifier' component (leaf 'archetype_id_bijection').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_signals::setup_classifier::*;

#[test]
fn archetype_id_is_stable_and_bijective() {
    let all = [
        SetupFamily::None,
        SetupFamily::BreakoutRetest,
        SetupFamily::FailedBreakdownReversal,
        SetupFamily::Reclaim,
        SetupFamily::CompressionExpansion,
        SetupFamily::ShortHorizonMeanReversion,
        SetupFamily::OrderFlowDislocation,
    ];
    let ids: Vec<u16> = all.iter().map(|f| f.archetype_id()).collect();
    assert_eq!(ids, vec![0, 1, 2, 3, 4, 5, 6]);
    assert_eq!(SetupFamily::None.archetype_id(), 0);
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), all.len());
    for f in all.iter().filter(|f| **f != SetupFamily::None) {
        assert_ne!(f.archetype_id(), 0);
    }
}
