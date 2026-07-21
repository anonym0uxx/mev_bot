// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'exit_ladder' component (leaf 'el_escalation').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports)]
use pump_quant_strategy::exit_ladder::*;

#[test]
fn prop_escalation_monotone_and_urgent() {
    let s0 = EscalationState::new(1_000);
    let s1 = next_escalation(s0, true, 1, 1_000);
    let s2 = next_escalation(s1, true, 1, 1_000);
    assert!(s2.level > s1.level && s1.level > s0.level);
    let slow = next_escalation(s0, true, 1, 1_000).cooldown_ms;
    let fast = next_escalation(s0, true, 100, 1_000).cooldown_ms;
    assert!(fast < slow); // collapse => shorter cooldown (defect-list exception)
    let mut s = s0;
    for _ in 0..10 { s = next_escalation(s, true, 10, 1_000); }
    assert_eq!(s.level, 4);
    assert!(s.emergency_path_required);
}
