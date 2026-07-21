#![allow(unused_imports)]
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
    let fe = FieldEvidence { evidence_id: 7, parsed: None };
    assert_eq!(resolve_field(Some(fe), true), FieldState::Incomplete);
    let fe = FieldEvidence { evidence_id: 7, parsed: None };
    assert_eq!(resolve_field(Some(fe), false), FieldState::Incomplete);
}
#[test]
fn resolved_known_carries_evidence() {
    let fe = FieldEvidence { evidence_id: 42, parsed: Some(-5) };
    match resolve_field(Some(fe), true) {
        FieldState::Known(e) => { assert_eq!(e.evidence_id, 42); assert_eq!(e.value, -5); }
        other => panic!("expected Known, got {:?}", other),
    }
}
