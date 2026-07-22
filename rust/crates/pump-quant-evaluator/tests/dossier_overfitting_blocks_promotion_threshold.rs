// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'overfitting' component (leaf 'blocks_promotion_threshold').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_evaluator::overfitting::*;

#[test]
fn blocks_promotion_threshold() {
    // pbo_bps = 10_000 (full overfit).
    let noise = vec![vec![100, -100], vec![-100, 100]];
    let hi = pbo_cscv(&noise).unwrap();
    assert_eq!(hi.pbo_bps, 10_000);
    assert!(hi.blocks_promotion(10_000)); // inclusive: >= threshold
    assert!(hi.blocks_promotion(9_999));
    assert!(!hi.blocks_promotion(10_001));

    // pbo_bps = 0 (no overfit).
    let skilled = vec![
        vec![100, 100, 100, 100],
        vec![10, 10, 10, 10],
        vec![20, 20, 20, 20],
        vec![30, 30, 30, 30],
    ];
    let lo = pbo_cscv(&skilled).unwrap();
    assert_eq!(lo.pbo_bps, 0);
    assert!(lo.blocks_promotion(0)); // 0 >= 0
    assert!(!lo.blocks_promotion(1));
}
