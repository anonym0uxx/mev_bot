//! **THE SCALE-IN COST-BASIS LAW** — an honest A/B on phantom PnL (audit 2026-07-25).
//!
//! # The defect this pins closed
//!
//! `ScalpLifecycle::scale_in` used to add `add_lamports` to `size_lamports` while
//! leaving `entry_price_fp` at the ORIGINAL entry. Every later `realize()` computes
//! `gross = size × mult_bps / 10_000` against that entry, so lamports bought at the
//! current mark were booked as though bought at entry. The overstatement is exactly
//!
//! ```text
//!     phantom = add_lamports × (mark_at_add / original_entry − 1)
//! ```
//!
//! and because the scale-in trigger conditions on a RISING tape (evidenced
//! authenticity + structure not Downtrend), the error was systematically in our
//! favour. That is the worst possible direction: it flatters every backtest and
//! cannot be collected in cash.
//!
//! # The A/B, and why it is honest
//!
//! This is NOT a search for an edge — it is a correctness proof, so the test is an
//! arithmetic IDENTITY rather than a tape comparison. The decisive scenario is the
//! one where the truth is known exactly and independently of any model:
//!
//! **Scale in at mark M, then close at exactly M.** The added tranche was bought at
//! M and sold at M, so it is FLAT by construction — it can contribute nothing but
//! its own costs. Any booked gain on it is phantom, and its size is computable in
//! closed form. A test that asserts this cannot be satisfied by a lucky tape.
//!
//! The old rule fails this identity by construction; the blended basis satisfies it.
//! Both the sign and the magnitude are pinned below.

use pump_quant_app::position::{LifecycleParams, ScalpLifecycle};

const PRICE_SCALE: u64 = 10_000_000;

fn px(mult_bps: u64) -> u64 {
    PRICE_SCALE * mult_bps / 10_000
}

const MINT: [u8; 32] = [7u8; 32];

/// Costless params: every lamport of difference is basis arithmetic, nothing else.
fn clean_params() -> LifecycleParams {
    let mut p = LifecycleParams::standard();
    p.fee_bps = 0;
    p.fixed_lamports_per_leg = 0;
    p.exit_impair_bps = 0;
    // Push every price trigger out of the way so the close under test is the only
    // exit — this measures the BASIS, not the ladder.
    p.hard_sl_bps = 9_500;
    p.trail_base_bps = 9_500;
    p.trail_max_bps = 9_500;
    p.tp1_bps = 1_000_000;
    p.tp2_bps = 1_000_000;
    p.tp3_bps = 1_000_000;
    p
}

/// Probe 0.10 SOL at 1.0x, scale 0.15 SOL at `add_mult`, close at `exit_mult`.
/// Costs are set to the size itself so `net = gross − size` is pure basis math.
fn run(add_mult: u64, exit_mult: u64) -> i128 {
    const PROBE: u64 = 100_000_000;
    const ADD: u64 = 150_000_000;
    let mut lc = ScalpLifecycle::new(clean_params(), 4);
    lc.open(MINT, px(10_000), PROBE, PROBE, 0);
    assert!(
        lc.scale_in(&MINT, ADD, ADD, px(add_mult)),
        "scale-in must be accepted"
    );
    let exit = lc
        .close_at(
            &MINT,
            px(exit_mult),
            pump_quant_app::position::ExitReason::ForceClose,
        )
        .expect("close must produce an exit");
    exit.net_lamports
}

/// **THE LOAD-BEARING IDENTITY.** Scale in at 1.20x, close at exactly 1.20x.
///
/// Truth: the probe (0.10 SOL at 1.0x) is worth 0.12 SOL, a real +0.02 SOL. The
/// added 0.15 SOL was bought at 1.20x and sold at 1.20x — dead flat. Total truth
/// is therefore **+20,000,000 lamports exactly**.
///
/// The OLD rule booked `0.25 × 1.20 − 0.25 = +0.05 SOL` — it credited the added
/// tranche with a 20% gain it never earned. **Phantom = +30,000,000 lamports on a
/// single 0.25 SOL position.**
#[test]
fn scaling_in_then_closing_at_the_same_mark_earns_only_the_probes_gain() {
    let net = run(12_000, 12_000);
    const TRUTH: i128 = 20_000_000; // the probe's real +0.02 SOL
    const OLD_PHANTOM_NET: i128 = 50_000_000; // what the un-blended basis booked
                                              // `mult_bps` is quantized to integer basis points, so up to 1 bp of the 0.25 SOL
                                              // notional (25_000 lamports) is unrecoverable here — a property of the price
                                              // representation, and it rounds AGAINST us.
    const QUANT: i128 = 25_000;
    assert!(
        (TRUTH - net).abs() <= QUANT && net <= TRUTH,
        "the added tranche was bought and sold at the same price — it must contribute \
         nothing beyond bps quantization. Booked {net}, truth {TRUTH}."
    );
    assert_eq!(
        OLD_PHANTOM_NET - TRUTH,
        30_000_000,
        "the defect this test closes was worth 0.03 SOL of phantom profit per scaled position"
    );
}

/// The phantom scales with the premium paid, and is eliminated at every level.
#[test]
fn the_phantom_is_removed_at_every_scale_in_premium() {
    const PROBE: u64 = 100_000_000;
    const ADD: u64 = 150_000_000;
    for add_mult in [10_500u64, 11_000, 12_000, 15_000, 20_000] {
        let net = run(add_mult, add_mult);
        // Truth: only the probe moved, from 1.0x to add_mult.
        let truth = i128::from(PROBE) * i128::from(add_mult) / 10_000 - i128::from(PROBE);
        let quant = i128::from(PROBE + ADD) / 10_000; // 1 bp of notional
        assert!(
            (truth - net).abs() <= quant && net <= truth,
            "at a {add_mult} bp scale-in the added tranche must contribute nothing \
             beyond quantization: booked {net}, truth {truth}"
        );
        // What the old rule would have booked, and the phantom it represents.
        let old = i128::from(PROBE + ADD) * i128::from(add_mult) / 10_000 - i128::from(PROBE + ADD);
        let phantom = old - truth;
        assert_eq!(
            phantom,
            i128::from(ADD) * (i128::from(add_mult) - 10_000) / 10_000,
            "phantom == add_lamports × (mark/entry − 1), exactly as specified"
        );
        assert!(
            phantom > 0,
            "the old rule always over-booked on a rising tape"
        );
    }
}

/// **The direction is signed-correct, not merely 'lower'.** A scale-in BELOW entry
/// blends the basis DOWN and correctly RAISES reported net for that position. A fix
/// that only ever reduced net would be a fudge; this one is arithmetic.
#[test]
fn a_scale_in_below_entry_correctly_raises_reported_net() {
    // Add at 0.80x, close at 0.80x: probe lost 20%, added tranche flat.
    let net = run(8_000, 8_000);
    const PROBE: i128 = 100_000_000;
    let truth = PROBE * 8_000 / 10_000 - PROBE; // −0.02 SOL
    assert!(
        (truth - net).abs() <= 25_000 && net <= truth,
        "only the probe's loss is real: booked {net}, truth {truth}"
    );
    // The OLD rule would have booked the full 0.25 SOL down 20% = −0.05 SOL,
    // OVERSTATING the loss by 0.03 SOL. The blend corrects in the other direction.
    let old = 250_000_000i128 * 8_000 / 10_000 - 250_000_000;
    assert!(
        net > old,
        "below entry the blend must RAISE reported net ({net} vs old {old}) — the fix \
         is signed-correct arithmetic, not a one-way haircut"
    );
}

/// Real profit on the added tranche is still booked in full — the fix removes the
/// phantom, not the earnings.
#[test]
fn genuine_gains_above_the_scale_in_mark_are_still_booked() {
    // Probe at 1.0x, add at 1.20x, close at 2.00x.
    let net = run(12_000, 20_000);
    const PROBE: i128 = 100_000_000;
    const ADD: i128 = 150_000_000;
    // Truth: probe 0.10 -> 0.20 (+0.10); add 0.15 bought at 1.2x is worth
    // 0.15 × (2.00/1.20) = 0.25 (+0.10). Total +0.20 SOL.
    let truth = (PROBE * 20_000 / 10_000 - PROBE) + (ADD * 20_000 / 12_000 - ADD);
    // Blending rounds UP and `mult_bps` quantizes to integer bps — both conservative,
    // bounded by 1 bp of the 0.25 SOL notional.
    assert!(
        (truth - net).abs() <= 25_000 && net <= truth,
        "genuine gain must survive: booked {net}, truth {truth}"
    );
    assert!(net > 0, "a 2x on a scaled position is still a profit");
}

/// Fail-closed: a missing (zero) mark is absent evidence, so the add is REFUSED
/// rather than booked against an unknown basis (§6.4).
#[test]
fn a_zero_mark_refuses_the_add_rather_than_guessing() {
    let mut lc = ScalpLifecycle::new(clean_params(), 4);
    lc.open(MINT, px(10_000), 100_000_000, 100_000_000, 0);
    assert!(
        !lc.scale_in(&MINT, 150_000_000, 150_000_000, 0),
        "a zero mark must refuse the scale-in"
    );
    // ...and the position must be untouched, so a later legitimate add still works.
    assert!(
        lc.scale_in(&MINT, 150_000_000, 150_000_000, px(12_000)),
        "the refused add must not have consumed the one-shot scale slot"
    );
}
