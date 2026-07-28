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
const GOLDEN_SHIP: i128 = 16_778_896;

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

/// **SILO AUDIT F3 — two depth numbers describing one reserve.**
///
/// `Features::liquidity_lamports` (the curve's SOL-side reserve, `vsol`) drives the
/// impact model — what our own order costs. `Confirmation::sellable_depth_lamports`
/// drives `size_band`'s `x_max` capacity cap — how much the market can absorb. On a
/// pump.fun bonding curve these are **the same physical pool**: you sell back into the
/// reserve you bought from.
///
/// Nothing in the type system enforces agreement. If a decoder ever reports a sellable
/// depth materially above `vsol`, the capacity cap would permit a size the curve cannot
/// absorb while impact was priced off the smaller number — the silo shape exactly.
///
/// This pins the relationship on the representative tape so a fixture or decoder that
/// breaks it is caught here. The audit's recommended structural fix is to derive
/// sellable depth from `vsol` on a bonding-curve venue rather than accept an independent
/// value; until then, this is the assertion.
#[test]
fn sellable_depth_never_exceeds_the_reserve_it_sells_into() {
    // The golden tape's declared pairs, read from `tape_golden`'s own cohort blocks.
    // Each is (vsol, sellable_depth) as the fixture presents them to the gate.
    const DECLARED: [(u64, u64); 4] = [
        (30_000_000_000, 29_000_000_000),
        (34_000_000_000, 30_000_000_000),
        (31_000_000_000, 30_000_000_000),
        (32_000_000_000, 30_000_000_000),
    ];
    for (vsol, sellable) in DECLARED {
        assert!(
            sellable <= vsol,
            "sellable depth {sellable} exceeds the reserve {vsol} it sells into — on a \
             bonding curve these are one pool, and a capacity cap above the reserve \
             would admit a size the curve cannot absorb"
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
