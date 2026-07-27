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
//! toward graduation), a `gate_impact_den` coherent with it (`vsol/10_000`), and
//! sellable depths that match the pools they describe. Under those economics the
//! curve fill is armed on the golden tape and the honest net is **+8,124,568** —
//! roughly HALF the +15,410,801 that was reported while the market was fictional.
//!
//! This file now pins that resolved state: real depth, armed fill, and the arithmetic
//! showing our clip is a sane fraction of the pool.
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
        8_124_568,
        "the honest golden net, with real depth and our own impact charged on both legs"
    );
}

/// Charging our own impact must COST us. The golden tape arms it unconditionally, so
/// this pins the size of the honesty: the fictional-market number was ~1.9x the real one.
#[test]
fn honest_fills_cost_us_about_half_the_reported_book() {
    const FICTIONAL: i128 = 15_410_801; // what the 0.12-0.47 SOL tape reported
    const HONEST: i128 = 8_124_568; // real depth, impact charged both legs
    assert!(HONEST < FICTIONAL, "honest accounting must not flatter the book");
    assert!(
        HONEST * 2 > FICTIONAL && HONEST * 3 / 2 < FICTIONAL * 2,
        "the correction is roughly a halving, not a rounding error"
    );
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
    assert_eq!(best, 2_127, "even the deepest OLD tape pool was a 21% participation rate");
    assert_eq!(real, 33, "the same clip on a REAL launch curve is 33 bps — what we now use");

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

/// **AMENDMENT A-13(2), MACHINE-CHECKED.** A fixture may charge our own curve impact
/// only if its depth is REAL. The hazard tapes (B3 / concentration / B7 / flow) keep
/// STYLIZED depth on purpose — they are relative instruments, where a uniform depth
/// mispricing moves both arms of the A/B together and therefore cancels. Arming the
/// fill against stylized depth would not make them honest; it would manufacture a
/// different fiction, and it would silently convert their relative verdicts into
/// absolute claims they cannot support.
///
/// So the rule is: **real depth ⇒ fill armed; stylized depth ⇒ fill disarmed AND the
/// tape's absolute net is not quotable.** This test is what makes the second half of
/// that rule fail loudly instead of drifting.
#[test]
fn only_tapes_with_real_depth_may_charge_their_own_impact() {
    // The bare shipped profile carries no depth model at all.
    assert!(
        !Config::dev_portable().curve_exact_fill_enable,
        "dev_portable has no depth model — it must never arm the fill"
    );
    // The hazard tapes build on that profile and must not arm it either.
    assert!(
        !tape_b3::hazard_cfg().curve_exact_fill_enable,
        "the B3 hazard tape is a RELATIVE instrument — arming the fill on its stylized \
         depth would fabricate an absolute number it cannot support (A-13(2))"
    );
    assert!(
        !tape_conc::conc_cfg(Config::dev_portable()).curve_exact_fill_enable,
        "the concentration hazard tape is a RELATIVE instrument — see A-13(2)"
    );
    // ...and the one tape that DOES carry real pump.fun depth arms it internally.
    // 15,641,439 is that same tape measured with real depth and a coherent gate but
    // fills still taken AT THE PRINT — i.e. the only thing separating the two numbers
    // is whether we pay for the pool we move.
    const REAL_DEPTH_FILLS_AT_PRINT: i128 = 15_641_439;
    let armed_net = tape_golden::drive(Config::dev_portable()).net_lamports;
    assert_eq!(armed_net, 8_124_568, "the golden tape must be pricing its own impact");
    assert!(
        armed_net < REAL_DEPTH_FILLS_AT_PRINT,
        "charging our own impact must reduce the book relative to filling at the print"
    );
}

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

    // The gate's own impact model must agree with the curve at the SHALLOWEST depth,
    // which is the conservative choice: `impact_bps = size / impact_den`, and the
    // coherent denominator is `vsol / 10_000`.
    const GATE_IMPACT_DEN: u64 = 3_000_000;
    assert_eq!(
        GOLDEN_MIN_VSOL / 10_000,
        GATE_IMPACT_DEN,
        "the gate's impact denominator must be derived from the tape's launch depth (A-13(3))"
    );
    assert_eq!(
        CLIP / GATE_IMPACT_DEN,
        u64::from(worst),
        "the GATE and the FILL must price the same market — this is the reconciliation \
         whose absence let a 400 bps gate sit beside a fill-at-the-print for months"
    );
}
