//! `sizing_validator` — Layer-2 canonical research sizing validator
//! (constitution §33).
//!
//! This is the frozen-evaluator port of `analysis/kelly_montecarlo.py`: given an
//! empirical, already-reconciled per-trade return distribution, it computes the
//! log-utility (Kelly) optimal capital fraction, a bootstrap uncertainty band
//! around that fraction, and a Monte-Carlo drawdown / survival profile at a
//! chosen fraction. §33 names pq-evaluator as the *canonical* sizing validator,
//! so this is the single deterministic authority that a proposed `LogUtility`
//! sleeve size must clear.
//!
//! §22 determinism is a hard contract. There is **no** `f32`/`f64` anywhere: all
//! money/return math is integer / fixed-point in `i128`. "Monte-Carlo" and
//! "bootstrap" resampling use a *seeded, deterministic* splitmix64 generator —
//! the seed is a caller-supplied `u64`, never wall-clock or OS entropy — so
//! identical `(inputs, seed)` always produce byte-for-byte identical bands and
//! survival numbers. All state is bounded (§99): the caller fixes the resample
//! count and path length; nothing grows unboundedly.
//!
//! Fixed-point convention: a "growth factor" is carried at [`SCALE`] = 1e9, so a
//! value of `1_000_000_000` means exactly `1.0`. Returns and fractions are in
//! basis points (bps): a fraction `f_bps` of `2_000` is 20% of capital, a return
//! `r_bps` of `50_000` is `+5.0x` (a five-fold move).

/// Fixed-point scale for growth factors and log values: `SCALE` units == `1.0`.
pub const SCALE: i128 = 1_000_000_000;

/// `ln(2)` in fixed point (`SCALE` units).
const LN2: i128 = 693_147_180;

/// bps denominator (`10_000` bps == `1.0`).
const BPS: i128 = 10_000;

// ============================================================================
// Deterministic PRNG (splitmix64) — seeded, no entropy, no wall-clock.
// ============================================================================

/// Deterministic splitmix64 state. Seeded only from a caller-supplied `u64`; it
/// never reads the clock or OS entropy, so a run is a pure function of its seed.
#[derive(Clone, Copy, Debug)]
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        SplitMix64 { state: seed }
    }

    /// Advance and return the next 64-bit value (standard splitmix64).
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform index in `[0, n)` for `n > 0`, via unbiased rejection sampling so
    /// the resample is exactly uniform and deterministic.
    fn index(&mut self, n: usize) -> usize {
        debug_assert!(n > 0);
        let n64 = n as u64;
        // Largest multiple of n that fits in u64; reject the biased tail.
        let limit = u64::MAX - (u64::MAX % n64);
        loop {
            let r = self.next_u64();
            if r < limit {
                return (r % n64) as usize;
            }
        }
    }
}

// ============================================================================
// Fixed-point natural logarithm (integer-only, bounded series).
// ============================================================================

/// Natural log of `x_fp/SCALE`, returned in `SCALE` fixed point.
///
/// `x_fp` must be strictly positive (a non-positive growth factor is ruin and is
/// handled by the caller before reaching here). Uses range reduction to
/// `m ∈ [1, 2)` then a bounded `atanh` series — no floats, fully deterministic.
fn ln_fp(x_fp: i128) -> i128 {
    debug_assert!(x_fp > 0, "ln_fp requires x_fp > 0");
    let mut m = x_fp;
    let mut e: i128 = 0;
    while m >= 2 * SCALE {
        m /= 2;
        e += 1;
    }
    while m < SCALE {
        m *= 2;
        e -= 1;
    }
    // ln(m/SCALE) = 2 * atanh(t), t = (m-SCALE)/(m+SCALE).
    let t = ((m - SCALE) * SCALE) / (m + SCALE);
    let t2 = (t * t) / SCALE;
    let mut term = t;
    let mut sum = t;
    let mut k: i128 = 3;
    // 24 odd terms is far past convergence for |t| < 1/3.
    for _ in 0..24 {
        term = (term * t2) / SCALE;
        sum += term / k;
        k += 2;
    }
    2 * sum + e * LN2
}

/// Growth factor `1 + (f_bps/1e4) * (r_bps/1e4)` in `SCALE` fixed point, or
/// `None` when the position is wiped out (`growth <= 0` — ruin, log undefined).
fn growth_factor_fp(f_bps: i128, r_bps: i128) -> Option<i128> {
    // contribution = f_bps * r_bps * SCALE / (BPS*BPS)
    let contribution = (f_bps * r_bps * SCALE) / (BPS * BPS);
    let g = SCALE + contribution;
    if g <= 0 {
        None
    } else {
        Some(g)
    }
}

// ============================================================================
// Log-utility optimal fraction (grid search).
// ============================================================================

/// Result of the log-utility (Kelly) optimization over an empirical return set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogUtilityFit {
    /// Optimal capital fraction, bps (`2_000` == 20%).
    pub optimal_f_bps: u32,
    /// Mean expected log-growth per trade at `optimal_f_bps`, `SCALE` fixed
    /// point (positive == compounding, negative == bleeding). `0` when there
    /// were no returns.
    pub expected_log_growth: i128,
    /// Number of returns the fit was computed over.
    pub n: u32,
}

/// Mean expected log-growth per trade at fraction `f_bps`, `SCALE` fixed point.
///
/// Returns `i128::MIN` as a "ruinous / infeasible" sentinel if any single return
/// wipes the position out (`growth <= 0`) at this fraction — such a fraction can
/// never be optimal under log utility (one ruin makes terminal wealth zero).
fn mean_log_growth(returns_bps: &[i64], f_bps: u32) -> i128 {
    if returns_bps.is_empty() {
        return 0;
    }
    let mut sum: i128 = 0;
    for &r in returns_bps {
        match growth_factor_fp(f_bps as i128, r as i128) {
            Some(g) => sum += ln_fp(g),
            None => return i128::MIN,
        }
    }
    sum / returns_bps.len() as i128
}

/// Log-utility optimal capital fraction over an empirical return distribution.
///
/// Grid-searches `f_bps` over `0, step_bps, 2*step_bps, …, f_max_bps` and keeps
/// the fraction maximizing mean per-trade log-growth. The all-cash fraction
/// (`0`) is always feasible with log-growth `0`, so a distribution with no
/// positive edge correctly returns `optimal_f_bps == 0`. Ties keep the *smaller*
/// fraction (least risk for equal growth). Deterministic; empty input yields a
/// zero fit.
///
/// Panics if `step_bps == 0` (a zero step is not a grid).
pub fn optimal_log_utility(returns_bps: &[i64], f_max_bps: u32, step_bps: u32) -> LogUtilityFit {
    assert!(step_bps > 0, "optimal_log_utility: step_bps must be > 0");
    if returns_bps.is_empty() {
        return LogUtilityFit {
            optimal_f_bps: 0,
            expected_log_growth: 0,
            n: 0,
        };
    }
    let mut best_f: u32 = 0;
    let mut best_growth: i128 = mean_log_growth(returns_bps, 0); // == 0
    let mut f = step_bps;
    while f <= f_max_bps {
        let g = mean_log_growth(returns_bps, f);
        if g > best_growth {
            best_growth = g;
            best_f = f;
        }
        f = f.saturating_add(step_bps);
    }
    LogUtilityFit {
        optimal_f_bps: best_f,
        expected_log_growth: best_growth,
        n: returns_bps.len() as u32,
    }
}

// ============================================================================
// Bootstrap uncertainty band around the optimal fraction.
// ============================================================================

/// Percentile band (p05 / p50 / p95) of a bootstrap statistic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Band {
    /// 5th percentile (nearest-rank).
    pub p05: u32,
    /// Median.
    pub p50: u32,
    /// 95th percentile (nearest-rank).
    pub p95: u32,
    /// Number of bootstrap resamples.
    pub n_resamples: u32,
}

/// Nearest-rank percentile of an already-sorted `u32` slice (`num`/`den`).
fn pct_sorted_u32(sorted: &[u32], num: usize, den: usize) -> u32 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (num * sorted.len()).div_ceil(den);
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

/// Bootstrap uncertainty band for the log-utility optimal fraction.
///
/// Draws `n_resamples` resamples-with-replacement of the empirical returns
/// (each the same length as the input) using the seeded generator, recomputes
/// the optimal fraction on each, and returns the p05/p50/p95 band over those
/// fractions. A wide band means the sizing is fragile to sampling noise — the
/// research plane's uncertainty signal. Deterministic in `(returns, seed)`;
/// empty input or `n_resamples == 0` returns an all-zero band.
pub fn bootstrap_fraction_band(
    returns_bps: &[i64],
    f_max_bps: u32,
    step_bps: u32,
    n_resamples: u32,
    seed: u64,
) -> Band {
    if returns_bps.is_empty() || n_resamples == 0 {
        return Band {
            p05: 0,
            p50: 0,
            p95: 0,
            n_resamples: 0,
        };
    }
    let mut rng = SplitMix64::new(seed);
    let n = returns_bps.len();
    let mut sample: Vec<i64> = vec![0; n];
    let mut fits: Vec<u32> = Vec::with_capacity(n_resamples as usize);
    for _ in 0..n_resamples {
        for slot in sample.iter_mut() {
            *slot = returns_bps[rng.index(n)];
        }
        fits.push(optimal_log_utility(&sample, f_max_bps, step_bps).optimal_f_bps);
    }
    fits.sort_unstable();
    Band {
        p05: pct_sorted_u32(&fits, 5, 100),
        p50: pct_sorted_u32(&fits, 50, 100),
        p95: pct_sorted_u32(&fits, 95, 100),
        n_resamples,
    }
}

// ============================================================================
// Monte-Carlo drawdown / survival at a chosen fraction.
// ============================================================================

/// Monte-Carlo drawdown / survival profile at a fixed sizing fraction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurvivalReport {
    /// Fraction evaluated, bps.
    pub f_bps: u32,
    /// Number of simulated paths.
    pub n_paths: u32,
    /// Paths that never breached the ruin floor and never wiped out.
    pub survived: u32,
    /// Median worst-drawdown across paths, bps of peak equity (`0`..`10_000`).
    pub median_max_drawdown_bps: u32,
    /// 95th-percentile worst-drawdown across paths, bps of peak equity.
    pub p95_max_drawdown_bps: u32,
}

/// Monte-Carlo path simulation of drawdown and survival at fraction `f_bps`.
///
/// Each of `n_paths` paths compounds `path_len` returns drawn with replacement
/// (seeded generator) at fraction `f_bps`, starting from equity `SCALE` (== 1.0).
/// A path *dies* the first time either a single return wipes the position out
/// (`growth <= 0`) or cumulative equity falls to / below the ruin floor
/// (`ruin_bps` of the starting equity). `survived` counts paths that finish
/// without dying. Worst-drawdown per path is measured against the running peak
/// and reported as bps of that peak; the report gives the median and p95 across
/// paths. Deterministic in `(returns, seed)`; empty returns / zero paths / zero
/// length yields a zeroed report.
///
/// Panics if `ruin_bps >= 10_000` (a ruin floor at or above starting equity is
/// meaningless — the path is dead before it starts).
pub fn monte_carlo_survival(
    returns_bps: &[i64],
    f_bps: u32,
    n_paths: u32,
    path_len: u32,
    ruin_bps: u32,
    seed: u64,
) -> SurvivalReport {
    assert!(
        ruin_bps < 10_000,
        "monte_carlo_survival: ruin_bps must be < 10_000"
    );
    if returns_bps.is_empty() || n_paths == 0 || path_len == 0 {
        return SurvivalReport {
            f_bps,
            n_paths: 0,
            survived: 0,
            median_max_drawdown_bps: 0,
            p95_max_drawdown_bps: 0,
        };
    }
    let mut rng = SplitMix64::new(seed);
    let n = returns_bps.len();
    // Ruin floor as fixed-point equity (fraction of starting SCALE).
    let ruin_floor: i128 = (SCALE * ruin_bps as i128) / BPS;
    let mut survived: u32 = 0;
    let mut drawdowns: Vec<u32> = Vec::with_capacity(n_paths as usize);

    for _ in 0..n_paths {
        let mut equity: i128 = SCALE;
        let mut peak: i128 = SCALE;
        let mut worst_dd_bps: u32 = 0;
        let mut alive = true;
        for _ in 0..path_len {
            let r = returns_bps[rng.index(n)];
            match growth_factor_fp(f_bps as i128, r as i128) {
                None => {
                    alive = false;
                    worst_dd_bps = 10_000;
                    break;
                }
                Some(g) => {
                    equity = (equity * g) / SCALE;
                    if equity > peak {
                        peak = equity;
                    }
                    // Drawdown vs running peak, in bps (peak > 0 always).
                    let dd_bps = (((peak - equity) * BPS) / peak).clamp(0, 10_000) as u32;
                    if dd_bps > worst_dd_bps {
                        worst_dd_bps = dd_bps;
                    }
                    if equity <= ruin_floor {
                        alive = false;
                        break;
                    }
                }
            }
        }
        if alive {
            survived += 1;
        }
        drawdowns.push(worst_dd_bps);
    }

    drawdowns.sort_unstable();
    SurvivalReport {
        f_bps,
        n_paths,
        survived,
        median_max_drawdown_bps: pct_sorted_u32(&drawdowns, 50, 100),
        p95_max_drawdown_bps: pct_sorted_u32(&drawdowns, 95, 100),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ln_fp_known_values() {
        // ln(1) == 0.
        assert_eq!(ln_fp(SCALE), 0);
        // ln(2) ≈ 0.6931 -> within a small fixed-point tolerance.
        let l2 = ln_fp(2 * SCALE);
        assert!((l2 - LN2).abs() < 200, "ln2 fp off: {l2}");
        // ln(e) ≈ 1.0; e ≈ 2.718281828.
        let e_fp = 2_718_281_828i128;
        let le = ln_fp(e_fp);
        assert!((le - SCALE).abs() < 300, "ln(e) fp off: {le}");
        // Monotonicity.
        assert!(ln_fp(3 * SCALE) > ln_fp(2 * SCALE));
        assert!(ln_fp(SCALE / 2) < 0);
    }

    #[test]
    fn growth_factor_ruin_detection() {
        // f=100% (10_000 bps), r=-100% (-10_000 bps) -> growth 0 -> ruin.
        assert_eq!(growth_factor_fp(10_000, -10_000), None);
        // f=50%, r=-100% -> 1 - 0.5 = 0.5 -> feasible.
        assert_eq!(growth_factor_fp(5_000, -10_000), Some(SCALE / 2));
        // f=20%, r=+500% (50_000 bps) -> 1 + 0.2*5 = 2.0.
        assert_eq!(growth_factor_fp(2_000, 50_000), Some(2 * SCALE));
    }

    #[test]
    fn optimal_no_edge_stays_cash() {
        // Symmetric coin that loses money in log terms -> optimum is all cash.
        let returns = vec![5_000i64, -5_000]; // +50% / -50%
        let fit = optimal_log_utility(&returns, 10_000, 500);
        assert_eq!(fit.optimal_f_bps, 0);
        assert_eq!(fit.expected_log_growth, 0);
        assert_eq!(fit.n, 2);
    }

    #[test]
    fn optimal_positive_edge_sizes_up() {
        // Favorable coin: +100% / -50% has positive Kelly edge.
        let returns = vec![10_000i64, -5_000];
        let fit = optimal_log_utility(&returns, 10_000, 100);
        // Classic Kelly for +1/-0.5 is f* = 0.5 (5_000 bps). Grid should land near.
        assert!(fit.optimal_f_bps > 0, "expected positive sizing");
        assert!(fit.expected_log_growth > 0, "expected positive log growth");
        assert!(
            (4_000..=6_000).contains(&fit.optimal_f_bps),
            "kelly fraction off grid: {}",
            fit.optimal_f_bps
        );
    }

    #[test]
    fn optimal_empty_is_zero_fit() {
        let fit = optimal_log_utility(&[], 10_000, 100);
        assert_eq!(fit.optimal_f_bps, 0);
        assert_eq!(fit.n, 0);
    }

    #[test]
    #[should_panic(expected = "step_bps")]
    fn optimal_zero_step_panics() {
        let _ = optimal_log_utility(&[1_000], 10_000, 0);
    }

    #[test]
    fn bootstrap_is_deterministic_and_bounded() {
        let returns = vec![10_000i64, -5_000, 8_000, -4_000, 12_000];
        let a = bootstrap_fraction_band(&returns, 10_000, 500, 200, 42);
        let b = bootstrap_fraction_band(&returns, 10_000, 500, 200, 42);
        assert_eq!(a, b, "same seed must reproduce band");
        assert_eq!(a.n_resamples, 200);
        assert!(a.p05 <= a.p50 && a.p50 <= a.p95, "band must be ordered");
        // A different seed generally shifts the band but stays in range.
        let c = bootstrap_fraction_band(&returns, 10_000, 500, 200, 7);
        assert!(c.p95 <= 10_000);
    }

    #[test]
    fn bootstrap_empty_or_zero_resamples() {
        assert_eq!(
            bootstrap_fraction_band(&[], 10_000, 500, 10, 1).n_resamples,
            0
        );
        assert_eq!(
            bootstrap_fraction_band(&[1_000], 10_000, 500, 0, 1).n_resamples,
            0
        );
    }

    #[test]
    fn survival_all_winners_never_dies_no_drawdown() {
        let returns = vec![1_000i64, 2_000, 500]; // all positive
        let rep = monte_carlo_survival(&returns, 2_000, 50, 30, 5_000, 99);
        assert_eq!(rep.survived, 50, "all-winner paths must all survive");
        assert_eq!(rep.median_max_drawdown_bps, 0);
        assert_eq!(rep.p95_max_drawdown_bps, 0);
    }

    #[test]
    fn survival_wipeout_kills_every_path() {
        // Single-return set that wipes out at full sizing: growth 0.
        let returns = vec![-10_000i64];
        let rep = monte_carlo_survival(&returns, 10_000, 25, 10, 5_000, 3);
        assert_eq!(rep.survived, 0, "ruinous single return kills all");
        assert_eq!(rep.median_max_drawdown_bps, 10_000);
    }

    #[test]
    fn survival_deterministic_in_seed() {
        let returns = vec![3_000i64, -2_000, 4_000, -1_500];
        let a = monte_carlo_survival(&returns, 1_500, 40, 20, 3_000, 11);
        let b = monte_carlo_survival(&returns, 1_500, 40, 20, 3_000, 11);
        assert_eq!(a, b);
        assert!(a.survived <= a.n_paths);
        assert!(a.median_max_drawdown_bps <= a.p95_max_drawdown_bps);
    }

    #[test]
    fn survival_empty_inputs_zeroed() {
        let rep = monte_carlo_survival(&[], 1_000, 10, 10, 5_000, 1);
        assert_eq!(rep.n_paths, 0);
        assert_eq!(rep.survived, 0);
    }

    #[test]
    #[should_panic(expected = "ruin_bps")]
    fn survival_bad_ruin_floor_panics() {
        let _ = monte_carlo_survival(&[1_000], 1_000, 1, 1, 10_000, 1);
    }
}
