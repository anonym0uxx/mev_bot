//! `metrics` — required trading-metrics suite (constitution §54).
//!
//! §54 names a suite the frozen evaluator must compute over reconciled outcomes:
//! CVaR / tail-loss, profit factor, median return, Brier score, and calibration
//! error. `evaluator_stats` had none of these — only net-SOL, MFE capture,
//! top-k excision, markouts and the PRFS ledger. This module adds them as pure
//! offline evaluator computations over the same reconciled return vectors, plus
//! a paired `(predicted_probability, realized_outcome)` type for the probabilistic
//! metrics (Brier, calibration) fed from hazard / fill-landing / entry-score
//! predictions reconciled against realized outcomes.
//!
//! §22: integer / fixed-point only, deterministic. Returns are bps (`i64`).
//! Probabilities are parts-per-million (`ppm ∈ [0, 1_000_000]`). Ratios are bps
//! or ppm; there is no `f32`/`f64` anywhere and no wall-clock / RNG.

/// One part-per-million denominator (`1.0`).
const PPM: i128 = 1_000_000;

// ============================================================================
// CVaR / tail-loss.
// ============================================================================

/// Conditional value-at-risk (expected shortfall) over a return distribution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CvarReport {
    /// Tail level in bps (`500` == worst 5%).
    pub alpha_bps: u32,
    /// Number of returns in the tail sample.
    pub tail_n: u32,
    /// Value-at-risk: the `alpha` quantile return, bps (the tail boundary — the
    /// least-bad return still inside the tail).
    pub var_bps: i64,
    /// Conditional VaR / expected shortfall: mean of the tail returns, bps
    /// (typically negative — the average loss given we are in the bad tail).
    pub cvar_bps: i64,
}

/// CVaR / tail-loss at level `alpha_bps` over reconciled returns (§54).
///
/// Sorts returns ascending, takes the worst `k = ceil(n · alpha_bps / 10_000)`
/// (at least one), and reports the tail boundary (`var_bps`) and the mean of the
/// tail (`cvar_bps`). Because the tail is the *most negative* returns, a fat
/// left tail drives `cvar_bps` sharply negative. Deterministic; empty input or
/// `alpha_bps == 0` yields `None` (no tail is defined).
///
/// Panics if `alpha_bps > 10_000` (a tail wider than the whole sample).
pub fn cvar(returns_bps: &[i64], alpha_bps: u32) -> Option<CvarReport> {
    assert!(alpha_bps <= 10_000, "cvar: alpha_bps must be <= 10_000");
    if returns_bps.is_empty() || alpha_bps == 0 {
        return None;
    }
    let n = returns_bps.len();
    let mut sorted = returns_bps.to_vec();
    sorted.sort_unstable();
    // k = ceil(n * alpha / 10_000), clamped to [1, n].
    let k = ((n as u64 * alpha_bps as u64).div_ceil(10_000) as usize).clamp(1, n);
    let tail = &sorted[..k];
    let sum: i128 = tail.iter().map(|&v| v as i128).sum();
    let cvar_bps = (sum / k as i128) as i64;
    Some(CvarReport {
        alpha_bps,
        tail_n: k as u32,
        var_bps: tail[k - 1],
        cvar_bps,
    })
}

// ============================================================================
// Profit factor.
// ============================================================================

/// Gross profit factor over reconciled returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfitFactor {
    /// `sum(wins) · 10_000 / sum(losses)` in bps (`10_000` == break-even 1.0x).
    Bps(u64),
    /// Undefined — there were no losing returns (denominator zero).
    NoLosses,
    /// No returns at all.
    Empty,
}

/// Profit factor: total winnings over total losses, in bps (§54).
///
/// `sum(returns > 0) · 10_000 / sum(|returns < 0|)`. A value of `10_000` is
/// break-even (1.0x); `> 10_000` is net-winning gross. Zero-valued returns are
/// neither win nor loss. [`ProfitFactor::NoLosses`] when nothing lost,
/// [`ProfitFactor::Empty`] for no returns. Integer-only, deterministic.
pub fn profit_factor(returns_bps: &[i64]) -> ProfitFactor {
    if returns_bps.is_empty() {
        return ProfitFactor::Empty;
    }
    let mut wins: i128 = 0;
    let mut losses: i128 = 0; // positive magnitude
    for &r in returns_bps {
        if r > 0 {
            wins += r as i128;
        } else if r < 0 {
            losses += -(r as i128);
        }
    }
    if losses == 0 {
        return ProfitFactor::NoLosses;
    }
    let pf = (wins * 10_000) / losses;
    ProfitFactor::Bps(pf.clamp(0, u64::MAX as i128) as u64)
}

// ============================================================================
// Median return.
// ============================================================================

/// Median reconciled return in bps, or `None` for an empty sample (§54).
///
/// Even-length samples average the two central elements with integer division.
/// Deterministic (sorts a copy).
pub fn median_return_bps(returns_bps: &[i64]) -> Option<i64> {
    if returns_bps.is_empty() {
        return None;
    }
    let mut s = returns_bps.to_vec();
    s.sort_unstable();
    let n = s.len();
    let m = if n % 2 == 1 {
        s[n / 2]
    } else {
        let a = s[n / 2 - 1] as i128;
        let b = s[n / 2] as i128;
        ((a + b) / 2) as i64
    };
    Some(m)
}

// ============================================================================
// Probabilistic calibration: Brier score + expected calibration error.
// ============================================================================

/// One reconciled probabilistic prediction: a forecast and its realized outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PredictionRow {
    /// Predicted probability the event occurs, parts-per-million (`[0, 1e6]`).
    pub predicted_ppm: u32,
    /// Whether the event actually occurred.
    pub occurred: bool,
}

impl PredictionRow {
    /// Test/golden-vector constructor.
    pub fn new(predicted_ppm: u32, occurred: bool) -> Self {
        PredictionRow {
            predicted_ppm,
            occurred,
        }
    }
}

/// Brier score over reconciled predictions, in parts-per-million (§54).
///
/// `mean((p − o)²)` with `p ∈ [0,1]` and `o ∈ {0,1}`, returned in ppm so
/// `0` is a perfect forecaster and `1_000_000` is maximally wrong. Computed as
/// `mean((p_ppm − o·1e6)² / 1e6)` in `i128` — no floats. Deterministic; `None`
/// for an empty sample.
///
/// Panics if any `predicted_ppm > 1_000_000`.
pub fn brier_score_ppm(preds: &[PredictionRow]) -> Option<u32> {
    if preds.is_empty() {
        return None;
    }
    let mut sum: i128 = 0;
    for p in preds {
        assert!(
            p.predicted_ppm <= 1_000_000,
            "brier_score_ppm: predicted_ppm > 1.0"
        );
        let o_ppm: i128 = if p.occurred { PPM } else { 0 };
        let diff = p.predicted_ppm as i128 - o_ppm;
        // (diff^2) rescaled from ppm^2 back to ppm.
        sum += (diff * diff) / PPM;
    }
    Some((sum / preds.len() as i128) as u32)
}

/// Expected calibration error over reconciled predictions, in ppm (§54).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalibrationReport {
    /// Number of predictions.
    pub n: u32,
    /// Number of occupied bins.
    pub occupied_bins: u32,
    /// Expected calibration error (weighted mean bin gap), ppm.
    pub ece_ppm: u32,
    /// Maximum calibration error across bins (worst bin gap), ppm.
    pub mce_ppm: u32,
}

/// Expected & maximum calibration error over reconciled predictions (§54).
///
/// Partitions `[0,1]` into `n_bins` equal-width bins; for each occupied bin the
/// gap `|mean_predicted − empirical_frequency|` (ppm) is computed, ECE is the
/// count-weighted mean of those gaps and MCE is the worst. A well-calibrated
/// forecaster has ECE near zero. Integer-only (bin means and frequencies are ppm
/// via integer division); deterministic. `None` for an empty sample.
///
/// Panics if `n_bins == 0` or any `predicted_ppm > 1_000_000`.
pub fn calibration_error(preds: &[PredictionRow], n_bins: u32) -> Option<CalibrationReport> {
    assert!(n_bins > 0, "calibration_error: n_bins must be > 0");
    if preds.is_empty() {
        return None;
    }
    let b = n_bins as usize;
    let mut count = vec![0u64; b];
    let mut sum_pred = vec![0i128; b];
    let mut sum_occ = vec![0u64; b];

    for p in preds {
        assert!(
            p.predicted_ppm <= 1_000_000,
            "calibration_error: predicted_ppm > 1.0"
        );
        // bin index = floor(p * n_bins / 1e6), clamped to last bin at p == 1.0.
        let bin = ((p.predicted_ppm as u64 * n_bins as u64) / 1_000_000) as usize;
        let bin = bin.min(b - 1);
        count[bin] += 1;
        sum_pred[bin] += p.predicted_ppm as i128;
        if p.occurred {
            sum_occ[bin] += 1;
        }
    }

    let n = preds.len() as i128;
    let mut weighted_gap_sum: i128 = 0;
    let mut mce: i128 = 0;
    let mut occupied: u32 = 0;
    for i in 0..b {
        if count[i] == 0 {
            continue;
        }
        occupied += 1;
        let c = count[i] as i128;
        let mean_pred = sum_pred[i] / c; // ppm
        let freq = (sum_occ[i] as i128 * PPM) / c; // ppm
        let gap = (mean_pred - freq).abs();
        weighted_gap_sum += gap * c;
        if gap > mce {
            mce = gap;
        }
    }

    let ece_ppm = (weighted_gap_sum / n) as u32;
    Some(CalibrationReport {
        n: preds.len() as u32,
        occupied_bins: occupied,
        ece_ppm,
        mce_ppm: mce as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cvar_worst_tail() {
        // 10 returns, worst 20% (alpha=2000) -> 2 worst.
        let returns = vec![-900i64, -800, -100, 0, 100, 200, 300, 400, 500, 1_000];
        let r = cvar(&returns, 2_000).unwrap();
        assert_eq!(r.tail_n, 2);
        assert_eq!(r.var_bps, -800); // boundary (least-bad in tail)
        assert_eq!(r.cvar_bps, -850); // mean(-900, -800)
    }

    #[test]
    fn cvar_min_one_in_tail() {
        let returns = vec![-500i64, 100, 200];
        // alpha 1% of 3 rounds up to 1.
        let r = cvar(&returns, 100).unwrap();
        assert_eq!(r.tail_n, 1);
        assert_eq!(r.cvar_bps, -500);
        assert_eq!(r.var_bps, -500);
    }

    #[test]
    fn cvar_empty_and_zero_alpha() {
        assert!(cvar(&[], 500).is_none());
        assert!(cvar(&[100, 200], 0).is_none());
    }

    #[test]
    #[should_panic(expected = "alpha_bps")]
    fn cvar_bad_alpha_panics() {
        let _ = cvar(&[1, 2], 10_001);
    }

    #[test]
    fn profit_factor_basic() {
        // wins = 100+300 = 400, losses = 200. PF = 400*10000/200 = 20000 (2.0x).
        let returns = vec![100i64, -200, 300, 0];
        assert_eq!(profit_factor(&returns), ProfitFactor::Bps(20_000));
    }

    #[test]
    fn profit_factor_no_losses_and_empty() {
        assert_eq!(profit_factor(&[100, 200]), ProfitFactor::NoLosses);
        assert_eq!(profit_factor(&[]), ProfitFactor::Empty);
        // All zeros: no wins, no losses -> NoLosses (denominator zero).
        assert_eq!(profit_factor(&[0, 0]), ProfitFactor::NoLosses);
    }

    #[test]
    fn median_return_odd_even() {
        assert_eq!(median_return_bps(&[3, 1, 2]), Some(2));
        assert_eq!(median_return_bps(&[1, 2, 3, 4]), Some(2)); // (2+3)/2 = 2 (trunc)
        assert_eq!(median_return_bps(&[]), None);
    }

    #[test]
    fn brier_perfect_and_worst() {
        // Perfect: p=1 when occurred, p=0 when not.
        let perfect = vec![
            PredictionRow::new(1_000_000, true),
            PredictionRow::new(0, false),
        ];
        assert_eq!(brier_score_ppm(&perfect), Some(0));
        // Worst: p=1 but did not occur, p=0 but occurred -> (1)^2 each -> 1.0.
        let worst = vec![
            PredictionRow::new(1_000_000, false),
            PredictionRow::new(0, true),
        ];
        assert_eq!(brier_score_ppm(&worst), Some(1_000_000));
    }

    #[test]
    fn brier_half_prediction() {
        // p=0.5 always: (0.5)^2 = 0.25 regardless of outcome -> 250_000 ppm.
        let p = vec![
            PredictionRow::new(500_000, true),
            PredictionRow::new(500_000, false),
        ];
        assert_eq!(brier_score_ppm(&p), Some(250_000));
        assert_eq!(brier_score_ppm(&[]), None);
    }

    #[test]
    fn calibration_perfectly_calibrated() {
        // Bin at 1.0 occurs, bin at 0.0 does not -> zero gap in each bin.
        let preds = vec![
            PredictionRow::new(1_000_000, true),
            PredictionRow::new(0, false),
        ];
        let r = calibration_error(&preds, 10).unwrap();
        assert_eq!(r.ece_ppm, 0);
        assert_eq!(r.mce_ppm, 0);
        assert_eq!(r.occupied_bins, 2);
    }

    #[test]
    fn calibration_miscalibrated() {
        // Predict 0.9 four times but it never happens: gap = 0.9 in that bin.
        let preds = vec![
            PredictionRow::new(900_000, false),
            PredictionRow::new(900_000, false),
            PredictionRow::new(900_000, false),
            PredictionRow::new(900_000, false),
        ];
        let r = calibration_error(&preds, 10).unwrap();
        assert_eq!(r.occupied_bins, 1);
        assert_eq!(r.ece_ppm, 900_000);
        assert_eq!(r.mce_ppm, 900_000);
    }

    #[test]
    #[should_panic(expected = "n_bins")]
    fn calibration_zero_bins_panics() {
        let _ = calibration_error(&[PredictionRow::new(1, true)], 0);
    }
}
