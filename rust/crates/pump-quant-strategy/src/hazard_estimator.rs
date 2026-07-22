//! # hazard_estimator — hierarchical partial-pooled hazard + OOS revert gate (criterion 100)
//!
//! The hierarchical estimator behind the Section 24/48 hazard family:
//!
//! * [`shrink_within_phase`] — a cell estimate (archetype × phase × catalyst ×
//!   regime) shrinks toward its **phase-level parent** by the empirical-Bayes
//!   weight `n / (n + k)`: more cell samples → less shrinkage. Shrinkage never
//!   crosses the phase boundary — the function returns
//!   [`ShrinkError::PhaseMismatch`] if a cell and parent from different phases are
//!   combined.
//! * [`cell_hazard`] — the minimum-effective-sample gate: a cell below the gate
//!   defaults to the fixed-constant baseline; a cell at/above the gate uses the
//!   partial-pooled shrink estimate. Every result carries its effective sample
//!   size and an uncertainty band for the DecisionRecord.
//! * [`evaluate_calibration`] — the per-cell **and** pooled out-of-sample
//!   baseline-beat-or-revert gate: a cell whose adaptive calibration fails to beat
//!   the baseline reverts individually, and if the pooled comparison loses, every
//!   cell reverts.
//!
//! ## Constitution
//! §24 Hold-horizon calibration law (hierarchical partial pooling, min-effective
//! sample, per-cell + pooled Experiment #9 revert). §22 integer fixed-point
//! (hazard in bps, 0..=10_000); pure statistics, no floats, deterministic.

use crate::scalp_position::Phase;

/// A phase-level parent estimate (the shrinkage target for its cells).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhaseParent {
    /// The phase this parent summarizes.
    pub phase: Phase,
    /// Parent hazard estimate in bps (0..=10_000).
    pub est_bps: u32,
    /// Pooled sample count backing the parent.
    pub n: u32,
}

/// A single conditioning-cell estimate before shrinkage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellEstimate {
    /// The cell's phase (must match its parent's).
    pub phase: Phase,
    /// Raw cell hazard estimate in bps (0..=10_000).
    pub est_bps: u32,
    /// Cell sample count.
    pub n: u32,
}

/// Why shrinkage was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShrinkError {
    /// The cell and parent belong to different phases — shrinkage may never
    /// cross the phase boundary.
    PhaseMismatch,
}

/// Partial-pooling shrinkage toward the phase parent (leaf **he_shrink**).
///
/// Returns the shrunk estimate in bps:
/// `(n·cell + k·parent) / (n + k)`, computed in `u128` so it never overflows.
/// The cell weight `n / (n + k)` rises with sample size, so a well-sampled cell
/// barely shrinks and a starved cell collapses toward the parent. `k` is the
/// shrinkage pseudo-count (a larger `k` pools harder). With `n = 0` the result is
/// exactly the parent. Refuses if the phases differ (shrinkage never crosses the
/// phase boundary).
pub fn shrink_within_phase(
    cell: &CellEstimate,
    parent: &PhaseParent,
    k: u32,
) -> Result<u32, ShrinkError> {
    if cell.phase != parent.phase {
        return Err(ShrinkError::PhaseMismatch);
    }
    let n = cell.n as u128;
    let k = k as u128;
    let denom = n + k;
    if denom == 0 {
        // No cell samples and no pooling weight: fall back to the parent.
        return Ok(parent.est_bps);
    }
    let num = n * cell.est_bps as u128 + k * parent.est_bps as u128;
    Ok((num / denom) as u32)
}

/// Source of a produced hazard estimate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HazardSource {
    /// Cell was below the min-effective-sample gate → fixed-constant baseline.
    Baseline,
    /// Cell met the gate → partial-pooled shrink estimate.
    Shrunk,
}

/// A produced hazard estimate with its DecisionRecord metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HazardEstimate {
    /// Hazard value in bps (0..=10_000).
    pub value_bps: u32,
    /// Effective sample size backing the value.
    pub effective_sample: u32,
    /// Uncertainty band in bps (higher = less certain).
    pub uncertainty_bps: u32,
    /// Whether this came from the baseline or a shrink estimate.
    pub source: HazardSource,
}

/// Integer uncertainty band that shrinks with effective sample size.
///
/// `uncertainty = 10_000 / (n + 1)`: full uncertainty at zero samples, decaying
/// monotonically. Deterministic, no floats.
#[inline]
pub fn uncertainty_bps(effective_sample: u32) -> u32 {
    10_000 / (effective_sample as u64 + 1) as u32
}

/// The minimum-effective-sample gate over the shrink estimate (leaf **he_shrink**).
///
/// * `cell.n < min_effective_sample` → the cell defaults to `baseline_bps` (the
///   fixed constant) with [`HazardSource::Baseline`].
/// * otherwise → the [`shrink_within_phase`] estimate with [`HazardSource::Shrunk`].
///
/// Either way the result carries the cell's effective sample size and an
/// uncertainty band. Propagates [`ShrinkError::PhaseMismatch`].
pub fn cell_hazard(
    cell: &CellEstimate,
    parent: &PhaseParent,
    k: u32,
    min_effective_sample: u32,
    baseline_bps: u32,
) -> Result<HazardEstimate, ShrinkError> {
    if cell.phase != parent.phase {
        return Err(ShrinkError::PhaseMismatch);
    }
    if cell.n < min_effective_sample {
        return Ok(HazardEstimate {
            value_bps: baseline_bps,
            effective_sample: cell.n,
            uncertainty_bps: uncertainty_bps(cell.n),
            source: HazardSource::Baseline,
        });
    }
    let value_bps = shrink_within_phase(cell, parent, k)?;
    Ok(HazardEstimate {
        value_bps,
        effective_sample: cell.n,
        uncertainty_bps: uncertainty_bps(cell.n),
        source: HazardSource::Shrunk,
    })
}

// ===========================================================================
// OOS baseline-beat-or-revert gate (leaf: he_revert)
// ===========================================================================

/// An out-of-sample net-SOL comparison for one cell (or the pool).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalibResult {
    /// Cell identifier (ignored for the pooled result).
    pub cell_id: u32,
    /// Adaptive-calibration OOS net SOL, fixed-point.
    pub adaptive_net_fp: i64,
    /// Fixed-constant baseline OOS net SOL, fixed-point.
    pub baseline_net_fp: i64,
}

/// Per-cell revert decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalibDecision {
    /// Keep the adaptive calibration for this cell (it beat the baseline).
    KeepAdaptive,
    /// Revert this cell to the fixed-constant baseline.
    Revert,
}

/// Whether an adaptive result strictly beats its baseline (leaf helper).
///
/// Ties revert: adaptive must *beat* the baseline to be retained, per the
/// null-hypothesis discipline (elegance is not a reason to keep it).
#[inline]
pub fn adaptive_beats_baseline(r: &CalibResult) -> bool {
    r.adaptive_net_fp > r.baseline_net_fp
}

/// The per-cell **and** pooled baseline-beat-or-revert gate (leaf **he_revert**).
///
/// Returns one [`CalibDecision`] per input cell, in order. Rules:
/// * If the **pooled** adaptive result fails to beat the pooled baseline, **every**
///   cell reverts (global revert — the calibration is not carrying overall).
/// * Otherwise each cell is decided individually: a cell that fails to beat its own
///   baseline reverts, the rest keep the adaptive calibration.
///
/// Pure and deterministic.
pub fn evaluate_calibration(cells: &[CalibResult], pooled: &CalibResult) -> Vec<CalibDecision> {
    let pooled_keep = adaptive_beats_baseline(pooled);
    cells
        .iter()
        .map(|c| {
            if pooled_keep && adaptive_beats_baseline(c) {
                CalibDecision::KeepAdaptive
            } else {
                CalibDecision::Revert
            }
        })
        .collect()
}
