//! `promotion_verdict` — the combined §51 promotion-blocking statistical verdict.
//!
//! §51 names the frozen evaluator as the authority that must clear TWO
//! multiple-testing gates before a challenger is promoted: Benjamini–Hochberg FDR
//! control over the family of (challenger-vs-baseline) p-values ([`crate::fdr`]),
//! and PBO / CSCV overfitting control over the block-split performance matrix
//! ([`crate::overfitting`]). This module folds both into one
//! [`PromotionStatisticalVerdict`] that `pump-quant-app::authority` consults —
//! this crate *exposes* the verdict; it never calls the authority.
//!
//! Fail-closed (§51, §55): an inadmissible CSCV matrix does not silently pass —
//! it *blocks*, because a promotion whose overfitting cannot even be measured has
//! not cleared the gate. Integer-only, deterministic (§22): the underlying leaves
//! carry no floats and this fold adds none.

use crate::fdr::{self, Hypothesis};
use crate::overfitting::{self, CscvError};

/// Why a promotion was (not) blocked by the statistical gates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromotionBlockReason {
    /// Neither gate blocked — the challenger cleared FDR and PBO.
    Clear,
    /// The candidate did not survive BH-FDR family correction.
    FdrOnly,
    /// PBO / CSCV overfitting was at/above threshold (or unmeasurable).
    PboOnly,
    /// Both gates blocked.
    Both,
}

/// The combined §51 verdict the authority consults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PromotionStatisticalVerdict {
    /// True iff BH-FDR blocks the promotion (candidate not among discoveries).
    pub fdr_blocks: bool,
    /// True iff PBO/CSCV blocks the promotion (overfit at/above threshold, or
    /// the matrix was inadmissible so overfitting could not be ruled out).
    pub pbo_blocks: bool,
    /// PBO in bps, or `None` when the CSCV matrix was inadmissible.
    pub pbo_bps: Option<u32>,
    /// Human-facing block reason.
    pub reason: PromotionBlockReason,
}

impl PromotionStatisticalVerdict {
    /// True iff the promotion is blocked by *either* gate.
    pub fn blocks(&self) -> bool {
        self.fdr_blocks || self.pbo_blocks
    }
}

/// Fold BH-FDR and PBO/CSCV into a single promotion-blocking verdict (§51).
///
/// `family` is the set of (challenger-vs-baseline) hypotheses and `candidate_id`
/// the specific challenger whose promotion is at stake; it is FDR-blocked unless
/// it is among the Benjamini–Hochberg discoveries at `alpha_ppm`. `perf` is the
/// candidate-strategies × time-blocks performance matrix; PBO is computed via
/// CSCV and blocks when `pbo_bps >= pbo_threshold_bps`. An inadmissible matrix
/// ([`CscvError`]) is treated as a block with `pbo_bps == None` — fail-closed,
/// never a silent pass. Pure, deterministic.
pub fn promotion_verdict(
    family: &[Hypothesis],
    alpha_ppm: u32,
    candidate_id: u64,
    perf: &[Vec<i64>],
    pbo_threshold_bps: u32,
) -> PromotionStatisticalVerdict {
    let fdr_blocks = fdr::blocks_promotion(family, alpha_ppm, candidate_id);

    let (pbo_blocks, pbo_bps) = match overfitting::pbo_cscv(perf) {
        Ok(report) => (
            report.blocks_promotion(pbo_threshold_bps),
            Some(report.pbo_bps),
        ),
        Err(_e) => {
            // Inadmissible matrix: overfitting could not be measured -> block.
            let _e: CscvError = _e;
            (true, None)
        }
    };

    let reason = match (fdr_blocks, pbo_blocks) {
        (false, false) => PromotionBlockReason::Clear,
        (true, false) => PromotionBlockReason::FdrOnly,
        (false, true) => PromotionBlockReason::PboOnly,
        (true, true) => PromotionBlockReason::Both,
    };

    PromotionStatisticalVerdict {
        fdr_blocks,
        pbo_blocks,
        pbo_bps,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skilled_perf() -> Vec<Vec<i64>> {
        // Trial 0 dominates -> PBO 0.
        vec![
            vec![100, 100, 100, 100],
            vec![10, 10, 10, 10],
            vec![20, 20, 20, 20],
            vec![30, 30, 30, 30],
        ]
    }

    fn noise_perf() -> Vec<Vec<i64>> {
        // Mirror-image flip-flop -> PBO 10000.
        vec![vec![100, -100], vec![-100, 100]]
    }

    #[test]
    fn clear_when_discovered_and_low_pbo() {
        let fam = vec![Hypothesis::new(1, 5_000), Hypothesis::new(2, 500_000)];
        let v = promotion_verdict(&fam, 50_000, 1, &skilled_perf(), 5_000);
        assert!(!v.blocks());
        assert_eq!(v.reason, PromotionBlockReason::Clear);
        assert_eq!(v.pbo_bps, Some(0));
    }

    #[test]
    fn fdr_blocks_when_candidate_not_discovered() {
        let fam = vec![Hypothesis::new(1, 5_000), Hypothesis::new(2, 500_000)];
        // candidate 2 (p=0.5) is not discovered.
        let v = promotion_verdict(&fam, 50_000, 2, &skilled_perf(), 5_000);
        assert!(v.fdr_blocks);
        assert!(!v.pbo_blocks);
        assert_eq!(v.reason, PromotionBlockReason::FdrOnly);
        assert!(v.blocks());
    }

    #[test]
    fn pbo_blocks_on_high_overfitting() {
        let fam = vec![Hypothesis::new(1, 5_000)];
        let v = promotion_verdict(&fam, 50_000, 1, &noise_perf(), 5_000);
        assert!(!v.fdr_blocks);
        assert!(v.pbo_blocks);
        assert_eq!(v.pbo_bps, Some(10_000));
        assert_eq!(v.reason, PromotionBlockReason::PboOnly);
    }

    #[test]
    fn both_gates_block() {
        let fam = vec![Hypothesis::new(1, 5_000), Hypothesis::new(2, 500_000)];
        let v = promotion_verdict(&fam, 50_000, 2, &noise_perf(), 5_000);
        assert!(v.fdr_blocks && v.pbo_blocks);
        assert_eq!(v.reason, PromotionBlockReason::Both);
    }

    #[test]
    fn inadmissible_matrix_fails_closed() {
        let fam = vec![Hypothesis::new(1, 5_000)];
        // single-row matrix -> TooFewTrials -> pbo_blocks, pbo_bps None.
        let v = promotion_verdict(&fam, 50_000, 1, &[vec![1, 2]], 5_000);
        assert!(v.pbo_blocks);
        assert_eq!(v.pbo_bps, None);
        assert!(v.blocks());
    }

    #[test]
    fn empty_family_blocks_fdr() {
        // No hypotheses -> candidate cannot be discovered -> FDR blocks.
        let v = promotion_verdict(&[], 50_000, 1, &skilled_perf(), 5_000);
        assert!(v.fdr_blocks);
    }

    #[test]
    fn deterministic_repeat() {
        let fam = vec![Hypothesis::new(1, 5_000), Hypothesis::new(2, 20_000)];
        let a = promotion_verdict(&fam, 50_000, 1, &skilled_perf(), 5_000);
        let b = promotion_verdict(&fam, 50_000, 1, &skilled_perf(), 5_000);
        assert_eq!(a, b);
    }
}
