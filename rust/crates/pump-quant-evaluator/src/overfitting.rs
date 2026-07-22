//! `overfitting` — combinatorially-symmetric cross-validation (CSCV) and the
//! probability of backtest overfitting (PBO) (constitution §51).
//!
//! §51 requires the frozen evaluator to compute PBO / CSCV overfitting
//! diagnostics as promotion-blocking gates. Given a performance matrix over many
//! candidate strategies and a set of time blocks, CSCV asks: when we pick the
//! best strategy *in sample*, how often does it land in the bottom half *out of
//! sample*? A high PBO means the selection procedure is fitting noise, and the
//! promotion must be blocked regardless of the winner's headline backtest.
//!
//! §22: integer-only, fully deterministic. Performance is integer (e.g. net bps
//! per block). CSCV enumerates every way to split the blocks into two equal
//! halves; the "logit ≤ 0" overfit criterion is evaluated as the integer
//! statement "the in-sample winner's out-of-sample performance is at or below
//! the out-of-sample median", which needs no floats. No RNG — the combinatorial
//! enumeration is exhaustive and order-stable.

/// Error explaining why a performance matrix is not admissible for CSCV.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CscvError {
    /// Fewer than two candidate strategies — no selection to overfit.
    TooFewTrials,
    /// Fewer than two blocks, or an odd block count (halves must be equal).
    BadBlockCount,
    /// Rows are not all the same length as the block count.
    RaggedMatrix,
}

/// PBO / CSCV diagnostic result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PboReport {
    /// Number of candidate strategies (matrix rows).
    pub n_trials: u32,
    /// Number of time blocks (matrix columns).
    pub n_blocks: u32,
    /// Number of CSCV splits enumerated (`C(n_blocks, n_blocks/2)`).
    pub n_splits: u32,
    /// Splits where the in-sample winner was at/below the out-of-sample median
    /// (an overfit event).
    pub overfit_splits: u32,
    /// Probability of backtest overfitting, bps (`overfit_splits*10_000/n_splits`).
    pub pbo_bps: u32,
}

impl PboReport {
    /// True iff PBO is at/above `threshold_bps` — the promotion-blocking test.
    pub fn blocks_promotion(&self, threshold_bps: u32) -> bool {
        self.pbo_bps >= threshold_bps
    }
}

/// Enumerate, in ascending lexicographic order, every size-`k` subset of
/// `0..n` as a bitmask (`k <= n <= 63`). Deterministic and bounded.
fn combinations_bitmask(n: usize, k: usize) -> Vec<u64> {
    debug_assert!(n <= 63);
    let mut out = Vec::new();
    // Standard "smallest set bit" combination enumeration (Gosper-free, simple).
    let mut idx = vec![0usize; k];
    for (j, slot) in idx.iter_mut().enumerate() {
        *slot = j;
    }
    if k == 0 {
        return vec![0];
    }
    loop {
        let mut mask: u64 = 0;
        for &b in &idx {
            mask |= 1u64 << b;
        }
        out.push(mask);
        // Advance: find rightmost index that can move.
        let mut i = k as isize - 1;
        while i >= 0 && idx[i as usize] == n - k + i as usize {
            i -= 1;
        }
        if i < 0 {
            break;
        }
        idx[i as usize] += 1;
        for j in (i as usize + 1)..k {
            idx[j] = idx[j - 1] + 1;
        }
    }
    out
}

/// Integer median of an already-sorted slice (even -> lower-of-two-central to
/// keep the "at or below median" test strict against the winner). We compare
/// against a threshold, so we use the *upper-middle* value as the median cut so
/// that "bottom half" has exactly `floor(n/2)` strictly-better competitors.
fn median_cut(sorted: &[i64]) -> i64 {
    let n = sorted.len();
    debug_assert!(n > 0);
    sorted[n / 2]
}

/// Compute the PBO via CSCV over a performance matrix (§51).
///
/// `perf[trial][block]` is strategy `trial`'s performance on time `block`
/// (integer, higher is better). CSCV enumerates all `C(n_blocks, n_blocks/2)`
/// ways to choose the in-sample half; for each split the in-sample-best trial is
/// selected (max summed IS performance, ties -> lowest trial index) and flagged
/// *overfit* when its out-of-sample performance is at or below the out-of-sample
/// median across trials. PBO is the fraction of overfit splits, in bps.
///
/// Returns `Err` for an inadmissible matrix rather than fabricating a diagnostic.
pub fn pbo_cscv(perf: &[Vec<i64>]) -> Result<PboReport, CscvError> {
    let n_trials = perf.len();
    if n_trials < 2 {
        return Err(CscvError::TooFewTrials);
    }
    let n_blocks = perf[0].len();
    if n_blocks < 2 || !n_blocks.is_multiple_of(2) {
        return Err(CscvError::BadBlockCount);
    }
    if perf.iter().any(|row| row.len() != n_blocks) {
        return Err(CscvError::RaggedMatrix);
    }

    let half = n_blocks / 2;
    let is_masks = combinations_bitmask(n_blocks, half);
    let n_splits = is_masks.len() as u32;
    let mut overfit_splits: u32 = 0;

    for mask in &is_masks {
        // Per-trial IS and OOS sums for this split.
        let mut is_sum = vec![0i128; n_trials];
        let mut oos_sum = vec![0i128; n_trials];
        for (t, row) in perf.iter().enumerate() {
            for (b, &v) in row.iter().enumerate() {
                if mask & (1u64 << b) != 0 {
                    is_sum[t] += v as i128;
                } else {
                    oos_sum[t] += v as i128;
                }
            }
        }
        // In-sample winner: max IS, ties -> lowest index.
        let mut best_t = 0usize;
        for t in 1..n_trials {
            if is_sum[t] > is_sum[best_t] {
                best_t = t;
            }
        }
        // Out-of-sample median cut across trials.
        let mut oos_sorted: Vec<i64> = oos_sum.iter().map(|&x| x as i64).collect();
        oos_sorted.sort_unstable();
        let cut = median_cut(&oos_sorted) as i128;
        // Overfit iff the IS winner is at/below the OOS median.
        if oos_sum[best_t] <= cut {
            overfit_splits += 1;
        }
    }

    let pbo_bps = if n_splits == 0 {
        0
    } else {
        ((overfit_splits as u64 * 10_000) / n_splits as u64) as u32
    };

    Ok(PboReport {
        n_trials: n_trials as u32,
        n_blocks: n_blocks as u32,
        n_splits,
        overfit_splits,
        pbo_bps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combinations_count_and_order() {
        // C(4,2) = 6, ascending masks.
        let c = combinations_bitmask(4, 2);
        assert_eq!(c.len(), 6);
        // First is {0,1} = 0b0011.
        assert_eq!(c[0], 0b0011);
        // Last is {2,3} = 0b1100.
        assert_eq!(c[5], 0b1100);
    }

    #[test]
    fn rejects_bad_shapes() {
        assert_eq!(pbo_cscv(&[vec![1, 2]]), Err(CscvError::TooFewTrials));
        assert_eq!(
            pbo_cscv(&[vec![1, 2, 3], vec![4, 5, 6]]),
            Err(CscvError::BadBlockCount)
        );
        assert_eq!(
            pbo_cscv(&[vec![1, 2], vec![4, 5, 6, 7]]),
            Err(CscvError::RaggedMatrix)
        );
    }

    #[test]
    fn genuinely_skilled_strategy_low_pbo() {
        // Trial 0 dominates every block; it is best IS and best OOS every split.
        // It is never at/below the OOS median -> PBO = 0.
        let perf = vec![
            vec![100, 100, 100, 100],
            vec![10, 10, 10, 10],
            vec![20, 20, 20, 20],
            vec![30, 30, 30, 30],
        ];
        let r = pbo_cscv(&perf).unwrap();
        assert_eq!(r.n_splits, 6); // C(4,2)
        assert_eq!(r.overfit_splits, 0);
        assert_eq!(r.pbo_bps, 0);
        assert!(!r.blocks_promotion(5_000));
    }

    #[test]
    fn pure_noise_flip_flop_high_pbo() {
        // Two trials that are perfect mirror images across two blocks: whichever
        // wins IS necessarily loses OOS. With n=2 trials the OOS median cut is
        // the upper of the two, and the IS winner's OOS sum is the lower ->
        // overfit on every split.
        let perf = vec![vec![100, -100], vec![-100, 100]];
        let r = pbo_cscv(&perf).unwrap();
        assert_eq!(r.n_splits, 2); // C(2,1)
        assert_eq!(r.overfit_splits, 2);
        assert_eq!(r.pbo_bps, 10_000);
        assert!(r.blocks_promotion(5_000));
    }

    #[test]
    fn deterministic_repeat() {
        let perf = vec![
            vec![5, -3, 8, -1, 4, -2],
            vec![-2, 6, -4, 7, -1, 3],
            vec![1, 1, 1, 1, 1, 1],
        ];
        let a = pbo_cscv(&perf).unwrap();
        let b = pbo_cscv(&perf).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.n_blocks, 6);
        assert_eq!(a.n_splits, 20); // C(6,3)
        assert!(a.pbo_bps <= 10_000);
    }
}
