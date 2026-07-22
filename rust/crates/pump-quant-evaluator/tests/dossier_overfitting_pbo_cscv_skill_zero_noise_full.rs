// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'overfitting' component (leaf 'pbo_cscv_skill_zero_noise_full').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_evaluator::overfitting::*;

#[test]
fn pbo_cscv_skill_zero_noise_full() {
    // Trial 0 dominates every block -> best IS and best OOS on every split,
    // never at/below the OOS median -> PBO = 0.
    let skilled = vec![
        vec![100, 100, 100, 100],
        vec![10, 10, 10, 10],
        vec![20, 20, 20, 20],
        vec![30, 30, 30, 30],
    ];
    let r = pbo_cscv(&skilled).unwrap();
    assert_eq!(r.n_trials, 4);
    assert_eq!(r.n_blocks, 4);
    assert_eq!(r.n_splits, 6); // C(4,2)
    assert_eq!(r.overfit_splits, 0);
    assert_eq!(r.pbo_bps, 0);
    assert!(!r.blocks_promotion(1));

    // Perfect mirror-image noise: the IS winner always loses OOS -> PBO = 100%.
    let noise = vec![vec![100, -100], vec![-100, 100]];
    let n = pbo_cscv(&noise).unwrap();
    assert_eq!(n.n_splits, 2); // C(2,1)
    assert_eq!(n.overfit_splits, 2);
    assert_eq!(n.pbo_bps, 10_000);
    assert!(n.blocks_promotion(5_000));

    // pbo_bps = overfit_splits * 10_000 / n_splits, always in [0, 10_000].
    assert_eq!(n.pbo_bps, n.overfit_splits * 10_000 / n.n_splits);
    assert!(r.pbo_bps <= 10_000 && n.pbo_bps <= 10_000);
}
