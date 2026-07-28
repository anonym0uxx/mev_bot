//! **THE OPERATOR TARGET BAND ($9k–$20k) — pre-registered two-sided test.**
//!
//! The operator's instruction is to optimise for low-market-cap memecoins in the
//! **$9k–$20k** band. This file prices that instruction rather than assuming it, and
//! pins what the band can and cannot do.
//!
//! # What the band actually is, in curve coordinates
//!
//! At the SOL/USD conversion recorded in `docs/BAND_THESIS_2026-07-28.md` (≈$76), the
//! band is **118.42–263.16 SOL of market cap**, i.e. a SOL-side reserve of **61.74–92.04
//! SOL**, which is **37%–72% of the way to graduation**. Two things follow that the
//! phrase "low market cap" actively obscures, and both are pinned below:
//!
//! * **It is the MIDDLE of the bonding curve, not the launch.** Every token in the band
//!   has already survived the phase where the overwhelming majority die.
//! * **It is entirely pre-graduation**, so no migration event can fire mid-hold.
//!
//! # The pre-registered rule (written before any number here was measured)
//!
//! `mcap_band_enable` may ship ARMED only if ALL hold:
//!
//! * **(P1) SEED-ONLY AT DEFAULT** — off reproduces every decision number exactly.
//! * **(P2) THE ARITHMETIC IS REAL** — the band must measurably reduce our own execution
//!   cost, computed from the curve rather than asserted.
//! * **(P3) MATERIALITY ON THE ARBITER** — a gain over one 0.1-SOL bite on a
//!   pre-existing corpus.
//! * **(P4) NO HAZARD HARM.**
//!
//! # THE VERDICT: **DISARMED. P2 passes; P3 is UNMEASURABLE on any corpus we own.**
//!
//! P2 holds and is pinned: the band cuts own-curve impact from 33 bps a leg at launch
//! depth to 16 bps at $9k and 10 bps at $20k — a **46 bps** round-trip saving that is
//! pure arithmetic, not a fitted result.
//!
//! P3 cannot be evaluated, and saying so is the honest outcome rather than a failure of
//! effort. Band selection is a claim about *which population to trade*, and
//! `docs/EDGE_PROVENANCE_2026-07-27.md` establishes that the golden tape carries **no
//! information linking any observable to any outcome** — its trajectories are a hash of
//! the mint tag. A corpus that cannot distinguish a good token from a bad one certainly
//! cannot distinguish a good *band* of tokens from a bad one. Arming this law on a
//! synthetic result would be fitting to a fixture, which A-11's arbiter rule forbids and
//! A-13(4) names outright.
//!
//! So the law ships built, wired, guarded and OFF, with the exact measurement that would
//! arm it stated in the study. That is the same disposition as LAW B7 and
//! flow-persistence, and for the same reason: **the blocker is a missing measurement,
//! not a disproven theory.**

mod tape_golden;

use pump_quant_app::config::Config;
use pump_quant_app::curve_state;
use pump_quant_app::gate::{decide, Confirmation, GateDecision, GateReject};
use pump_quant_watchlist::candidate::{Candidate, Features, Lane, Mint};

/// One 0.1-SOL bite — the materiality bar shared with every other law study here.
const MATERIAL_LAMPORTS: i128 = 100_000_000;

/// The golden reference net at re-pin #24.
const GOLDEN_SHIP: i128 = 8_124_568;

fn band_cfg() -> Config {
    let mut c = Config::dev_portable();
    c.mcap_band_enable = true;
    c
}

fn cand() -> Candidate {
    Candidate::new(
        Mint::new([7u8; 32]),
        Lane::ActiveMarketScalp,
        1_000,
        0,
        Features {
            liquidity_lamports: 0,
            buy_pressure_bp: 6_000,
            unique_buyers: 40,
            age_slots: 300,
        },
    )
}

fn conf_at(vsol: u64) -> Confirmation {
    Confirmation {
        sellable_depth_lamports: vsol,
        numeric: Features {
            liquidity_lamports: vsol,
            buy_pressure_bp: 6_000,
            unique_buyers: 40,
            age_slots: 300,
        },
    }
}

/// **P1 — the shipped default is OFF and changes nothing.**
#[test]
fn the_band_law_ships_disarmed_and_is_decision_inert() {
    let c = Config::dev_portable();
    assert!(!c.mcap_band_enable, "the band law must ship DISARMED");
    assert_eq!(
        tape_golden::drive(c).net_lamports,
        GOLDEN_SHIP,
        "with the law off, every golden decision number must be unchanged"
    );
    // And the defaults encode the operator's stated band, not something else.
    let c = Config::dev_portable();
    assert_eq!(c.mcap_band_lo_lamports, 118_420_000_000, "$9k at the recorded conversion");
    assert_eq!(c.mcap_band_hi_lamports, 263_160_000_000, "$20k at the recorded conversion");
}

/// **THE LAW DOES WHAT IT SAYS.** In-band admits, out-of-band refuses, and the refusal
/// is a SELECTION code distinct from the economic one — so band tuning can never be
/// mistaken for a cost-floor problem in the reject statistics.
#[test]
fn the_band_admits_inside_and_refuses_outside_with_a_distinct_reason() {
    let cfg = band_cfg();
    let lo = curve_state::vsol_for_mcap(u128::from(cfg.mcap_band_lo_lamports)).unwrap();
    let hi = curve_state::vsol_for_mcap(u128::from(cfg.mcap_band_hi_lamports)).unwrap();

    // Inside the band: the band law does not refuse (the economic gate still rules).
    for vsol in [lo, (lo + hi) / 2, hi - 1] {
        let d = decide(&cand(), Some(conf_at(vsol)), &cfg, None);
        assert!(
            !matches!(d, GateDecision::Reject(GateReject::OutsideMcapBand)),
            "vsol {vsol} is inside the band and must not be refused for being outside it"
        );
    }
    // Below the band (a fresh launch) and above it (post-graduation): refused, and
    // refused for the RIGHT reason.
    for vsol in [curve_state::LAUNCH_VSOL_LAMPORTS, hi, curve_state::GRADUATION_VSOL_LAMPORTS] {
        assert_eq!(
            decide(&cand(), Some(conf_at(vsol)), &cfg, None),
            GateDecision::Reject(GateReject::OutsideMcapBand),
            "vsol {vsol} is outside the band and must be refused as a SELECTION event"
        );
    }
    // With the law off, none of those are band-refused.
    let off = Config::dev_portable();
    assert!(!matches!(
        decide(&cand(), Some(conf_at(curve_state::LAUNCH_VSOL_LAMPORTS)), &off, None),
        GateDecision::Reject(GateReject::OutsideMcapBand)
    ));
}

/// **P2 — the arithmetic benefit, computed rather than claimed.** This is the only leg
/// of the pre-registered rule that a synthetic corpus can settle, because it is a
/// property of the curve and our own clip, not of any token's behaviour.
#[test]
fn the_band_measurably_reduces_our_own_execution_cost() {
    const CLIP: u64 = 100_000_000; // the 0.1 SOL operator floor
    let imp = |vsol: u64| pump_quant_app::curve_fill::own_impact_bps(vsol, CLIP).unwrap();

    let launch = imp(curve_state::LAUNCH_VSOL_LAMPORTS);
    let lo = imp(curve_state::vsol_for_mcap(118_420_000_000).unwrap());
    let hi = imp(curve_state::vsol_for_mcap(263_160_000_000).unwrap());

    assert_eq!((launch, lo, hi), (33, 16, 10), "own impact per leg: launch / $9k / $20k");
    assert!(lo < launch && hi < lo, "cost must fall monotonically across the band");
    assert_eq!(2 * (launch - hi), 46, "the band is worth 46 bps of round-trip impact");

    // Honest scale: 46 bps against a ~300 bps round trip is ~15% of cost, and the fee
    // — the dominant term — is untouched. See `no_pre_graduation_band_can_reduce_the_fee`.
    assert!(46 * 6 < 300, "the saving is a minority of the round trip, not a fix for it");
}

/// **P3 — WHY THIS CANNOT BE SETTLED HERE, asserted rather than asserted-away.**
///
/// The golden tape's markets sit at reserves chosen to model realistic depth, but their
/// OUTCOMES are a hash of the mint tag (`edge_provenance.rs`). Arming the band on that
/// corpus therefore measures which hash buckets happen to fall in the reserve window —
/// a fitted result with the shape of a finding. This test pins the fact that the golden
/// tape's own reserves do not even overlap the operator band, which is why the law is
/// decision-inert there and why no golden A/B could ever price it.
#[test]
fn the_representative_corpus_cannot_price_a_band_selection() {
    let cfg = band_cfg();
    // The golden tape's shallowest and deepest modelled pools, in MARKET CAP terms.
    const GOLDEN_MIN_VSOL: u64 = 30_000_000_000;
    const GOLDEN_MAX_VSOL: u64 = 30_000_000_000 + 5 * 4_000_000_000 + 349 * 50_000_000;
    let lo_mcap = curve_state::mcap_lamports(GOLDEN_MIN_VSOL).unwrap();
    let hi_mcap = curve_state::mcap_lamports(GOLDEN_MAX_VSOL).unwrap();

    // The corpus spans 27.96 -> 141.33 SOL of market cap; the operator band is
    // 118.42 -> 263.16. They overlap in a SLIVER at the very bottom of the band.
    assert_eq!(lo_mcap, 27_958_993_476);
    assert_eq!(hi_mcap, 141_332_789_686);
    assert!(
        hi_mcap > u128::from(cfg.mcap_band_lo_lamports),
        "the corpus does reach into the band..."
    );
    assert!(
        hi_mcap * 2 < u128::from(cfg.mcap_band_hi_lamports) * 2 - u128::from(cfg.mcap_band_lo_lamports),
        "...but only into its lowest sliver: the corpus tops out at {hi_mcap}, barely \
         above the band floor {} and far below the ceiling {}",
        cfg.mcap_band_lo_lamports,
        cfg.mcap_band_hi_lamports
    );

    // So arming the band on this corpus is not a verdict on the band — it is a verdict
    // on how many of the tape's hash-drawn markets happen to be deep enough. Measured,
    // not assumed:
    let armed = tape_golden::drive(cfg).net_lamports;
    assert_eq!(
        armed, 0,
        "arming the band admits (almost) nothing on this corpus — a DEGENERATE outcome, \
         not evidence for or against the band"
    );
    // And the "delta" such an A/B would report is simply the whole book disappearing,
    // which no pre-registered rule would ever accept as a measurement.
    let delta = armed - GOLDEN_SHIP;
    assert_eq!(delta, -GOLDEN_SHIP, "the delta is the entire book, i.e. not a measurement");
    assert!(delta.abs() < MATERIAL_LAMPORTS, "and it is sub-material besides: {delta}");
}

/// **THE GUARD.** Arming the band because the impact arithmetic (P2) looks good is the
/// available mistake here: P2 is real but it is a ~15% cost effect, and P3 — whether the
/// band contains better TOKENS — is entirely unmeasured. This test states that so a
/// future arming has to trip an explicit failure rather than slide in.
#[test]
fn arming_on_the_cost_argument_alone_is_not_permitted() {
    assert!(
        !Config::dev_portable().mcap_band_enable,
        "P3 (does the band contain better tokens?) is UNMEASURED on every corpus we own. \
         The 46 bps impact saving is real but is a cost effect, not a selection edge. \
         Arm this only after a live/replay corpus stratified by curve progress shows the \
         band's realized per-trade net clears the bar in `docs/BAND_THESIS_2026-07-28.md`."
    );
}
