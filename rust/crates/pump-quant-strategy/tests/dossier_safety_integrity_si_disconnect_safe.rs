// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'safety_integrity' component (leaf 'si_disconnect_safe').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_strategy::safety_integrity::*;

#[test]
fn disconnect_opens_explicit_gap_no_interpolation() {
    let mut s = StreamState::new();
    let m = on_disconnect(&mut s, 500);
    assert_eq!(m.from_seq, 500);
    assert_eq!(m.to_seq, None);
    assert!(!s.connected);
    assert_eq!(s.gaps.len(), 1);
    // gapped seqs read as Unknown (no synthesized events)
    assert!(s.is_gapped(600));
}
#[test]
fn reconnect_is_distinct_epoch_boundary() {
    let mut s = StreamState::new();
    on_disconnect(&mut s, 500);
    let e0 = s.epoch;
    s.reconnect(800);
    assert_eq!(s.epoch, e0 + 1);
    assert_eq!(s.gaps[0].to_seq, Some(800));
    assert!(!s.is_gapped(900));
}
