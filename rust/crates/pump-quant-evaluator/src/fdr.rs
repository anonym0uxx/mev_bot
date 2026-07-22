//! `fdr` — Benjamini–Hochberg false-discovery-rate control within an experiment
//! family (constitution §51).
//!
//! When many strategy variants are tested against the same reconciled data, the
//! best raw p-value is not evidence — the family must be corrected for multiple
//! testing before any variant is promoted. §51 names the *frozen evaluator* as
//! the authority that computes Benjamini–Hochberg FDR within experiment families
//! as a promotion-blocking gate. The prior `baseline_destruction` inflation
//! (`required_margin * K`) is a crude Bonferroni flavour; this is the proper
//! step-up procedure.
//!
//! §22: integer-only. A p-value is carried as parts-per-million (`p_ppm ∈
//! [0, 1_000_000]`, where `1_000_000` == `1.0`). The BH comparison
//! `p_(i) <= (i/m)·α` is evaluated by cross-multiplication in `u128`, so no
//! division and no float ever enters the decision. Deterministic: identical
//! `(family, alpha)` always yield the identical discovery set, ordered by id.

/// One hypothesis in an experiment family: an opaque id and its raw p-value in
/// parts-per-million.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hypothesis {
    /// Opaque, stable hypothesis id. Ordering drives deterministic output order.
    pub id: u64,
    /// Raw (uncorrected) p-value, parts-per-million (`1_000_000` == `p = 1.0`).
    pub p_ppm: u32,
}

impl Hypothesis {
    /// Test/golden-vector constructor.
    pub fn new(id: u64, p_ppm: u32) -> Self {
        Hypothesis { id, p_ppm }
    }
}

/// Result of the Benjamini–Hochberg step-up procedure over one family.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BhResult {
    /// Ids of every rejected null (i.e. discovered / significant), ascending id.
    pub discovered: Vec<u64>,
    /// The largest p-value (ppm) that was rejected — the effective significance
    /// threshold. `0` when nothing was discovered.
    pub threshold_ppm: u32,
    /// Family size `m`.
    pub m: u32,
}

impl BhResult {
    /// True iff at least one hypothesis was discovered.
    pub fn any_discovered(&self) -> bool {
        !self.discovered.is_empty()
    }
}

/// Benjamini–Hochberg FDR control at level `alpha_ppm` (§51).
///
/// Procedure: sort the `m` p-values ascending; find the largest rank `i`
/// (1-based) for which `p_(i) ≤ (i/m)·α`; reject every hypothesis with
/// `p ≤ p_(i)`. The rank test is evaluated as `p_(i)·m ≤ α·i` in `u128` — exact,
/// division-free, integer. Discoveries are returned in ascending id order
/// (deterministic even when p-values or ids tie). An empty family discovers
/// nothing.
///
/// Panics if any `p_ppm > 1_000_000` (a p-value above 1.0 is malformed input) or
/// if `alpha_ppm > 1_000_000`.
pub fn benjamini_hochberg(family: &[Hypothesis], alpha_ppm: u32) -> BhResult {
    assert!(
        alpha_ppm <= 1_000_000,
        "benjamini_hochberg: alpha_ppm > 1.0"
    );
    for h in family {
        assert!(h.p_ppm <= 1_000_000, "benjamini_hochberg: p_ppm > 1.0");
    }
    let m = family.len();
    if m == 0 {
        return BhResult {
            discovered: Vec::new(),
            threshold_ppm: 0,
            m: 0,
        };
    }

    // Sort a copy ascending by p, tie-broken by id (deterministic).
    let mut sorted: Vec<Hypothesis> = family.to_vec();
    sorted.sort_by(|a, b| a.p_ppm.cmp(&b.p_ppm).then(a.id.cmp(&b.id)));

    // Largest rank i (1-based) with p_(i)*m <= alpha*i.
    let alpha = alpha_ppm as u128;
    let m_u = m as u128;
    let mut max_reject_rank: usize = 0; // 0 == none
    for (idx, h) in sorted.iter().enumerate() {
        let i = (idx + 1) as u128;
        let lhs = h.p_ppm as u128 * m_u;
        let rhs = alpha * i;
        if lhs <= rhs {
            max_reject_rank = idx + 1;
        }
    }

    if max_reject_rank == 0 {
        return BhResult {
            discovered: Vec::new(),
            threshold_ppm: 0,
            m: m as u32,
        };
    }

    // Reject every hypothesis with p <= p_(max_reject_rank) (step-up: all below
    // the largest surviving rank are rejected even if they individually failed).
    let threshold_ppm = sorted[max_reject_rank - 1].p_ppm;
    let mut discovered: Vec<u64> = sorted.iter().take(max_reject_rank).map(|h| h.id).collect();
    discovered.sort_unstable();

    BhResult {
        discovered,
        threshold_ppm,
        m: m as u32,
    }
}

/// Convenience: does the family, corrected at `alpha_ppm`, block promotion?
///
/// A promotion that rests on a specific `candidate_id` is *blocked* unless that
/// candidate is among the BH discoveries — a raw win that does not survive
/// family correction is not evidence (§51). Returns `true` when promotion is
/// blocked.
pub fn blocks_promotion(family: &[Hypothesis], alpha_ppm: u32, candidate_id: u64) -> bool {
    let res = benjamini_hochberg(family, alpha_ppm);
    !res.discovered.contains(&candidate_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_family_discovers_nothing() {
        let r = benjamini_hochberg(&[], 50_000);
        assert!(!r.any_discovered());
        assert_eq!(r.m, 0);
        assert_eq!(r.threshold_ppm, 0);
    }

    #[test]
    fn single_significant_hypothesis() {
        // m=1: reject iff p <= alpha.
        let fam = vec![Hypothesis::new(7, 10_000)]; // p=0.01
        let r = benjamini_hochberg(&fam, 50_000); // alpha=0.05
        assert_eq!(r.discovered, vec![7]);
        assert_eq!(r.threshold_ppm, 10_000);
    }

    #[test]
    fn single_insignificant_hypothesis() {
        let fam = vec![Hypothesis::new(7, 100_000)]; // p=0.10
        let r = benjamini_hochberg(&fam, 50_000);
        assert!(!r.any_discovered());
    }

    #[test]
    fn classic_bh_step_up() {
        // Benjamini-Hochberg 1995 style example. m=4, alpha=0.05.
        // p: 0.005, 0.02, 0.03, 0.5.
        // critical i/m*alpha: 0.0125, 0.025, 0.0375, 0.05.
        // p1=0.005<=0.0125 ok; p2=0.02<=0.025 ok; p3=0.03<=0.0375 ok; p4=0.5 no.
        // Largest passing rank = 3 -> reject first 3.
        let fam = vec![
            Hypothesis::new(1, 5_000),
            Hypothesis::new(2, 20_000),
            Hypothesis::new(3, 30_000),
            Hypothesis::new(4, 500_000),
        ];
        let r = benjamini_hochberg(&fam, 50_000);
        assert_eq!(r.discovered, vec![1, 2, 3]);
        assert_eq!(r.threshold_ppm, 30_000);
        assert_eq!(r.m, 4);
    }

    #[test]
    fn step_up_rejects_below_largest_even_if_individually_failing() {
        // A middle p can individually fail its critical value but still be
        // rejected because a later, larger rank passes (the step-up property).
        // m=3, alpha=0.05: crit = 0.0167, 0.0333, 0.05.
        // p: 0.01 (ok), 0.04 (fails 0.0333), 0.045 (ok at 0.05).
        // Largest passing rank = 3 -> all three rejected including the middle.
        let fam = vec![
            Hypothesis::new(1, 10_000),
            Hypothesis::new(2, 40_000),
            Hypothesis::new(3, 45_000),
        ];
        let r = benjamini_hochberg(&fam, 50_000);
        assert_eq!(r.discovered, vec![1, 2, 3]);
    }

    #[test]
    fn nothing_significant_returns_empty() {
        let fam = vec![
            Hypothesis::new(1, 400_000),
            Hypothesis::new(2, 600_000),
            Hypothesis::new(3, 900_000),
        ];
        let r = benjamini_hochberg(&fam, 50_000);
        assert!(!r.any_discovered());
    }

    #[test]
    fn blocks_promotion_unless_discovered() {
        let fam = vec![Hypothesis::new(1, 5_000), Hypothesis::new(2, 500_000)];
        // id 1 is discovered -> not blocked.
        assert!(!blocks_promotion(&fam, 50_000, 1));
        // id 2 is not discovered -> blocked.
        assert!(blocks_promotion(&fam, 50_000, 2));
        // an unknown candidate is always blocked.
        assert!(blocks_promotion(&fam, 50_000, 99));
    }

    #[test]
    fn determinism_with_tied_pvalues() {
        let fam = vec![
            Hypothesis::new(3, 10_000),
            Hypothesis::new(1, 10_000),
            Hypothesis::new(2, 10_000),
        ];
        let a = benjamini_hochberg(&fam, 50_000);
        let b = benjamini_hochberg(&fam, 50_000);
        assert_eq!(a, b);
        assert_eq!(a.discovered, vec![1, 2, 3]);
    }

    #[test]
    #[should_panic(expected = "p_ppm > 1.0")]
    fn malformed_pvalue_panics() {
        let _ = benjamini_hochberg(&[Hypothesis::new(1, 1_000_001)], 50_000);
    }

    #[test]
    #[should_panic(expected = "alpha_ppm > 1.0")]
    fn malformed_alpha_panics() {
        let _ = benjamini_hochberg(&[Hypothesis::new(1, 1_000)], 1_000_001);
    }
}
