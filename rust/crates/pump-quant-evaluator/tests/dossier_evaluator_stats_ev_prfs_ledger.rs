// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'evaluator_stats' component (leaf 'ev_prfs_ledger').
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
fn prop_prfs_both_sides_of_the_ledger() {
    let s = |g, r, p| PrfsSample::test(g, r, p, 3600);
    let samples = vec![
        s(1, 1_000, 400),   // halved: filter ate a loss
        s(1, 1_000, 2_500), // doubled: filter ate a winner
        s(2, 1_000, 990),
    ];
    let ledgers = prfs_fold(&samples);
    let g1 = &ledgers[0];
    assert_eq!(g1.halved_within_24h, 1);
    assert_eq!(g1.doubled_within_24h, 1); // over-rejection is visible, not hidden
    assert!(g1.loss_avoided_bps_sum > 0 && g1.upside_foregone_bps_sum > 0);
}
