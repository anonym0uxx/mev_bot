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

/// **Contention is real.** Widening `promote_k` strictly increases the number of
/// promotions, which is only possible if the top-`k` cut was BINDING — i.e. more
/// gate-eligible candidates cleared `promote_min_rank` on a tick than there were
/// slots. Without this the lane weight has nothing to decide and the whole A/B is
/// vacuous, so it is asserted rather than assumed.
#[test]
fn contention_is_real_more_candidates_than_promotion_slots() {
    let narrow = run(Config::dev_portable(), false, 250, Tape::happy());
    let mut wide_cfg = Config::dev_portable();
    wide_cfg.promote_k = 48;
    let wide = run(wide_cfg, false, 250, Tape::happy());
    println!(
        "CONTENTION promote_k=8 -> promoted={} admitted={}; promote_k=48 -> promoted={} admitted={}",
        narrow.report.promoted, narrow.report.admitted, wide.report.promoted, wide.report.admitted
    );
    assert!(
        wide.report.promoted > narrow.report.promoted,
        "the top-k cut must bind ({} vs {}), else the tape has no promotion contention",
        wide.report.promoted,
        narrow.report.promoted
    );
    assert!(
        narrow.report.admitted > 0,
        "the tape must actually trade, else there are no lamports to compare"
    );
    // …and both lanes must actually reach capital, else there is no cross-lane
    // ordering for a weight step to change.
    let social_net = narrow.report.per_lane_net[Lane::CreationSniper.index()].1;
    let wallet_net = narrow.report.per_lane_net[Lane::GraduationTransition.index()].1;
    println!("CONTENTION per-lane net social={social_net} wallet={wallet_net}");
    assert!(
        social_net != 0 && wallet_net != 0,
        "both contending lanes must realize P&L (social={social_net}, wallet={wallet_net})"
    );
}

/// **The decay flag genuinely fires — in BOTH regimes.** An "unhappy path" on which
/// the flag never fires proves nothing, and neither does a happy path.
///
/// This also pins the property that makes the comparison a test of the BRAIN rather
/// than of the incumbent estimator: the social lane's realized-net AGGREGATE is
/// POSITIVE (its runners carry it), so the incumbent per-lane reflection RAISES its
/// weight above the seeded 8_000 — the brain is the only thing pushing it down.
#[test]
fn the_decay_flag_fires_and_the_incumbent_does_not_already_downweight() {
    let cfg = Config::dev_portable();
    for (name, tape) in [("happy", Tape::happy()), ("unhappy", Tape::unhappy())] {
        let neutral = run_traced(cfg, false, 250, tape, true);
        let armed = run_traced(cfg, true, 250, tape, true);
        let social_net = neutral.report.per_lane_net[Lane::CreationSniper.index()].1;
        let w_n = neutral.report.final_weights[Lane::CreationSniper.index()].1;
        let w_a = armed.report.final_weights[Lane::CreationSniper.index()].1;
        println!(
            "FLAG {name}: neutral_trace={:?} armed_trace={:?} social_lane_net={social_net} \
             CreationSniper weight neutral={w_n} armed={w_a}",
            neutral.trace, armed.trace
        );
        // The flag genuinely fires, sustained over many rounds — sampled LIVE,
        // because the END-OF-RUN flag set is not representative: on this tape the
        // armed arm finishes with an EMPTY flag set even though it spent a quarter of
        // the run flagged. (That unflagging is the law self-correcting as forward
        // evidence accumulates, which is real behaviour and is reported as such.)
        assert!(
            armed.trace.social_rounds >= 8,
            "{name}: the decay flag must fire on the contended lane over a sustained \
             run of rounds ({} of {}), else nothing is being tested",
            armed.trace.social_rounds,
            armed.trace.samples
        );
        // …and the weight divergence is the airtight proof that it fired AT A
        // REFLECTION PASS: the ONLY path in `reflect_with_brain` that can lower a
        // weight below the unarmed pass is `decay.is_decayed(lane)`, so a gap of at
        // least one whole `brain_reflect_step_bp` cannot be produced any other way.
        assert!(
            w_a + 250 <= w_n,
            "{name}: the armed arm must carry a lane weight at least one full step \
             lower ({w_a} vs {w_n})"
        );
        // The property that makes this a test of the BRAIN and not of the incumbent:
        // the flagged lane's realized-net AGGREGATE is POSITIVE (its runners carry
        // it), so the incumbent per-lane reflection RAISED its weight above the seed.
        assert!(
            w_n > Lane::CreationSniper.default_weight_bp(),
            "{name}: the INCUMBENT realized-net reflection must have RAISED the lane \
             ({w_n} vs seed {}), else this measures the incumbent and not the brain",
            Lane::CreationSniper.default_weight_bp()
        );
        assert!(
            social_net > 0,
            "{name}: the flagged lane's aggregate must look ACCEPTABLE ({social_net})"
        );
        assert_reduce_only_and_bounded(&neutral, &armed, &cfg, name);
    }
}

// ===========================================================================
// The two-sided A/B.
// ===========================================================================

/// Pinned happy-path arms (armed − neutral), lamports. Re-measured at re-pin #26 on
/// a tape that actually trades; the retired pair (479_556_343 / 506_253_592) was
/// taken while the tape declared 0.2 SOL pools.
const HAPPY_NEUTRAL_NET: i128 = 555_444_680;
const HAPPY_ARMED_NET: i128 = 643_653_672;
/// Pinned unhappy-path (false-positive) arms. Retired pair: 601_202_914 / 580_193_240.
const UNHAPPY_NEUTRAL_NET: i128 = 1_346_209_124;
const UNHAPPY_ARMED_NET: i128 = 1_330_959_228;

/// **The pre-registered two-sided A/B at the default step.**
///
/// Re-pin #26: leg (a) still fails — by 12% — and **leg (b) now PASSES at 5.78×
/// against a 3× bar**, where it previously failed at 1.27×. The default stays OFF
/// because leg (a) is a leg and because arming is not a test author's call, NOT
/// because the law is inert. Every number is pinned so the next move on either leg
/// is loud.
#[test]
fn the_two_sided_verdict_at_the_default_step() {
    let cfg = Config::dev_portable();
    let step = cfg.brain_reflect_step_bp;

    let h_n = run(cfg, false, step, Tape::happy());
    let h_a = run(cfg, true, step, Tape::happy());
    let u_n = run(cfg, false, step, Tape::unhappy());
    let u_a = run(cfg, true, step, Tape::unhappy());
    assert_reduce_only_and_bounded(&h_n, &h_a, &cfg, "happy");
    assert_reduce_only_and_bounded(&u_n, &u_a, &cfg, "unhappy");

    let happy_gain = h_a.report.net_lamports - h_n.report.net_lamports;
    let unhappy_delta = u_a.report.net_lamports - u_n.report.net_lamports;
    println!(
        "B7-TWO-SIDED step={step} | HAPPY neutral={} armed={} gain={happy_gain} \
         (admitted {}->{}) | UNHAPPY neutral={} armed={} delta={unhappy_delta} \
         (admitted {}->{})",
        h_n.report.net_lamports,
        h_a.report.net_lamports,
        h_n.report.admitted,
        h_a.report.admitted,
        u_n.report.net_lamports,
        u_a.report.net_lamports,
        u_n.report.admitted,
        u_a.report.admitted,
    );

    assert_eq!(h_n.report.net_lamports, HAPPY_NEUTRAL_NET);
    assert_eq!(h_a.report.net_lamports, HAPPY_ARMED_NET);
    assert_eq!(u_n.report.net_lamports, UNHAPPY_NEUTRAL_NET);
    assert_eq!(u_a.report.net_lamports, UNHAPPY_ARMED_NET);

    // ---- Leg (a): does the happy path earn a MATERIAL amount? It does not.
    assert!(
        happy_gain > 0,
        "the happy path does at least move in the right direction ({happy_gain})"
    );
    assert!(
        happy_gain <= MATERIAL_LAMPORTS,
        "MEASURED: the happy-path gain ({happy_gain}) cleared the pre-registered \
         materiality bar of {MATERIAL_LAMPORTS} lamports (one 0.1-SOL bite). If this \
         fires, LAW B7 has started earning materially and leg (a) must be re-taken."
    );
    // …and it now MISSES that bar by 12%, not by an order of magnitude. Pinned as a
    // number rather than left to the inequality above, because "fails leg (a)" reads
    // very differently at 88_208_992 than it did at 26_697_249 and the difference is
    // the whole reason this law is now an open A-11 question.
    assert!(
        happy_gain * 10 > MATERIAL_LAMPORTS * 8,
        "the happy-path gain ({happy_gain}) is within 20% of the materiality bar — if \
         it ever falls well clear of it again, the A-11 study this file requests is no \
         longer owed and this comment should be retired"
    );

    // ---- Leg (b): is the false-positive cost small enough to justify arming?
    let loss = -unhappy_delta;
    assert!(
        loss > 0,
        "MEASURED: the false-positive arm must genuinely COST ({unhappy_delta}); a \
         zero-cost unhappy path would make leg (b) vacuous"
    );
    println!(
        "B7-RATIO happy_gain={happy_gain} unhappy_loss={loss} ratio={}.{:02} \
         (pre-registered bar: {REQUIRED_RATIO})",
        happy_gain / loss,
        (happy_gain * 100 / loss) % 100
    );
    // **LEG (b) NOW PASSES.** This assertion used to read `happy_gain < REQUIRED_RATIO
    // * loss` and to fire with "cleared the pre-registered bar" as a warning. It fired.
    // The bar is cleared — 5.78× against 3× — and the honest record is the pin below,
    // not a re-tightened inequality. Leg (b) failing again would be a real change and
    // is what this now catches.
    assert!(
        happy_gain >= REQUIRED_RATIO * loss,
        "MEASURED: leg (b) has gone back to FAILING ({happy_gain}/{loss} under the \
         {REQUIRED_RATIO}× bar). It passed at re-pin #26 at 5.78×; a return to failure \
         is a substantive change and the A-11 study request should be withdrawn."
    );
}

/// **THE VERDICT *IS* AN ARTIFACT OF THE STEP SIZE — the single most important thing
/// this file now says.**
///
/// This test was written to rule that out, and at re-pin #26 it caught the opposite.
/// The two-sided comparison is run at 250, 1_000 and 5_000 bp (the last is ~13% of the
/// §56.2 envelope width):
///
/// * **250 bp (shipped)** — gain +88_208_992 against a 100_000_000 bar: leg (a) fails
///   by 12%. The rule does NOT pass.
/// * **1_000 bp** — gain +375_495_781, loss −38_190_136 (9.83×). The rule PASSES.
/// * **5_000 bp** — gain +703_394_355, loss −145_823_020 (4.82×). The rule PASSES.
///
/// So "LAW B7 does not clear its bar" is true of the shipped step and false of the
/// two larger ones, and the shipped step was never itself the output of a study — it
/// is `brain_reflect_step_bp`'s default. **A law whose verdict is decided by an
/// unstudied knob has not been decided.** The default stays OFF (arming is not a test
/// author's call) and this pins the shape of the open question for A-11.
#[test]
fn the_verdict_is_an_artifact_of_the_step_size() {
    let cfg = Config::dev_portable();
    for step in [250u32, 1_000, 5_000] {
        let h_n = run(cfg, false, step, Tape::happy());
        let h_a = run(cfg, true, step, Tape::happy());
        let u_n = run(cfg, false, step, Tape::unhappy());
        let u_a = run(cfg, true, step, Tape::unhappy());
        assert_reduce_only_and_bounded(&h_n, &h_a, &cfg, "happy-sweep");
        assert_reduce_only_and_bounded(&u_n, &u_a, &cfg, "unhappy-sweep");

        let gain = h_a.report.net_lamports - h_n.report.net_lamports;
        let delta = u_a.report.net_lamports - u_n.report.net_lamports;
        println!(
            "B7-STEP-SWEEP step={step} happy_gain={gain} unhappy_delta={delta} \
             happy_w={:?} unhappy_w={:?}",
            h_a.report.final_weights.map(|w| w.1),
            u_a.report.final_weights.map(|w| w.1),
        );
        let passes = gain > MATERIAL_LAMPORTS && (delta >= 0 || gain >= REQUIRED_RATIO * (-delta));
        // Pinned per step, MEASURED. The shipped step is the ONLY one that fails, and
        // it fails on leg (a) alone.
        assert_eq!(
            passes,
            step > 250,
            "MEASURED: the pre-registered rule's answer at step {step} changed \
             (gain={gain}, unhappy={delta}, passes={passes}). Re-pin #26 measured \
             FAIL at 250 and PASS at 1_000 and 5_000; either direction of drift is a \
             substantive change to an open A-11 question and must be reported, not \
             re-pinned silently."
        );
    }
    // The shipped step is the failing one — stated explicitly, because the whole
    // finding is that the verdict rides on this default and this default was never
    // studied.
    assert_eq!(
        Config::dev_portable().brain_reflect_step_bp,
        250,
        "the shipped step is the one at which the rule does not pass; if the default \
         moves, LAW B7's verdict moves with it and must be re-taken"
    );
}

/// **THE RESHUFFLE DIAGNOSIS IS RETIRED.**
///
/// A law that earns should earn *because the flag was right*. The strongest argument
/// for LAW B7's default-OFF was that it did not: re-running the identical experiment
/// on neighbouring market shapes produced cells where the armed arm did BETTER when
/// its flag was a FALSE POSITIVE than when it was correct, which is the signature of
/// a permutation of the promotion order rather than of an edge.
///
/// **At real depth that inversion does not occur on any of the five shapes.** The
/// true-positive delta exceeds the false-positive delta everywhere:
///
/// | shape | true-positive | false-positive |
/// |---|---|---|
/// | headline | +88_208_992 | −15_249_896 |
/// | mild-bleeder | +219_767_450 | +9_367_800 |
/// | small-runner | +54_111_031 | −205_543_902 |
/// | huge-runner | +6_152_118 | −26_450_910 |
/// | huge-runner-mild | +51_880_006 | −42_260_853 |
///
/// The old table was measured on a tape whose 0.2 SOL pools refused every trade, so
/// every cell in it was `0` against `0` and the "inversion" was read out of a corpus
/// that had no admissions in it at all.
///
/// The assertion is inverted to match: no shape may invert. If one ever does, the
/// reshuffle diagnosis is back and the A-11 study this file requests is answered in
/// the negative.
#[test]
fn the_sign_of_the_effect_now_tracks_whether_the_flag_is_right() {
    let cfg = Config::dev_portable();
    let shapes: [(&str, [i64; 3], [i64; 3]); 5] = [
        ("headline", RUNNER_TAIL_BP, BAD_TAIL_BP),
        ("mild-bleeder", RUNNER_TAIL_BP, MILD_BAD_TAIL_BP),
        ("small-runner", SMALL_RUNNER_TAIL_BP, BAD_TAIL_BP),
        ("huge-runner", HUGE_RUNNER_TAIL_BP, BAD_TAIL_BP),
        ("huge-runner-mild", HUGE_RUNNER_TAIL_BP, MILD_BAD_TAIL_BP),
    ];
    let mut inversion_seen = false;
    let mut best_true_positive = i128::MIN;
    for (name, runner, bad) in shapes {
        let happy = Tape::happy().with_tails(runner, bad);
        let unhappy = Tape::unhappy().with_tails(runner, bad);
        let h_n = run(cfg, false, 250, happy);
        let h_a = run(cfg, true, 250, happy);
        let u_n = run(cfg, false, 250, unhappy);
        let u_a = run(cfg, true, 250, unhappy);
        assert_reduce_only_and_bounded(&h_n, &h_a, &cfg, name);
        assert_reduce_only_and_bounded(&u_n, &u_a, &cfg, name);
        let gain = h_a.report.net_lamports - h_n.report.net_lamports;
        let delta = u_a.report.net_lamports - u_n.report.net_lamports;
        println!(
            "B7-SHAPE {name}: true_positive_delta={gain} false_positive_delta={delta} \
             (bases {} / {})",
            h_n.report.net_lamports, u_n.report.net_lamports
        );
        best_true_positive = best_true_positive.max(gain);
        if delta > gain {
            inversion_seen = true;
        }
    }
    assert!(
        !inversion_seen,
        "MEASURED: on at least one market shape the armed arm does BETTER when the \
         decay flag is a FALSE POSITIVE than when it is correct. That is the RESHUFFLE \
         signature, and it returning would answer the open A-11 question on LAW B7 in \
         the negative — report it, do not re-pin it."
    );
    // …and the true-positive side must actually be positive, or "no inversion" would
    // be satisfiable by a law that does nothing on either side.
    assert!(
        best_true_positive > 0,
        "the armed arm must genuinely EARN on at least one true-positive shape \
         ({best_true_positive}), else the no-inversion result is vacuous"
    );
}

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
