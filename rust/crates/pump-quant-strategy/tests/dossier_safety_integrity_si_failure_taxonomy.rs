// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'safety_integrity' component (leaf 'si_failure_taxonomy').
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
fn construction_never_retryable_with_capital() {
    let c = classify_failure(&DecodedProgramError::MalformedInstruction);
    assert_eq!(c, FailureClass::Construction);
    assert!(!c.retryable_with_capital());
    assert!(c.triggers_quarantine());
    let c2 = classify_failure(&DecodedProgramError::AccountMismatch);
    assert_eq!(c2, FailureClass::Construction);
    assert!(!c2.retryable_with_capital());
}
#[test]
fn transient_retryable() {
    let t = classify_failure(&DecodedProgramError::SlippageExceeded);
    assert_eq!(t, FailureClass::Transient);
    assert!(t.retryable_with_capital());
    assert_eq!(
        classify_failure(&DecodedProgramError::PriceMoved),
        FailureClass::Transient
    );
}
#[test]
fn unknown_conservative() {
    let u = classify_failure(&DecodedProgramError::Unknown(7));
    assert_eq!(u, FailureClass::Unknown);
    assert!(!u.retryable_with_capital());
    assert!(!u.triggers_quarantine());
}
