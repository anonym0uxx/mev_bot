// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'lifecycle' component (leaf 'cls_absorbing').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(
    unused_imports,
    dead_code,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_domain::lifecycle::*;

#[test]
fn cls_terminal_and_failure_are_absorbing() {
    use CandidateLifecycleState::*;
    // Archived never transitions.
    let mut archived_succ = 0u32;
    for to in CandidateLifecycleState::ALL {
        if Archived.can_transition_to(to) {
            archived_succ += 1;
        }
    }
    assert_eq!(archived_succ, 0);

    // Each resting failure state has exactly one successor: Archived.
    for from in [Rejected, PermanentlyInvalidated] {
        let mut succ = 0u32;
        for to in CandidateLifecycleState::ALL {
            assert_eq!(from.can_transition_to(to), to == Archived, "{from} -> {to}");
            if from.can_transition_to(to) {
                succ += 1;
            }
        }
        assert_eq!(succ, 1);
    }

    // No self-loops anywhere.
    for s in CandidateLifecycleState::ALL {
        assert!(!s.can_transition_to(s), "self loop {s}");
    }

    // Exited proceeds only to archival, never retroactive failure.
    assert!(!Exited.can_transition_to(Rejected));
    assert!(!Exited.can_transition_to(PermanentlyInvalidated));
    assert!(Exited.can_transition_to(Archived));
}
