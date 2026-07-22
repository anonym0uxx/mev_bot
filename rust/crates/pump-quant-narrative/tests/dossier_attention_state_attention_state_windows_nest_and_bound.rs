// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'attention_state' component (leaf 'attention_state_windows_nest_and_bound').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_narrative::attention_state::*;

#[test]
fn prop_attention_state_windows_nest_and_bound() {
    let mk = |ts_ns: u64, weight: u64| Mention {
        ts_ns,
        source_id: 1,
        community_id: 1,
        weight,
        copycat: false,
    };
    // Property: for window_1m <= window_5m, weighted_mentions_1m <= weighted_mentions_5m.
    let now = 1_000u64;
    let mentions = [mk(990, 5), mk(950, 7), mk(800, 11), mk(600, 13)];
    for w1 in 0..200u64 {
        for w5 in w1..=w1 + 50 {
            let st = nv_attention_state(&mentions, now, w1, w5, &[], 1, 10_000);
            assert!(st.weighted_mentions_1m <= st.weighted_mentions_5m);
        }
    }
    // Concrete: now=1000, 1m window=60 -> (940,1000], 5m=300 -> (700,1000].
    let st = nv_attention_state(&mentions, 1_000, 60, 300, &[], 1, 10_000);
    assert_eq!(st.weighted_mentions_1m, 12); // 990,950
    assert_eq!(st.weighted_mentions_5m, 23); // 990,950,800
                                             // Edge/boundary: lower bound exclusive, `now` inclusive. window=100 -> (900,1000].
    let edge = [mk(900, 4), mk(1000, 8)];
    let st = nv_attention_state(&edge, 1_000, 100, 100, &[], 1, 10_000);
    assert_eq!(st.weighted_mentions_1m, 8); // 900 excluded, 1000 included
    assert_eq!(st.weighted_mentions_5m, 8);
    // Rejection/no-data: empty stream -> zero on both windows.
    let st = nv_attention_state(&[], 1_000, 60, 300, &[], 1, 10_000);
    assert_eq!(st.weighted_mentions_1m, 0);
    assert_eq!(st.weighted_mentions_5m, 0);
}
