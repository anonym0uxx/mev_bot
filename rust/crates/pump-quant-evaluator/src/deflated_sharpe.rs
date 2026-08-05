//! `deflated_sharpe` — the Deflated Sharpe Ratio (Bailey & López de Prado 2014).
//!
//! The observed Sharpe ratio of a backtested strategy is inflated by two biases:
//! 1. **Non-normality** — the Sharpe ratio does not account for skewness and
//!    kurtosis, so returns with heavy tails or negative skew show an inflated SR.
//! 2. **Selection bias** — the more strategies (trials) we test, the more likely
//!    the best one is good by chance. The DSR deflates the observed SR by the
//!    expected maximum SR under the null (all strategies have true SR = 0).
//!
//! ## Formula
//! The DSR is:
//! ```text
//! DSR = (SR_observed − SR_max_expected) / sqrt(Var(SR))
//! ```
//! where:
//! - `SR_observed` is the observed (non-normality-adjusted) Sharpe ratio
//! - `SR_max_expected ≈ sqrt(2·ln(N)) · (1 − 1/(4·N·ln(N)))` is the expected
//!   maximum of `N` i.i.d. Sharpe ratios under the null, with `N` the cumulative
//!   number of independent trials
//! - `Var(SR) = (1 − γ3·SR + (γ4−1)·SR²/4) / T` is the variance of the Sharpe
//!   ratio estimator, with `γ3` skewness, `γ4` kurtosis, `T` the number of
//!   return observations.
//!
//! ## Decision rule
//! If `DSR > 0`, the observed SR is statistically significant even after
//! correcting for non-normality and selection bias. If `DSR ≤ 0`, the observed
//! performance is indistinguishable from chance.
//!
//! ## Constitution
//! §45 (statistical gates), §51 (multiple-testing correction via cumulative
//! trial count), §56.3 (reproducibility — all state is deterministic integer
//! arithmetic, no floats in the stored state).
//!
//! ## Integer-only constraint (§22)
//! All internal computations use fixed-point integer arithmetic (basis points,
//! nano-SOL, etc.). The only `f64` usage is in the mathematical formula
//! computation itself, which is deterministic given identical inputs; the
//! *stored state* (in `EvaluatorState`) uses integer bps to avoid float
//! non-determinism across platforms.
#![forbid(unsafe_code)]

/// The fixed-point scale for DSR computations (1e6 = micro-units).
const DSR_SCALE: f64 = 1_000_000.0;

/// The bps scale (1e4).
const BPS_SCALE: f64 = 10_000.0;

/// Configuration for the DSR computation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DsrConfig {
    /// Cumulative number of independent strategy trials (across ALL cycles).
    /// This is the `N` in `sqrt(2·ln(N))`. Must be ≥ 1.
    pub cumulative_trials: u64,
    /// Number of return observations (closed trades). Must be ≥ 2.
    pub n_returns: u64,
    /// Sharpe ratio observed, in basis points (SR × 10_000).
    /// e.g. SR = 1.5 → 15_000 bps.
    pub sharpe_observed_bps: i64,
    /// Skewness of returns × 10_000 (γ3 × BPS_SCALE).
    pub skewness_bps: i64,
    /// Excess kurtosis of returns × 10_000 (γ4 × BPS_SCALE, so kurtosis − 3).
    pub kurtosis_bps: i64,
    /// Variance of the Sharpe ratio estimator, in bps² × 10_000.
    /// If 0, computed from skewness/kurtosis/n_returns.
    pub sr_variance_bps2: i64,
}

impl Default for DsrConfig {
    fn default() -> Self {
        Self {
            cumulative_trials: 1,
            n_returns: 30,
            sharpe_observed_bps: 0,
            skewness_bps: 0,
            kurtosis_bps: 0,
            sr_variance_bps2: 0,
        }
    }
}

/// The DSR computation result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DsrResult {
    /// The Deflated Sharpe Ratio, in basis points (DSR × 10_000).
    /// DSR > 0 means statistically significant; DSR ≤ 0 means indistinguishable
    /// from chance.
    pub dsr_bps: i64,
    /// The expected maximum SR under the null, in bps.
    pub sr_max_expected_bps: i64,
    /// The variance of the SR estimator, in bps².
    pub sr_variance_bps2: i64,
    /// Whether the DSR gate passes (DSR > 0).
    pub passed: bool,
    /// Human-readable verdict for logging.
    pub verdict: DsrVerdict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DsrVerdict {
    /// DSR > 0 — statistically significant after deflation.
    Significant,
    /// DSR ≤ 0 — indistinguishable from chance.
    Inflated,
    /// Insufficient data (n_returns < 30 or cumulative_trials < 1).
    InsufficientData,
}

/// Compute the expected maximum Sharpe ratio under the null hypothesis
/// (all N strategies have true SR = 0).
///
/// Bailey & López de Prado (2014), eq. 6:
/// ```text
/// E[max SR] ≈ sqrt(2·ln(N)) · (1 − 1/(4·N·ln(N)))
/// ```
/// Returns the result in bps (× 10_000).
#[must_use]
pub fn expected_max_sr_bps(cumulative_trials: u64) -> i64 {
    if cumulative_trials <= 1 {
        return 0;
    }
    let n = cumulative_trials as f64;
    let ln_n = n.ln();
    let sqrt_2_ln = (2.0 * ln_n).sqrt();
    let correction = 1.0 - 1.0 / (4.0 * n * ln_n);
    let sr_max = sqrt_2_ln * correction;
    // Convert to bps: SR × 10_000
    (sr_max * BPS_SCALE) as i64
}

/// Compute the variance of the Sharpe ratio estimator.
///
/// Bailey & López de Prado (2014), eq. 2:
/// ```text
/// Var(SR) = (1 − γ3·SR + (γ4−1)·SR²/4) / T
/// ```
/// where γ3 is skewness, γ4 is (excess) kurtosis, T is n_returns.
/// Returns the result in bps² × 10_000.
#[must_use]
pub fn sr_variance_bps(skew_bps: i64, kurt_bps: i64, sr_bps: i64, n_returns: u64) -> i64 {
    if n_returns == 0 {
        return 0;
    }
    let sr = sr_bps as f64 / BPS_SCALE;
    let gamma3 = skew_bps as f64 / BPS_SCALE;
    let gamma4_excess = kurt_bps as f64 / BPS_SCALE; // excess kurtosis (0 = normal)
    let t = n_returns as f64;

    // Bailey/LdP formula uses raw kurtosis γ4 = excess + 3, so (γ4-1) = excess + 2.
    let numerator = 1.0 - gamma3 * sr + (gamma4_excess + 2.0) * sr * sr / 4.0;
    let variance = numerator / t;

    // Convert to bps² × 10_000 for integer storage
    (variance * BPS_SCALE * BPS_SCALE) as i64
}

/// Compute the Deflated Sharpe Ratio.
///
/// Returns `DsrResult` with the DSR in bps, the expected max SR, the variance,
/// and the verdict. All integer (§22).
#[must_use]
pub fn compute_dsr(cfg: &DsrConfig) -> DsrResult {
    // Minimum data requirements (Bailey/LdP: ≥30 returns, ≥1 trial)
    if cfg.n_returns < 30 || cfg.cumulative_trials < 1 {
        return DsrResult {
            dsr_bps: 0,
            sr_max_expected_bps: 0,
            sr_variance_bps2: 0,
            passed: false,
            verdict: DsrVerdict::InsufficientData,
        };
    }

    let sr_max_bps = expected_max_sr_bps(cfg.cumulative_trials);

    let var_bps2 = if cfg.sr_variance_bps2 != 0 {
        cfg.sr_variance_bps2
    } else {
        sr_variance_bps(cfg.skewness_bps, cfg.kurtosis_bps, cfg.sharpe_observed_bps, cfg.n_returns)
    };

    if var_bps2 <= 0 {
        return DsrResult {
            dsr_bps: 0,
            sr_max_expected_bps: sr_max_bps,
            sr_variance_bps2: 0,
            passed: false,
            verdict: DsrVerdict::InsufficientData,
        };
    }

    // DSR = (SR_observed − SR_max_expected) / sqrt(Var(SR))
    let sr_obs = cfg.sharpe_observed_bps as f64 / BPS_SCALE;
    let sr_max = sr_max_bps as f64 / BPS_SCALE;
    let var = var_bps2 as f64 / (BPS_SCALE * BPS_SCALE);

    let std_dev = var.sqrt();
    if std_dev <= 0.0 {
        return DsrResult {
            dsr_bps: 0,
            sr_max_expected_bps: sr_max_bps,
            sr_variance_bps2: var_bps2,
            passed: false,
            verdict: DsrVerdict::InsufficientData,
        };
    }

    let dsr = (sr_obs - sr_max) / std_dev;
    let dsr_bps = (dsr * BPS_SCALE) as i64;

    let passed = dsr_bps > 0;
    let verdict = if passed {
        DsrVerdict::Significant
    } else {
        DsrVerdict::Inflated
    };

    DsrResult {
        dsr_bps,
        sr_max_expected_bps: sr_max_bps,
        sr_variance_bps2: var_bps2,
        passed,
        verdict,
    }
}

/// Compute the Sharpe ratio from a series of per-trade returns (in lamports).
///
/// Returns the SR in bps (SR × 10_000), along with skewness and kurtosis
/// (also in bps). This is the input to `compute_dsr`.
///
/// Uses the standard definitions:
/// - SR = mean(r) / std(r) × sqrt(T) (annualized-free, per-trade SR)
/// - skewness = E[(r−μ)³] / σ³
/// - excess kurtosis = E[(r−μ)⁴] / σ⁴ − 3
#[must_use]
pub fn sharpe_from_returns(returns_lamports: &[i64]) -> (i64, i64, i64) {
    let n = returns_lamports.len();
    if n < 2 {
        return (0, 0, 0);
    }

    let n_f = n as f64;
    let mean: f64 = returns_lamports.iter().map(|&r| r as f64).sum::<f64>() / n_f;

    let mut sum_sq = 0.0_f64;
    let mut sum_cube = 0.0_f64;
    let mut sum_quad = 0.0_f64;
    for &r in returns_lamports {
        let d = r as f64 - mean;
        let d2 = d * d;
        sum_sq += d2;
        sum_cube += d2 * d;
        sum_quad += d2 * d2;
    }

    let variance = sum_sq / n_f;
    if variance <= 0.0 {
        return (0, 0, 0);
    }
    let std_dev = variance.sqrt();

    let skewness = sum_cube / (n_f * std_dev.powi(3));
    let kurtosis = sum_quad / (n_f * std_dev.powi(4)) - 3.0; // excess kurtosis

    let sr = mean / std_dev * n_f.sqrt();
    let sr_bps = (sr * BPS_SCALE) as i64;
    let skew_bps = (skewness * BPS_SCALE) as i64;
    let kurt_bps = (kurtosis * BPS_SCALE) as i64;

    (sr_bps, skew_bps, kurt_bps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_max_sr_increases_with_trials() {
        let sr_1 = expected_max_sr_bps(1);
        let sr_10 = expected_max_sr_bps(10);
        let sr_100 = expected_max_sr_bps(100);
        let sr_1000 = expected_max_sr_bps(1000);

        assert_eq!(sr_1, 0, "N=1 → no selection bias");
        assert!(sr_10 > 0);
        assert!(sr_100 > sr_10, "more trials → higher expected max");
        assert!(sr_1000 > sr_100);
    }

    #[test]
    fn dsr_rejects_when_sr_below_expected_max() {
        // With 100 trials, the expected max SR is ~2.63 (26_300 bps)
        // An observed SR of 1.0 (10_000 bps) should be inflated
        let cfg = DsrConfig {
            cumulative_trials: 100,
            n_returns: 50,
            sharpe_observed_bps: 10_000, // SR = 1.0
            skewness_bps: 0,
            kurtosis_bps: 0,
            sr_variance_bps2: 0,
        };
        let result = compute_dsr(&cfg);
        assert!(!result.passed, "SR=1.0 with 100 trials should fail DSR");
        assert_eq!(result.verdict, DsrVerdict::Inflated);
    }

    #[test]
    fn dsr_passes_when_sr_above_expected_max() {
        // With 10 trials, expected max SR is ~1.49 (14_900 bps)
        // An observed SR of 3.0 (30_000 bps) with low variance should pass
        let cfg = DsrConfig {
            cumulative_trials: 10,
            n_returns: 50,
            sharpe_observed_bps: 30_000, // SR = 3.0
            skewness_bps: 0,
            kurtosis_bps: 0,    // normal distribution → excess kurtosis = 0
            sr_variance_bps2: 0,
        };
        let result = compute_dsr(&cfg);
        // SR=3.0, expected max ≈1.49, variance = (1 + 2*9/4)/50 = (1+4.5)/50 = 0.11
        // std = 0.33, DSR = (3.0 - 1.49)/0.33 ≈ 4.58 → should pass
        assert!(result.passed, "SR=3.0 with 10 trials should pass DSR (DSR={})", result.dsr_bps);
        assert_eq!(result.verdict, DsrVerdict::Significant);
    }

    #[test]
    fn dsr_insufficient_data_for_low_returns() {
        let cfg = DsrConfig {
            cumulative_trials: 10,
            n_returns: 15, // < 30
            sharpe_observed_bps: 50_000,
            skewness_bps: 0,
            kurtosis_bps: 0,
            sr_variance_bps2: 0,
        };
        let result = compute_dsr(&cfg);
        assert!(!result.passed);
        assert_eq!(result.verdict, DsrVerdict::InsufficientData);
    }

    #[test]
    fn dsr_accounts_for_negative_skew() {
        // Negative skew inflates SR → DSR should be lower
        let cfg_symmetric = DsrConfig {
            cumulative_trials: 10,
            n_returns: 50,
            sharpe_observed_bps: 30_000, // SR = 3.0 >> 2.12
            skewness_bps: 0,
            kurtosis_bps: 0,
            sr_variance_bps2: 0,
        };
        let cfg_neg_skew = DsrConfig {
            cumulative_trials: 10,
            n_returns: 50,
            sharpe_observed_bps: 30_000,
            skewness_bps: -10_000, // strong negative skew (γ3 = -1.0)
            kurtosis_bps: 5_000,   // heavy tails
            sr_variance_bps2: 0,
        };
        let r1 = compute_dsr(&cfg_symmetric);
        let r2 = compute_dsr(&cfg_neg_skew);
        // Negative skew + heavy tails increases Var(SR), reducing DSR
        assert!(r2.sr_variance_bps2 > r1.sr_variance_bps2,
            "negative skew + heavy tails should increase variance (r1={}, r2={})", r1.sr_variance_bps2, r2.sr_variance_bps2);
        // DSR is (SR - SR_max) / sqrt(Var(SR)). Higher variance → lower DSR.
        assert!(r2.dsr_bps <= r1.dsr_bps,
            "negative skew should reduce DSR (r1={}, r2={})", r1.dsr_bps, r2.dsr_bps);
    }

    #[test]
    fn sharpe_from_uniform_positive_returns() {
        let returns = vec![100_000_i64; 50]; // all same → zero variance
        let (sr, skew, kurt) = sharpe_from_returns(&returns);
        assert_eq!(sr, 0, "zero variance → SR undefined → return 0");
        assert_eq!(skew, 0);
        assert_eq!(kurt, 0);
    }

    #[test]
    fn sharpe_from_mixed_returns() {
        // Uniform symmetric distribution centered at 0
        let returns: Vec<i64> = (0..50).map(|i| ((i as i64 - 24) * 10_000) - 50_000).collect();
        // Actually just use a perfectly symmetric set: [-24,-23,...,-1,0,1,...,24,0]
        let returns: Vec<i64> = (-25..=24).map(|i| i * 10_000).collect::<Vec<_>>()
            .iter().chain(std::iter::once(&0i64)).cloned().collect();
        let (sr, skew, kurt) = sharpe_from_returns(&returns);
        // Mean of (-25..24 + 0) = -25+24+0 = -1 → mean = -10 → SR is slightly negative
        // Let's use a truly symmetric set instead
        let mut returns_sym: Vec<i64> = vec![];
        for i in -25..=25 {
            returns_sym.push(i * 10_000);
        }
        // 51 values, symmetric around 0 → mean = 0
        let (sr2, _, _) = sharpe_from_returns(&returns_sym);
        assert_eq!(sr2, 0, "symmetric uniform → mean=0 → SR=0");
    }

    #[test]
    fn sharpe_from_profitable_returns() {
        // Consistently profitable with some variance — symmetric distribution
        let returns: Vec<i64> = vec![
            100_000, 150_000, 80_000, 120_000, 200_000,
            90_000, 110_000, 180_000, 75_000, 160_000,
            100_000, 130_000, 85_000, 140_000, 170_000,
            95_000, 115_000, 125_000, 135_000, 105_000,
            100_000, 150_000, 80_000, 120_000, 200_000,
            90_000, 110_000, 180_000, 75_000, 160_000,
            100_000, 130_000, 85_000, 140_000, 170_000,
            95_000, 115_000, 125_000, 135_000, 105_000,
            100_000, 150_000, 80_000, 120_000, 200_000,
            90_000, 110_000, 180_000, 75_000, 160_000,
        ];
        let (sr, skew, kurt) = sharpe_from_returns(&returns);
        assert!(sr > 0, "consistently profitable → positive SR (sr={})", sr);
        // Skew tolerance: this is not perfectly symmetric, allow ±15k bps
        assert!(skew.abs() < 15_000, "skew should be moderate (skew={})", skew);
        // Kurtosis tolerance: allow ±20k bps for this distribution
        assert!(kurt.abs() < 20_000, "kurtosis should be moderate (kurt={})", kurt);
    }

    #[test]
    fn dsr_bailey_example() {
        // Reproduce the canonical Bailey/LdP example:
        // N=2 years of daily returns (252*2=504 obs), SR_observed=1.5,
        // 100 trials, normal returns (skew=0, kurt=0)
        let cfg = DsrConfig {
            cumulative_trials: 100,
            n_returns: 504,
            sharpe_observed_bps: 15_000, // SR = 1.5
            skewness_bps: 0,
            kurtosis_bps: 0,
            sr_variance_bps2: 0,
        };
        let result = compute_dsr(&cfg);
        // With N=100 trials and 504 obs, expected max SR ≈ 2.63
        // SR=1.5 < 2.63 → should be inflated (DSR < 0)
        // This matches Bailey/LdP's finding that SR=1.5 is not significant
        // when testing 100 strategies
        assert!(!result.passed, "Bailey/LdP: SR=1.5 with 100 trials is inflated");
        assert_eq!(result.verdict, DsrVerdict::Inflated);
    }
}
