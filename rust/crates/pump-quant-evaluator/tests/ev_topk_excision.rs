#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_evaluator::evaluator_stats::*;
#[test]
fn prop_excision_fragility_detected() {
    let t = |id, v: i128| (TradeId::test(id), v);
    // Kamat-shaped book: +117 total, top-3 carry it
    let book = vec![t(1, 60), t(2, 40), t(3, 30), t(4, -5), t(5, -8)];
    let ex = topk_excision(&book, &[1, 3]);
    assert_eq!(ex[0].net_without_topk, 57);
    assert!(!ex[0].flipped_negative);
    assert_eq!(ex[1].net_without_topk, -13);
    assert!(ex[1].flipped_negative); // the lottery ticket exposed
}
