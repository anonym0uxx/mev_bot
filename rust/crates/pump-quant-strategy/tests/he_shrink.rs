//! Leaf he_shrink: hierarchical partial-pooled hazard + min-effective-sample gate (criterion 100).

use pump_quant_strategy::hazard_estimator::{
    cell_hazard, shrink_within_phase, uncertainty_bps, CellEstimate, HazardSource, PhaseParent,
    ShrinkError,
};
use pump_quant_strategy::scalp_position::Phase;

fn cell(phase: Phase, est: u32, n: u32) -> CellEstimate {
    CellEstimate {
        phase,
        est_bps: est,
        n,
    }
}
fn parent(phase: Phase, est: u32, n: u32) -> PhaseParent {
    PhaseParent {
        phase,
        est_bps: est,
        n,
    }
}

#[test]
fn shrink_weights_by_sample_size() {
    // (n*cell + k*parent)/(n+k), computed independently.
    let p = parent(Phase::Curve, 2_000, 100);
    // n=4,k=4 -> (4*8000 + 4*2000)/8 = 5000.
    assert_eq!(
        shrink_within_phase(&cell(Phase::Curve, 8_000, 4), &p, 4),
        Ok(5_000)
    );
    // n=12,k=4 -> (96000+8000)/16 = 6500 (less shrinkage, closer to cell).
    assert_eq!(
        shrink_within_phase(&cell(Phase::Curve, 8_000, 12), &p, 4),
        Ok(6_500)
    );
    // n=0 -> exactly the parent.
    assert_eq!(
        shrink_within_phase(&cell(Phase::Curve, 8_000, 0), &p, 4),
        Ok(2_000)
    );
    // k=0 with n>0 -> exactly the cell (no pooling).
    assert_eq!(
        shrink_within_phase(&cell(Phase::Curve, 8_000, 5), &p, 0),
        Ok(8_000)
    );
}

#[test]
fn more_samples_move_estimate_toward_cell() {
    let p = parent(Phase::Pool, 1_000, 50);
    let low = shrink_within_phase(&cell(Phase::Pool, 9_000, 2), &p, 8).unwrap();
    let high = shrink_within_phase(&cell(Phase::Pool, 9_000, 200), &p, 8).unwrap();
    assert!(high > low, "high-n should shrink less: {high} vs {low}");
    assert!(high <= 9_000 && low >= 1_000);
}

#[test]
fn shrinkage_never_crosses_phase_boundary() {
    let p = parent(Phase::Pool, 2_000, 10);
    let c = cell(Phase::Curve, 8_000, 4);
    assert_eq!(
        shrink_within_phase(&c, &p, 4),
        Err(ShrinkError::PhaseMismatch)
    );
}

#[test]
fn min_effective_sample_gate_defaults_to_baseline() {
    let p = parent(Phase::Curve, 2_000, 100);
    // n=3 < min 5 -> baseline (7777), source Baseline.
    let starved = cell_hazard(&cell(Phase::Curve, 8_000, 3), &p, 4, 5, 7_777).unwrap();
    assert_eq!(starved.value_bps, 7_777);
    assert_eq!(starved.source, HazardSource::Baseline);
    assert_eq!(starved.effective_sample, 3);
    assert_eq!(starved.uncertainty_bps, uncertainty_bps(3));
}

#[test]
fn gate_graduation_uses_shrink_estimate() {
    let p = parent(Phase::Curve, 2_000, 100);
    // n=4 >= min 4 -> shrink estimate 5000, source Shrunk.
    let graduated = cell_hazard(&cell(Phase::Curve, 8_000, 4), &p, 4, 4, 7_777).unwrap();
    assert_eq!(graduated.value_bps, 5_000);
    assert_eq!(graduated.source, HazardSource::Shrunk);
    assert_eq!(graduated.effective_sample, 4);
}

#[test]
fn uncertainty_shrinks_with_samples() {
    assert_eq!(uncertainty_bps(0), 10_000);
    assert_eq!(uncertainty_bps(1), 5_000);
    assert_eq!(uncertainty_bps(9), 1_000);
    assert!(uncertainty_bps(100) < uncertainty_bps(10));
}

#[test]
fn cell_hazard_propagates_phase_mismatch() {
    let p = parent(Phase::Pool, 2_000, 10);
    let c = cell(Phase::Curve, 8_000, 10);
    assert_eq!(
        cell_hazard(&c, &p, 4, 4, 1_000),
        Err(ShrinkError::PhaseMismatch)
    );
}
