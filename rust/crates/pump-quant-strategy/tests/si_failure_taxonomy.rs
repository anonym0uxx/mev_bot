#![allow(
    unused_imports,
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
