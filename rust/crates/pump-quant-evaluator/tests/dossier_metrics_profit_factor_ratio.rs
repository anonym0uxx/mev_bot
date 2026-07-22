// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'metrics' component (leaf 'profit_factor_ratio').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_evaluator::metrics::*;

#[test]
fn gd_profit_factor_ratio() {
    // wins = 100+300 = 400, losses = 200. PF = 400*10000/200 = 20000 (2.0x).
    let returns = vec![100i64, -200, 300, 0];
    assert_eq!(profit_factor(&returns), ProfitFactor::Bps(20_000));

    // Zeros are neither win nor loss; adding them does not change the ratio.
    let with_zeros = vec![0i64, 100, -200, 0, 300, 0];
    assert_eq!(profit_factor(&with_zeros), ProfitFactor::Bps(20_000));

    // Break-even: equal wins and losses -> exactly 10_000 bps (1.0x).
    assert_eq!(profit_factor(&[300i64, -300]), ProfitFactor::Bps(10_000));

    // No losing returns -> NoLosses (denominator zero), even for all-zero input.
    assert_eq!(profit_factor(&[100i64, 200]), ProfitFactor::NoLosses);
    assert_eq!(profit_factor(&[0i64, 0]), ProfitFactor::NoLosses);

    // Rejection/edge: empty input -> Empty.
    assert_eq!(profit_factor(&[]), ProfitFactor::Empty);
}
