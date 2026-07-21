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
