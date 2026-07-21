#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_evaluator::evaluator_stats::*;
#[test]
fn prop_markout_sign_adjustment() {
    let f =
        |side, fill: u64, later: u64| FillRow::test(FillClass::ScalpEntry, side, fill, later, 30);
    let rows = vec![f(Side::Buy, 1_000, 1_100), f(Side::Sell, 1_000, 900)];
    let m = markouts(&rows, &[30]);
    assert_eq!(m[0].n, 2);
    assert_eq!(m[0].median_bps, 1_000); // both favorable: +10% == +1000 bps
}
