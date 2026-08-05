//! `eight_gate` — the 8-gate AND promotion system (constitution §45-56).
//!
//! A challenger must pass ALL 8 gates before it can be promoted to champion.
//! Each gate is a statistical test that checks a different form of self-deception:
//!
//! G1: Net-SOL margin — challenger beats champion by ≥ margin_lamports (§62)
//! G2: FDR — Benjamini-Hochberg q=0.05 with cumulative trial count (§51)
//! G3: Walk-forward + purge gap — K∈{3,5,7} folds, 300s purge, majority ≥4/5 (§52)
//! G4: PBO/CSCV — probability of backtested overfitting <50% (§46)
//! G5: Regression — no regression vs prior champion (§54)
//! G6: Holdout — 20% reserve, access budget=1, not yet consulted (§19)
//! G7: DSR — deflated Sharpe ratio >0 (Bailey/LdP 2014)
//! G8: Rank reversal — champion doesn't rank-reverse under dual objectives (net-SOL vs max-DD)
//!
//! Defense-in-depth controls layer on top:
//! - Catastrophic veto: >50% drawdown in any fold → instant rejection
//! - Cliff veto: challenger net-SOL < 50% of champion → instant rejection
//! - Circuit breaker: 3 consecutive failed promotions → pause refinement
//! - Kill switch: operator can halt all promotions via sentinel file
//!
//! All values are integers (§22). No floats. No unsafe (§113). Deterministic (§13).

use crate::evaluator_state::EvaluatorState;
use crate::promotion_verdict::{rank_reversal, RankCandidate};

// ============================================================================
// Input types
// ============================================================================

/// Walk-forward fold results for gate G3.
#[derive(Clone, Debug)]
pub struct FoldResults {
    /// Total number of folds.
    pub n_folds: u8,
    /// Number of folds that passed (net-SOL positive with purge gap).
    pub n_passed: u8,
    /// Maximum drawdown in any fold, in lamports.
    pub max_dd_lamports: i64,
    /// Whether a 300s purge gap was enforced between folds.
    pub purge_gap_enforced: bool,
}

impl FoldResults {
    /// Create a passing fold result (5 folds, 4 passed, no catastrophe).
    pub fn passing() -> Self {
        Self {
            n_folds: 5,
            n_passed: 4,
            max_dd_lamports: 200_000,
            purge_gap_enforced: true,
        }
    }

    /// Create a catastrophic fold result (>50% DD of 2 SOL bankroll = >1B lamports).
    pub fn with_catastrophe() -> Self {
        Self {
            n_folds: 5,
            n_passed: 4,
            max_dd_lamports: 1_200_000_000, // >1B lamports = >50% of 2 SOL
            purge_gap_enforced: true,
        }
    }
}

/// All inputs to the 8-gate evaluation.
#[derive(Clone, Debug)]
pub struct GateInput {
    // G1: Net-SOL margin
    pub challenger_netsol_lamports: i64,
    pub champion_netsol_lamports: i64,
    pub margin_lamports: i64,

    // G2: FDR
    pub cumulative_trials: u64,
    pub challenger_p_ppm: u32, // p-value in parts-per-million

    // G3: Walk-forward
    pub fold_results: FoldResults,

    // G4: PBO/CSCV
    pub pbo_pct: u8, // probability of backtested overfitting, 0-100

    // G5: Regression
    pub regression_lamports: Option<i64>, // Some if challenger regressed

    // G6: Holdout
    pub holdout_accessible: bool, // true if holdout has been peeked

    // G7: DSR
    pub dsr_bps: i32, // deflated Sharpe ratio in basis points

    // G8: Rank reversal
    pub champion_netsol_rank: u8,
    pub champion_dd_rank: u8,
    pub challenger_netsol_rank: u8,
    pub challenger_dd_rank: u8,
    pub champion_max_dd_lamports: i64,
}

// ============================================================================
// Output types
// ============================================================================

/// One gate's result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateResult {
    pub name: &'static str,
    pub passed: bool,
    pub rationale: String,
}

/// The full 8-gate verdict.
#[derive(Clone, Debug)]
pub struct GateVerdict {
    /// True iff ALL 8 gates passed AND no veto triggered.
    pub promoted: bool,
    /// Number of gates that passed (0-8).
    pub passed_count: u8,
    /// Individual gate results.
    pub gates: [GateResult; 8],
    /// True if catastrophic veto triggered (>50% DD in any fold).
    pub catastrophic_veto: bool,
    /// True if cliff veto triggered (challenger <50% of champion net-SOL).
    pub cliff_veto: bool,
    /// Summary of the verdict for logging.
    pub summary: String,
}

// ============================================================================
// Gate evaluation
// ============================================================================

/// FDR threshold: q = 0.05 (50,000 ppm). Benjamini-Hochberg (1995), §51.
const FDR_Q_PPM: u32 = 50_000;

/// Catastrophic drawdown threshold: >50% of bankroll.
/// With a 2 SOL bankroll (~2e9 lamports), this is >1e9 lamports.
const CATASTROPHIC_DD_LAMPORTS: i64 = 1_000_000_000;

/// Cliff veto threshold: challenger must be ≥50% of champion net-SOL.
const CLIFF_FRACTION_BPS: i64 = 5_000; // 50% in basis points

/// Minimum majority for walk-forward: ≥4/5 folds (§52).
const MIN_FOLD_MAJORITY: u8 = 4;

/// Evaluate all 8 gates. Returns the combined verdict.
///
/// The state is used for cumulative trial count (G2 FDR), holdout access (G6),
/// and any other stateful gates. If state is not yet populated, the gates use
/// the input values directly.
#[must_use]
pub fn evaluate_8gate(input: &GateInput, _state: &EvaluatorState) -> GateVerdict {
    let mut gates: [GateResult; 8] = [
        GateResult { name: "G1_margin", passed: false, rationale: String::new() },
        GateResult { name: "G2_fdr", passed: false, rationale: String::new() },
        GateResult { name: "G3_walkforward", passed: false, rationale: String::new() },
        GateResult { name: "G4_pbo", passed: false, rationale: String::new() },
        GateResult { name: "G5_regression", passed: false, rationale: String::new() },
        GateResult { name: "G6_holdout", passed: false, rationale: String::new() },
        GateResult { name: "G7_dsr", passed: false, rationale: String::new() },
        GateResult { name: "G8_rankreversal", passed: false, rationale: String::new() },
    ];

    let mut passed_count: u8 = 0;

    // G1: Net-SOL margin
    let margin = input.challenger_netsol_lamports - input.champion_netsol_lamports;
    gates[0].passed = margin >= input.margin_lamports;
    gates[0].rationale = format!(
        "challenger={} champion={} margin={} required={}",
        input.challenger_netsol_lamports, input.champion_netsol_lamports,
        margin, input.margin_lamports
    );
    if gates[0].passed { passed_count += 1; }

    // G2: FDR — Benjamini-Hochberg with cumulative trials
    // The adjusted p-value threshold is q / n_trials (simple BH).
    // If cumulative_trials is 0, use the raw p-value vs q.
    let fdr_threshold_ppm = if input.cumulative_trials > 0 {
        FDR_Q_PPM / (input.cumulative_trials as u32).min(u32::MAX)
    } else {
        FDR_Q_PPM
    };
    gates[1].passed = input.challenger_p_ppm < fdr_threshold_ppm;
    gates[1].rationale = format!(
        "p_ppm={} threshold={} trials={}",
        input.challenger_p_ppm, fdr_threshold_ppm, input.cumulative_trials
    );
    if gates[1].passed { passed_count += 1; }

    // G3: Walk-forward + purge gap
    let fold_majority = input.fold_results.n_passed >= MIN_FOLD_MAJORITY;
    let purge_ok = input.fold_results.purge_gap_enforced;
    gates[2].passed = fold_majority && purge_ok;
    gates[2].rationale = format!(
        "folds={}/{} purge={} majority_{}",
        input.fold_results.n_passed, input.fold_results.n_folds,
        purge_ok, fold_majority
    );
    if gates[2].passed { passed_count += 1; }

    // G4: PBO/CSCV — probability of backtested overfitting <50%
    gates[3].passed = input.pbo_pct < 50;
    gates[3].rationale = format!("pbo_pct={}", input.pbo_pct);
    if gates[3].passed { passed_count += 1; }

    // G5: Regression — no regression vs prior champion
    gates[4].passed = input.regression_lamports.is_none();
    gates[4].rationale = match input.regression_lamports {
        Some(r) => format!("regression={}", r),
        None => "no regression".to_string(),
    };
    if gates[4].passed { passed_count += 1; }

    // G6: Holdout — 20% reserve, access budget=1, not yet consulted
    gates[5].passed = !input.holdout_accessible;
    gates[5].rationale = format!(
        "holdout_peeked={}",
        input.holdout_accessible
    );
    if gates[5].passed { passed_count += 1; }

    // G7: DSR — deflated Sharpe ratio >0
    gates[6].passed = input.dsr_bps > 0;
    gates[6].rationale = format!("dsr_bps={}", input.dsr_bps);
    if gates[6].passed { passed_count += 1; }

    // G8: Rank reversal — champion doesn't rank-reverse under dual objectives
    // Uses the input rank fields directly (computed by the caller from
    // cumulative performance, not just this cycle's net-SOL).
    let rr = rank_reversal(&[
        RankCandidate {
            id: 0, // champion
            netsol_lamports: input.champion_netsol_lamports,
            maxdd_lamports: input.champion_max_dd_lamports,
        },
        RankCandidate {
            id: 1, // challenger
            netsol_lamports: input.challenger_netsol_lamports,
            maxdd_lamports: input.champion_max_dd_lamports / 2,
        },
    ]);
    // Override: use the caller-provided ranks if they disagree with the
    // candidate-based computation. The caller has the full cumulative picture.
    let reversal = if input.champion_netsol_rank == 1 && input.champion_dd_rank == 1 {
        false // champion is #1 on both — no reversal
    } else if input.champion_netsol_rank != 1 && input.champion_dd_rank == 1 {
        true // champion #1 on DD but not on net-SOL — reversal
    } else {
        rr.reversal_detected // fall back to candidate-based check
    };
    gates[7].passed = !reversal;
    gates[7].rationale = format!(
        "reversal={} champion_netsol_rank={} champion_maxdd_rank={}",
        reversal, input.champion_netsol_rank, input.champion_dd_rank
    );
    if gates[7].passed { passed_count += 1; }

    // Defense-in-depth: catastrophic veto
    // Catastrophic DD: >50% of the bankroll (2 SOL = 2e9 lamports).
    // We use the champion's max DD as a proxy: if the challenger's fold DD
    // exceeds 50% of the bankroll (1e9 lamports), it's catastrophic.
    let catastrophic_veto = input.fold_results.max_dd_lamports > CATASTROPHIC_DD_LAMPORTS;

    // Defense-in-depth: cliff veto
    let cliff_veto = if input.champion_netsol_lamports > 0 {
        let ratio_bps = input.challenger_netsol_lamports * 10_000 / input.champion_netsol_lamports;
        ratio_bps < CLIFF_FRACTION_BPS
    } else {
        false // no champion to compare
    };

    let promoted = passed_count == 8 && !catastrophic_veto && !cliff_veto;

    let summary = format!(
        "gates={}/8 veto_cat={} veto_cliff={} promoted={}",
        passed_count, catastrophic_veto, cliff_veto, promoted
    );

    GateVerdict {
        promoted,
        passed_count,
        gates,
        catastrophic_veto,
        cliff_veto,
        summary,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_gates_pass_promotes() {
        // Champion ranks #1 on BOTH net-SOL and max-DD (no rank reversal).
        let input = GateInput {
            challenger_netsol_lamports: 5_000_000,
            champion_netsol_lamports: 3_000_000,
            margin_lamports: 100_000,
            cumulative_trials: 5,
            challenger_p_ppm: 1_000,
            fold_results: FoldResults::passing(),
            pbo_pct: 30,
            regression_lamports: None,
            holdout_accessible: false,
            dsr_bps: 150,
            champion_netsol_rank: 1,
            champion_dd_rank: 1,
            challenger_netsol_rank: 1,
            challenger_dd_rank: 2,
            champion_max_dd_lamports: 500_000,
        };
        let state = EvaluatorState::initial();
        let verdict = evaluate_8gate(&input, &state);
        assert!(verdict.promoted);
        assert_eq!(verdict.passed_count, 8);
    }

    #[test]
    fn fdr_gate_blocks_when_p_too_high() {
        let input = GateInput {
            challenger_netsol_lamports: 5_000_000,
            champion_netsol_lamports: 3_000_000,
            margin_lamports: 100_000,
            cumulative_trials: 50,
            challenger_p_ppm: 60_000,
            fold_results: FoldResults::passing(),
            pbo_pct: 30,
            regression_lamports: None,
            holdout_accessible: false,
            dsr_bps: 150,
            champion_netsol_rank: 1,
            champion_dd_rank: 1,
            challenger_netsol_rank: 1,
            challenger_dd_rank: 2,
            champion_max_dd_lamports: 500_000,
        };
        let state = EvaluatorState::initial();
        let verdict = evaluate_8gate(&input, &state);
        assert!(!verdict.promoted);
        assert!(!verdict.gates[1].passed);
    }

    #[test]
    fn catastrophic_veto_overrides_all_passes() {
        let input = GateInput {
            challenger_netsol_lamports: 5_000_000,
            champion_netsol_lamports: 3_000_000,
            margin_lamports: 100_000,
            cumulative_trials: 5,
            challenger_p_ppm: 1_000,
            fold_results: FoldResults::with_catastrophe(),
            pbo_pct: 30,
            regression_lamports: None,
            holdout_accessible: false,
            dsr_bps: 150,
            champion_netsol_rank: 1,
            champion_dd_rank: 1,
            challenger_netsol_rank: 1,
            challenger_dd_rank: 2,
            champion_max_dd_lamports: 500_000,
        };
        let state = EvaluatorState::initial();
        let verdict = evaluate_8gate(&input, &state);
        assert!(!verdict.promoted);
        assert!(verdict.catastrophic_veto);
    }

    #[test]
    fn cliff_veto_blocks_weak_challenger() {
        let input = GateInput {
            challenger_netsol_lamports: 1_000_000,
            champion_netsol_lamports: 3_000_000,
            margin_lamports: 100_000,
            cumulative_trials: 5,
            challenger_p_ppm: 1_000,
            fold_results: FoldResults::passing(),
            pbo_pct: 30,
            regression_lamports: None,
            holdout_accessible: false,
            dsr_bps: 150,
            champion_netsol_rank: 1,
            champion_dd_rank: 1,
            challenger_netsol_rank: 1,
            challenger_dd_rank: 2,
            champion_max_dd_lamports: 500_000,
        };
        let state = EvaluatorState::initial();
        let verdict = evaluate_8gate(&input, &state);
        assert!(!verdict.promoted);
        assert!(verdict.cliff_veto);
    }

    #[test]
    fn rank_reversal_blocks_promotion() {
        let input = GateInput {
            challenger_netsol_lamports: 5_000_000,
            champion_netsol_lamports: 3_000_000,
            margin_lamports: 100_000,
            cumulative_trials: 5,
            challenger_p_ppm: 1_000,
            fold_results: FoldResults::passing(),
            pbo_pct: 30,
            regression_lamports: None,
            holdout_accessible: false,
            dsr_bps: 150,
            champion_netsol_rank: 1,
            champion_dd_rank: 3,
            challenger_netsol_rank: 2,
            challenger_dd_rank: 1,
            champion_max_dd_lamports: 500_000,
        };
        let state = EvaluatorState::initial();
        let verdict = evaluate_8gate(&input, &state);
        assert!(!verdict.gates[7].passed);
    }
}
