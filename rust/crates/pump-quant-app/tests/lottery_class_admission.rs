//! **THE LOTTERY-CLASS ADMISSION LAW** (audit 2026-07-25).
//!
//! LAW B3 is the only ARMED discretionary law in the system: at admission it VETOES
//! or HAIRCUTS position size based on what the brain recalls about the setup class.
//! Its "this class bled" test used to be `median_net_lamports < 0` **alone**, with a
//! win-rate bar layered on top (veto at ≤ 1_500 bp, haircut at ≤ 3_500 bp).
//!
//! That is a proxy test pointed directly against the objective. **The canonical
//! profitable memecoin payoff is a fat RIGHT tail** — most trades lose small, rare
//! trades win huge — which produces exactly a negative median AND a low win rate.
//! A median-only test therefore vetoed the classes the strategy exists to catch,
//! and the veto is ABSORBING: a vetoed candidate is never admitted, so it books no
//! episode, so the class's statistics freeze and it can never redeem itself. One
//! unlucky early sample would blacklist a paying setup permanently.
//!
//! The codebase already knew the right answer. `brain_analysis::is_conditioned_negative`
//! — used for RETIREMENT — has always required `median < 0 && mean < 0`, and says why
//! in its own doc comment: *"A negative median with a positive mean is a subject that
//! pays rarely and hugely — a lottery, but not necessarily a losing one."* The far
//! more aggressive ADMISSION veto simply omitted the conjunct. This file pins the
//! correction so it cannot regress.

use pump_quant_app::brain::{BrainPlane, BrainSizeVerdict};
use pump_quant_brain::recall::RecallStats;

/// The B3 defaults (`config.rs`): veto at ≤ 15% win rate, haircut at ≤ 35%.
const VETO_WR_BP: u32 = 1_500;
const HAIRCUT_WR_BP: u32 = 3_500;
const HAIRCUT_MULT_BP: u32 = 5_000;

/// The decision the law makes for a class with these statistics, isolated from the
/// recall index so the arithmetic under test is the VERDICT RULE itself.
fn verdict_for(median: i128, mean: i128, win_rate_bp: u32) -> BrainSizeVerdict {
    BrainPlane::verdict_from_stats(
        &RecallStats {
            n_matched: 20,
            median_net_lamports: median,
            mean_net_lamports: mean,
            win_count: 2,
            loss_count: 18,
            win_rate_bp,
            p25_net_lamports: 0,
            p75_net_lamports: 0,
            median_hold_ns: 0,
            nearest_distance: 0,
            nearest_weighted_distance: 0,
            nearest_episode_id: 0,
        },
        8,
        HAIRCUT_WR_BP,
        VETO_WR_BP,
        HAIRCUT_MULT_BP,
    )
}

/// **The load-bearing case.** 18 losses of −0.10 SOL against 2 wins of +3.00 SOL:
/// median −0.10 SOL, win rate 1_000 bp, **total +4.20 SOL**. Under the old
/// median-only rule this was VETOED — the single most profitable class in the book
/// refused at admission. It must now pass untouched.
#[test]
fn a_lottery_class_that_pays_in_aggregate_is_admitted_at_full_size() {
    const LOSS: i128 = -100_000_000; // 0.10 SOL
    const WIN: i128 = 3_000_000_000; // 3.00 SOL
    let total = 18 * LOSS + 2 * WIN;
    assert_eq!(total, 4_200_000_000, "the class pays +4.2 SOL in aggregate");
    let mean = total / 20;
    assert!(mean > 0, "mean is positive: {mean}");

    assert_eq!(
        verdict_for(LOSS, mean, 1_000),
        BrainSizeVerdict::Identity,
        "a negative-median, low-win-rate, POSITIVE-MEAN class must be admitted at \
         full size — vetoing it is optimizing a proxy against realized net SOL"
    );
    // The haircut band is the same disease and must also be inert.
    assert_eq!(
        verdict_for(LOSS, mean, 2_500),
        BrainSizeVerdict::Identity,
        "the haircut band must not fade a class whose aggregate pays"
    );
}

/// The law still does its job: a class that fails in BOTH senses is still refused.
#[test]
fn a_class_that_bleeds_in_both_senses_is_still_vetoed() {
    assert_eq!(
        verdict_for(-100_000_000, -80_000_000, 1_000),
        BrainSizeVerdict::Veto,
        "median AND mean negative with a low win rate is a genuine bleeder"
    );
    assert_eq!(
        verdict_for(-100_000_000, -80_000_000, 2_500),
        BrainSizeVerdict::Haircut(HAIRCUT_MULT_BP),
        "median AND mean negative in the haircut band still fades"
    );
}

/// The fat LEFT tail (many small wins, rare large losses) stays the exit ladder's
/// problem, not an admission problem — unchanged behaviour, pinned so it stays so.
#[test]
fn a_fat_left_tail_class_is_left_to_the_exit_ladder() {
    assert_eq!(
        verdict_for(50_000_000, -400_000_000, 8_000),
        BrainSizeVerdict::Identity,
        "positive median with negative mean is a tail-shape problem, not an admission one"
    );
}

/// The admission veto and the retirement review must apply the SAME definition of
/// "not paying". Divergence between them is what caused this defect.
#[test]
fn admission_and_retirement_agree_on_what_bleeding_means() {
    use pump_quant_app::brain_analysis::is_conditioned_negative;
    for (median, mean) in [
        (-100_000_000i128, 210_000_000i128), // lottery: pays in aggregate
        (-100_000_000, -80_000_000),         // genuine bleeder
        (50_000_000, -400_000_000),          // fat left tail
        (50_000_000, 60_000_000),            // healthy
    ] {
        let stats = RecallStats {
            n_matched: 20,
            median_net_lamports: median,
            mean_net_lamports: mean,
            win_count: 2,
            loss_count: 18,
            win_rate_bp: 1_000,
            p25_net_lamports: 0,
            p75_net_lamports: 0,
            median_hold_ns: 0,
            nearest_distance: 0,
            nearest_weighted_distance: 0,
            nearest_episode_id: 0,
        };
        let retirement_says_negative = is_conditioned_negative(&stats, 8);
        let admission_acts = !matches!(
            BrainPlane::verdict_from_stats(&stats, 8, HAIRCUT_WR_BP, VETO_WR_BP, HAIRCUT_MULT_BP),
            BrainSizeVerdict::Identity
        );
        assert_eq!(
            retirement_says_negative, admission_acts,
            "admission and retirement disagreed on median={median} mean={mean} — \
             they must share one definition of 'not paying'"
        );
    }
}
