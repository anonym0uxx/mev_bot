// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'attention' component (leaf 'at_fade_cap').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_app::attention::*;

#[test]
fn at_emit_is_corroboration_tier_and_fade_capped() {
    let m = |ts: u64, src: u64, w: u64| pump_quant_narrative::attention_state::Mention {
        ts_ns: ts,
        source_id: src,
        community_id: src,
        weight: w,
        copycat: false,
    };
    let mut f = AttentionField::new(AttentionParams::standard());
    let mint = [7u8; 32];
    for i in 0..6u64 {
        f.observe(mint, m(1_000 + i * 10, i, 500));
    }
    let mut buf = Vec::new();
    f.emit_into(&mut buf, 1, |_| 0, |_| false);
    buf.clear();
    for i in 6..12u64 {
        f.observe(mint, m(2_000 + i * 10, i, 800));
    }
    // Money flat + unconfirmed => attention-leads, hard-capped at 500 (fade-first).
    f.emit_into(&mut buf, 2, |_| 0, |_| false);
    assert_eq!(buf.len(), 1);
    assert_eq!(
        buf[0].lane,
        pump_quant_watchlist::candidate::Lane::EarlyConfirmation
    );
    assert!(buf[0].discovery_score <= 500);
}
