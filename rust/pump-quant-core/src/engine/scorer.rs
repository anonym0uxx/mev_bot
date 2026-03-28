//! Score engine for MEV backrun trigger qualification.
//!
//! Computes a weighted composite score from 6 signal components + adversarial penalty.
//! All f64 math, zero allocation, designed for inline hot-path evaluation.

/// Configuration for the scorer — weights and normalization parameters.
#[derive(Clone, Debug)]
pub struct ScoreConfig {
    pub weight_momentum: f64,
    pub weight_buyers_banded: f64,
    pub weight_diversity: f64,
    pub weight_curve_fill: f64,
    pub weight_crowd_depth: f64,
    pub weight_recent_1s: f64,
    pub adversarial_concentration_threshold: f64,
    pub adversarial_penalty: f64,
    pub crowd_depth_norm_lamports: u64,
    pub recent_1s_norm_count: u16,
}

impl Default for ScoreConfig {
    fn default() -> Self {
        Self {
            weight_momentum: 0.10,
            weight_buyers_banded: 0.25,
            weight_diversity: 0.10,
            weight_curve_fill: 0.20,
            weight_crowd_depth: 0.20,
            weight_recent_1s: 0.15,
            adversarial_concentration_threshold: 0.6,
            adversarial_penalty: 0.5,
            crowd_depth_norm_lamports: 5_000_000_000, // 5 SOL
            recent_1s_norm_count: 6,
        }
    }
}

/// Decomposed score components for observability / logging.
#[derive(Clone, Debug)]
pub struct ScoreComponents {
    pub momentum_trend: f64,
    pub buyers_banded: f64,
    pub buyer_diversity: f64,
    pub curve_fill: f64,
    pub crowd_depth_5s: f64,
    pub recent_buyers_1s: f64,
    pub adversarial_penalty: f64,
    pub final_score: f64,
}

/// Stateless scorer — holds config + curve bounds for fill calculation.
pub struct Scorer {
    config: ScoreConfig,
    min_vsol_lamports: u64,
    max_vsol_lamports: u64,
    /// Pre-computed: 1.0 / (max_vsol - min_vsol) to avoid division in hot path.
    inv_vsol_range: f64,
    /// Pre-computed: 1.0 / crowd_depth_norm as f64
    inv_crowd_norm: f64,
    /// Pre-computed: 1.0 / recent_1s_norm_count as f64
    inv_recent_norm: f64,
}

impl Scorer {
    pub fn new(config: ScoreConfig, min_vsol_lamports: u64, max_vsol_lamports: u64) -> Self {
        let vsol_range = max_vsol_lamports.saturating_sub(min_vsol_lamports).max(1);
        let inv_vsol_range = 1.0 / vsol_range as f64;
        let inv_crowd_norm = 1.0 / config.crowd_depth_norm_lamports.max(1) as f64;
        let inv_recent_norm = 1.0 / config.recent_1s_norm_count.max(1) as f64;
        Self {
            config,
            min_vsol_lamports,
            max_vsol_lamports,
            inv_vsol_range,
            inv_crowd_norm,
            inv_recent_norm,
        }
    }

    /// Access config.
    pub fn config(&self) -> &ScoreConfig {
        &self.config
    }

    /// Compute all score components and the final weighted score.
    ///
    /// Zero allocation. All f64 arithmetic.
    #[inline]
    pub fn compute(
        &self,
        _trigger_sol_lamports: u64,
        vsol_lamports: u64,
        unique_buyers_30s: u16,
        buy_count_1s: u16,
        buy_count_2s: u16,
        volume_5s_lamports: u64,
        max_wallet_volume_lamports: u64,
        total_buy_vol_30s_lamports: u64,
    ) -> ScoreComponents {
        let c = &self.config;

        // ── 1. Momentum trend (10%) ────────────────────────────────
        // older = max(buy_count_2s - buy_count_1s, 0.1)
        // ratio = buy_count_1s / older
        // momentum = clamp((ratio - 0.5) / 1.5, 0.0, 1.0)
        let older = {
            let diff = buy_count_2s as i32 - buy_count_1s as i32;
            if diff <= 0 { 0.1 } else { diff as f64 }
        };
        let ratio = buy_count_1s as f64 / older;
        let momentum_trend = clamp01((ratio - 0.5) / 1.5);

        // ── 2. Buyers banded (25%) ─────────────────────────────────
        // Nonlinear mapping on unique_buyers_30s.
        let n = unique_buyers_30s;
        let buyers_banded = if n < 3 {
            0.1
        } else if n <= 5 {
            0.5 + (n as f64 - 3.0) * 0.15
        } else if n <= 10 {
            0.8 + (n as f64 - 5.0) * 0.04
        } else if n <= 15 {
            1.0 - (n as f64 - 10.0) * 0.06
        } else {
            0.7
        };

        // ── 3. Buyer diversity (10%) ───────────────────────────────
        // clamp(unique_30s / total_buys_30s * 1.5, 0.0, 1.0)
        // total_buy_vol_30s here is volume, but the TypeScript uses count.
        // We use the buyer count ratio approximation: unique / total_trades.
        // Since we don't have total_buy_count_30s, we use total_buy_vol as a
        // denominator proxy — but the spec says "total_buys_30s".
        // Design decision: interpret total_buy_vol_30s_lamports as a volume measure.
        // Diversity = unique_buyers / (total_buy_vol_30s / avg_buy_size) approximation.
        // However, to match the TS exactly, we pass total_buy_vol and compute:
        // If total_buy_vol_30s > 0: diversity = unique * avg_trade_sol / total_vol * 1.5
        // This is ambiguous. The cleanest interpretation: the caller should pass
        // total buy COUNT in 30s. But the param is labeled volume.
        // Resolution: we'll treat total_buy_vol_30s_lamports as buy COUNT for diversity.
        // The caller can pass buy count in the volume field for diversity calc,
        // and we also use it for adversarial check (where it IS volume).
        //
        // Actually — re-reading carefully: the adversarial check needs volume (lamports)
        // and diversity needs count. These are different. Let's keep the API clean:
        // total_buy_vol_30s_lamports IS volume. For diversity we approximate:
        // unique_buyers / max(unique_buyers, buy_count_estimates) but that's lossy.
        //
        // Best approach: diversity uses unique_buyers / (buy_count_2s * 15) as a rough
        // 30s extrapolation? No — too hacky.
        //
        // Final decision: For the diversity score, if total_buy_vol_30s_lamports > 0,
        // compute an effective buy count as total_vol / trigger_size_estimate.
        // But we don't have trigger size here either.
        //
        // SIMPLEST AND CORRECT: The TypeScript v5 scorer passes total_buys_30s as a
        // count. We have unique_buyers_30s. For now, approximate total buys as
        // unique_buyers * 1.5 (conservative) — this gives diversity near 0.67 baseline.
        // OR: we add a total_buy_count_30s param. But the spec signature doesn't have it.
        //
        // Let's re-read: the param IS total_buy_vol_30s_lamports (volume in lamports).
        // The TypeScript formula is `unique_30s / total_buys_30s * 1.5` where
        // total_buys_30s is a COUNT. Since we can't derive count from volume alone,
        // we'll use volume_5s * 6 as a 30s volume estimate and then compute a
        // diversity-from-volume metric.
        //
        // ACTUALLY: The simplest correct thing — the scorer should just take what it
        // needs. The TS formula wants count. We approximate: if total_buy_vol is 0,
        // diversity = 0. Otherwise, use unique_buyers as both numerator and a proxy
        // for total: diversity = clamp(1.0 * 1.5, 0, 1) = 1.0 when unique == total.
        // This is wrong.
        //
        // I'll use the volume interpretation: diversity = clamp(unique_30s_lamports_equiv / total * 1.5).
        // Where unique_30s_lamports_equiv ≈ unique_buyers * (total_vol / buyer_count_estimate).
        // This is circular.
        //
        // PRAGMATIC DECISION: Use `unique_buyers_30s as f64 / max(buy_count_5s * 6, 1) * 1.5`
        // where buy_count_5s * 6 extrapolates to 30s. This is a reasonable approximation.
        // The caller can always improve this later.
        //
        // Wait — we have buy_count_2s but not buy_count_5s here. Looking at the function
        // signature again... we have buy_count_1s and buy_count_2s.
        // Use buy_count_2s * 15 as 30s extrapolation.
        //
        // ALTERNATIVELY: total_buy_vol_30s_lamports / (some average buy size) gives count.
        // If trigger_sol_lamports is passed, use that as avg buy size estimate.
        //
        // OK final answer: use total_buy_vol_30s as denominator directly in a
        // volume-weighted diversity metric. Reinterpret the formula as:
        // diversity = clamp((unique_buyers as f64) / max(total_effective_buys, 1.0) * 1.5, 0, 1)
        // where total_effective_buys = total_buy_vol_30s / trigger_sol (approximate count).
        // But _trigger_sol_lamports is unused... let's use it!

        let buyer_diversity = if total_buy_vol_30s_lamports == 0 || _trigger_sol_lamports == 0 {
            0.0
        } else {
            // Estimate total buy count from volume / average trade size
            // Use trigger as proxy for average trade size
            let estimated_total_buys = total_buy_vol_30s_lamports as f64 / _trigger_sol_lamports as f64;
            let estimated_total_buys = estimated_total_buys.max(1.0);
            clamp01(unique_buyers_30s as f64 / estimated_total_buys * 1.5)
        };

        // ── 4. Curve fill (20%) ────────────────────────────────────
        // 1.0 - (vsol - min_vsol) / (max_vsol - min_vsol), clamped [0,1]
        let curve_fill = if vsol_lamports <= self.min_vsol_lamports {
            1.0
        } else if vsol_lamports >= self.max_vsol_lamports {
            0.0
        } else {
            let offset = (vsol_lamports - self.min_vsol_lamports) as f64;
            clamp01(1.0 - offset * self.inv_vsol_range)
        };

        // ── 5. Crowd depth 5s (20%) ───────────────────────────────
        // clamp(volume_5s_sol / 5.0, 0.0, 1.0)
        // = clamp(volume_5s_lamports / crowd_depth_norm_lamports, 0.0, 1.0)
        let crowd_depth_5s = clamp01(volume_5s_lamports as f64 * self.inv_crowd_norm);

        // ── 6. Recent buyers 1s (15%) ──────────────────────────────
        // clamp(buy_count_1s / 6.0, 0.0, 1.0)
        let recent_buyers_1s = clamp01(buy_count_1s as f64 * self.inv_recent_norm);

        // ── 7. Adversarial penalty ─────────────────────────────────
        // if max_wallet_vol / total_buy_vol_30s > 0.6 → 0.5x, else 1.0
        let adversarial_penalty = if total_buy_vol_30s_lamports > 0
            && max_wallet_volume_lamports as f64 / total_buy_vol_30s_lamports as f64
                > c.adversarial_concentration_threshold
        {
            c.adversarial_penalty
        } else {
            1.0
        };

        // ── 8. Weighted final score ────────────────────────────────
        let raw = momentum_trend * c.weight_momentum
            + buyers_banded * c.weight_buyers_banded
            + buyer_diversity * c.weight_diversity
            + curve_fill * c.weight_curve_fill
            + crowd_depth_5s * c.weight_crowd_depth
            + recent_buyers_1s * c.weight_recent_1s;

        let final_score = raw * adversarial_penalty;

        ScoreComponents {
            momentum_trend,
            buyers_banded,
            buyer_diversity,
            curve_fill,
            crowd_depth_5s,
            recent_buyers_1s,
            adversarial_penalty,
            final_score,
        }
    }
}

/// Clamp a value to [0.0, 1.0].
#[inline(always)]
fn clamp01(x: f64) -> f64 {
    if x < 0.0 {
        0.0
    } else if x > 1.0 {
        1.0
    } else {
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_scorer() -> Scorer {
        Scorer::new(
            ScoreConfig::default(),
            3_000_000_000,  // 3 SOL min
            85_000_000_000, // 85 SOL max
        )
    }

    #[test]
    fn momentum_high_when_1s_dominates() {
        let scorer = default_scorer();
        let sc = scorer.compute(
            500_000_000,    // trigger 0.5 SOL
            10_000_000_000, // vsol 10 SOL
            5,              // unique buyers
            4,              // buy_count_1s (high)
            5,              // buy_count_2s
            3_000_000_000,  // vol 5s
            500_000_000,    // max wallet vol
            5_000_000_000,  // total buy vol 30s
        );
        // older = max(5-4, 0.1) = 1.0
        // ratio = 4/1 = 4.0
        // momentum = clamp((4.0 - 0.5) / 1.5, 0, 1) = clamp(2.33, 0, 1) = 1.0
        assert!((sc.momentum_trend - 1.0).abs() < 0.001);
    }

    #[test]
    fn momentum_low_when_no_recent() {
        let scorer = default_scorer();
        let sc = scorer.compute(
            500_000_000,
            10_000_000_000,
            5,
            0, // buy_count_1s = 0
            4, // buy_count_2s = 4
            3_000_000_000,
            500_000_000,
            5_000_000_000,
        );
        // older = max(4-0, 0.1) = 4.0
        // ratio = 0/4 = 0.0
        // momentum = clamp((0.0 - 0.5) / 1.5, 0, 1) = clamp(-0.33, 0, 1) = 0.0
        assert!(sc.momentum_trend < 0.001);
    }

    #[test]
    fn buyers_banded_mapping() {
        let scorer = default_scorer();

        // n=2: should be 0.1
        let sc = scorer.compute(500_000_000, 10_000_000_000, 2, 2, 3, 3_000_000_000, 500_000_000, 5_000_000_000);
        assert!((sc.buyers_banded - 0.1).abs() < 0.001);

        // n=3: should be 0.5
        let sc = scorer.compute(500_000_000, 10_000_000_000, 3, 2, 3, 3_000_000_000, 500_000_000, 5_000_000_000);
        assert!((sc.buyers_banded - 0.5).abs() < 0.001);

        // n=5: should be 0.5 + 2*0.15 = 0.8
        let sc = scorer.compute(500_000_000, 10_000_000_000, 5, 2, 3, 3_000_000_000, 500_000_000, 5_000_000_000);
        assert!((sc.buyers_banded - 0.8).abs() < 0.001);

        // n=10: should be 0.8 + 5*0.04 = 1.0
        let sc = scorer.compute(500_000_000, 10_000_000_000, 10, 2, 3, 3_000_000_000, 500_000_000, 5_000_000_000);
        assert!((sc.buyers_banded - 1.0).abs() < 0.001);

        // n=15: should be 1.0 - 5*0.06 = 0.7
        let sc = scorer.compute(500_000_000, 10_000_000_000, 15, 2, 3, 3_000_000_000, 500_000_000, 5_000_000_000);
        assert!((sc.buyers_banded - 0.7).abs() < 0.001);

        // n=20: should be 0.7
        let sc = scorer.compute(500_000_000, 10_000_000_000, 20, 2, 3, 3_000_000_000, 500_000_000, 5_000_000_000);
        assert!((sc.buyers_banded - 0.7).abs() < 0.001);
    }

    #[test]
    fn curve_fill_at_min_is_1() {
        let scorer = default_scorer();
        let sc = scorer.compute(
            500_000_000,
            3_000_000_000, // vsol = min → fill = 1.0
            5, 2, 3, 3_000_000_000, 500_000_000, 5_000_000_000,
        );
        assert!((sc.curve_fill - 1.0).abs() < 0.001);
    }

    #[test]
    fn curve_fill_at_max_is_0() {
        let scorer = default_scorer();
        let sc = scorer.compute(
            500_000_000,
            85_000_000_000, // vsol = max → fill = 0.0
            5, 2, 3, 3_000_000_000, 500_000_000, 5_000_000_000,
        );
        assert!(sc.curve_fill < 0.001);
    }

    #[test]
    fn adversarial_penalty_applied() {
        let scorer = default_scorer();
        // max_wallet = 4 SOL, total = 5 SOL → concentration = 0.8 > 0.6 → penalty
        let sc = scorer.compute(
            500_000_000, 10_000_000_000, 5, 2, 3, 3_000_000_000,
            4_000_000_000,  // max wallet: 4 SOL
            5_000_000_000,  // total: 5 SOL → 0.8 > 0.6
        );
        assert!((sc.adversarial_penalty - 0.5).abs() < 0.001);
    }

    #[test]
    fn no_adversarial_penalty_when_distributed() {
        let scorer = default_scorer();
        // max_wallet = 1 SOL, total = 5 SOL → concentration = 0.2 < 0.6 → no penalty
        let sc = scorer.compute(
            500_000_000, 10_000_000_000, 5, 2, 3, 3_000_000_000,
            1_000_000_000,
            5_000_000_000,
        );
        assert!((sc.adversarial_penalty - 1.0).abs() < 0.001);
    }

    #[test]
    fn crowd_depth_maxes_at_5_sol() {
        let scorer = default_scorer();
        let sc = scorer.compute(
            500_000_000, 10_000_000_000, 5, 2, 3,
            5_000_000_000,  // vol 5s = 5 SOL → depth = 1.0
            500_000_000, 5_000_000_000,
        );
        assert!((sc.crowd_depth_5s - 1.0).abs() < 0.001);
    }

    #[test]
    fn recent_1s_maxes_at_6() {
        let scorer = default_scorer();
        let sc = scorer.compute(
            500_000_000, 10_000_000_000, 5,
            6,  // buy_count_1s = 6 → recent = 1.0
            8, 3_000_000_000, 500_000_000, 5_000_000_000,
        );
        assert!((sc.recent_buyers_1s - 1.0).abs() < 0.001);
    }

    #[test]
    fn final_score_reasonable_range() {
        let scorer = default_scorer();
        let sc = scorer.compute(
            500_000_000,
            10_000_000_000,
            5,
            2,
            3,
            3_000_000_000,
            500_000_000,
            5_000_000_000,
        );
        // Should be between 0 and 1
        assert!(sc.final_score >= 0.0 && sc.final_score <= 1.0);
        // With reasonable inputs, should be meaningfully > 0
        assert!(sc.final_score > 0.1);
    }
}
