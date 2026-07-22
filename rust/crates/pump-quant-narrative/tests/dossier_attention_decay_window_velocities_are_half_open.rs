// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'attention_decay' component (leaf 'window_velocities_are_half_open').
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
fn window_velocities_are_half_open() {
    // now=10_000, window=1_000. current=(9_000,10_000], prev=(8_000,9_000].
    let events = [
        ev(9_500, EventKind::Post),
        ev(9_800, EventKind::Post),
        ev(9_900, EventKind::Post),
        ev(8_500, EventKind::Post), // prev window
        ev(7_000, EventKind::Post), // older, ignored
        ev(9_600, EventKind::Comment),
        ev(9_700, EventKind::Reply),
        ev(9_100, EventKind::CreatorEvent),
    ];
    let m = nv_attention_decay(&events, 10_000, 1_000, &base_inputs());
    assert_eq!(m.post_velocity, 3);
    assert_eq!(m.post_acceleration, 2); // 3 current - 1 prev
    assert_eq!(m.comment_velocity, 1);
    assert_eq!(m.reply_velocity, 1);
    assert_eq!(m.creator_cadence, 1);

    // Half-open boundaries: ts == now included; ts == cur_lo excluded from current
    // (it is the top of the previous window).
    let bnd = [
        ev(10_000, EventKind::Post), // == now -> current
        ev(9_000, EventKind::Post),  // == cur_lo -> previous, not current
    ];
    let mb = nv_attention_decay(&bnd, 10_000, 1_000, &base_inputs());
    assert_eq!(mb.post_velocity, 1);
    assert_eq!(mb.post_acceleration, 0); // 1 current - 1 prev

    // Saturating window: window > now must not underflow/panic; all events land in current.
    let sat = [ev(5, EventKind::Post)];
    let ms = nv_attention_decay(&sat, 10, 1_000_000, &base_inputs());
    assert_eq!(ms.post_velocity, 1);
    assert_eq!(ms.post_acceleration, 1); // prev window empty

    // Empty stream -> all velocities and acceleration zero.
    let me = nv_attention_decay(&[], 10_000, 1_000, &base_inputs());
    assert_eq!(me.post_velocity, 0);
    assert_eq!(me.comment_velocity, 0);
    assert_eq!(me.reply_velocity, 0);
    assert_eq!(me.creator_cadence, 0);
    assert_eq!(me.post_acceleration, 0);
}
