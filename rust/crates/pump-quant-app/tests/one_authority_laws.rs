//! **ONE AUTHORITY PER QUANTITY — the standing guard against siloed decision inputs.**
//!
//! # The defect class this file exists to prevent
//!
//! Re-pin #26 removed a defect where round-trip COST was computed independently in three
//! places: the gate (which decided whether to trade), the engine + lifecycle (which
//! booked the P&L), and `scalp.rs` (the paper-fill path). They disagreed by up to 247 bps
//! and in OPPOSITE directions depending on configuration. The engine admitted trades
//! believing one number and reported profit believing another.
//!
//! What made it survive so long is worth stating, because it is the thing to guard
//! against rather than the specific numbers: **no single file was wrong on its own
//! terms.** Reading `position::realize` alone showed an entry leg that paid no fee.
//! Reading the gate alone showed a round trip that paid no rent. Each file was locally
//! coherent and the system was wrong between them. A reviewer could audit any one site
//! and find nothing.
//!
//! `docs/SILO_AUDIT_2026-07-28.md` swept the rest of the decision path for the same
//! shape. This file pins what that audit found, so a regression fails loudly here
//! instead of being discovered by a future audit.
//!
//! # The pattern the codebase already had, and should copy
//!
//! `BankrollOrigin` is the model answer: the sizing base is either
//! `PaperSeed(cfg.bankroll_initial_lamports)` or a live reconciled balance, and the
//! distinction is carried in the TYPE rather than in a comment. The operator's standing
//! rule — live bankroll always from the reconciled wallet, never the config seed — is
//! therefore enforced by the compiler. Cost had no such type, which is precisely why it
//! drifted into three implementations. Any future fix in this class should make the
//! provenance a type, not a convention.

mod tape_golden;

use pump_quant_app::config::Config;
use pump_quant_app::{cost_model, curve_state};

/// The golden reference net at re-pin #26.
/// Re-pin #27 (2026-07-28): 16_778_896 -> 31_111_528. The move is the confirmed-set
/// eviction key reordering under corrected fixture depth, NOT either provenance fix —
/// both were measured decision-inert on this tape. See `golden_digest.rs`.
const GOLDEN_SHIP: i128 = 31_111_528;

/// **SILO AUDIT F1 — one expected move per trade.**
///
/// `Engine::gate_evaluate` makes two consecutive decisions about one candidate: it
/// prices the size band (admission), then ranks the slot (§23 arbitration). Arbitration
/// used to reach independently for `conditional_edge_bps` — a PER-LANE estimate, about
/// six numbers for the entire universe — and discard the per-candidate `move_override`
/// that had just priced the band.
///
/// While `expected_move_model_enable` is false the override is always `None`, so the
/// per-lane estimate is the only path and the fix is decision-inert. This test pins that
/// inertness: if threading the priced move into arbitration ever changes a shipped
/// number, the change was not the no-op it is documented to be.
///
/// The defect it guards is LATENT, not live — which is exactly why it needs a guard. It
/// costs nothing today and becomes a silent mis-ranking of the scarce position slots on
/// the day the estimator is armed.
#[test]
fn admission_and_arbitration_price_the_same_trade() {
    let cfg = Config::dev_portable();
    assert!(
        !cfg.expected_move_model_enable,
        "the estimator ships DISARMED — while it is off, arbitration's per-lane \
         fallback is the only path and this law is inert by construction"
    );
    assert_eq!(
        tape_golden::drive(cfg).net_lamports,
        GOLDEN_SHIP,
        "threading the priced expected move into §23 arbitration must be byte-identical \
         with the estimator disarmed; if this moved, the fix was not decision-inert"
    );
}

/// **SILO AUDIT F3 — CORRECTED. They are not two views of one number; they are two
/// DIFFERENT quantities with an exact relationship, and the first version of this test
/// asserted a bound so weak it passed while the fixtures were 30x wrong.**
///
/// The original finding said `liquidity_lamports` and `sellable_depth_lamports`
/// "describe the same pool" and asserted `sellable <= vsol`. Every fixture passed. The
/// truth is sharper and worse:
///
/// * `virtual_sol` sets the **price curve**. pump.fun seeds it at 30 SOL.
/// * `real_sol` is the SOL **actually in the pool** — the only SOL a seller can be paid.
///   It is seeded at ZERO, and every buy adds the same lamports to both, so
///   `real_sol = virtual_sol - 30 SOL` exactly.
///
/// The identity is confirmed by the venue's own published constant: at graduation
/// `virtual_sol = 115,005,359,056`, which predicts a raise of **85.005 SOL** — precisely
/// the figure the ecosystem quotes as pump.fun's graduation threshold. An identity that
/// reproduces the platform's headline number from first principles is the identity.
///
/// Against the CORRECT bound the old fixtures were not close. A curve at
/// `vsol = 30 SOL` is one nobody has bought into: it can pay out **nothing**, and the
/// tape declared 29 SOL of sellable depth there. At `vsol = 31 SOL` the pool holds
/// 1 SOL and the tape claimed 30 — a **30x** overstatement feeding the `x_max` cap.
///
/// This is the third member of a family this codebase keeps rediscovering: market cap
/// (`vsol^2 / 32_190_000_000`), own-curve impact (`notional * 10_000 / vsol`) and now
/// payout depth (`vsol - 30 SOL`) are all pure functions of the SOL-side reserve. Each
/// was independently sourced, configured or guessed until someone did the algebra.
#[test]
fn payout_depth_is_bounded_by_sol_that_actually_exists() {
    // A curve nobody has bought into can pay out nothing, however deep its PRICE
    // reserve looks. This single case is what the retired `sellable <= vsol` missed.
    assert_eq!(
        curve_state::real_sol_for(curve_state::LAUNCH_VSOL_LAMPORTS),
        Some(0),
        "a freshly launched curve holds no real SOL and can pay out nothing"
    );

    // The identity reproduces the venue's published graduation raise. Measured one
    // lamport BELOW graduation, because at graduation itself the derivation correctly
    // refuses — the curve is complete and there is no curve left to derive against.
    let just_before = curve_state::real_sol_for(curve_state::GRADUATION_VSOL_LAMPORTS - 1)
        .expect("one lamport below graduation is still on the curve");
    assert_eq!(
        just_before / 1_000_000,
        85_005,
        "the curve must raise 85.005 SOL to graduate — the ecosystem's own constant, \
         reproduced from `virtual_sol - LAUNCH_VSOL` and nothing else"
    );

    // **THE VENUE BOUNDARY, and it must REFUSE rather than answer.** The −30 SOL offset
    // is a bonding-curve fact. Applying it to a migrated PumpSwap pool would understate
    // payout depth by 30 SOL; the post-graduation case belongs to the caller's venue
    // branch (`CurveDepth::MigratedPool`), and this derivation must not silently serve it.
    assert_eq!(
        curve_state::real_sol_for(curve_state::GRADUATION_VSOL_LAMPORTS),
        None,
        "at and beyond graduation the curve derivation REFUSES — the offset is a \
         bonding-curve fact and a migrated pool's reserves are its own"
    );
    // And a reserve below the seeded floor is a broken decode, not a thin market.
    assert_eq!(
        curve_state::real_sol_for(curve_state::LAUNCH_VSOL_LAMPORTS - 1),
        None,
        "a reserve below the 30 SOL seed cannot exist on this venue — refuse, never clamp"
    );

    // Payout depth is strictly less than the price reserve everywhere on the curve,
    // by exactly the 30 SOL virtual offset — never equal, which is what the retired
    // assertion permitted.
    for vsol in [
        curve_state::LAUNCH_VSOL_LAMPORTS,
        45_000_000_000,
        61_740_908_643,
        92_038_689_691,
        curve_state::GRADUATION_VSOL_LAMPORTS - 1,
    ] {
        let payout = curve_state::real_sol_for(vsol).expect("on-curve reserve");
        assert_eq!(
            payout,
            vsol - curve_state::LAUNCH_VSOL_LAMPORTS,
            "payout depth is the price reserve minus the virtual offset, exactly"
        );
        assert!(
            payout < vsol,
            "payout must be STRICTLY below the price reserve; `sellable <= vsol` was              the assertion that let a 30x overstatement through"
        );
    }
}

/// **The impact identity, restated as a silo guard rather than an arithmetic one.**
///
/// The gate's linear impact model and the curve's constant-product impact were two
/// implementations of one quantity until re-pin #26 made the gate derive its denominator
/// per candidate. They are now provably the same function. This asserts the identity
/// directly across the operator's target band, so a future edit to either side that
/// breaks the equivalence fails here — where the message explains what was lost — rather
/// than as an unexplained drift in a net somewhere.
#[test]
fn the_gate_and_the_curve_are_one_impact_model() {
    const CLIP: u64 = 100_000_000;
    for vsol in [
        curve_state::LAUNCH_VSOL_LAMPORTS,
        61_740_908_643,
        92_038_689_691,
        curve_state::GRADUATION_VSOL_LAMPORTS,
    ] {
        let den = cost_model::impact_den_for(vsol);
        let gate_bps = CLIP / den; // ImpactCurve::linear_test semantics
        let curve_bps = u64::from(pump_quant_app::curve_fill::own_impact_bps(vsol, CLIP).unwrap());
        assert!(
            gate_bps.abs_diff(curve_bps) <= 1,
            "the gate ({gate_bps} bps) and the curve ({curve_bps} bps) must price our own \
             order identically at vsol {vsol}; they diverge only if someone reintroduces \
             a standing impact denominator, which can be right for exactly one depth"
        );
    }
}

/// **The venue fee has one source, and it is a function of the market, not of config.**
///
/// Four legacy config fields (`entry_fee_bps`, `exit_fee_bps`, `entry_tip_lamports`,
/// `exit_tip_lamports`) survive so an existing operator config still parses. They are
/// documented as decision-inert. This pins that they cannot reach a decision by proving
/// the shipped net is invariant to them — if setting a fee field moves a number, the
/// "decision-inert" comment has become a lie.
#[test]
fn the_retired_fee_fields_cannot_reach_a_decision() {
    let mut c = Config::dev_portable();
    c.entry_fee_bps = 9_999;
    c.exit_fee_bps = 9_999;
    c.entry_tip_lamports = 500_000_000;
    c.exit_tip_lamports = 500_000_000;
    assert_eq!(
        tape_golden::drive(c).net_lamports,
        GOLDEN_SHIP,
        "the legacy fee/tip fields are retained ONLY so an old config parses; if moving \
         them to absurd values changes the book, they are back on the decision path and \
         the cost model has a second source again"
    );
}
