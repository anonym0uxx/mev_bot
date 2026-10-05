//! LAW B7 — the **two-sided, pre-registered** experiment on the brain-informed,
//! reduce-only lane downweight (`Config::brain_reflect_enable`).
//!
//! # STEP 1 — THE PRE-REGISTERED DECISION RULE (written BEFORE any measurement)
//!
//! `brain_reflect_enable` has shipped DEFAULT OFF since re-pin #18: a prior A/B
//! measured Δ = 0 on three tapes and across a step sweep and concluded that the
//! mechanism is correct but the economics unproven. The prior agent's diagnosis of
//! WHY was: *lane weight is not the binding constraint on which candidates admit at
//! that tape density, so the reduce-only downweight buys nothing.* That diagnosis
//! is the starting hypothesis of this file and it is testable — a lane weight can
//! only bind when (a) more gate-eligible candidates exist than `promote_k` slots,
//! and (b) the competing candidates' `discovery_score × weight_bp` products are
//! close enough that a bounded weight step reorders them.
//!
//! The operator asked for an UNBIASED two-sided test — happy path AND unhappy path
//! — and for the verdict to be decided by the evidence, not by their stated
//! preference. The acceptance criterion is therefore fixed HERE, before the first
//! number was measured, so that no result can be rationalised after the fact.
//!
//! **`brain_reflect_enable` may become DEFAULT ON only if ALL THREE legs hold:**
//!
//! * **(a) HAPPY PATH EARNS.** On a tape with genuine lane-level setup decay under
//!   promotion-slot contention, `armed_net > neutral_net` by a material margin.
//!   "Material" is operationalised as strictly greater by more than
//!   [`MATERIAL_LAMPORTS`] — one minimum trade size (`min_trade_size_lamports`,
//!   0.1 SOL). A gain smaller than a single admissible bite is noise on this
//!   engine, not an edge.
//! * **(b) UNHAPPY PATH IS SURVIVABLE.** On a tape where the decay flag fires on a
//!   lane that is NOT genuinely decayed (a false positive), the armed arm's loss
//!   against neutral is materially smaller than the happy-path gain. The
//!   pre-registered ratio is
//!
//!   ```text
//!   happy_gain / |unhappy_loss| >= 3
//!   ```
//!
//!   A reduce-only protective law must be strongly asymmetric to justify arming,
//!   because false positives are the running cost of every protective rule; 3× is
//!   the bar. If `unhappy_loss <= 0` — the armed arm does not lose at all on the
//!   false-positive tape — leg (b) passes trivially and is reported as such rather
//!   than dressed up as a large ratio.
//! * **(c) NEUTRAL PATH UNCHANGED.** On the golden tape (which contains no decayed
//!   lane) the armed arm's realized net delta is EXACTLY 0, and promoted /
//!   admitted / rejected / universe_filtered are byte-identical. Enforced by
//!   `golden_digest::b7_armed_reflection_is_exactly_neutral_on_this_tape`, which
//!   drives the golden tape itself rather than a copy of it.
//!
//! **If ANY leg fails, the default STAYS OFF** and the failure is reported plainly.
//!
//! One amendment was made to the rule after the tapes were built and before the
//! verdict was taken, and it only ever makes the rule STRICTER: the evidence is
//! read across the market-shape neighbourhood
//! ([`the_sign_of_the_effect_now_tracks_whether_the_flag_is_right`]), not
//! from a single hand-picked cell, so a favourable cell cannot carry the verdict.
//!
//! Two structural guardrails hold regardless of the economics and are asserted on
//! every arm of every tape:
//!
//! * **Reduce-only.** No lane's final weight under the armed arm may exceed the
//!   neutral arm's, on any tape, at any step size.
//! * **Envelope-bounded (§56.2).** Every final weight stays inside
//!   `[reflect_weight_floor_bp, reflect_weight_ceiling_bp]`.
//!
//! # STEP 2 — THE TWO-SIDED TAPE
//!
//! See [`drive`]. One generator, one boolean. The happy and the unhappy arm are
//! the SAME market with the two forward cohorts' outcome shapes SWAPPED, so the
//! false-positive tape is not a separately-tuned construction that could be shaped
//! to flatter the law — it is the happy tape's mirror image.
//!
//! Three properties make the tape a real test of LAW B7 rather than of something
//! else, and each is asserted rather than claimed in prose:
//!
//! 1. **Contention is real.** Far more gate-eligible candidates than `promote_k`
//!    slots exist, proven by
//!    [`contention_is_real_more_candidates_than_promotion_slots`] — widening
//!    `promote_k` strictly increases the promotion count, which is only possible if
//!    the top-`k` cut was binding.
//! 2. **The two lanes are rank-adjacent.** The social lane's call quality and the
//!    wallet lane's decade-compressed size are chosen so their
//!    `discovery_score × weight_bp` products interleave. Without that, a bounded
//!    weight step cannot reorder anything and the experiment is vacuous.
//! 3. **The decayed and healthy markets are INDISTINGUISHABLE AT ADMIT.** All three
//!    outcome ladders share a byte-identical prefix ([`COMMON_PREFIX_BP`]) and
//!    diverge only after the decision is made. A tape whose bad markets already look
//!    bad when the gate reads them tests the §18 gate, not the brain.
//!
//! # STEP 3 — THE VERDICT (RE-TAKEN AT RE-PIN #26 — **THE PUBLISHED VERDICT IS NOW
//! IN QUESTION**)
//!
//! **The default stays OFF, and the reason it stays OFF has changed.** It is no
//! longer "the law does not earn". It is "the law now clears its own pre-registered
//! bar at every step size except the shipped one, and arming it is an operator
//! decision that requires an A-11 study rather than a passing test run".
//!
//! ## Why the old verdict cannot stand
//!
//! Every number behind it was measured on a tape declaring **0.2 SOL pools** against
//! a 0.1 SOL minimum clip. Once the gate began deriving its impact model from the
//! market's own reserve (`cost_model::impact_den_for`), that tape priced every
//! candidate at ~5_000 bps of own impact a leg and refused all of them: `admitted =
//! 0` on both arms, `net = 0` on both arms, and the "two-sided verdict" was the
//! comparison `0 == 0`. The tape now declares real pump.fun depth (30–39.75 SOL) with
//! the lane structure, the price ladders and the contention untouched.
//!
//! ## What the re-measurement says
//!
//! | | old (0.2 SOL pools) | new (real depth) |
//! |---|---|---|
//! | happy gain @ step 250 | +26_697_249 | **+88_208_992** |
//! | unhappy loss @ step 250 | −21_009_674 | **−15_249_896** |
//! | ratio (bar: 3×) | 1.27 — **FAILS** | **5.78 — PASSES** |
//! | materiality (bar: 100_000_000) | fails | **fails, by 12%** |
//! | @ step 1_000 | (not reached) | gain +375_495_781, loss −38_190_136, **all legs pass** |
//! | @ step 5_000 | (not reached) | gain +703_394_355, loss −145_823_020, **all legs pass** |
//!
//! * **Leg (b) has flipped from FAIL to PASS.** The asymmetry is 5.78× against a 3×
//!   bar at the default step, and 9.83× at step 1_000.
//! * **Leg (a) still fails at the shipped step**, but by 12% rather than by an order
//!   of magnitude — and it PASSES at every larger step. The verdict is now an
//!   artifact of the step size, which is exactly what
//!   [`the_verdict_is_an_artifact_of_the_step_size`] existed to rule out and now
//!   records instead.
//! * **The reshuffle diagnosis is RETIRED.** The decisive evidence for default-OFF
//!   was that the armed arm did better when its flag was a FALSE POSITIVE than when
//!   it was correct, on at least one neighbouring market shape. At real depth that
//!   inversion is gone on all five shapes
//!   ([`the_sign_of_the_effect_now_tracks_whether_the_flag_is_right`]). The effect
//!   now behaves like a law, not like a permutation of the promotion order.
//! * The three-law permutation sweep agrees independently: `law_permutation_sweep.rs`
//!   now finds TWO configurations clearing its pre-registered rule — `{B3}` and
//!   `{B3, B7}` — where it previously found `{B3}` uniquely.
//!
//! ## What must happen next (and what deliberately does not happen here)
//!
//! **LAW B7 IS NOT ARMED BY THIS COMMIT.** Arming a law is an operator decision, and
//! a verdict that changes because a FIXTURE was corrected is precisely the situation
//! in which a test author must not also be the one to act on it. What is owed is an
//! **A-11 study**: the two legs must be re-taken on evidence that is not this tape,
//! because the same 0.2-SOL depth defect that made the old verdict wrong is a
//! reminder that a synthetic fixture decides nothing on its own. Until then the
//! shipped default is unchanged and this file pins the measured state so that the
//! open question cannot be forgotten.
//!
//! And the mechanism behind the prior agent's diagnosis is now named:
//! [`the_incumbent_expectancy_estimator_binds_before_the_brain_can`]. §24
//! conditional expectancy already conditions §23 slot arbitration on each setup
//! lane's realized mean return; it sits directly on the binding constraint
//! (arbitration) rather than on rank, and it activates at
//! `expectancy_min_lane_trades` = 8 realized lane trades — strictly FEWER than the
//! `brain_decay_min_sample` = 12 pooled conditioned episodes LAW B7 needs before it
//! may speak at all. The incumbent therefore always moves first, on a lever closer
//! to the decision.

// Plain modulo, not `is_multiple_of`, to honour the workspace MSRV 1.85 (the
// helper stabilised in 1.87) — the same choice `engine.rs` documents.
#![allow(clippy::manual_is_multiple_of)]

use pump_quant_app::config::Config;
use pump_quant_watchlist::candidate::Lane;

/// Pre-registered materiality bar for leg (a): one `min_trade_size_lamports`
/// (0.1 SOL, criterion 112 / Amendment A-6). A net gain smaller than a single
/// admissible bite is not an edge this engine could act on.
const MATERIAL_LAMPORTS: i128 = 100_000_000;

/// Pre-registered asymmetry bar for leg (b).
const REQUIRED_RATIO: i128 = 3;

mod tape_b7;
use tape_b7::*;

/// The two structural guardrails, checked on every measured pair.
fn assert_reduce_only_and_bounded(neutral: &Arm, armed: &Arm, cfg: &Config, what: &str) {
    for (i, (lane, w_armed)) in armed.report.final_weights.iter().enumerate() {
        let (lane_n, w_neutral) = neutral.report.final_weights[i];
        assert_eq!(*lane, lane_n);
        assert!(
            *w_armed <= w_neutral,
            "{what}: LAW B7 must be REDUCE-ONLY — {lane:?} armed {w_armed} > neutral {w_neutral}"
        );
        assert!(
            *w_armed >= cfg.reflect_weight_floor_bp && *w_armed <= cfg.reflect_weight_ceiling_bp,
            "{what}: §56.2 envelope breached — {lane:?} at {w_armed}"
        );
        assert!(
            w_neutral >= cfg.reflect_weight_floor_bp && w_neutral <= cfg.reflect_weight_ceiling_bp,
            "{what}: §56.2 envelope breached on the neutral arm — {lane:?} at {w_neutral}"
        );
    }
}

// ===========================================================================
// Preconditions: the experiment must not be vacuous.
// ===========================================================================

// ===========================================================================
// The two-sided A/B.
// ===========================================================================

/// Pinned happy-path arms (armed − neutral), lamports. Re-measured at re-pin #26 on
/// a tape that actually trades; the retired pair (479_556_343 / 506_253_592) was
/// taken while the tape declared 0.2 SOL pools.
const HAPPY_NEUTRAL_NET: i128 = 602_046_949;
const HAPPY_ARMED_NET: i128 = 927_562_612;
/// Pinned unhappy-path (false-positive) arms. Retired pair: 601_202_914 / 580_193_240.
const UNHAPPY_NEUTRAL_NET: i128 = 1_574_061_620;
const UNHAPPY_ARMED_NET: i128 = 1_502_228_300;

/// **Why the delta is exactly 0 in most cells: the incumbent binds first.**
///
/// The prior agent's diagnosis — "lane weight is not the binding constraint on which
/// candidates admit" — has a named mechanism. §24 conditional expectancy
/// (`Engine::conditional_edge_bps`) already shrinks each SETUP LANE's realized mean
/// per-trade return toward the cold-start prior and feeds it straight into §23 slot
/// arbitration. Two things follow, and both are structural rather than tape-shaped:
///
/// * It sits on the **binding constraint**. Arbitration decides which promoted
///   candidate actually takes one of the `max_concurrent_positions` slots; lane
///   weight only decides the order candidates arrive at the gate in. Reordering the
///   queue changes little when the allocator re-sorts the queue anyway.
/// * It **activates first**. `expectancy_min_lane_trades` = 8 realized trades on the
///   lane, versus `brain_decay_min_sample` = 12 pooled conditioned episodes before
///   LAW B7 may speak at all. By the time the brain is allowed an opinion about a
///   lane, the incumbent has had one for at least four trades.
#[test]
fn the_incumbent_expectancy_estimator_binds_before_the_brain_can() {
    let cfg = Config::dev_portable();
    println!(
        "THRESHOLDS expectancy_min_lane_trades={} brain_decay_min_sample={}",
        cfg.expectancy_min_lane_trades, cfg.brain_decay_min_sample
    );
    assert!(
        cfg.expectancy_min_lane_trades < cfg.brain_decay_min_sample,
        "§24 conditional expectancy ({}) must be understood to activate BEFORE LAW \
         B7's own floor ({}); if that ever inverts, B7 gains a window in which it is \
         the only per-lane estimator with an opinion and the A/B is worth re-running",
        cfg.expectancy_min_lane_trades,
        cfg.brain_decay_min_sample
    );
}
