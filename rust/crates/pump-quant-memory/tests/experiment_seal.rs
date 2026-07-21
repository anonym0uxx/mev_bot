//! Leaf: experiment_seal. Verifies the "seal → hash → reject mutation"
//! immutability contract (§56.1, §56.4, §59 property: sealed segments immutable).

use pump_quant_memory::experiment::ExperimentError;
use pump_quant_memory::rows::{Experiment, ExperimentId, HypothesisId};

fn sample(id: u64) -> Experiment {
    Experiment {
        id: ExperimentId(id),
        hypothesis_id: HypothesisId(7),
        schema_version: 1,
        title_hash: [1u8; 32],
        causal_mechanism_hash: [2u8; 32],
        dataset_hash: [3u8; 32],
        config_hash: 0xdead_beef,
        created_at_ns: 1_700_000_000_000_000_000,
        sealed: false,
        seal_hash: None,
    }
}

#[test]
fn seal_records_hash_and_marks_sealed() {
    let mut e = sample(1);
    assert!(e.seal_hash.is_none());
    let h = e.seal().expect("first seal succeeds");
    assert!(e.sealed);
    assert_eq!(e.seal_hash, Some(h));
    assert!(e.verify_integrity());
}

#[test]
fn seal_hash_equals_independent_compute() {
    let mut e = sample(1);
    let expected = e.compute_seal_hash();
    let got = e.seal().unwrap();
    assert_eq!(got, expected);
}

#[test]
fn resealing_is_rejected() {
    let mut e = sample(1);
    e.seal().unwrap();
    assert_eq!(e.seal(), Err(ExperimentError::AlreadySealed));
}

#[test]
fn identical_content_seals_to_identical_hash() {
    let mut a = sample(42);
    let mut b = sample(42);
    assert_eq!(a.seal().unwrap(), b.seal().unwrap());
}

#[test]
fn differing_content_seals_to_different_hash() {
    let mut a = sample(42);
    let mut b = sample(42);
    b.config_hash = 0x1234; // one field differs
    assert_ne!(a.seal().unwrap(), b.seal().unwrap());
}

#[test]
fn mutating_setter_refused_after_seal() {
    let mut e = sample(1);
    // Allowed while unsealed.
    assert_eq!(e.set_dataset_hash([9u8; 32]), Ok(()));
    assert_eq!(e.set_config_hash(0xaa), Ok(()));
    e.seal().unwrap();
    // Refused once sealed.
    assert_eq!(
        e.set_dataset_hash([0u8; 32]),
        Err(ExperimentError::SealedImmutable)
    );
    assert_eq!(e.set_config_hash(0), Err(ExperimentError::SealedImmutable));
    // Content unchanged, still verifies.
    assert!(e.verify_integrity());
}

#[test]
fn out_of_band_tamper_is_detected() {
    let mut e = sample(1);
    e.seal().unwrap();
    assert!(e.verify_integrity());
    // Bypass the safe API and mutate a public field directly (simulated tamper).
    e.dataset_hash = [0xFF; 32];
    assert!(
        !e.verify_integrity(),
        "tamper must be detected by hash mismatch"
    );
}

#[test]
fn unsealed_experiment_does_not_verify() {
    let e = sample(1);
    assert!(!e.verify_integrity());
}
