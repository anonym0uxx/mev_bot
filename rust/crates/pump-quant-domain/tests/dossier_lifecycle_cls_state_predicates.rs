// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'lifecycle' component (leaf 'cls_state_predicates').
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
fn cls_state_predicates_partition() {
    use CandidateLifecycleState::*;
    let mut terminal = 0u32;
    let mut failed = 0u32;
    let mut holds = 0u32;
    for s in CandidateLifecycleState::ALL {
        assert_eq!(s.is_terminal(), s == Archived);
        assert_eq!(
            s.is_failed(),
            matches!(s, Rejected | PermanentlyInvalidated)
        );
        assert_eq!(s.holds_position(), matches!(s, Entered | Managing));
        // Terminal and failed are mutually exclusive.
        assert!(!(s.is_terminal() && s.is_failed()));
        if s.is_terminal() {
            terminal += 1;
        }
        if s.is_failed() {
            failed += 1;
        }
        if s.holds_position() {
            holds += 1;
        }
    }
    assert_eq!(terminal, 1);
    assert_eq!(failed, 2);
    assert_eq!(holds, 2);
    assert_eq!(CandidateLifecycleState::ALL.len(), 10);
}
