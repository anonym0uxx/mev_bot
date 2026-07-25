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
//! # THE VERDICT: **DISARMED (`k = 1`). The mechanism is real; the edge is not.**
//!
//! On [`tape_flow`] — the two-sided tape built FOR this hypothesis — `k = 5` clears
//! both bars handsomely (gain +152,694,678, loss −44,095,911, ratio 3.46). If that
//! tape were the arbiter, this law would ship armed.
//!
//! (Scale caveat, stated plainly because it governs how every number here reads:
//! the golden tape's ENTIRE book is 15,410,801 lamports — smaller than one 0.1-SOL
//! bite. Materiality is therefore judged RELATIVELY on golden and ABSOLUTELY only
//! on the large hazard tapes, where a bite is a meaningful unit.)
//!
//! **It is not the arbiter, and the pre-existing tapes overturn it.** On the
//! REPRESENTATIVE golden tape (a realistic pump.fun outcome mix, authored long
//! before this hypothesis existed) the very same `k = 5` destroys **85% of net**
//! (15,410,801 → 2,322,301), and on the concentration-hazard tape it flips a
//! positive book NEGATIVE (16,567,514 → −22,894,988). The `k` that wins on the
//! purpose-built tape is catastrophic on the realistic one — the textbook signature
//! of a result fitted to its own fixture. **P1 and P2 both fail.**
//!
//! A 135-configuration JOINT lattice (`k` × trail × hard-stop × TP-margin) was then
//! swept on the golden tape, because relaxing `k` is what would UNBIND the price
//! geometry. Best of all 135: **+438,538 lamports — 1/228th of one bite.** There is
//! no joint configuration that earns either.
//!
//! # Why the capability is KEPT rather than deleted
//!
//! Because the mechanism is genuine and LARGE (it moves net by ±85% — it is the
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

const GOLDEN_SHIP: i128 = 15_410_801;
const CONC_H_SHIP: i128 = 16_567_514;
const FLOW_SHIP: i128 = 13_170_840;

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
    assert_eq!(golden(1), GOLDEN_SHIP, "k=1 must reproduce the golden net exactly");
    assert_eq!(conc_happy(1), CONC_H_SHIP, "k=1 must reproduce conc-happy exactly");
}

#[test]
fn the_mechanism_is_real_on_its_own_two_sided_tape() {
    // Below the shakeout run-length the position dies at the shakeout on BOTH
    // sides, so the two are indistinguishable — which is what makes the mirror fair.
    for k in [1u32, 2, 3] {
        assert_eq!(flow(k, tape_flow::Side::ShakeoutThenRun), FLOW_SHIP);
        assert_eq!(flow(k, tape_flow::Side::TrueTop), FLOW_SHIP);
    }
    // At k = 5 the position survives the shakeout: it rides the runner on the happy
    // side and pays the collapse on the mirror.
    let happy = flow(5, tape_flow::Side::ShakeoutThenRun);
    let mirror = flow(5, tape_flow::Side::TrueTop);
    let gain = happy - FLOW_SHIP;
    let loss = FLOW_SHIP - mirror;
    assert!(gain > 0 && loss > 0, "both sides must actually move (gain {gain}, loss {loss})");
    assert!(
        gain > MATERIAL_LAMPORTS,
        "on its OWN tape the gain is material: {gain}"
    );
    assert!(
        gain >= loss * REQUIRED_RATIO,
        "P3 asymmetry holds on its own tape: gain {gain} vs {REQUIRED_RATIO}x loss {loss}"
    );
}

#[test]
fn but_it_fails_on_every_pre_existing_tape_so_it_stays_disarmed() {
    // NOTE ON SCALE — this matters for reading every number here honestly. The
    // golden tape's ENTIRE book is 15,410,801 lamports, which is itself SMALLER
    // than one 0.1-SOL materiality bite (100,000,000). So an absolute bite bar is
    // meaningless on this tape and is NOT the test applied: materiality is judged
    // RELATIVELY on golden (fraction of the book moved) and ABSOLUTELY only on the
    // large hazard tapes (B3 ≈ 293M, B7 ≈ 479–601M), where a bite is a real unit.
    //
    // P1 FAILS: the best gain any k produces on the representative tape is
    // +257,400 lamports at k = 2 — under 1.7% of the book, and nowhere near a bite.
    let best_gain = [2u32, 3, 4, 5, 6, 8, 12]
        .iter()
        .map(|&k| golden(k) - GOLDEN_SHIP)
        .max()
        .unwrap();
    assert_eq!(best_gain, 257_400, "pinned best golden gain across all k (k = 2)");
    assert!(
        best_gain * 50 < GOLDEN_SHIP,
        "the best gain is under 2% of the book — immaterial by any reading: {best_gain}"
    );

    // P2 FAILS at the k its OWN tape favours: catastrophic on the realistic tape.
    // Judged relatively, since the whole book is under one bite.
    let g5 = golden(5);
    assert!(
        g5 * 5 < GOLDEN_SHIP,
        "k=5 must destroy the large majority of representative net: {GOLDEN_SHIP} -> {g5}"
    );
    // ...and on the concentration hazard book it flips POSITIVE -> NEGATIVE, which
    // is a qualitative harm that needs no scale bar at all.
    let c5 = conc_happy(5);
    assert!(
        CONC_H_SHIP > 0 && c5 < 0,
        "k=5 must flip conc-happy positive -> negative: {CONC_H_SHIP} -> {c5}"
    );
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
    // The specific, pinned magnitude of the harm: ~85% of net destroyed.
    assert_eq!(golden(5), 2_322_301, "pinned k=5 golden harm");
    assert!(
        golden(5) * 5 < ship,
        "k=5 destroys the large majority of representative net"
    );
}
