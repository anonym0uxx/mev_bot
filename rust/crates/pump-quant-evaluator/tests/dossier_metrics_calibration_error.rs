// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'metrics' component (leaf 'calibration_error').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_evaluator::metrics::*;

#[test]
fn gd_calibration_error() {
    // Perfectly calibrated: bin at 1.0 occurs, bin at 0.0 does not -> zero gaps.
    let good = vec![
        PredictionRow::new(1_000_000, true),
        PredictionRow::new(0, false),
    ];
    let rg = calibration_error(&good, 10).unwrap();
    assert_eq!(rg.n, 2);
    assert_eq!(rg.occupied_bins, 2);
    assert_eq!(rg.ece_ppm, 0);
    assert_eq!(rg.mce_ppm, 0);

    // Miscalibrated: predict 0.9 four times, never occurs -> gap 0.9 in one bin.
    let bad = vec![
        PredictionRow::new(900_000, false),
        PredictionRow::new(900_000, false),
        PredictionRow::new(900_000, false),
        PredictionRow::new(900_000, false),
    ];
    let rb = calibration_error(&bad, 10).unwrap();
    assert_eq!(rb.n, 4);
    assert_eq!(rb.occupied_bins, 1);
    assert_eq!(rb.ece_ppm, 900_000);
    assert_eq!(rb.mce_ppm, 900_000);
    // ECE is a count-weighted mean of per-bin gaps, so never exceeds MCE.
    assert!(rb.ece_ppm <= rb.mce_ppm);

    // Rejection/edge: empty sample -> None.
    assert!(calibration_error(&[], 10).is_none());
}
