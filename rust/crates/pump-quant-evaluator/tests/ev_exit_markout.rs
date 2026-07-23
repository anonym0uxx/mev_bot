//! §47 exit-side markout API — integration coverage of the public surface.
use pump_quant_evaluator::evaluator_stats::Side;
use pump_quant_evaluator::exit_markout::{
    exit_markout_cells, foregone_upside, ExitMarkoutRow, ExitReason, MarkoutHorizonNs, H_1S_NS,
    H_250MS_NS, MANDATED_HORIZONS_NS,
};

#[test]
fn cells_and_foregone_agree_on_direction() {
    let rows = vec![
        ExitMarkoutRow::test(ExitReason::StopLoss, Side::Sell, 1_000, H_1S_NS, 1_500),
        ExitMarkoutRow::test(ExitReason::StopLoss, Side::Sell, 1_000, H_1S_NS, 1_500),
    ];
    let cells = exit_markout_cells(&rows, &MANDATED_HORIZONS_NS);
    assert_eq!(cells.len(), 1);
    assert_eq!(cells[0].reason, ExitReason::StopLoss);
    assert_eq!(cells[0].delta_bp, 5_000);
    let f = foregone_upside(&rows, &MANDATED_HORIZONS_NS);
    assert_eq!(f[0].foregone_bp_sum, 10_000);
    assert_eq!(f[0].loss_avoided_bp_sum, 0);
}

#[test]
fn mandated_horizon_ladder_is_five_marks() {
    assert_eq!(MANDATED_HORIZONS_NS.len(), 5);
    assert_eq!(MarkoutHorizonNs(H_250MS_NS).0, 250_000_000);
}

#[test]
fn empty_input_yields_no_cells() {
    assert!(exit_markout_cells(&[], &MANDATED_HORIZONS_NS).is_empty());
    assert!(foregone_upside(&[], &MANDATED_HORIZONS_NS).is_empty());
}

#[test]
fn per_reason_bucketing_is_deterministic() {
    let rows = vec![
        ExitMarkoutRow::test(ExitReason::TakeProfit, Side::Sell, 2_000, H_1S_NS, 2_100),
        ExitMarkoutRow::test(ExitReason::TimeStop, Side::Sell, 2_000, H_1S_NS, 1_800),
    ];
    let a = exit_markout_cells(&rows, &[H_1S_NS]);
    let b = exit_markout_cells(&rows, &[H_1S_NS]);
    assert_eq!(a, b);
    // TakeProfit precedes TimeStop in enum order.
    assert_eq!(a[0].reason, ExitReason::TakeProfit);
    assert_eq!(a[1].reason, ExitReason::TimeStop);
}
