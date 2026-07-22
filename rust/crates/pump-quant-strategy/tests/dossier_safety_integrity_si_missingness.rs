// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'safety_integrity' component (leaf 'si_missingness').
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
use pump_quant_strategy::safety_integrity::*;

#[test]
fn required_missing_rejects() {
    assert_eq!(resolve_field(None, true), FieldState::Reject);
}
#[test]
fn optional_missing_unknown() {
    assert_eq!(resolve_field(None, false), FieldState::Unknown);
}
#[test]
fn unparseable_incomplete() {
    let fe = FieldEvidence {
        evidence_id: 7,
        parsed: None,
    };
    assert_eq!(resolve_field(Some(fe), true), FieldState::Incomplete);
    let fe = FieldEvidence {
        evidence_id: 7,
        parsed: None,
    };
    assert_eq!(resolve_field(Some(fe), false), FieldState::Incomplete);
}
#[test]
fn resolved_known_carries_evidence() {
    let fe = FieldEvidence {
        evidence_id: 42,
        parsed: Some(-5),
    };
    match resolve_field(Some(fe), true) {
        FieldState::Known(e) => {
            assert_eq!(e.evidence_id, 42);
            assert_eq!(e.value, -5);
        }
        other => panic!("expected Known, got {:?}", other),
    }
}
