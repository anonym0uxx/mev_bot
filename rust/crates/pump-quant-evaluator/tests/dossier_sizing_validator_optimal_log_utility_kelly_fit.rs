// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'sizing_validator' component (leaf 'optimal_log_utility_kelly_fit').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_evaluator::sizing_validator::*;

#[test]
fn prop_optimal_log_utility_kelly_fit() {
    // Empty input -> canonical zero fit.
    let empty = optimal_log_utility(&[], 10_000, 100);
    assert_eq!(empty.optimal_f_bps, 0);
    assert_eq!(empty.expected_log_growth, 0);
    assert_eq!(empty.n, 0);

    // Symmetric no-edge coin (+50%/-50%) loses in log terms -> all cash, 0 growth.
    let flat = optimal_log_utility(&[5_000i64, -5_000], 10_000, 500);
    assert_eq!(flat.optimal_f_bps, 0);
    assert_eq!(flat.expected_log_growth, 0);
    assert_eq!(flat.n, 2);

    // Purely negative returns can never beat all-cash -> stays at 0.
    let losers = optimal_log_utility(&[-2_000i64, -3_000, -1_000], 10_000, 100);
    assert_eq!(losers.optimal_f_bps, 0);
    assert_eq!(losers.expected_log_growth, 0);
    assert_eq!(losers.n, 3);

    // Favorable coin +100%/-50%: classic Kelly f* = 0.5 -> grid lands in [4000,6000].
    let edge = optimal_log_utility(&[10_000i64, -5_000], 10_000, 100);
    assert!(
        edge.optimal_f_bps >= 4_000 && edge.optimal_f_bps <= 6_000,
        "kelly off grid: {}",
        edge.optimal_f_bps
    );
    assert!(edge.expected_log_growth > 0);
    assert_eq!(edge.n, 2);
    // Optimal fraction never exceeds the searched ceiling.
    assert!(edge.optimal_f_bps <= 10_000);
}

#[test]
#[should_panic(expected = "step_bps")]
fn prop_optimal_log_utility_zero_step_panics() {
    let _ = optimal_log_utility(&[1_000i64], 10_000, 0);
}
