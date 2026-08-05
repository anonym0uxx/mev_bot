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

// ===========================================================================
// Rank Reversal Diagnostic (AlgoXpert 2026, López de Prado)
// ===========================================================================
/// The two objectives a strategy can be ranked on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RankObjective {
    /// Net SOL (primary objective — we maximize total SOL earned).
    NetSol,
    /// Maximum drawdown (risk objective — we minimize worst-case loss).
    MaxDd,
}

/// One ranked candidate for the rank-reversal diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RankCandidate {
    /// Candidate id (champion = 0, challengers = 1, 2, ...).
    pub id: u64,
    /// Net SOL in lamports (higher = better).
    pub netsol_lamports: i64,
    /// Maximum drawdown in lamports (lower magnitude = better; stored as
    /// negative or zero, with 0 = no drawdown).
    pub maxdd_lamports: i64,
}

/// The rank-reversal diagnostic result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RankReversalResult {
    /// Whether a rank reversal was detected (champion ranks #1 on one
    /// objective but NOT #1 on the other).
    pub reversal_detected: bool,
    /// Rank of the champion (id=0) under NetSol (1 = best).
    pub champion_netsol_rank: u32,
    /// Rank of the champion (id=0) under MaxDd (1 = best).
    pub champion_maxdd_rank: u32,
    /// The id of the candidate ranked #1 on NetSol.
    pub netsol_winner_id: u64,
    /// The id of the candidate ranked #1 on MaxDd.
    pub maxdd_winner_id: u64,
    /// Human-readable verdict.
    pub verdict: RankReversalVerdict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RankReversalVerdict {
    /// No reversal: champion wins both objectives.
    Consistent,
    /// Reversal: champion wins one objective but loses the other.
    Reversed,
}

/// Compute the rank-reversal diagnostic for a set of candidates.
///
/// Each candidate is ranked under two objectives: NetSol (higher = better)
/// and MaxDd (lower magnitude = better). A **rank reversal** occurs when the
/// champion (id=0) does NOT rank #1 on both objectives — meaning the ranking
/// is not robust to the choice of objective function.
///
/// This implements the AlgoXpert (2026) rank-reversal test: a strategy that
/// wins on one metric but loses on another is overfit to that metric's
/// peculiarities, not to a genuine edge.
#[must_use]
pub fn rank_reversal(candidates: &[RankCandidate]) -> RankReversalResult {
    if candidates.is_empty() {
        return RankReversalResult {
            reversal_detected: false,
            champion_netsol_rank: 0,
            champion_maxdd_rank: 0,
            netsol_winner_id: 0,
            maxdd_winner_id: 0,
            verdict: RankReversalVerdict::Consistent,
        };
    }

    // Rank by NetSol (descending — higher is better).
    let mut netsol_sorted: Vec<&RankCandidate> = candidates.iter().collect();
    netsol_sorted.sort_by(|a, b| b.netsol_lamports.cmp(&a.netsol_lamports));

    // Rank by MaxDd (ascending — lower magnitude is better; maxdd is stored
    // as negative or zero, so ascending = most negative first = worst first...
    // no: we want LEAST negative (closest to 0) first. So sort descending.
    let mut maxdd_sorted: Vec<&RankCandidate> = candidates.iter().collect();
    // Higher maxdd (less negative) = better. Sort descending.
    maxdd_sorted.sort_by(|a, b| b.maxdd_lamports.cmp(&a.maxdd_lamports));

    // Find champion's rank under each objective (1-based).
    let champion_netsol_rank = netsol_sorted
        .iter()
        .position(|c| c.id == 0)
        .map(|r| r as u32 + 1)
        .unwrap_or(0);
    let champion_maxdd_rank = maxdd_sorted
        .iter()
        .position(|c| c.id == 0)
        .map(|r| r as u32 + 1)
        .unwrap_or(0);

    let netsol_winner_id = netsol_sorted[0].id;
    let maxdd_winner_id = maxdd_sorted[0].id;

    // Rank reversal: the champion does NOT rank #1 on both objectives.
    let reversal_detected = champion_netsol_rank != 1 || champion_maxdd_rank != 1;

    let verdict = if reversal_detected {
        RankReversalVerdict::Reversed
    } else {
        RankReversalVerdict::Consistent
    };

    RankReversalResult {
        reversal_detected,
        champion_netsol_rank,
        champion_maxdd_rank,
        netsol_winner_id,
        maxdd_winner_id,
        verdict,
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

    // ===== Rank Reversal Diagnostic Tests =====

    #[test]
    fn rank_reversal_no_reversal_when_champion_wins_both() {
        let candidates = vec![
            RankCandidate { id: 0, netsol_lamports: 500_000, maxdd_lamports: -10_000 },
            RankCandidate { id: 1, netsol_lamports: 300_000, maxdd_lamports: -50_000 },
            RankCandidate { id: 2, netsol_lamports: 100_000, maxdd_lamports: -80_000 },
        ];
        let r = rank_reversal(&candidates);
        assert!(!r.reversal_detected);
        assert_eq!(r.champion_netsol_rank, 1);
        assert_eq!(r.champion_maxdd_rank, 1);
        assert_eq!(r.netsol_winner_id, 0);
        assert_eq!(r.maxdd_winner_id, 0);
        assert_eq!(r.verdict, RankReversalVerdict::Consistent);
    }

    #[test]
    fn rank_reversal_detected_when_champion_wins_netsol_but_loses_maxdd() {
        let candidates = vec![
            RankCandidate { id: 0, netsol_lamports: 500_000, maxdd_lamports: -100_000 },
            RankCandidate { id: 1, netsol_lamports: 300_000, maxdd_lamports: -10_000 },
        ];
        let r = rank_reversal(&candidates);
        assert!(r.reversal_detected);
        assert_eq!(r.champion_netsol_rank, 1);
        assert_eq!(r.champion_maxdd_rank, 2);
        assert_eq!(r.netsol_winner_id, 0);
        assert_eq!(r.maxdd_winner_id, 1);
        assert_eq!(r.verdict, RankReversalVerdict::Reversed);
    }

    #[test]
    fn rank_reversal_detected_when_champion_loses_both() {
        let candidates = vec![
            RankCandidate { id: 0, netsol_lamports: 100_000, maxdd_lamports: -80_000 },
            RankCandidate { id: 1, netsol_lamports: 500_000, maxdd_lamports: -10_000 },
            RankCandidate { id: 2, netsol_lamports: 300_000, maxdd_lamports: -30_000 },
        ];
        let r = rank_reversal(&candidates);
        assert!(r.reversal_detected);
        assert_eq!(r.champion_netsol_rank, 3);
        assert_eq!(r.champion_maxdd_rank, 3);
        assert_eq!(r.verdict, RankReversalVerdict::Reversed);
    }

    #[test]
    fn rank_reversal_empty_candidates() {
        let r = rank_reversal(&[]);
        assert!(!r.reversal_detected);
        assert_eq!(r.verdict, RankReversalVerdict::Consistent);
    }

    #[test]
    fn rank_reversal_single_candidate_no_reversal() {
        let candidates = vec![
            RankCandidate { id: 0, netsol_lamports: 200_000, maxdd_lamports: -20_000 },
        ];
        let r = rank_reversal(&candidates);
        assert!(!r.reversal_detected);
        assert_eq!(r.champion_netsol_rank, 1);
        assert_eq!(r.champion_maxdd_rank, 1);
    }
}
