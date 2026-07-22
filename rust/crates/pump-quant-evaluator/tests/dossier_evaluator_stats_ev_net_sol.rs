// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'evaluator_stats' component (leaf 'ev_net_sol').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_evaluator::evaluator_stats::*;

#[test]
fn prop_net_sol_golden() {
    let t = |lane, gross: i128, fee: u128, tip: u128, failc: u128| {
        ReconTrade::test(lane, gross, fee, tip, failc)
    };
    let trades = vec![
        t(Lane::Scalp, 1_000, 100, 50, 10),
        t(Lane::Scalp, -400, 100, 50, 0),
        t(Lane::Early, 9_999, 1, 1, 1), // excluded: other lane
    ];
    let s = net_sol(&trades, Lane::Scalp);
    assert_eq!(s.n, 2);
    assert_eq!(s.gross_lamports, 600);
    assert_eq!(s.net_lamports, 600 - 200 - 100 - 10);
    assert!(net_sol(&[], Lane::Scalp).is_missing());
}
