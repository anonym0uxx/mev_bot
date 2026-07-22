// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'attention_state' component (leaf 'attention_state_distinct_concentration_bounded').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_narrative::attention_state::*;

#[test]
fn prop_attention_state_distinct_concentration_bounded() {
    let mk = |source_id: u64, community_id: u64, weight: u64| Mention {
        ts_ns: 100,
        source_id,
        community_id,
        weight,
        copycat: false,
    };
    // Property: unique_sources never exceeds MAX_TRACKED and concentration stays in [0, FP_ONE].
    for n in 0..(MAX_TRACKED as u64 + 20) {
        let mentions: Vec<Mention> = (0..n).map(|i| mk(i, 0, 1)).collect();
        let st = nv_attention_state(&mentions, 1_000, 10_000, 10_000, &[], 1, 10_000);
        assert!(st.unique_sources as usize <= MAX_TRACKED);
        assert!(st.source_concentration <= 10_000); // FP_ONE
    }
    // Concrete: source 1 accumulates 80 of total 100 weight -> 8_000 bps; 3 sources, 2 communities.
    let mentions = [mk(1, 10, 70), mk(1, 10, 10), mk(2, 11, 10), mk(3, 11, 10)];
    let st = nv_attention_state(&mentions, 1_000, 10_000, 10_000, &[], 1, 10_000);
    assert_eq!(st.unique_sources, 3);
    assert_eq!(st.unique_communities, 2);
    assert_eq!(st.source_concentration, 8_000);
    // Edge: distinct sources past the cap saturate at MAX_TRACKED.
    let mentions: Vec<Mention> = (0..(MAX_TRACKED as u64 + 10))
        .map(|i| mk(i, 0, 1))
        .collect();
    let st = nv_attention_state(&mentions, 1_000, 10_000, 10_000, &[], 1, 10_000);
    assert_eq!(st.unique_sources as usize, MAX_TRACKED);
    // Rejection/no-weight: no observations -> concentration 0.
    let st = nv_attention_state(&[], 1_000, 10_000, 10_000, &[], 1, 10_000);
    assert_eq!(st.source_concentration, 0);
    assert_eq!(st.unique_sources, 0);
}
