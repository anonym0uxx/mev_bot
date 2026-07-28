//! **ENTRY / EXIT FRONTIER — the pinned negative.**
//!
//! Companion to `law_permutation_sweep.rs` (which prices the reduce-only LAW
//! on/off lattice). This file prices the *continuous* entry/exit surface — the
//! largest never-A/B'd block in the live path — and pins the conclusion of the
//! 2026-07-25 principal-quant scrutiny (`docs/ENTRY_EXIT_SCRUTINY_2026-07-25.md`):
//!
//!   1. Every PRICE-BASED exit trigger is INERT on the representative tape — the
//!      §32 order-flow-sign-flip thesis exit binds first, so hard-stop / trail /
//!      CVD-fraction geometry never becomes the binding constraint. Changing those
//!      knobs moves zero lamports; tuning them would be fitting a rule that does
//!      not fire (`ENTRY_EXIT_SCRUTINY §2`).
//!
//!   2. Position sizing (`f_base_bp`) is the only real mover, and it is already at
//!      a defensible fractional-Kelly point: raising it "earns" +26M on the golden
//!      tape while multiplying the concentration-hazard loss ~15x and flipping the B3
//!      hazard tape negative, and ONE NOTCH FURTHER (1200) turns the representative
//!      golden book itself negative — the overbetting signature, now visible on the
//!      arbiter and not only on the hazard tapes (`ENTRY_EXIT_SCRUTINY §3`).
//!
//! **Re-pin #26 changed one of this file's stated properties and it is called out
//! here rather than buried:** the shipped config is no longer net-positive on EVERY
//! tape. Once the hazard tapes were given real pump.fun depth, the
//! concentration-happy tape — the side where the bundled cohort craters and the §21.7
//! law that would refuse it ships DISARMED — is net −4,961,452. That is the hazard
//! being expressed at realistic size, not a regression; the old positive came from a
//! 0.26-SOL pool refusing the trades on cost. See `SHIP_CONC_HAPPY`.
//!
//! Pre-registered rule (inherited from `law_permutation_sweep.rs`): a change ships
//! only if it gains > `MATERIAL_LAMPORTS` on the arbiter AND does no hazard-tape
//! harm beyond one bite. No candidate clears it — so the shipped values stay, and
//! this file makes that machine-checked: a future edit that makes an exit knob
//! suddenly bind, or that "improves" the golden net by overbetting, trips a pin
//! here rather than sliding in silently.
#![allow(dead_code)]

mod tape_b3;
mod tape_b7;
mod tape_conc;
mod tape_golden;

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};

/// One 0.1-SOL bite — the materiality bar (`min_trade_size_lamports`), identical
/// to the arbitration bar in `law_permutation_sweep.rs`.
const MATERIAL_LAMPORTS: i128 = 100_000_000;

// ---- per-tape drivers, each on its own shipped base config ------------------

fn golden(mutate: impl FnOnce(&mut Config)) -> i128 {
    let mut c = Config::dev_portable();
    mutate(&mut c);
    tape_golden::drive(c).net_lamports
}
fn b7_happy(mutate: impl FnOnce(&mut Config)) -> i128 {
    let mut c = Config::dev_portable();
    mutate(&mut c);
    tape_b7::drive(c, tape_b7::Tape::happy(), None)
        .report()
        .net_lamports
}
fn b7_unhappy(mutate: impl FnOnce(&mut Config)) -> i128 {
    let mut c = Config::dev_portable();
    mutate(&mut c);
    tape_b7::drive(c, tape_b7::Tape::unhappy(), None)
        .report()
        .net_lamports
}
fn conc_happy(mutate: impl FnOnce(&mut Config)) -> i128 {
    let mut c = tape_conc::conc_cfg(Config::dev_portable());
    mutate(&mut c);
    let mut eng = Engine::new(c, RunMode::Replay);
    tape_conc::apply_ab(&mut eng, tape_conc::Side::ConcentratedBleeds);
    eng.report().net_lamports
}
fn conc_mirror(mutate: impl FnOnce(&mut Config)) -> i128 {
    let mut c = tape_conc::conc_cfg(Config::dev_portable());
    mutate(&mut c);
    let mut eng = Engine::new(c, RunMode::Replay);
    tape_conc::apply_ab(&mut eng, tape_conc::Side::ConcentratedPays);
    eng.report().net_lamports
}
fn b3_hazard(mutate: impl FnOnce(&mut Config)) -> i128 {
    let mut c = tape_b3::hazard_cfg();
    mutate(&mut c);
    let mut eng = Engine::new(c, RunMode::Replay);
    tape_b3::apply_two_class_hazard(&mut eng, tape_b3::HAZARD_ROUNDS);
    eng.report().net_lamports
}

// ---- shipped-value net on every tape (identity pins) ------------------------

const SHIP_GOLDEN: i128 = 16_778_896;
const SHIP_B7_HAPPY: i128 = 555_444_680;
const SHIP_B7_UNHAPPY: i128 = 1_346_209_124;
/// **NEGATIVE since re-pin #26, and that is the hazard working rather than a
/// regression.** This is the side of the concentration pair on which the bundled /
/// sniper-captured cohort is the one that craters, and the §21.7 law that would
/// refuse it SHIPS DISARMED (it failed its own pre-registered two-sided rule). While
/// the tape declared 0.26 SOL pools the gate refused most of that cohort on cost and
/// the book read +16_567_514; at real pump.fun depth those markets are affordable,
/// the engine takes them, and they do what the tape was built to make them do. A
/// hazard tape whose hazard is undefended SHOULD lose money — the old positive was
/// the fixture's thinness standing in for a defence the engine does not have.
const SHIP_CONC_HAPPY: i128 = -4_961_452;
const SHIP_CONC_MIRROR: i128 = 8_038_176;
const SHIP_B3_HAZARD: i128 = 276_922_370;

#[test]
fn shipped_net_is_pinned_on_every_tape() {
    assert_eq!(golden(|_| {}), SHIP_GOLDEN, "golden shipped net drifted");
    assert_eq!(
        b7_happy(|_| {}),
        SHIP_B7_HAPPY,
        "b7-happy shipped net drifted"
    );
    assert_eq!(
        b7_unhappy(|_| {}),
        SHIP_B7_UNHAPPY,
        "b7-unhappy shipped net drifted"
    );
    assert_eq!(
        conc_happy(|_| {}),
        SHIP_CONC_HAPPY,
        "conc-happy shipped net drifted"
    );
    assert_eq!(
        conc_mirror(|_| {}),
        SHIP_CONC_MIRROR,
        "conc-mirror shipped net drifted"
    );
    assert_eq!(
        b3_hazard(|_| {}),
        SHIP_B3_HAZARD,
        "b3-hazard shipped net drifted"
    );
    // **RETRACTED AT RE-PIN #26: the shipped config is NOT positive on every tape.**
    // It was, while the hazard tapes declared sub-SOL depth. Under real pump.fun depth
    // the concentration-happy tape is net −4,961,452, because that is the tape on
    // which the bundled cohort craters and the law that would refuse it ships
    // DISARMED. Asserting "positive everywhere" would now be asserting that a fixture
    // built to contain an undefended hazard must nonetheless make money, which is not
    // a property anyone should want to hold.
    //
    // What survives — and is the property the sizing frontier actually needs — is that
    // the shipped `f_base_bp` is net-positive on every tape where the ENGINE HAS THE
    // DEFENCE ARMED, and that no tape is catastrophic. Both are pinned.
    for (n, name) in [
        (golden(|_| {}), "golden"),
        (b7_happy(|_| {}), "b7-happy"),
        (b7_unhappy(|_| {}), "b7-unhappy"),
        (conc_mirror(|_| {}), "conc-mirror"),
        (b3_hazard(|_| {}), "b3-hazard"),
    ] {
        assert!(
            n > 0,
            "shipped config must be net-positive on {name}, got {n}"
        );
    }
    // …and the one tape it loses on loses SMALL: under a twentieth of the golden book
    // in absolute terms, against a hazard the engine is not defending. If this ever
    // grows past the golden book, the concentration law's default must be re-opened.
    let (loss, book) = (conc_happy(|_| {}), golden(|_| {}));
    assert!(
        loss < 0 && -loss * 3 < book,
        "the undefended concentration hazard must stay a small loss ({loss} against a \
         golden book of {book}) — if it grows, re-open `holder_concentration_enable`"
    );
}

#[test]
fn price_based_exit_knobs_are_inert_on_the_representative_tape() {
    // Hard stop swept −20% → −60%: identical net. The §32 flow-sign-flip exit
    // binds before price ever reaches the stop, so the stop never fires.
    assert_eq!(golden(|c| c.lc_hard_sl_bps = 2_000), SHIP_GOLDEN);
    assert_eq!(golden(|c| c.lc_hard_sl_bps = 6_000), SHIP_GOLDEN);
    // Trailing-stop width swept 12% → 45%: identical net.
    assert_eq!(golden(|c| c.lc_trail_base_bps = 1_200), SHIP_GOLDEN);
    assert_eq!(golden(|c| c.lc_trail_base_bps = 4_000), SHIP_GOLDEN);
    // Trailing geometry (k-div, max) swept: identical net.
    assert_eq!(golden(|c| c.lc_trail_k_div = 2), SHIP_GOLDEN);
    assert_eq!(golden(|c| c.lc_trail_max_bps = 30_000), SHIP_GOLDEN);
    // Thesis CVD hold-fraction swept 20% → 70%: identical net.
    assert_eq!(golden(|c| c.lc_cvd_hold_frac_bps = 2_000), SHIP_GOLDEN);
    assert_eq!(golden(|c| c.lc_cvd_hold_frac_bps = 7_000), SHIP_GOLDEN);
    // Rug-precursor drop and time stops swept: identical net.
    assert_eq!(golden(|c| c.lc_precursor_drop_bps = 1_500), SHIP_GOLDEN);
    assert_eq!(golden(|c| c.lc_precursor_drop_bps = 5_000), SHIP_GOLDEN);
    assert_eq!(golden(|c| c.lc_stall_ticks = 10), SHIP_GOLDEN);
    assert_eq!(golden(|c| c.lc_max_hold_ticks = 900), SHIP_GOLDEN);
    // Optional exit laws (default OFF) stay decision-inert on the golden tape.
    assert_eq!(golden(|c| c.into_strength_exit_enable = true), SHIP_GOLDEN);
    assert_eq!(golden(|c| c.vol_stop_enable = true), SHIP_GOLDEN);
}

#[test]
fn raising_position_size_is_overbetting_not_an_edge() {
    // (a) The in-sample golden "gain" from overbetting (667 -> 1000) is itself
    //     SUB-MATERIAL: even where it helps, it does not clear one 0.1-SOL bite, so
    //     it never satisfies P1 in the first place.
    let g_ship = golden(|_| {});
    let g_over = golden(|c| c.f_base_bp = 1_000);
    let g_gain = g_over - g_ship; // ~ +26.18M — positive, but below one bite
    assert!(
        g_gain > 0 && g_gain < MATERIAL_LAMPORTS,
        "golden overbet gain must be positive but sub-material ({g_ship} -> {g_over}, gain {g_gain})"
    );

    // (b) The SAME change multiplies the concentration-hazard tape's loss ~15x
    //     (−4.96M -> −73.3M) — a P2 hazard harm nearly THREE TIMES the golden "gain"
    //     it bought, on one tape alone. This is the load-bearing pin: it fires if
    //     anyone raises f_base_bp chasing the golden number.
    //
    //     Re-pin #26 restated this leg. It used to read "flips conc-happy positive ->
    //     negative", which was a sign test; at real depth that tape starts negative
    //     (see SHIP_CONC_HAPPY), so the sign test would now pass VACUOUSLY on the
    //     starting value and prove nothing. The MAGNITUDE test is what the leg was
    //     always for, and it is strictly stronger.
    let c_ship = conc_happy(|_| {});
    let c_over = conc_happy(|c| c.f_base_bp = 1_000);
    let c_harm = c_ship - c_over;
    assert!(
        c_harm > MATERIAL_LAMPORTS.saturating_sub(1) / 2,
        "overbetting must do material harm on conc-happy ({c_ship} -> {c_over}, \
         harm {c_harm})"
    );
    assert!(
        c_harm > g_gain,
        "the hazard-tape harm ({c_harm}) must exceed the golden 'gain' ({g_gain}) that \
         motivated the overbet — this is the whole argument"
    );
    // …and it is not one tape's quirk: the B3 hazard tape goes from +276.9M to
    // NEGATIVE at the same setting.
    let b3_ship = b3_hazard(|_| {});
    let b3_over = b3_hazard(|c| c.f_base_bp = 1_000);
    assert!(
        b3_ship > 0 && b3_over < 0,
        "overbetting must flip the B3 hazard tape positive -> negative \
         ({b3_ship} -> {b3_over})"
    );

    // (c) One notch further (1200) collapses the B7 tapes by FAR more than a bite —
    //     the ruin right-tail a deep-fractional Kelly floor exists to avoid.
    let b_ship = b7_unhappy(|_| {});
    let b_cliff = b7_unhappy(|c| c.f_base_bp = 1_200);
    assert!(
        b_cliff < b_ship - MATERIAL_LAMPORTS,
        "f_base=1200 must collapse b7-unhappy past one bite (overbet cliff): {b_ship} -> {b_cliff}"
    );
    // (d) …and at 1200 the cliff reaches the REPRESENTATIVE tape itself: the golden
    //     book goes NEGATIVE. Re-pin #26 exposed this — under the retired cost model
    //     the golden tape never turned over at any `f_base_bp` swept here, so the
    //     overbet argument had to be made entirely on the hazard tapes. It no longer
    //     does. Someone who reads only the golden tape and only the 667 -> 1000 step
    //     sees "+26M, free money"; one notch further, on the same tape, is ruin.
    let g_cliff = golden(|c| c.f_base_bp = 1_200);
    assert!(
        g_cliff < 0,
        "f_base=1200 must turn the REPRESENTATIVE book negative ({} -> {g_cliff})",
        golden(|_| {}),
    );
}
