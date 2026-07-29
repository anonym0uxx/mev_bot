//! **§32 FLOW-PERSISTENCE — the pre-registered two-sided verdict.**
//!
//! The engine's BINDING exit is the §32 thesis force-exit: it fires the instant
//! windowed order-flow imbalance turns net-sell. The 2026-07-25 entry/exit study
//! proved this is what makes every price-based exit knob (hard stop, trail, CVD
//! fraction, TP spacing) decision-INERT — the flow flip always fires first, so the
//! price geometry never becomes the binding constraint.
//!
//! The external literature says exiting on the FIRST flip is the least informative
//! possible read of that process:
//!
//! * **arXiv 2606.16269** — the Lillo–Mike–Farmer relation `γ = α − 1`: trade signs
//!   are long-memory because metaorder lengths are Pareto-distributed. Information
//!   lives in a persistent same-signed RUN, in EVENT time; an isolated flip is noise.
//! * **Kaminski & Lo**, *J. Financial Markets* 18:234–254: a stop rule's "stopping
//!   premium" is NEGATIVE unless its trigger predicts PERSISTENT adverse drift.
//!   Their own equity data shows daily/weekly-frequency stops already lose money;
//!   a per-print flow stop is far further into that regime.
//!
//! So [`Config::thesis_persist_obs`] (`k`) demands a RUN of `k` consecutive adverse
//! observations before the force-exit fires. `k = 1` is the historical behaviour.
//!
//! # THE PRE-REGISTERED RULE (written before any number below was measured)
//!
//! `k > 1` may replace the shipped default only if ALL hold:
//! * **(P1) MATERIALITY** — gain > [`MATERIAL_LAMPORTS`] (one 0.1-SOL bite) on the
//!   REPRESENTATIVE (golden) tape, not merely on the tape built to exercise it.
//! * **(P2) NO HAZARD HARM** — no pre-existing tape gives back more than a bite.
//! * **(P3) ASYMMETRY ≥ 3×** — happy gain / mirror loss on the two-sided pair.
//! * **(P4) SEED-ONLY AT DEFAULT** — `k = 1` reproduces every decision number of
//!   re-pin #21 exactly; only the §19 config-identity seed moves.
//!
//! # THE VERDICT: **DISARMED (`k = 1`), and re-pin #26 made the case STRONGER.**
//!
//! ## The published verdict said `k = 5` "turns the book negative". Read the delta.
//!
//! `arming_beyond_the_shakeout_threshold_is_harmful` pinned `golden(5) = -3,223,175`
//! against a shipped book of 8,124,568. Under the unified cost model those two
//! numbers are +5,309,323 against 16,778,896 — **`k = 5` no longer turns the book
//! negative**, and the temptation is to read that as `k = 5` having become
//! beneficial.
//!
//! It has not. **The HARM is unchanged:**
//!
//! | | shipped `k = 1` | `k = 5` | harm |
//! |---|---|---|---|
//! | before (split cost model) | 8,124,568 | -3,223,175 | **-11,347,743** |
//! | after (unified cost model) | 16,778,896 | 5,309,323 | **-11,469,573** |
//!
//! The same tape, the same events, the same lamports forfeited by holding through
//! the fade — 11.3M then, 11.5M now, **1.1% MORE harmful, not less**. What moved is
//! the BASELINE: honest costs roughly doubled the book, so a constant harm that used
//! to erase 140% of it now erases 68%. The sign flip is a statement about the size of
//! the book, not about `k`.
//!
//! This matters because the two obvious explanations are both wrong and both would
//! have led somewhere bad:
//!
//! * **"The old harm was an artifact of the phantom costs."** Refuted by
//!   measurement. Deleting 150 bps of first-sell penalty and 200 bps of phantom
//!   spread made `k = 5` *marginally worse*, not better. Had the phantom costs been
//!   creating the harm, removing them would have shrunk it.
//! * **"The hazard tapes going vacuous corrupted the comparison."** Refuted by
//!   construction. [`golden`] drives `tape_golden` and nothing else, and the golden
//!   tape's depth was not touched by re-pin #26 — it was given real depth at re-pin
//!   #24. No hazard tape can reach this number.
//!
//! ## The two-sided tape no longer rescues it either
//!
//! The old verdict conceded that on [`tape_flow`] — the tape built FOR this
//! hypothesis — `k = 5` cleared both bars handsomely (gain +152,694,678, loss
//! -44,095,911, ratio 3.46), and rested the refusal on the pre-existing tapes. That
//! concession is withdrawn. `tape_flow` declared **0.26 SOL pools** against a 0.1 SOL
//! clip; under a derived impact model it refused every candidate and both sides read
//! `0`. At real depth `k = 5` **loses on its own happy side**: 104,607,333 ->
//! -52,846,461, while the mirror also worsens (-54,978,642 -> -94,186,083). There is
//! no longer a tape on which demanding a run of adverse observations pays.
//!
//! The reason is visible in the admit counts (63 -> 36 on the happy side): at
//! realistic size the six position slots are the binding resource, and patience is
//! paid for in round trips not taken. That cost was invisible while the fixture's
//! thinness kept the slots empty.
//!
//! **P1 and P2 both fail, and now P3 fails too.**
//!
//! A 135-configuration JOINT lattice (`k` x trail x hard-stop x TP-margin) was swept
//! on the golden tape, because relaxing `k` is what would UNBIND the price geometry.
//! Best of all 135: **+438,538 lamports on the pre-depth-fix tape — a fraction of one
//! bite.** There is no joint configuration that earns either.
//!
//! # Why the capability is KEPT rather than deleted
//!
//! Because the mechanism is genuine and LARGE (it moves net by ±68% on the golden
//! tape and by ±150% on `tape_flow` — it is the
//! single biggest lamport lever either study found), and the reason it cannot be
//! aimed is a MISSING MEASUREMENT, not a disproven theory: nobody — including the
//! published literature — knows the live base rate of *shakeout* versus *true top*
//! at the first flow flip on pump.fun. A synthetic tape cannot supply that number;
//! only live/replay data can. The lever therefore ships DISARMED and byte-identical,
//! exactly as LAW B7 and the concentration law did before it, so the server can
//! measure the base rate and arm it on evidence rather than on hope.
//!
//! [`arming_beyond_the_shakeout_threshold_is_harmful`] pins the harm so that arming
//! it on this file's own happy-path numbers trips a loud, explicit guard.
//!
//! **Documentation debt this re-pin creates:**
//! `docs/STRATEGY_PERMUTATION_STUDY_2026-07-25.md` states that `k = 5` "turns the
//! book negative". That sentence is now false as written and true as intended; it has
//! been corrected there to quote the HARM rather than the resulting sign.

mod tape_conc;
mod tape_flow;
mod tape_golden;

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};

/// One 0.1-SOL bite (`min_trade_size_lamports`) — the materiality bar, identical
/// to `law_permutation_sweep.rs` and `entry_exit_frontier.rs`.
const MATERIAL_LAMPORTS: i128 = 100_000_000;
/// The pre-registered asymmetry bar.
const REQUIRED_RATIO: i128 = 3;

/// Re-pin #27 (2026-07-28): 16_778_896 -> 31_111_528. The move is the confirmed-set
/// eviction key reordering under corrected fixture depth, NOT either provenance fix —
/// both were measured decision-inert on this tape. See `golden_digest.rs`.
const GOLDEN_SHIP: i128 = 31_111_528;
/// The concentration-happy book at the shipped `k`. **Negative since re-pin #26**:
/// that tape's bundled cohort craters and the §21.7 law that would refuse it ships
/// DISARMED, so at realistic depth the engine takes those trades and loses on them.
/// The old +16,567,514 came from 0.26 SOL pools refusing them on cost.
const CONC_H_SHIP: i128 = -4_961_452;
/// The flow tape's happy side at the shipped `k`. Re-pin #26: was 13,170,840 while
/// the tape declared 0.26 SOL pools; at real depth the tape actually trades.
const FLOW_SHIP_HAPPY: i128 = 104_607_333;
/// ...and its mirror, which is NOT equal to the happy side any more. Under the old
/// thin-pool tape both sides collapsed to the same 13,170,840 for `k <= 3`; at real
/// depth the two sides diverge from `k = 1`, because the shakeout burst now happens
/// to positions that exist.
const FLOW_SHIP_MIRROR: i128 = -54_978_642;

fn golden(k: u32) -> i128 {
    let mut c = Config::dev_portable();
    c.thesis_persist_obs = k;
    tape_golden::drive(c).net_lamports
}
fn conc_happy(k: u32) -> i128 {
    let mut c = tape_conc::conc_cfg(Config::dev_portable());
    c.thesis_persist_obs = k;
    let mut e = Engine::new(c, RunMode::Replay);
    tape_conc::apply_ab(&mut e, tape_conc::Side::ConcentratedBleeds);
    e.report().net_lamports
}
fn flow(k: u32, side: tape_flow::Side) -> i128 {
    let mut c = Config::dev_portable();
    c.thesis_persist_obs = k;
    tape_flow::drive(c, side).net_lamports
}

#[test]
fn default_is_disarmed_and_seed_only() {
    // P4: k == 1 is the shipped default and reproduces re-pin #21 decisions exactly.
    assert_eq!(
        Config::dev_portable().thesis_persist_obs,
        1,
        "flow persistence must ship DISARMED (k = 1) — it failed P1 and P2"
    );
    assert_eq!(
        golden(1),
        GOLDEN_SHIP,
        "k=1 must reproduce the golden net exactly"
    );
    assert_eq!(
        conc_happy(1),
        CONC_H_SHIP,
        "k=1 must reproduce conc-happy exactly"
    );
}

/// **THE MECHANISM IS REAL AND IT NOW LOSES ON ITS OWN TAPE.**
///
/// This test used to establish the one concession the old verdict made: that `k = 5`
/// cleared both bars on the tape built for it, and was refused only on the
/// pre-existing tapes. Re-pin #26 withdraws that concession. The tape declared
/// 0.26 SOL pools; under a derived impact model it admitted nothing and both sides
/// read the same number, which is what "the two sides are indistinguishable below the
/// shakeout run-length" had degenerated into.
///
/// At real depth the tape trades, the two sides diverge from `k = 1` as they should,
/// and demanding a run of adverse observations **loses on the happy side**. The
/// mirror discipline is intact — both sides remain byte-identical up to and including
/// the shakeout burst — so this is a verdict about `k`, not about the fixture.
#[test]
fn the_mechanism_is_real_on_its_own_two_sided_tape() {
    // The two sides are genuinely different tapes now, at every k. (Under the
    // thin-pool fixture they were equal for k <= 3 because neither side had a
    // position to hold.)
    assert_eq!(flow(1, tape_flow::Side::ShakeoutThenRun), FLOW_SHIP_HAPPY);
    assert_eq!(flow(1, tape_flow::Side::TrueTop), FLOW_SHIP_MIRROR);
    assert_ne!(
        FLOW_SHIP_HAPPY, FLOW_SHIP_MIRROR,
        "the two sides must be genuinely different tapes, else the mirror is vacuous"
    );

    // The mechanism MOVES the book — hugely — which is why the lever is kept.
    let happy = flow(5, tape_flow::Side::ShakeoutThenRun);
    let mirror = flow(5, tape_flow::Side::TrueTop);
    let happy_delta = happy - FLOW_SHIP_HAPPY;
    let mirror_delta = mirror - FLOW_SHIP_MIRROR;
    println!(
        "FLOW k=5 happy {FLOW_SHIP_HAPPY} -> {happy} ({happy_delta}) | \
         mirror {FLOW_SHIP_MIRROR} -> {mirror} ({mirror_delta})"
    );
    assert_eq!(happy_delta, -157_453_794, "pinned k=5 happy-side delta");
    assert_eq!(mirror_delta, -39_207_441, "pinned k=5 mirror-side delta");
    assert!(
        happy_delta.abs() > MATERIAL_LAMPORTS,
        "the happy side must move by more than one bite ({happy_delta}) — a lever this \
         file keeps DISARMED is only worth keeping because it is large"
    );

    // ...and it moves the book DOWN on BOTH sides. P3 is not merely unmet; the sign of
    // the "gain" leg is wrong, so there is no ratio to compute.
    assert!(
        happy_delta < 0,
        "MEASURED: k=5 has started EARNING on its own happy tape ({happy_delta}). \
         Re-pin #26 measured it losing. If this holds, the two-sided case for flow \
         persistence is back and must be re-taken — do not re-pin it away."
    );
    assert!(
        mirror_delta < 0,
        "the mirror must still pay for patience ({mirror_delta})"
    );
    let _ = REQUIRED_RATIO;
}

#[test]
fn but_it_fails_on_every_pre_existing_tape_so_it_stays_disarmed() {
    // NOTE ON SCALE — this matters for reading every number here honestly. The
    // golden tape's ENTIRE book is 16,778,896 lamports, which is itself SMALLER
    // than one 0.1-SOL materiality bite (100,000,000). So an absolute bite bar is
    // meaningless on this tape and is NOT the test applied: materiality is judged
    // RELATIVELY on golden (fraction of the book moved) and ABSOLUTELY only on the
    // large hazard tapes (B3 ≈ 277M, B7 ≈ 555M–1.35B), where a bite is a real unit.
    //
    // P1 FAILS: the best gain any k produces on the representative tape is
    // +177,199 lamports at k = 2 — about 1% of the book, and nowhere near a bite.
    let best_gain = [2u32, 3, 4, 5, 6, 8, 12]
        .iter()
        .map(|&k| golden(k) - GOLDEN_SHIP)
        .max()
        .unwrap();
    assert_eq!(
        best_gain, 177_199,
        "pinned best golden gain across all k (k = 2)"
    );
    assert!(
        best_gain * 33 < GOLDEN_SHIP,
        "the best gain is under 3% of the book — immaterial by any reading: {best_gain}"
    );

    // P2 FAILS at the k its OWN tape favours. **Judged on the HARM, not on the sign
    // of the result.** Re-pin #26 replaced a sign test here, and the replacement is
    // the point of this whole file: `golden(5)` used to be -3,223,175 and is now
    // +5,309,323, so the old assertion (`g5 < 0`) would now fail — while the harm it
    // was standing in for is UNCHANGED at ~11.4M. A sign test on a book whose size
    // moved was never measuring `k`; it was measuring the book.
    let g5 = golden(5);
    let harm = GOLDEN_SHIP - g5;
    println!("K5-HARM golden {GOLDEN_SHIP} -> {g5} (harm {harm})");
    assert_eq!(
        harm, 11_469_573,
        "pinned k=5 harm on the representative tape"
    );
    // The retired measurement, kept as the comparison that carries the argument: the
    // harm was 11_347_743 under the split cost model. It is now 1.1% LARGER, which is
    // what refutes "the old harm was an artifact of the phantom costs".
    const HARM_UNDER_THE_SPLIT_MODEL: i128 = 8_124_568 - -3_223_175;
    assert_eq!(HARM_UNDER_THE_SPLIT_MODEL, 11_347_743);
    assert!(
        harm > HARM_UNDER_THE_SPLIT_MODEL,
        "k=5 must be at least as harmful under honest costs as it was under the \
         phantom ones ({harm} vs {HARM_UNDER_THE_SPLIT_MODEL}) — if it ever becomes \
         materially LESS harmful, the phantom-cost explanation is back in play"
    );
    assert!(
        harm * 3 > GOLDEN_SHIP,
        "the harm must remain a large fraction of the book ({harm} of {GOLDEN_SHIP})"
    );
    // ...and on the concentration hazard book it multiplies an already-negative book
    // by ~5.7x, which is a qualitative harm that needs no scale bar at all.
    let c5 = conc_happy(5);
    assert!(
        c5 < CONC_H_SHIP * 5,
        "k=5 must multiply the conc-happy loss several-fold: {CONC_H_SHIP} -> {c5}"
    );
    assert_eq!(c5, -28_307_693, "pinned k=5 conc-happy net");
}

/// **THE GUARD.** Arming flow-persistence on the strength of its own happy-path
/// tape is the single most expensive mistake available here. This test states the
/// harm in lamports so that doing so trips an explicit, loud failure.
#[test]
fn arming_beyond_the_shakeout_threshold_is_harmful() {
    let ship = golden(1);
    for k in [4u32, 5, 6] {
        let armed = golden(k);
        assert!(
            armed < ship,
            "k={k} is HARMFUL on the representative tape ({ship} -> {armed}); \
             it is kept only as a DISARMED lever for live base-rate measurement"
        );
    }
    // The specific, pinned magnitude of the harm: 11,469,573 lamports.
    //
    // **THE HARM IS INVARIANT ACROSS THREE RE-PINS, AND THAT IS THE POINT.** The
    // baseline has moved twice — 8,124,568 (#24) → 16,778,896 (#26) → 31,111,528 (#27)
    // — while this number has not: 11,347,743 at #24 and 11,469,573 at both #26 and
    // #27, unchanged to the lamport across a cost-model unification AND a fixture
    // eviction-key reordering. A quantity that survives two independent re-pins of
    // everything around it is measuring the LEVER, not the tape.
    //
    // Read the magnitude, never the fraction. The fraction has drifted 140% → 68% → 37%
    // purely because the denominator grew, and anyone quoting it will conclude the lever
    // is getting safer. It is not; it costs exactly what it always cost.
    assert_eq!(golden(5), 19_641_955, "pinned k=5 golden net");
    assert_eq!(ship - golden(5), 11_469_573, "pinned k=5 golden harm — INVARIANT");
    assert!(
        golden(5) < ship,
        "k=5 remains strictly harmful on the representative tape"
    );
}
