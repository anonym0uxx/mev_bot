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

/// The golden tape with fills taken AT THE PRINT under the unified cost model —
/// measured, not computed, from the first run of
/// `only_tapes_with_real_depth_may_charge_their_own_impact`.
/// Re-pin #29: 37_070_067 → 37_171_418 — cost-aware TP ladder (fixed fractions,
/// research-synthesized values from arXiv:2606.08232 fat-tail capture design).
const MEASURED_FILLS_AT_PRINT: i128 = 47_760_357;

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
