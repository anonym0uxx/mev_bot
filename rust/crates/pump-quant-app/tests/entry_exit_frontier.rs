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
//!      a defensible fractional-Kelly point: raising it "earns" on the easy tapes
//!      but drives the CONCENTRATION-HAZARD tape NEGATIVE and one notch further
//!      collapses the B7 tapes — the overbetting signature. The shipped 667 is the
//!      only value positive on every tape (`ENTRY_EXIT_SCRUTINY §3`).
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
    tape_b7::drive(c, tape_b7::Tape::happy(), None).report().net_lamports
}
fn b7_unhappy(mutate: impl FnOnce(&mut Config)) -> i128 {
    let mut c = Config::dev_portable();
    mutate(&mut c);
    tape_b7::drive(c, tape_b7::Tape::unhappy(), None).report().net_lamports
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

const SHIP_GOLDEN: i128 = 15_410_801;
const SHIP_B7_HAPPY: i128 = 479_556_343;
const SHIP_B7_UNHAPPY: i128 = 601_202_914;
const SHIP_CONC_HAPPY: i128 = 16_567_514;
const SHIP_CONC_MIRROR: i128 = 55_684_531;
const SHIP_B3_HAZARD: i128 = 293_235_710;

#[test]
fn shipped_net_is_pinned_on_every_tape() {
    assert_eq!(golden(|_| {}), SHIP_GOLDEN, "golden shipped net drifted");
    assert_eq!(b7_happy(|_| {}), SHIP_B7_HAPPY, "b7-happy shipped net drifted");
    assert_eq!(b7_unhappy(|_| {}), SHIP_B7_UNHAPPY, "b7-unhappy shipped net drifted");
    assert_eq!(conc_happy(|_| {}), SHIP_CONC_HAPPY, "conc-happy shipped net drifted");
    assert_eq!(conc_mirror(|_| {}), SHIP_CONC_MIRROR, "conc-mirror shipped net drifted");
    assert_eq!(b3_hazard(|_| {}), SHIP_B3_HAZARD, "b3-hazard shipped net drifted");
    // The shipped config is positive on EVERY tape — including both concentration
    // sides. That is the property the sizing frontier must preserve (see below).
    for (n, name) in [
        (golden(|_| {}), "golden"),
        (b7_happy(|_| {}), "b7-happy"),
        (b7_unhappy(|_| {}), "b7-unhappy"),
        (conc_happy(|_| {}), "conc-happy"),
        (conc_mirror(|_| {}), "conc-mirror"),
        (b3_hazard(|_| {}), "b3-hazard"),
    ] {
        assert!(n > 0, "shipped config must be net-positive on {name}, got {n}");
    }
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
    let g_gain = g_over - g_ship; // ~ +30.18M — positive, but below one bite
    assert!(
        g_gain > 0 && g_gain < MATERIAL_LAMPORTS,
        "golden overbet gain must be positive but sub-material ({g_ship} -> {g_over}, gain {g_gain})"
    );

    // (b) The SAME change flips the concentration-hazard tape from POSITIVE to
    //     NEGATIVE — a qualitative P2 hazard harm (sign flip, regardless of size).
    //     This is the load-bearing pin: it fires if anyone raises f_base_bp chasing
    //     the golden number.
    let c_ship = conc_happy(|_| {});
    let c_over = conc_happy(|c| c.f_base_bp = 1_000);
    assert!(
        c_ship > 0 && c_over < 0,
        "overbetting must flip conc-happy positive -> negative ({c_ship} -> {c_over})"
    );

    // (c) One notch further (1200) collapses the B7 tapes by FAR more than a bite —
    //     the ruin right-tail a deep-fractional Kelly floor exists to avoid.
    let b_ship = b7_unhappy(|_| {});
    let b_cliff = b7_unhappy(|c| c.f_base_bp = 1_200);
    assert!(
        b_cliff < b_ship - MATERIAL_LAMPORTS,
        "f_base=1200 must collapse b7-unhappy past one bite (overbet cliff): {b_ship} -> {b_cliff}"
    );
}
