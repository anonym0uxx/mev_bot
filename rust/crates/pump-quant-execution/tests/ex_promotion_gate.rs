//! Tests for `ex_promotion_gate`.
//!
//! The controls that matter are the refusals. A promotion gate that has never
//! been observed to refuse a profitable-looking paper run is not a gate — it is
//! a formality standing between a green number and real money.

use pump_quant_execution::ex_live_arming::LiveEnvelope;
use pump_quant_execution::ex_promotion_gate::*;

const SOL: i64 = 1_000_000_000;

/// Build evidence from an explicit list of per-position net PnLs, so the
/// significance arithmetic is checkable by hand.
fn evidence_from(pnls: &[i64]) -> PaperEvidence {
    let n = pnls.len() as u32;
    let sum: i64 = pnls.iter().sum();
    let sum_sq: i128 = pnls.iter().map(|&p| i128::from(p) * i128::from(p)).sum();
    PaperEvidence {
        closed_positions: n,
        net_pnl_lamports: sum,
        sum_sq_pnl_lamports: sum_sq,
        max_drawdown_lamports: 0,
        entries_attempted: n,
        entries_filled: n,
        slots_observed: 10_000,
        slots_missed: 0,
    }
}

/// A clean, strongly positive sample: 120 positions, every one +0.01 SOL with
/// small spread. Should promote under the conservative default.
fn strong_sample() -> PaperEvidence {
    let pnls: Vec<i64> = (0..120)
        .map(|i| SOL / 100 + ((i % 5) as i64 - 2) * (SOL / 10_000))
        .collect();
    evidence_from(&pnls)
}

/// A sample that is positive in total but dominated by variance: 120 positions
/// alternating large win and slightly smaller loss.
fn noisy_sample() -> PaperEvidence {
    let pnls: Vec<i64> = (0..120)
        .map(|i| {
            if i % 2 == 0 {
                SOL / 2
            } else {
                -SOL / 2 + SOL / 1000
            }
        })
        .collect();
    evidence_from(&pnls)
}

fn criteria() -> PromotionCriteria {
    PromotionCriteria::conservative()
}

// ─────────────────────────── promotion actually works ────────────────────────

#[test]
fn a_strong_sample_promotes() {
    let r = evaluate(&strong_sample(), &criteria());
    assert!(
        r.verdict.is_promote(),
        "strong sample should promote, got {:?}",
        r.verdict
    );
    assert_eq!(r.fill_rate_bps, 10_000);
    assert_eq!(r.slot_gap_bps, 0);
    assert!(r.mean_pnl_lamports > 0);
}

// ───────────────────────────── NEGATIVE CONTROLS ─────────────────────────────

#[test]
fn negative_control_profitable_but_noisy_is_refused() {
    // THE control this module exists for. Total PnL is positive, so a naive
    // "did we make money" check passes. The spread is so wide that the result
    // is indistinguishable from a coin flip, and it must be refused.
    let e = noisy_sample();
    assert!(e.net_pnl_lamports > 0, "the sample really is profitable");

    let r = evaluate(&e, &criteria());
    match r.verdict {
        PromotionVerdict::Refuse(RefusalReason::NotSignificant { .. }) => {}
        other => panic!("a positive-but-noisy sample must be refused, got {other:?}"),
    }
}

#[test]
fn negative_control_small_sample_is_refused_however_good() {
    // Ten perfect trades is not evidence, and the gate must say so before it
    // ever gets as far as looking at the PnL.
    let e = evidence_from(&[SOL / 100; 10]);
    match evaluate(&e, &criteria()).verdict {
        PromotionVerdict::Refuse(RefusalReason::SampleTooSmall { closed, required }) => {
            assert_eq!(closed, 10);
            assert_eq!(required, 100);
        }
        other => panic!("expected SampleTooSmall, got {other:?}"),
    }
}

#[test]
fn negative_control_gappy_feed_invalidates_the_sample() {
    // Results computed from a feed that missed slots are conditioned on what
    // arrived. That is a refusal, not a caveat.
    let mut e = strong_sample();
    e.slots_observed = 9_000;
    e.slots_missed = 1_000; // 1000 bps, far over the 50 bps ceiling
    match evaluate(&e, &criteria()).verdict {
        PromotionVerdict::Refuse(RefusalReason::FeedGappy {
            gap_bps,
            ceiling_bps,
        }) => {
            assert_eq!(gap_bps, 1_000);
            assert_eq!(ceiling_bps, 50);
        }
        other => panic!("expected FeedGappy, got {other:?}"),
    }
}

#[test]
fn negative_control_a_session_that_observed_nothing_is_maximally_gappy() {
    // No observation is the worst data quality, not the best. A zero/zero slot
    // count must not read as a perfect feed.
    let mut e = strong_sample();
    e.slots_observed = 0;
    e.slots_missed = 0;
    assert_eq!(slot_gap_bps(&e), 10_000);
    assert!(matches!(
        evaluate(&e, &criteria()).verdict,
        PromotionVerdict::Refuse(RefusalReason::FeedGappy { .. })
    ));
}

#[test]
fn negative_control_low_fill_rate_is_refused() {
    // If most entries never filled, the closed positions are a biased subset -
    // the ones the market let us have.
    let mut e = strong_sample();
    e.entries_attempted = 1_000;
    e.entries_filled = 120;
    match evaluate(&e, &criteria()).verdict {
        PromotionVerdict::Refuse(RefusalReason::FillRateTooLow {
            fill_bps,
            floor_bps,
        }) => {
            assert_eq!(fill_bps, 1_200);
            assert_eq!(floor_bps, 5_000);
        }
        other => panic!("expected FillRateTooLow, got {other:?}"),
    }
}

#[test]
fn negative_control_zero_attempts_is_not_a_perfect_fill_rate() {
    let mut e = strong_sample();
    e.entries_attempted = 0;
    e.entries_filled = 0;
    assert_eq!(fill_rate_bps(&e), 0);
}

#[test]
fn negative_control_losing_sample_is_refused() {
    let pnls: Vec<i64> = (0..120).map(|i| -(SOL / 100) - (i % 3) as i64).collect();
    let e = evidence_from(&pnls);
    match evaluate(&e, &criteria()).verdict {
        PromotionVerdict::Refuse(RefusalReason::NetTooLow { net_lamports, .. }) => {
            assert!(net_lamports < 0);
        }
        other => panic!("expected NetTooLow, got {other:?}"),
    }
}

#[test]
fn negative_control_zero_variance_is_no_evidence_not_infinite_confidence() {
    // Every position returning exactly the same amount makes t mathematically
    // infinite. In practice it means the sample is synthetic or the accounting
    // is broken, and it must not certify anything.
    let e = evidence_from(&[SOL / 100; 120]);
    let r = evaluate(&e, &criteria());
    assert_eq!(r.t_squared_den, 0);
    match r.verdict {
        PromotionVerdict::Refuse(RefusalReason::NotSignificant { .. }) => {}
        other => panic!("zero variance must refuse, got {other:?}"),
    }
}

#[test]
fn negative_control_deep_drawdown_is_refused() {
    let mut e = strong_sample();
    e.max_drawdown_lamports = (SOL / 2) as u64;
    let mut c = criteria();
    c.max_drawdown_lamports = (SOL / 10) as u64;
    match evaluate(&e, &c).verdict {
        PromotionVerdict::Refuse(RefusalReason::DrawdownTooDeep { observed, ceiling }) => {
            assert_eq!(observed, (SOL / 2) as u64);
            assert_eq!(ceiling, (SOL / 10) as u64);
        }
        other => panic!("expected DrawdownTooDeep, got {other:?}"),
    }
}

#[test]
fn negative_control_empty_evidence_refuses_without_panicking() {
    let e = PaperEvidence {
        closed_positions: 0,
        net_pnl_lamports: 0,
        sum_sq_pnl_lamports: 0,
        max_drawdown_lamports: 0,
        entries_attempted: 0,
        entries_filled: 0,
        slots_observed: 0,
        slots_missed: 0,
    };
    let r = evaluate(&e, &criteria());
    assert!(!r.verdict.is_promote());
    assert_eq!(r.mean_pnl_lamports, 0);
}

// ──────────────────────────── the arithmetic itself ──────────────────────────

#[test]
fn significance_is_checkable_by_hand() {
    // n = 4, values 2,4,4,6 (lamports). sum = 16, sum_sq = 72.
    // t^2 = sum^2 * (n-1) / (n*sum_sq - sum^2)
    //     = 256 * 3 / (288 - 256) = 768 / 32 = 24.
    let e = evidence_from(&[2, 4, 4, 6]);
    let mut c = criteria();
    c.min_closed_positions = 1;
    let r = evaluate(&e, &c);
    assert_eq!(r.t_squared_num, 768);
    assert_eq!(r.t_squared_den, 32);
    // 24 >= 4, so the significance check passes at t = 2.
    assert!(r.verdict.is_promote());
}

#[test]
fn the_threshold_actually_binds() {
    let e = evidence_from(&[2, 4, 4, 6]); // t^2 = 24
    let mut c = criteria();
    c.min_closed_positions = 1;
    // Require t^2 >= 24 exactly: still passes (inclusive).
    c.t_squared_num = 24;
    c.t_squared_den = 1;
    assert!(evaluate(&e, &c).verdict.is_promote());
    // Require t^2 >= 25: refused. The comparison is not decorative.
    c.t_squared_num = 25;
    assert!(matches!(
        evaluate(&e, &c).verdict,
        PromotionVerdict::Refuse(RefusalReason::NotSignificant { .. })
    ));
}

#[test]
fn fractional_thresholds_work_without_floats() {
    // t = 2.5 -> t^2 = 6.25 -> 25/4.
    let e = evidence_from(&[2, 4, 4, 6]);
    let mut c = criteria();
    c.min_closed_positions = 1;
    c.t_squared_num = 25;
    c.t_squared_den = 4;
    assert!(evaluate(&e, &c).verdict.is_promote());
}

// ─────────────────────────── the derived first envelope ──────────────────────

#[test]
fn a_refusal_yields_an_envelope_that_cannot_trade() {
    // Belt and braces: a caller that ignores the verdict and reaches for the
    // envelope anyway must still be unable to trade.
    let refused = PromotionVerdict::Refuse(RefusalReason::SampleTooSmall {
        closed: 1,
        required: 100,
    });
    let env = suggested_initial_envelope(refused, SOL as u64, 4, SOL as u64, 300_000);
    assert_eq!(env, LiveEnvelope::closed());
    assert!(!env.admits_anything());
}

#[test]
fn promotion_yields_a_smaller_envelope_than_paper_ran_at() {
    // The first live envelope must be smaller than paper. Live fills are
    // contested; the first hour of real submission is where an unmodelled cost
    // appears.
    let paper_max = SOL as u64; // 1 SOL positions in paper
    let env = suggested_initial_envelope(
        PromotionVerdict::Promote,
        paper_max,
        4,
        (SOL / 5) as u64,
        300_000,
    );
    assert_eq!(env.max_position_lamports, paper_max / 4);
    assert!(env.max_position_lamports < paper_max);
    assert_eq!(env.max_total_deployed_lamports, (paper_max / 4) * 3);
    assert_eq!(env.max_open_positions, 3);
    assert!(env.admits_anything());
}

#[test]
fn degenerate_envelope_inputs_close_the_envelope() {
    for (paper, div) in [(0u64, 4u32), (SOL as u64, 0u32)] {
        let env =
            suggested_initial_envelope(PromotionVerdict::Promote, paper, div, SOL as u64, 300_000);
        assert_eq!(env, LiveEnvelope::closed());
    }
}
