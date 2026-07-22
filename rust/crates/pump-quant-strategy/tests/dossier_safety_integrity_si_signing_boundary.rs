// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'safety_integrity' component (leaf 'si_signing_boundary').
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
fn unapproved_and_over_cap_denied() {
    let policy = SignPolicy {
        approved_programs: vec![10, 20],
        max_tx_size: 1000,
        key: KeyHandle::new(1),
    };
    let unapproved = SignRequest {
        program_id: 99,
        tx_size: 100,
        digest: 5,
    };
    assert_eq!(
        sign_through_policy(&unapproved, &policy),
        Err(SignError::PolicyDenied)
    );
    let overcap = SignRequest {
        program_id: 10,
        tx_size: 5000,
        digest: 5,
    };
    assert_eq!(
        sign_through_policy(&overcap, &policy),
        Err(SignError::PolicyDenied)
    );
}
#[test]
fn permitted_signs_deterministically() {
    let policy = SignPolicy {
        approved_programs: vec![10],
        max_tx_size: 1000,
        key: KeyHandle::new(1),
    };
    let req = SignRequest {
        program_id: 10,
        tx_size: 100,
        digest: 5,
    };
    let a = sign_through_policy(&req, &policy).expect("should sign");
    let b = sign_through_policy(&req, &policy).expect("should sign");
    assert_eq!(a, b); // deterministic, no key material exposed
}
