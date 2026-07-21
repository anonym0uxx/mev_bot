//! Leaf he_revert: per-cell and pooled OOS baseline-beat-or-revert gate (criterion 100).

use pump_quant_strategy::hazard_estimator::{
    adaptive_beats_baseline, evaluate_calibration, CalibDecision, CalibResult,
};

fn r(id: u32, adaptive: i64, baseline: i64) -> CalibResult {
    CalibResult {
        cell_id: id,
        adaptive_net_fp: adaptive,
        baseline_net_fp: baseline,
    }
}

#[test]
fn beat_predicate_requires_strict_improvement() {
    assert!(adaptive_beats_baseline(&r(0, 10, 9)));
    assert!(!adaptive_beats_baseline(&r(0, 9, 9))); // tie reverts
    assert!(!adaptive_beats_baseline(&r(0, 8, 9)));
}

#[test]
fn per_cell_reverts_losers_when_pool_wins() {
    let cells = [
        r(1, 100, 50), // win  -> keep
        r(2, 40, 60),  // lose -> revert
        r(3, 70, 70),  // tie  -> revert
    ];
    let pooled = r(0, 210, 180); // pooled wins
    assert_eq!(
        evaluate_calibration(&cells, &pooled),
        vec![
            CalibDecision::KeepAdaptive,
            CalibDecision::Revert,
            CalibDecision::Revert,
        ]
    );
}

#[test]
fn pooled_loss_forces_global_revert() {
    let cells = [
        r(1, 100, 50), // would win individually
        r(2, 200, 10), // would win individually
    ];
    let pooled = r(0, 100, 150); // pooled loses -> all revert
    assert_eq!(
        evaluate_calibration(&cells, &pooled),
        vec![CalibDecision::Revert, CalibDecision::Revert]
    );
}

#[test]
fn pooled_tie_also_reverts_all() {
    let cells = [r(1, 100, 1)];
    let pooled = r(0, 100, 100);
    assert_eq!(
        evaluate_calibration(&cells, &pooled),
        vec![CalibDecision::Revert]
    );
}

#[test]
fn all_win_kept_when_pool_wins() {
    let cells = [r(1, 5, 1), r(2, 8, 2), r(3, 3, 0)];
    let pooled = r(0, 16, 3);
    assert_eq!(
        evaluate_calibration(&cells, &pooled),
        vec![
            CalibDecision::KeepAdaptive,
            CalibDecision::KeepAdaptive,
            CalibDecision::KeepAdaptive,
        ]
    );
}

#[test]
fn empty_cells_ok() {
    assert_eq!(
        evaluate_calibration(&[], &r(0, 10, 1)),
        Vec::<CalibDecision>::new()
    );
}
