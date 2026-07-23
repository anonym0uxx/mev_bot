//! §100 (CRITERION 100) hold-horizon **hazard estimator scaffolding** — a
//! REPORT / hazard-scaffold plane readout ONLY.
//!
//! Realized paper-fill outcomes are folded into per-[`CellKey`] accumulators
//! (archetype × phase × catalyst × regime). Each cell's raw hazard estimate and
//! its **phase-level parent** are handed to the audited hierarchical estimator
//! [`pump_quant_strategy::hazard_estimator::cell_hazard`], which shrinks the cell
//! toward its parent and defaults a starved cell (below the min-effective-sample
//! gate) to the fixed baseline.
//!
//! ## Phase separation is load-bearing (§21.7 / criterion 100)
//!
//! Curve-phase fills and pool-phase fills are pooled into **separate** parents and
//! never combined — a bonding-curve position's hazard is never informed by an
//! AMM-pool position's, and vice-versa. The strategy `cell_hazard` additionally
//! refuses (`ShrinkError::PhaseMismatch`) any cross-phase shrink, so the separation
//! is enforced at both layers.
//!
//! ## Report-only (digest-safe)
//!
//! This scaffold does **not** replace the live §24(e) time-stop — that calibration
//! is live-gated. Nothing here is read by a sizing/gating/exit decision and nothing
//! is journaled, so accumulating fills leaves the golden decision path
//! byte-identical (the digest is unchanged). Integer-only, bounded (§22 / §99).

use std::collections::BTreeMap;

use pump_quant_strategy::hazard_estimator::{
    cell_hazard, CellEstimate, HazardEstimate, PhaseParent, ShrinkError,
};
use pump_quant_strategy::scalp_position::{CellKey, Phase};

/// Maximum distinct cells tracked before the least-sampled is evicted (§99).
const HAZARD_CELL_CAP: usize = 4_096;

/// Basis-point scale for a hazard rate (events / samples).
const BPS: u64 = 10_000;

/// A total-order key for [`CellKey`] (whose `Phase` is not `Ord`), so the cell map
/// is a deterministic `BTreeMap` (§22). Phase is tagged 0 = curve, 1 = pool.
type CellKeyOrd = (u16, u8, u16, u16);

#[inline]
fn phase_tag(phase: Phase) -> u8 {
    match phase {
        Phase::Curve => 0,
        Phase::Pool => 1,
    }
}

#[inline]
fn ord_key(cell: &CellKey) -> CellKeyOrd {
    (
        cell.archetype,
        phase_tag(cell.phase),
        cell.catalyst,
        cell.regime,
    )
}

/// A hazard accumulator: fills observed and the count that realized the hazard
/// event (e.g. the position hit its time-stop). The raw estimate is the event rate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct HazardAccum {
    /// Fills folded in.
    n: u32,
    /// Fills that realized the hazard event.
    events: u32,
}

impl HazardAccum {
    /// Event rate in bps (0 when unsampled). Integer, saturating (§22).
    #[inline]
    fn est_bps(&self) -> u32 {
        if self.n == 0 {
            0
        } else {
            u32::try_from(u64::from(self.events).saturating_mul(BPS) / u64::from(self.n))
                .unwrap_or(BPS as u32)
        }
    }
}

/// The report-plane hazard scaffold: per-cell accumulators plus the two
/// phase-separated parents.
#[derive(Clone, Debug, Default)]
pub struct HazardScaffold {
    cells: BTreeMap<CellKeyOrd, HazardAccum>,
    /// Pooled over curve-phase fills ONLY (the curve parent).
    curve_parent: HazardAccum,
    /// Pooled over pool-phase fills ONLY (the pool parent). Never combined with
    /// `curve_parent` — phase separation is load-bearing.
    pool_parent: HazardAccum,
}

impl HazardScaffold {
    /// A fresh, empty scaffold.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one realized paper-fill outcome for `cell`: `hazard_event` is `true`
    /// when the fill realized the estimated hazard (e.g. a time-stop exit). Updates
    /// the cell accumulator AND the cell's phase-separated parent. Bounded, integer,
    /// deterministic. REPORT-plane only — never feeds a live decision.
    pub fn record_fill(&mut self, cell: CellKey, hazard_event: bool) {
        // Phase-separated parent: a curve fill only ever touches the curve parent.
        let parent = match cell.phase {
            Phase::Curve => &mut self.curve_parent,
            Phase::Pool => &mut self.pool_parent,
        };
        parent.n = parent.n.saturating_add(1);
        if hazard_event {
            parent.events = parent.events.saturating_add(1);
        }

        let key = ord_key(&cell);
        if !self.cells.contains_key(&key) && self.cells.len() >= HAZARD_CELL_CAP {
            // Evict the least-sampled cell (deterministic; report-only bounded state).
            if let Some((&weakest, _)) = self.cells.iter().min_by_key(|(_, a)| a.n) {
                self.cells.remove(&weakest);
            }
        }
        let e = self.cells.entry(key).or_default();
        e.n = e.n.saturating_add(1);
        if hazard_event {
            e.events = e.events.saturating_add(1);
        }
    }

    /// The phase-separated parent accumulator for `phase`.
    #[inline]
    fn parent_of(&self, phase: Phase) -> HazardAccum {
        match phase {
            Phase::Curve => self.curve_parent,
            Phase::Pool => self.pool_parent,
        }
    }

    /// §100 REPORT-plane readout: the hierarchical hazard estimate for `cell`.
    ///
    /// Builds the cell's raw estimate and its **phase-matched** parent, then defers
    /// to the strategy `cell_hazard` gate: a cell below `min_effective_sample`
    /// returns `baseline_bps` ([`HazardSource::Baseline`]); at/above the gate it
    /// returns the partial-pooled shrink estimate. Cannot mispool across phases —
    /// the parent is selected by `cell.phase`. Never consulted by a live decision.
    ///
    /// [`HazardSource::Baseline`]: pump_quant_strategy::hazard_estimator::HazardSource::Baseline
    pub fn cell_hazard(
        &self,
        cell: CellKey,
        k: u32,
        min_effective_sample: u32,
        baseline_bps: u32,
    ) -> Result<HazardEstimate, ShrinkError> {
        let accum = self.cells.get(&ord_key(&cell)).copied().unwrap_or_default();
        let cell_est = CellEstimate {
            phase: cell.phase,
            est_bps: accum.est_bps(),
            n: accum.n,
        };
        let parent_acc = self.parent_of(cell.phase);
        let parent = PhaseParent {
            phase: cell.phase,
            est_bps: parent_acc.est_bps(),
            n: parent_acc.n,
        };
        cell_hazard(&cell_est, &parent, k, min_effective_sample, baseline_bps)
    }

    /// Number of distinct cells tracked (bounded by [`HAZARD_CELL_CAP`]).
    #[must_use]
    pub fn tracked(&self) -> usize {
        self.cells.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pump_quant_strategy::hazard_estimator::HazardSource;

    fn cell(archetype: u16, phase: Phase) -> CellKey {
        CellKey {
            archetype,
            phase,
            catalyst: 0,
            regime: 0,
        }
    }

    #[test]
    fn below_min_sample_defaults_to_baseline() {
        let mut h = HazardScaffold::new();
        // Two fills, one hazard event — but the gate needs 10 samples.
        h.record_fill(cell(1, Phase::Curve), true);
        h.record_fill(cell(1, Phase::Curve), false);
        let est = h
            .cell_hazard(cell(1, Phase::Curve), 8, 10, 3_000)
            .expect("same-phase parent never mismatches");
        assert_eq!(est.source, HazardSource::Baseline);
        assert_eq!(est.value_bps, 3_000, "starved cell defaults to baseline");
        assert_eq!(est.effective_sample, 2);
    }

    #[test]
    fn at_min_sample_uses_shrunk_estimate() {
        let mut h = HazardScaffold::new();
        // 10 fills, all hazard events ⇒ raw est 10_000 bps; meets the gate.
        for _ in 0..10 {
            h.record_fill(cell(2, Phase::Pool), true);
        }
        let est = h
            .cell_hazard(cell(2, Phase::Pool), 8, 10, 3_000)
            .expect("same-phase parent never mismatches");
        assert_eq!(est.source, HazardSource::Shrunk);
        // Shrunk toward its own pool parent (also all-events here) ⇒ stays 10_000.
        assert_eq!(est.value_bps, 10_000);
    }

    #[test]
    fn curve_and_pool_are_never_pooled() {
        // Same archetype/catalyst/regime, DIFFERENT phase, opposite hazard rates.
        let mut h = HazardScaffold::new();
        // Curve cell: 20 fills, ALL hazard events ⇒ raw + parent ≈ 10_000 bps.
        for _ in 0..20 {
            h.record_fill(cell(7, Phase::Curve), true);
        }
        // Pool cell: 20 fills, NO hazard events ⇒ raw + parent = 0 bps.
        for _ in 0..20 {
            h.record_fill(cell(7, Phase::Pool), false);
        }
        let curve = h.cell_hazard(cell(7, Phase::Curve), 8, 10, 5_000).unwrap();
        let pool = h.cell_hazard(cell(7, Phase::Pool), 8, 10, 5_000).unwrap();
        // If the phases were pooled, both would collapse toward a shared ~5_000
        // mid. Instead each holds its own extreme — separation preserved.
        assert_eq!(
            curve.value_bps, 10_000,
            "curve hazard is not diluted by pool"
        );
        assert_eq!(pool.value_bps, 0, "pool hazard is not inflated by curve");
        assert_ne!(curve.value_bps, pool.value_bps);
    }

    #[test]
    fn starved_cell_shrinks_toward_its_own_phase_parent_only() {
        // A well-sampled curve cell builds a strong curve parent; a *starved*
        // curve cell in a different archetype defaults to baseline (not the parent),
        // and crucially a pool cell can never borrow the curve parent's mass.
        let mut h = HazardScaffold::new();
        for _ in 0..50 {
            h.record_fill(cell(3, Phase::Curve), true); // curve parent → ~10_000
        }
        // Pool cell, exactly at the gate, all clean ⇒ shrinks toward the (empty,
        // 0-sample) pool parent, never the loud curve parent.
        for _ in 0..10 {
            h.record_fill(cell(3, Phase::Pool), false);
        }
        let pool = h.cell_hazard(cell(3, Phase::Pool), 8, 10, 5_000).unwrap();
        assert!(
            pool.value_bps < 5_000,
            "pool cell must not inherit curve hazard mass"
        );
    }

    #[test]
    fn untracked_cell_defaults_to_baseline() {
        let h = HazardScaffold::new();
        let est = h.cell_hazard(cell(9, Phase::Curve), 8, 10, 4_200).unwrap();
        assert_eq!(est.source, HazardSource::Baseline);
        assert_eq!(est.value_bps, 4_200);
        assert_eq!(est.effective_sample, 0);
    }
}
