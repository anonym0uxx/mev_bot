// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'attention' component (leaf 'at_bounded').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_app::attention::*;

#[test]
fn at_bounded_eviction_and_determinism() {
    let m = |ts: u64, src: u64, w: u64| pump_quant_narrative::attention_state::Mention {
        ts_ns: ts,
        source_id: src,
        community_id: src,
        weight: w,
        copycat: false,
    };
    let params = AttentionParams {
        track_cap: 2,
        ..AttentionParams::standard()
    };
    let mut f = AttentionField::new(params);
    f.observe([1u8; 32], m(1, 1, 10));
    f.observe([1u8; 32], m(2, 2, 10));
    f.observe([2u8; 32], m(3, 3, 10));
    f.observe([3u8; 32], m(4, 4, 10));
    assert_eq!(f.len(), 2);
    let build = || {
        let mut g = AttentionField::new(AttentionParams::standard());
        for i in 0..10u64 {
            g.observe([5u8; 32], m(1_000 + i * 5, i % 4, 300 + i * 7));
        }
        let mut b = Vec::new();
        g.emit_into(&mut b, 1, |_| 100, |_| false);
        b
    };
    assert_eq!(build(), build());
}
