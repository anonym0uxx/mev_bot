// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'sizing_validator' component (leaf 'monte_carlo_survival').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_evaluator::sizing_validator::*;

#[test]
fn prop_monte_carlo_survival() {
    // All-winner return set: no path ever dies, zero drawdown everywhere.
    let win = monte_carlo_survival(&[1_000i64, 2_000, 500], 2_000, 50, 30, 5_000, 99);
    assert_eq!(win.f_bps, 2_000);
    assert_eq!(win.n_paths, 50);
    assert_eq!(win.survived, 50);
    assert_eq!(win.median_max_drawdown_bps, 0);
    assert_eq!(win.p95_max_drawdown_bps, 0);

    // Full-sizing on a -100% return wipes out: every path dies, drawdown maxes out.
    let dead = monte_carlo_survival(&[-10_000i64], 10_000, 25, 10, 5_000, 3);
    assert_eq!(dead.survived, 0);
    assert_eq!(dead.median_max_drawdown_bps, 10_000);
    assert_eq!(dead.p95_max_drawdown_bps, 10_000);

    // Deterministic in seed; survivors bounded by paths; median <= p95 drawdown.
    let a = monte_carlo_survival(&[3_000i64, -2_000, 4_000, -1_500], 1_500, 40, 20, 3_000, 11);
    let b = monte_carlo_survival(&[3_000i64, -2_000, 4_000, -1_500], 1_500, 40, 20, 3_000, 11);
    assert_eq!(a, b);
    assert!(a.survived <= a.n_paths);
    assert!(a.median_max_drawdown_bps <= a.p95_max_drawdown_bps);
    assert!(a.p95_max_drawdown_bps <= 10_000);

    // Empty input -> zeroed report.
    let z = monte_carlo_survival(&[], 1_000, 10, 10, 5_000, 1);
    assert_eq!(z.n_paths, 0);
    assert_eq!(z.survived, 0);
    assert_eq!(z.median_max_drawdown_bps, 0);
}

#[test]
#[should_panic(expected = "ruin_bps")]
fn prop_monte_carlo_survival_bad_ruin_panics() {
    let _ = monte_carlo_survival(&[1_000i64], 1_000, 1, 1, 10_000, 1);
}
