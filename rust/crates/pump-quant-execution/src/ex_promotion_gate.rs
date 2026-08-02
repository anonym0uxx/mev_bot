//! Leaf `ex_promotion_gate`: does paper evidence justify arming live capital?
//!
//! ## The gap this closes
//! [`crate::ex_live_arming`] answers "may this trade be submitted", given that
//! an operator has armed the system. Nothing answered the question *before* it:
//! **on what evidence should anyone arm it at all?**
//!
//! Without this, promotion from paper to live is a judgement call made while
//! looking at a number that is green. That is the same defect shape as a gate
//! that cannot fail, applied at the single most expensive decision in the
//! system. This module makes promotion a pre-registered test with a stated
//! threshold, and it is allowed to answer no.
//!
//! ## Why a profitable paper run is not enough
//! A paper session that ends up is not evidence of edge. Twenty trades
//! averaging a small gain, with a spread far wider than that gain, is
//! indistinguishable from a coin landing heads eleven times. The gate therefore
//! requires **statistical separation from zero**, not merely a positive total —
//! and it requires it on an honest denominator, because per-trade variance in
//! memecoin scalping is large relative to per-trade edge.
//!
//! This is the same commission whose golden book was found to be statistically
//! zero and whose fixture could not measure alpha. Live data fixed the
//! instrument; it did not repeal the arithmetic.
//!
//! ## Data quality is a precondition, not a footnote
//! Results computed from a feed with slot gaps are not results. If the session
//! missed slots, some fills were invisible and the PnL is conditioned on the
//! subset that happened to arrive. [`PromotionCriteria::max_slot_gap_bps`]
//! makes that a hard refusal rather than a caveat in a report.
//!
//! ## Constitution refs
//! - §22: every quantity integer. PnL in lamports as `i64`, sums and the
//!   significance test widened to `i128`. The t-test is rearranged to avoid
//!   both division and any square root, so no float appears anywhere.
//! - Determinism: a pure function of the evidence. No clock, no RNG.
//! - §18.8: refusal carries the binding constraint and the numbers behind it.

use crate::ex_live_arming::LiveEnvelope;

/// One whole unit in basis points.
pub const BPS_ONE: u64 = 10_000;

/// Why promotion was refused. The first binding constraint, cheapest check
/// first, so the reason recorded is the one that actually bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalReason {
    /// Too few closed positions to say anything.
    SampleTooSmall { closed: u32, required: u32 },
    /// The feed missed slots, so the sample is conditioned on what arrived.
    FeedGappy { gap_bps: u32, ceiling_bps: u32 },
    /// Entries were admitted but did not fill often enough for the sample to
    /// represent what live execution would do.
    FillRateTooLow { fill_bps: u32, floor_bps: u32 },
    /// Net PnL did not clear the absolute floor.
    NetTooLow {
        net_lamports: i64,
        floor_lamports: i64,
    },
    /// Net PnL is positive but not separable from zero at the required
    /// threshold. The most important refusal in this module.
    NotSignificant {
        /// `t^2` numerator and denominator, as compared.
        t_squared_num: i128,
        t_squared_den: i128,
        /// Threshold `t^2`, same scaling.
        required_num: i128,
        required_den: i128,
    },
    /// Worst peak-to-trough exceeded what the operator will tolerate.
    DrawdownTooDeep { observed: u64, ceiling: u64 },
}

/// The gate's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionVerdict {
    /// Paper evidence clears every criterion.
    Promote,
    /// Do not arm live capital. Carries the binding constraint.
    Refuse(RefusalReason),
}

impl PromotionVerdict {
    /// Whether this verdict authorises arming.
    #[must_use]
    pub const fn is_promote(&self) -> bool {
        matches!(self, Self::Promote)
    }
}

/// Everything observed during paper trading that bears on promotion.
///
/// `net_pnl_lamports` and `sum_sq_pnl_lamports` must be accumulated from
/// **net** per-position results — after pump.fun fees, priority fees, tips, ATA
/// rent and its close refund, and realised slippage. A gross figure makes every
/// test below a fiction, and the significance test in particular will happily
/// certify a strategy whose entire apparent edge is unbooked cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaperEvidence {
    /// Positions opened and closed. The sample size.
    pub closed_positions: u32,
    /// Sum of net per-position PnL, lamports.
    pub net_pnl_lamports: i64,
    /// Sum of squares of net per-position PnL. Widened because a single
    /// position's square already exceeds `i64` at realistic sizes.
    pub sum_sq_pnl_lamports: i128,
    /// Worst peak-to-trough decline in cumulative net PnL, lamports.
    pub max_drawdown_lamports: u64,
    /// Entries the strategy attempted.
    pub entries_attempted: u32,
    /// Entries that actually filled.
    pub entries_filled: u32,
    /// Slots the session observed.
    pub slots_observed: u64,
    /// Slots the session detected as missing.
    pub slots_missed: u64,
}

/// Pre-registered thresholds. Set these BEFORE looking at a result.
///
/// The whole value of this type is that it is filled in first. Choosing a
/// threshold after seeing the number it will be compared against is not a test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromotionCriteria {
    /// Minimum closed positions.
    pub min_closed_positions: u32,
    /// Minimum net PnL in lamports. May be zero, but see the doc on
    /// [`Self::t_squared_num`] — clearing zero is not the same as clearing
    /// noise.
    pub min_net_pnl_lamports: i64,
    /// Required `t^2` as a fraction, numerator. For `t = 2.0`, use `4 / 1`.
    pub t_squared_num: u32,
    /// Required `t^2` as a fraction, denominator.
    pub t_squared_den: u32,
    /// Maximum tolerated peak-to-trough decline, lamports.
    pub max_drawdown_lamports: u64,
    /// Minimum fill rate in basis points of attempted entries.
    pub min_fill_rate_bps: u32,
    /// Maximum tolerated missing-slot rate in basis points.
    pub max_slot_gap_bps: u32,
}

impl PromotionCriteria {
    /// A deliberately demanding default: 100 closed positions, net above zero,
    /// `t >= 2`, fill rate at least 50%, and no more than 0.5% of slots missed.
    ///
    /// `t = 2` is roughly a 5% one-sided false-positive rate on a single test.
    /// It is a floor, not a guarantee — it says the result is unlikely to be
    /// noise, not that the edge will persist.
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            min_closed_positions: 100,
            min_net_pnl_lamports: 1,
            t_squared_num: 4,
            t_squared_den: 1,
            max_drawdown_lamports: u64::MAX,
            min_fill_rate_bps: 5_000,
            max_slot_gap_bps: 50,
        }
    }
}

/// Full result, with every intermediate exposed so a verdict can be audited
/// without re-deriving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromotionReport {
    /// The verdict.
    pub verdict: PromotionVerdict,
    /// Observed fill rate, basis points.
    pub fill_rate_bps: u32,
    /// Observed missing-slot rate, basis points.
    pub slot_gap_bps: u32,
    /// Numerator of the observed `t^2`, as used in the comparison.
    pub t_squared_num: i128,
    /// Denominator of the observed `t^2`. Zero when every position returned
    /// exactly the same amount, which is treated as no evidence rather than as
    /// infinite confidence.
    pub t_squared_den: i128,
    /// Mean net PnL per closed position, lamports, truncated toward zero.
    pub mean_pnl_lamports: i64,
}

/// Fill rate in basis points. Zero attempts is zero, not "perfect".
#[must_use]
pub fn fill_rate_bps(e: &PaperEvidence) -> u32 {
    if e.entries_attempted == 0 {
        return 0;
    }
    let filled = u64::from(e.entries_filled.min(e.entries_attempted));
    ((filled * BPS_ONE) / u64::from(e.entries_attempted)) as u32
}

/// Missing-slot rate in basis points.
///
/// A session that observed nothing reports the maximum gap rate, not zero: no
/// observation is the worst possible data quality, not the best.
#[must_use]
pub fn slot_gap_bps(e: &PaperEvidence) -> u32 {
    let total = e.slots_observed.saturating_add(e.slots_missed);
    if total == 0 {
        return BPS_ONE as u32;
    }
    ((e.slots_missed.saturating_mul(BPS_ONE)) / total) as u32
}

/// Decide whether paper evidence justifies arming live capital.
///
/// Checks run cheapest-first and stop at the first failure, so the recorded
/// reason is the binding constraint.
///
/// ## The significance test
/// The one-sample t-statistic against a null of zero edge is
/// `t = mean / (s / sqrt(n))`. Squaring and substituting the sample variance
/// `s^2 = (n * sum_sq - sum^2) / (n * (n - 1))` gives
///
/// ```text
/// t^2 = sum^2 * (n - 1) / (n * sum_sq - sum^2)
/// ```
///
/// which contains no division by a non-integer and no square root, so the whole
/// comparison is exact `i128` arithmetic and the verdict is reproducible
/// bit-for-bit in replay. The comparison is done by cross-multiplication rather
/// than by evaluating the ratio.
pub fn evaluate(e: &PaperEvidence, c: &PromotionCriteria) -> PromotionReport {
    let n = i128::from(e.closed_positions);
    let sum = i128::from(e.net_pnl_lamports);
    let sum_sq = e.sum_sq_pnl_lamports;

    let fill_bps = fill_rate_bps(e);
    let gap_bps = slot_gap_bps(e);

    let mean = if e.closed_positions == 0 {
        0i64
    } else {
        (sum / n) as i64
    };

    // t^2 = sum^2 * (n - 1) / (n * sum_sq - sum^2)
    let t2_num = sum.saturating_mul(sum).saturating_mul(n - 1);
    let t2_den = n
        .saturating_mul(sum_sq)
        .saturating_sub(sum.saturating_mul(sum));

    let mut report = PromotionReport {
        verdict: PromotionVerdict::Promote,
        fill_rate_bps: fill_bps,
        slot_gap_bps: gap_bps,
        t_squared_num: t2_num,
        t_squared_den: t2_den,
        mean_pnl_lamports: mean,
    };

    if e.closed_positions < c.min_closed_positions {
        report.verdict = PromotionVerdict::Refuse(RefusalReason::SampleTooSmall {
            closed: e.closed_positions,
            required: c.min_closed_positions,
        });
        return report;
    }

    if gap_bps > c.max_slot_gap_bps {
        report.verdict = PromotionVerdict::Refuse(RefusalReason::FeedGappy {
            gap_bps,
            ceiling_bps: c.max_slot_gap_bps,
        });
        return report;
    }

    if fill_bps < c.min_fill_rate_bps {
        report.verdict = PromotionVerdict::Refuse(RefusalReason::FillRateTooLow {
            fill_bps,
            floor_bps: c.min_fill_rate_bps,
        });
        return report;
    }

    if e.net_pnl_lamports < c.min_net_pnl_lamports {
        report.verdict = PromotionVerdict::Refuse(RefusalReason::NetTooLow {
            net_lamports: e.net_pnl_lamports,
            floor_lamports: c.min_net_pnl_lamports,
        });
        return report;
    }

    // Zero variance means every position returned exactly the same amount.
    // Mathematically t is infinite; in practice it means the sample is
    // synthetic or the accounting is broken. Treat it as no evidence.
    let required_num = i128::from(c.t_squared_num);
    let required_den = i128::from(c.t_squared_den);
    let significant = if t2_den <= 0 {
        false
    } else {
        // t2_num / t2_den >= required_num / required_den
        t2_num.saturating_mul(required_den) >= required_num.saturating_mul(t2_den)
    };

    if !significant {
        report.verdict = PromotionVerdict::Refuse(RefusalReason::NotSignificant {
            t_squared_num: t2_num,
            t_squared_den: t2_den,
            required_num,
            required_den,
        });
        return report;
    }

    if e.max_drawdown_lamports > c.max_drawdown_lamports {
        report.verdict = PromotionVerdict::Refuse(RefusalReason::DrawdownTooDeep {
            observed: e.max_drawdown_lamports,
            ceiling: c.max_drawdown_lamports,
        });
        return report;
    }

    report
}

/// Derive a deliberately conservative first live envelope from paper evidence.
///
/// The first live envelope should be **smaller** than what paper ran at. Paper
/// fills are modelled; live fills are contested, and the first hour of real
/// submission is where an unmodelled cost shows up. `size_divisor` shrinks the
/// per-position ceiling — 4 means start at a quarter of paper size.
///
/// Returns [`LiveEnvelope::closed`] when the verdict is a refusal, so a caller
/// that ignores the verdict and reaches for the envelope anyway still gets one
/// that cannot trade.
#[must_use]
pub fn suggested_initial_envelope(
    verdict: PromotionVerdict,
    paper_max_position_lamports: u64,
    size_divisor: u32,
    daily_loss_limit_lamports: u64,
    heartbeat_timeout_ms: u64,
) -> LiveEnvelope {
    if !verdict.is_promote() || paper_max_position_lamports == 0 || size_divisor == 0 {
        return LiveEnvelope::closed();
    }
    let per_position = (paper_max_position_lamports / u64::from(size_divisor)).max(1);
    LiveEnvelope {
        max_position_lamports: per_position,
        // Three positions' worth of headroom, not the whole paper book.
        max_total_deployed_lamports: per_position.saturating_mul(3),
        max_open_positions: 3,
        max_entries_per_hour: 20,
        daily_loss_limit_lamports,
        max_entry_slippage_bps: 500,
        heartbeat_timeout_ms,
    }
}
