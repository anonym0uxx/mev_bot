// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'attention_decay' component (leaf 'first_instants_are_minimum_per_kind').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_narrative::attention_decay::*;

fn base_inputs() -> DecayInputs {
    DecayInputs {
        semantic_duplication_bps: 1_500,
        source_diversity: 12,
        raid_activity: 2,
        narrative_saturation_bps: 3_000,
        conversion_to_new_wallets: 40,
        conversion_to_independent_breadth: 9,
        conversion_to_net_flow: 25_000,
        peak_level: 1_000,
        current_level: 600,
    }
}

fn ev(ts_ns: u64, kind: EventKind) -> AttentionEvent {
    AttentionEvent { ts_ns, kind }
}

#[test]
fn first_instants_are_minimum_per_kind() {
    // Empty stream -> every first-instant is None (rejection of "always Some").
    let m0 = nv_attention_decay(&[], 10_000, 1_000, &base_inputs());
    assert_eq!(m0.first_mention_ns, None);
    assert_eq!(m0.first_high_quality_source_ns, None);
    assert_eq!(m0.first_creator_event_ns, None);
    assert_eq!(m0.first_stream_comment_event_ns, None);

    // Unordered stream: first-instant is the MIN ts per kind, independent of slice order.
    let events = [
        ev(500, EventKind::Post),
        ev(900, EventKind::HighQualitySource),
        ev(300, EventKind::HighQualitySource),
        ev(700, EventKind::CreatorEvent),
        ev(650, EventKind::Comment),
        ev(800, EventKind::StreamEvent),
    ];
    let m = nv_attention_decay(&events, 10_000, 1_000, &base_inputs());
    assert_eq!(m.first_mention_ns, Some(300)); // global min across all kinds
    assert_eq!(m.first_high_quality_source_ns, Some(300)); // min(300,900)
    assert_eq!(m.first_creator_event_ns, Some(700));
    // item 4 is min of stream(800) OR comment(650).
    assert_eq!(m.first_stream_comment_event_ns, Some(650));

    // A kind absent from the stream stays None while others resolve.
    let only_post = [ev(42, EventKind::Post)];
    let mp = nv_attention_decay(&only_post, 10_000, 1_000, &base_inputs());
    assert_eq!(mp.first_mention_ns, Some(42));
    assert_eq!(mp.first_high_quality_source_ns, None);
    assert_eq!(mp.first_creator_event_ns, None);
    assert_eq!(mp.first_stream_comment_event_ns, None);
}
