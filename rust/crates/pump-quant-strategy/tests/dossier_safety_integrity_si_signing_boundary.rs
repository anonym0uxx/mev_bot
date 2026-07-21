#![allow(unused_imports)]
use pump_quant_strategy::safety_integrity::*;

#[test]
fn unapproved_and_over_cap_denied() {
    let policy = SignPolicy { approved_programs: vec![10, 20], max_tx_size: 1000, key: KeyHandle::new(1) };
    let unapproved = SignRequest { program_id: 99, tx_size: 100, digest: 5 };
    assert_eq!(sign_through_policy(&unapproved, &policy), Err(SignError::PolicyDenied));
    let overcap = SignRequest { program_id: 10, tx_size: 5000, digest: 5 };
    assert_eq!(sign_through_policy(&overcap, &policy), Err(SignError::PolicyDenied));
}
#[test]
fn permitted_signs_deterministically() {
    let policy = SignPolicy { approved_programs: vec![10], max_tx_size: 1000, key: KeyHandle::new(1) };
    let req = SignRequest { program_id: 10, tx_size: 100, digest: 5 };
    let a = sign_through_policy(&req, &policy).expect("should sign");
    let b = sign_through_policy(&req, &policy).expect("should sign");
    assert_eq!(a, b); // deterministic, no key material exposed
}
