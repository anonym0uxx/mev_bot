// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'overfitting' component (leaf 'pbo_cscv_rejects_inadmissible').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_evaluator::overfitting::*;

#[test]
fn pbo_cscv_rejects_inadmissible() {
    // Fewer than two trials.
    let empty: Vec<Vec<i64>> = Vec::new();
    assert_eq!(pbo_cscv(&empty), Err(CscvError::TooFewTrials));
    assert_eq!(pbo_cscv(&[vec![1, 2]]), Err(CscvError::TooFewTrials));
    // Odd block count.
    assert_eq!(
        pbo_cscv(&[vec![1, 2, 3], vec![4, 5, 6]]),
        Err(CscvError::BadBlockCount)
    );
    // Single block (< 2).
    assert_eq!(pbo_cscv(&[vec![1], vec![2]]), Err(CscvError::BadBlockCount));
    // Ragged rows.
    assert_eq!(
        pbo_cscv(&[vec![1, 2], vec![4, 5, 6, 7]]),
        Err(CscvError::RaggedMatrix)
    );
    // A well-formed matrix is accepted.
    assert!(pbo_cscv(&[vec![1, 2], vec![3, 4]]).is_ok());
}
