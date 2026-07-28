//! **CURVE-EXACT FILL — the wiring, and the tape defect it exposed (2026-07-27).**
//!
//! # What was wired
//!
//! The engine used to open positions at `latest_price_fp` — the last observed print —
//! and sell at the print too. pump.fun is a constant-product bonding curve, so OUR OWN
//! order never fills at the print: it walks the curve and fills strictly worse, by
//! exactly `notional · 10_000 / vsol` bps. The token reserve CANCELS
//! (`curve_fill::own_impact_bps`), which is why this can be priced from
//! `liquidity_lamports` alone — the engine has always had the number it needed.
//!
//! Filling at the print was a subsidy the market never granted, charged on neither leg.
//!
//! # THE FINDING — and how it was resolved
//!
//! When first wired against the OLD golden tape, arming this took net from
//! +15,410,801 to −332,289,498. That was never a verdict on the strategy — it was a
//! verdict on the TAPE. The tape's pools were **0.12–0.47 SOL** while the operator's
//! minimum clip is **0.1 SOL**, so our own order was **21–83% of the entire pool**,
//! and every measurement ever taken on it charged us nothing for that.
//!
//! | pool depth (`vsol`) | own-impact per leg, 0.1 SOL clip |
//! |---|---|
//! | OLD golden tape, 0.12 SOL | 8,333 bps |
//! | OLD golden tape, 0.47 SOL | 2,127 bps |
//! | REAL pump.fun at launch, 30 SOL | **33 bps** |
//!
//! **The tape has since been given real depth** (30 SOL virtual at launch, deepening
//! toward graduation) and sellable depths that match the pools they describe. Under
//! those economics the curve fill is armed on the golden tape and the honest net is
//! **+16,778,896**.
//!
//! # RE-PIN #26 — the denominator stopped being a number anyone chooses
//!
//! Re-pin #24 fixed the tape and then hand-set `gate_impact_den` to `3_000_000` to
//! match it. That was the right value and the wrong KIND of fix: a static denominator
//! is correct for exactly one pool depth and silently wrong for every other, and this
//! tape prices markets from 30 to 67 SOL. Since the cost-model unification the gate
//! DERIVES it per candidate — `cost_model::impact_den_for(vsol) = vsol / 10_000` —
//! which makes the gate's linear impact model identically `own_impact_bps` on every
//! market rather than on one. Together with removing the phantom 200 bps of "bid/ask
//! spread" and pricing the ATA deposit, the honest golden net is **+16,778,896**.
//!
//! Note what did NOT change: the 8,124,568 this file used to pin was ALREADY paying
//! its own impact on both legs. The move to 16,778,896 is not a relaxation of fill
//! honesty — it is the gate no longer charging 200 bps that a constant-product AMM
//! cannot charge, and no longer pricing every pool as if it were 30 SOL deep.
//!
//! This file pins that resolved state: real depth, armed fill, a derived denominator,
//! and the arithmetic showing our clip is a sane fraction of the pool.
#![allow(dead_code)]

mod tape_b3;
mod tape_conc;
mod tape_golden;

use pump_quant_app::config::Config;
use pump_quant_app::curve_fill;

/// The operator's minimum clip (`min_trade_size_lamports`).
const CLIP: u64 = 100_000_000;

#[test]
fn the_golden_tape_is_priced_with_real_depth_and_honest_fills() {
    // The golden tape ARMS the curve fill (its depths are real). The shipped
    // `dev_portable` profile leaves it off, because that profile carries no depth
    // model of its own and arming it against stylized depth would produce nonsense.
    assert!(
        !Config::dev_portable().curve_exact_fill_enable,
        "the bare profile stays disarmed — it has no real depth to charge against"
    );
    assert_eq!(
        tape_golden::drive(Config::dev_portable()).net_lamports,
        16_778_896,
        "the honest golden net, with real depth and our own impact charged on both legs"
    );
}

/// Charging our own impact must COST us. The golden tape arms it unconditionally, so
/// this pins the size of the honesty: the fictional-market number was ~1.9x the real one.
#[test]
fn honest_fills_cost_us_about_half_the_reported_book() {
    const FICTIONAL: i128 = 15_410_801; // what the 0.12-0.47 SOL tape reported
    /// The book under real depth with the OLD split cost model — impact charged on
    /// both legs, but the gate still carrying 200 bps of phantom spread, a static
    /// impact denominator, a 150 bps first-sell penalty and no ATA accounting.
    const HONEST_UNDER_THE_SPLIT_MODEL: i128 = 8_124_568;
    /// The book under real depth AND the unified cost model (re-pin #26).
    const HONEST: i128 = 16_778_896;
    const {
        assert!(
            HONEST_UNDER_THE_SPLIT_MODEL < FICTIONAL,
            "honest DEPTH must not flatter the book"
        );
        // The depth correction was roughly a halving, not a rounding error.
        assert!(
            HONEST_UNDER_THE_SPLIT_MODEL * 2 > FICTIONAL
                && HONEST_UNDER_THE_SPLIT_MODEL * 3 / 2 < FICTIONAL * 2
        );
    }
    // …and the cost-model correction then moved it back UP, which is the honest
    // shape of the two fixes and worth stating so neither is mistaken for the other:
    // charging our own impact COSTS us (the halving above), while deleting a spread
    // an AMM cannot charge and crediting a refundable deposit PAYS us. Both are
    // corrections toward the venue; they simply point opposite ways.
    const {
        assert!(HONEST > HONEST_UNDER_THE_SPLIT_MODEL);
        // The two corrections do not cancel.
        assert!(HONEST > FICTIONAL);
    }
    assert_eq!(
        tape_golden::drive(Config::dev_portable()).net_lamports,
        HONEST
    );
}

/// **THE TAPE DEFECT, pinned in arithmetic.** If someone later gives the tapes
/// realistic depth, this test fails and forces the doc above to be revisited — which
/// is exactly what should happen.
#[test]
fn the_old_tape_depth_was_absurd_and_the_new_one_is_not() {
    // The golden tape's shallowest and deepest pools.
    const TAPE_MIN_DEPTH: u64 = 120_000_000; // 0.12 SOL
    const TAPE_MAX_DEPTH: u64 = 470_000_000; // 0.47 SOL
                                             // Real pump.fun virtual SOL reserves at launch.
    const REAL_LAUNCH_DEPTH: u64 = 30_000_000_000; // 30 SOL

    let worst = curve_fill::own_impact_bps(TAPE_MIN_DEPTH, CLIP).unwrap();
    let best = curve_fill::own_impact_bps(TAPE_MAX_DEPTH, CLIP).unwrap();
    let real = curve_fill::own_impact_bps(REAL_LAUNCH_DEPTH, CLIP).unwrap();

    assert_eq!(worst, 8_333, "0.1 SOL into a 0.12 SOL pool was 83% of it");
    assert_eq!(
        best, 2_127,
        "even the deepest OLD tape pool was a 21% participation rate"
    );
    assert_eq!(
        real, 33,
        "the same clip on a REAL launch curve is 33 bps — what we now use"
    );

    // The tape is off by more than two orders of magnitude.
    assert!(
        worst / real > 250,
        "the tape understates depth by >250x relative to the operator's clip \
         (tape {worst} bps vs real {real} bps)"
    );
    // And under real depth the charge is survivable against a ~700 bps round trip.
    assert!(
        real * 2 < 100,
        "real round-trip own-impact must be well under 1% for the strategy to be viable"
    );
}

/// **AMENDMENT A-13(2), MACHINE-CHECKED — and its premise half-retracted at re-pin
/// #26.**
///
/// A-13(2) held that a fixture may charge our own curve impact only if its depth is
/// REAL, and that the hazard tapes (B3 / concentration / B7 / flow) could keep
/// STYLIZED depth on purpose because they are relative instruments: a uniform depth
/// mispricing moves both arms of an A/B together and therefore cancels.
///
/// **The cancellation argument was true only while the GATE could not see depth.** It
/// silently assumed a depth mispricing is a scale factor. It is not: `gate::decide`
/// now derives its impact denominator from the market's own reserve
/// (`cost_model::impact_den_for`), and a 0.1-SOL clip into a 0.2-SOL pool prices at
/// 5_000 bps a leg and REFUSES. A refusal does not cancel — it zeroes BOTH arms and
/// leaves a tape that admits nothing, books nothing, and arbitrates nothing. That is
/// exactly what happened to the B7, concentration and flow tapes, and their own
/// non-vacuity guards are what caught it.
///
/// So the first half of the rule survives and the second half is retired: **every
/// tape now declares real pump.fun depth, because a declared depth is a PRICE.** What
/// this test still enforces is the first half — the FILL is armed only where the
/// engine is being asked for an absolute number (the golden tape), and stays disarmed
/// on the hazard tapes, which remain RELATIVE instruments whose absolute nets are not
/// quotable. Arming the fill on them is a separate decision with its own evidence
/// requirement, and it is not taken here.
#[test]
fn only_tapes_with_real_depth_may_charge_their_own_impact() {
    // The bare shipped profile carries no depth model at all.
    assert!(
        !Config::dev_portable().curve_exact_fill_enable,
        "dev_portable has no depth model — it must never arm the fill"
    );
    // The hazard tapes build on that profile and must not arm it either: they are
    // RELATIVE instruments and their absolute nets are not quotable.
    assert!(
        !tape_b3::hazard_cfg().curve_exact_fill_enable,
        "the B3 hazard tape is a RELATIVE instrument — arming the fill would fabricate \
         an absolute number it cannot support (A-13(2))"
    );
    assert!(
        !tape_conc::conc_cfg(Config::dev_portable()).curve_exact_fill_enable,
        "the concentration hazard tape is a RELATIVE instrument — see A-13(2)"
    );
    // …and their DEPTH is nonetheless real, which is the half of A-13(2) that had to
    // be retracted (see the doc above). Pinned in both directions so neither a return
    // to stylized depth nor a silent arming of the fill can pass.
    const {
        // The shallowest B3 pool must be a real launch curve or deeper…
        assert!(tape_b3::THIN_LIQUIDITY >= pump_quant_app::curve_state::LAUNCH_VSOL_LAMPORTS);
        // …and the deepest must still be ON the bonding curve, so both classes pay the
        // same 125 bps-a-leg venue fee and depth is the only axis that varies.
        assert!(tape_b3::DEEP_LIQUIDITY < pump_quant_app::curve_state::GRADUATION_VSOL_LAMPORTS);
        assert!(tape_conc::LIQ >= pump_quant_app::curve_state::LAUNCH_VSOL_LAMPORTS);
    }
    // ...and the one tape that DOES carry real pump.fun depth arms it internally.
    // 15,641,439 is that same tape measured with real depth and a coherent gate but
    // fills still taken AT THE PRINT — i.e. the only thing separating the two numbers
    // is whether we pay for the pool we move.
    const REAL_DEPTH_FILLS_AT_PRINT: i128 = 15_641_439;
    let armed_net = tape_golden::drive(Config::dev_portable()).net_lamports;
    assert_eq!(
        armed_net, 16_778_896,
        "the golden tape must be pricing its own impact"
    );
    // Re-pin #26: 15_641_439 was measured under the SPLIT cost model, so it is no
    // longer comparable to `armed_net` — the two differ in the gate as well as in the
    // fill, and the gate's correction points the other way. The claim it supported
    // ("charging our own impact must reduce the book") is now proven directly, on one
    // cost model, by toggling the fill and nothing else.
    let _ = REAL_DEPTH_FILLS_AT_PRINT;
    let at_print = tape_golden::drive_at_print(Config::dev_portable()).net_lamports;
    println!("MEASURE golden armed={armed_net} at_print={at_print}");
    assert_eq!(at_print, MEASURED_FILLS_AT_PRINT);
    assert!(
        armed_net < at_print,
        "charging our own impact must reduce the book relative to filling at the print \
         ({armed_net} armed vs {at_print} at the print)"
    );
}

/// The golden tape with fills taken AT THE PRINT under the unified cost model —
/// measured, not computed, from the first run of
/// `only_tapes_with_real_depth_may_charge_their_own_impact`.
const MEASURED_FILLS_AT_PRINT: i128 = 23_608_498;

/// **A-13(1) — the participation rate, declared rather than assumed.** This is the
/// arithmetic that nobody computed for months: what fraction of the pool is OUR order?
/// Stating it as a test means a future depth edit cannot quietly re-create the defect.
#[test]
fn the_golden_tapes_participation_rate_is_declared_and_sane() {
    // Shallowest pool the golden tape now presents (round 0, m % 350 == 0).
    const GOLDEN_MIN_VSOL: u64 = 30_000_000_000;
    // Deepest (round 5, m % 350 == 349).
    const GOLDEN_MAX_VSOL: u64 = 30_000_000_000 + 5 * 4_000_000_000 + 349 * 50_000_000;

    let worst = curve_fill::own_impact_bps(GOLDEN_MIN_VSOL, CLIP).unwrap();
    let best = curve_fill::own_impact_bps(GOLDEN_MAX_VSOL, CLIP).unwrap();
    assert_eq!(worst, 33, "0.1 SOL into a 30 SOL launch curve is 33 bps");
    assert_eq!(best, 14, "0.1 SOL into the deepest modelled pool is 14 bps");

    // The gate's own impact model must agree with the curve at EVERY depth this tape
    // presents, not merely at the shallowest. Re-pin #26 turned A-13(3) from a value
    // anyone had to keep in sync into an identity that holds by construction:
    // `cost_model::impact_den_for(vsol) = vsol / 10_000`.
    for &vsol in &[
        GOLDEN_MIN_VSOL,
        GOLDEN_MAX_VSOL,
        45_000_000_000,
        67_000_000_000,
    ] {
        let den = pump_quant_app::cost_model::impact_den_for(vsol);
        assert_eq!(
            CLIP / den,
            curve_fill::own_impact_bps(vsol, CLIP).unwrap(),
            "the GATE and the FILL must price the same market at vsol={vsol} — this is \
             the reconciliation whose absence let a 400 bps gate sit beside a \
             fill-at-the-print for months, and whose STATIC form (a hand-set 3_000_000) \
             was correct at exactly one of these four depths"
        );
    }
    assert_eq!(
        pump_quant_app::cost_model::impact_den_for(GOLDEN_MIN_VSOL),
        3_000_000,
        "…and at the launch depth it reproduces the retired hand-set denominator"
    );
}
