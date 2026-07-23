//! §51 combined promotion-blocking verdict — integration coverage.
use pump_quant_evaluator::fdr::Hypothesis;
use pump_quant_evaluator::promotion_verdict::{promotion_verdict, PromotionBlockReason};

fn skilled() -> Vec<Vec<i64>> {
    vec![
        vec![100, 100, 100, 100],
        vec![10, 10, 10, 10],
        vec![20, 20, 20, 20],
        vec![30, 30, 30, 30],
    ]
}

fn noise() -> Vec<Vec<i64>> {
    vec![vec![100, -100], vec![-100, 100]]
}

#[test]
fn clears_both_gates() {
    let fam = vec![Hypothesis::new(1, 5_000), Hypothesis::new(2, 500_000)];
    let v = promotion_verdict(&fam, 50_000, 1, &skilled(), 5_000);
    assert!(!v.blocks());
    assert_eq!(v.reason, PromotionBlockReason::Clear);
}

#[test]
fn pbo_blocks_noise() {
    let fam = vec![Hypothesis::new(1, 5_000)];
    let v = promotion_verdict(&fam, 50_000, 1, &noise(), 5_000);
    assert!(v.pbo_blocks);
    assert_eq!(v.pbo_bps, Some(10_000));
    assert_eq!(v.reason, PromotionBlockReason::PboOnly);
}

#[test]
fn fdr_blocks_undiscovered_candidate() {
    let fam = vec![Hypothesis::new(1, 5_000), Hypothesis::new(2, 500_000)];
    let v = promotion_verdict(&fam, 50_000, 2, &skilled(), 5_000);
    assert!(v.fdr_blocks && !v.pbo_blocks);
    assert_eq!(v.reason, PromotionBlockReason::FdrOnly);
}

#[test]
fn inadmissible_matrix_fails_closed() {
    let fam = vec![Hypothesis::new(1, 5_000)];
    let v = promotion_verdict(&fam, 50_000, 1, &[vec![1, 2]], 5_000);
    assert!(v.pbo_blocks);
    assert_eq!(v.pbo_bps, None);
}
