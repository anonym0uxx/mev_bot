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
//! # THE FINDING — and why the default stays OFF
//!
//! Arming it does not produce a modest haircut. On the golden tape it takes net from
//! **+15,410,801 to −332,289,498**, and it flips both concentration tapes deeply
//! negative. That number is NOT a verdict on the strategy — **it is a verdict on the
//! TAPES**, and it is the most consequential thing this wiring found:
//!
//! | pool depth (`vsol`) | own-impact per leg on a 0.1 SOL clip |
//! |---|---|
//! | golden tape MIN, 0.12 SOL | **8,333 bps** |
//! | golden tape MAX, 0.47 SOL | **2,127 bps** |
//! | REAL pump.fun at launch, 30 SOL | **33 bps** |
//! | real, near graduation, 85 SOL | 11 bps |
//!
//! The golden tape's pools are **0.12–0.47 SOL** while the operator's minimum clip is
//! **0.1 SOL**. Our own order is therefore **21–83% of the entire pool** — and every
//! measurement ever taken on that tape was taken while charging ourselves *nothing*
//! for it. Real pump.fun virtual reserves start at ~30 SOL, so the tape understates
//! depth by roughly **250×** relative to our clip.
//!
//! What this does and does not invalidate, stated precisely:
//! * **Relative A/B verdicts survive.** Both arms of every prior comparison paid the
//!   same (zero) impact, so armed-vs-disarmed conclusions are not overturned.
//! * **The absolute net does not.** "+15,410,801 on a cost-realistic tape" is not a
//!   coherent economic statement when the clip is a fifth of the pool. The tape is
//!   cost-realistic in FEES and unrealistic in DEPTH.
//!
//! So the default stays OFF — arming it against unrealistic depth produces nonsense —
//! and the real fix is to give the tapes pump.fun-realistic reserves. Under REAL depth
//! the charge is ~33 bps/leg (~66 bps round trip) against a ~700 bps round trip:
//! material, and survivable.
//!
//! **For any real-data backtest it MUST be armed**, because there the depth is real.

mod tape_golden;

use pump_quant_app::config::Config;
use pump_quant_app::curve_fill;

/// The operator's minimum clip (`min_trade_size_lamports`).
const CLIP: u64 = 100_000_000;

#[test]
fn the_default_is_off_and_reproduces_the_pinned_baseline() {
    let cfg = Config::dev_portable();
    assert!(
        !cfg.curve_exact_fill_enable,
        "curve fill must ship DISARMED — armed against the current tapes' unrealistic \
         depth it produces nonsense, not a verdict"
    );
    assert_eq!(cfg.fill_landing_slots, 0, "landing delay ships at today's behaviour");
    assert_eq!(
        tape_golden::drive(cfg).net_lamports,
        15_410_801,
        "the disarmed default must reproduce the pinned golden net exactly"
    );
}

/// Arming it must move net STRICTLY DOWN — a fill model that could improve the book
/// would be modelling a subsidy, not a cost.
#[test]
fn arming_it_can_only_ever_cost_us() {
    let mut on = Config::dev_portable();
    on.curve_exact_fill_enable = true;
    let armed = tape_golden::drive(on).net_lamports;
    assert!(
        armed < 15_410_801,
        "paying our own curve impact must reduce net, never raise it (got {armed})"
    );
}

/// **THE TAPE DEFECT, pinned in arithmetic.** If someone later gives the tapes
/// realistic depth, this test fails and forces the doc above to be revisited — which
/// is exactly what should happen.
#[test]
fn the_golden_tape_puts_our_clip_at_an_absurd_share_of_the_pool() {
    // The golden tape's shallowest and deepest pools.
    const TAPE_MIN_DEPTH: u64 = 120_000_000; // 0.12 SOL
    const TAPE_MAX_DEPTH: u64 = 470_000_000; // 0.47 SOL
    // Real pump.fun virtual SOL reserves at launch.
    const REAL_LAUNCH_DEPTH: u64 = 30_000_000_000; // 30 SOL

    let worst = curve_fill::own_impact_bps(TAPE_MIN_DEPTH, CLIP).unwrap();
    let best = curve_fill::own_impact_bps(TAPE_MAX_DEPTH, CLIP).unwrap();
    let real = curve_fill::own_impact_bps(REAL_LAUNCH_DEPTH, CLIP).unwrap();

    assert_eq!(worst, 8_333, "0.1 SOL into a 0.12 SOL pool is 83% of it");
    assert_eq!(best, 2_127, "even the deepest tape pool is a 21% participation rate");
    assert_eq!(real, 33, "the same clip on a REAL launch curve is 33 bps");

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
