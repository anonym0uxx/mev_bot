#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_evaluator::evaluator_stats::*;
#[test]
fn prop_capture_ratio_golden_and_screening() {
    let r = |mfe, mae, real, scr| ExcursionRow::test(ArchetypeKey::test(), mfe, mae, real, scr);
    let rows = vec![
        r(400, 100, 200, true),
        r(600, 300, 100, true),
        r(10_000, 0, 10_000, false),
    ];
    let rep = mfe_capture(&rows, ArchetypeKey::test());
    assert_eq!(rep.n, 2);
    assert_eq!(rep.excluded_unscreened, 1); // phantom excursion excluded
    assert_eq!(rep.capture_bps_of_mfe, (300u32 * 10_000) / 1_000); // 3000 bps = 30%
}
