// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'attention_decay' component (leaf 'wired_inputs_pass_through_and_fatigue').
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
fn wired_inputs_pass_through_and_fatigue() {
    // Items 7,8,11,14,15,16,17 are wired inputs and must surface verbatim,
    // even with an empty event stream.
    let inp = base_inputs();
    let m = nv_attention_decay(&[], 10_000, 1_000, &inp);
    assert_eq!(m.semantic_duplication_bps, 1_500);
    assert_eq!(m.source_diversity, 12);
    assert_eq!(m.raid_activity, 2);
    assert_eq!(m.narrative_saturation_bps, 3_000);
    assert_eq!(m.conversion_to_new_wallets, 40);
    assert_eq!(m.conversion_to_independent_breadth, 9);
    assert_eq!(m.conversion_to_net_flow, 25_000);

    // Distinct inputs propagate exactly (rejection of hard-coded constants), incl. a
    // negative signed net flow.
    let mut inp2 = base_inputs();
    inp2.semantic_duplication_bps = 42;
    inp2.source_diversity = 7;
    inp2.raid_activity = 0;
    inp2.narrative_saturation_bps = 9_999;
    inp2.conversion_to_new_wallets = 1_000_000;
    inp2.conversion_to_independent_breadth = 3;
    inp2.conversion_to_net_flow = -123_456;
    let m2 = nv_attention_decay(&[ev(1, EventKind::Post)], 10_000, 1_000, &inp2);
    assert_eq!(m2.semantic_duplication_bps, 42);
    assert_eq!(m2.source_diversity, 7);
    assert_eq!(m2.raid_activity, 0);
    assert_eq!(m2.narrative_saturation_bps, 9_999);
    assert_eq!(m2.conversion_to_new_wallets, 1_000_000);
    assert_eq!(m2.conversion_to_independent_breadth, 3);
    assert_eq!(m2.conversion_to_net_flow, -123_456);

    // Streamer fatigue = prev-window stream count - current-window stream count.
    // 2 prev, 0 current -> +2 (declining).
    let declining = [
        ev(8_200, EventKind::StreamEvent),
        ev(8_800, EventKind::StreamEvent),
    ];
    assert_eq!(
        nv_attention_decay(&declining, 10_000, 1_000, &base_inputs()).streamer_fatigue,
        2
    );

    // 0 prev, 2 current -> -2 (rising, rejection of "always non-negative").
    let rising = [
        ev(9_200, EventKind::StreamEvent),
        ev(9_800, EventKind::StreamEvent),
    ];
    assert_eq!(
        nv_attention_decay(&rising, 10_000, 1_000, &base_inputs()).streamer_fatigue,
        -2
    );
}
