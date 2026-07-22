// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'metrics' component (leaf 'brier_score').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_evaluator::metrics::*;

#[test]
fn gd_brier_score() {
    // Perfect forecaster: p=1 on occur, p=0 on non-occur -> 0.
    let perfect = vec![
        PredictionRow::new(1_000_000, true),
        PredictionRow::new(0, false),
    ];
    assert_eq!(brier_score_ppm(&perfect), Some(0));

    // Maximally wrong: p=1 & not occurred, p=0 & occurred -> (1)^2 each -> 1.0.
    let worst = vec![
        PredictionRow::new(1_000_000, false),
        PredictionRow::new(0, true),
    ];
    assert_eq!(brier_score_ppm(&worst), Some(1_000_000));

    // Always 0.5: (0.5)^2 = 0.25 regardless of outcome -> 250_000 ppm.
    let half = vec![
        PredictionRow::new(500_000, true),
        PredictionRow::new(500_000, false),
    ];
    let b = brier_score_ppm(&half).unwrap();
    assert_eq!(b, 250_000);
    // Bounded within [0, 1_000_000].
    assert!(b <= 1_000_000);

    // Rejection/edge: empty sample -> None.
    assert_eq!(brier_score_ppm(&[]), None);
}
