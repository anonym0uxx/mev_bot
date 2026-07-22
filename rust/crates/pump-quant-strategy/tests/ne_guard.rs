//! Leaf ne_guard: No-Edge state + no-forced-entry guard (criterion 50).

use pump_quant_strategy::no_edge::{
    emit_entry, lane_on_evidence, may_emit_entry, EntryEmission, IdleReason, IntentKind, LaneState,
    OrderIntent,
};

#[test]
fn only_active_may_emit_entry() {
    assert!(may_emit_entry(LaneState::Active));
    assert!(!may_emit_entry(LaneState::Searching));
    assert!(!may_emit_entry(LaneState::NoEdge));
    assert!(!may_emit_entry(LaneState::Retired));
}

#[test]
fn active_emits_entry_intent() {
    assert_eq!(
        emit_entry(LaneState::Active, 777),
        EntryEmission::Emitted(OrderIntent {
            token_mint: 777,
            kind: IntentKind::Entry,
        })
    );
}

#[test]
fn no_edge_never_emits_entry() {
    assert_eq!(
        emit_entry(LaneState::NoEdge, 1),
        EntryEmission::Idle(IdleReason::NoEdge)
    );
}

#[test]
fn searching_is_idle_not_forced() {
    assert_eq!(
        emit_entry(LaneState::Searching, 1),
        EntryEmission::Idle(IdleReason::Searching)
    );
}

#[test]
fn retired_is_idle() {
    assert_eq!(
        emit_entry(LaneState::Retired, 1),
        EntryEmission::Idle(IdleReason::Retired)
    );
}

#[test]
fn transitions_are_deterministic() {
    // Searching + edge => Active; no edge => NoEdge.
    assert_eq!(
        lane_on_evidence(LaneState::Searching, true),
        LaneState::Active
    );
    assert_eq!(
        lane_on_evidence(LaneState::Searching, false),
        LaneState::NoEdge
    );
    // NoEdge can recover to Active on found edge.
    assert_eq!(lane_on_evidence(LaneState::NoEdge, true), LaneState::Active);
    assert_eq!(
        lane_on_evidence(LaneState::NoEdge, false),
        LaneState::NoEdge
    );
    // Active with lost edge falls to NoEdge.
    assert_eq!(
        lane_on_evidence(LaneState::Active, false),
        LaneState::NoEdge
    );
    assert_eq!(lane_on_evidence(LaneState::Active, true), LaneState::Active);
    // Retired is terminal.
    assert_eq!(
        lane_on_evidence(LaneState::Retired, true),
        LaneState::Retired
    );
}

#[test]
fn no_edge_after_transition_still_cannot_emit() {
    let s = lane_on_evidence(LaneState::Searching, false);
    assert_eq!(s, LaneState::NoEdge);
    assert_eq!(emit_entry(s, 5), EntryEmission::Idle(IdleReason::NoEdge));
}
