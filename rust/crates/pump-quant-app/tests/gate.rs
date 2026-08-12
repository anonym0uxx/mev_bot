//! The gate's corroboration + viability contract (§29, §18, §71).

use pump_quant_app::config::Config;
use pump_quant_app::curve_depth::CurveDepth;
use pump_quant_app::expected_move::MoveEstimate;
use pump_quant_app::gate::{decide, Confirmation, GateDecision, GateReject};
use pump_quant_app::priced_move::PricedMove;
use pump_quant_watchlist::candidate::{Candidate, Features, Lane, Mint};

fn cand(lane: Lane) -> Candidate {
    Candidate::new(Mint::new([9u8; 32]), lane, 1_000, 1, Features::default())
}

/// DEPTH REALISM (re-pin #26): the gate's impact model is DERIVED from the market's
/// own SOL-side reserve (`cost_model::impact_den_for`), so a fixture's declared depth
/// is a decision input rather than decoration.
///
/// **A REAL BONDING CURVE THAT HAS BEEN BOUGHT INTO (re-pin #27, 2026-07-28).**
/// pump.fun seeds a curve with **30 SOL of VIRTUAL reserve and ZERO real SOL**, and
/// escrows `real_sol = virtual_sol − 30 SOL` thereafter. This constant used to be the
/// bare seed reserve (30 SOL) paired with a declared "sellable depth" of 29–30 SOL — a
/// market that cannot exist, since a curve nobody has bought into can pay out nothing
/// at all. It is now a curve with 0.3 SOL genuinely raised: close enough to the seed
/// that own-impact on a 0.1 SOL floor clip is unchanged at 33 bps a leg, and honest
/// about escrowing only what was paid in. See `curve_state::real_sol_for`.
const REAL_CURVE_VSOL: u64 = 30_300_000_000;

fn numeric_feats() -> Features {
    Features {
        liquidity_lamports: REAL_CURVE_VSOL,
        buy_pressure_bp: 9_000,
        unique_buyers: 12,
        age_slots: 40,
        buy_ratio_bp: 10_000, // 100% buys — passes any entry filter
        max_trade_lamports: 0, // no whale trades
        trades_observed: 100,  // plenty of evidence
    }
}

/// The priced move these tests run under: no lane evidence, so `PricedMove` reports
/// the configured cold-start constant and records that as its source. There is no
/// constructor that takes a bare number, which is the point of the type.
fn cold_start(cfg: &Config) -> PricedMove {
    PricedMove::for_candidate(
        None, // estimator disarmed: no per-candidate model estimate
        Lane::ActiveMarketScalp,
        0, // no realized lane evidence
        0,
        cfg.gate_expected_move_bps,
        cfg.expectancy_min_lane_trades,
    )
}

#[test]
fn no_confirmation_is_refused_even_for_loud_social() {
    let cfg = Config::dev_portable();
    // A social-lane candidate with a huge score but no on-chain confirmation.
    let d = decide(&cand(Lane::CreationSniper), None, &cfg, cold_start(&cfg));
    assert_eq!(
        d,
        GateDecision::Reject(GateReject::NeedsOnchainConfirmation),
        "corroboration lanes must never authorise entry alone"
    );
}

#[test]
fn confirmation_without_numeric_evidence_is_refused() {
    let cfg = Config::dev_portable();
    // Confirmed depth but no numeric microstructure (default features = 0 liquidity).
    let conf = Confirmation {
        depth: CurveDepth::derived(REAL_CURVE_VSOL),
        numeric: Features::default(),
    };
    let d = decide(
        &cand(Lane::EarlyConfirmation),
        Some(conf),
        &cfg,
        cold_start(&cfg),
    );
    assert_eq!(d, GateDecision::Reject(GateReject::NoNumericConfirmation));
}

#[test]
fn confirmed_and_viable_is_admitted() {
    let cfg = Config::dev_portable();
    let conf = Confirmation {
        depth: CurveDepth::derived(REAL_CURVE_VSOL),
        numeric: numeric_feats(),
    };
    let d = decide(
        &cand(Lane::ActiveMarketScalp),
        Some(conf),
        &cfg,
        cold_start(&cfg),
    );
    match d {
        GateDecision::Admit(band) => {
            assert!(band.x_max > 0);
            assert!(band.x_min <= band.x_cost && band.x_cost <= band.x_max);
            // THE CORRECTED CAP. `x_max` can never exceed the SOL the curve actually
            // escrows, whatever the price reserve looks like.
            assert!(
                band.x_max <= REAL_CURVE_VSOL - 30_000_000_000,
                "x_max {} exceeds the payout reserve",
                band.x_max
            );
        }
        other => panic!("expected admit, got {other:?}"),
    }
}

#[test]
fn unviable_economics_is_refused() {
    // Margin so large the impact budget goes negative -> no viable size.
    let mut cfg = Config::dev_portable();
    cfg.apply("gate_margin_bps", 9_000).unwrap();
    let conf = Confirmation {
        depth: CurveDepth::derived(REAL_CURVE_VSOL),
        numeric: numeric_feats(),
    };
    let d = decide(
        &cand(Lane::ActiveMarketScalp),
        Some(conf),
        &cfg,
        cold_start(&cfg),
    );
    assert_eq!(d, GateDecision::Reject(GateReject::EconomicallyUnviable));
}

/// **UNPROVEN DEPTH AND ABSENT DEPTH ARE DIFFERENT FACTS (re-pin #27).**
///
/// The retired gate had one guard, `sellable_depth_lamports > 0`, and answered
/// `NeedsOnchainConfirmation` to both. They are not the same:
///
/// * an `Unknown` basis means we cannot SEE the depth — an undecoded pool, a reserve
///   below the seed the venue cannot produce, or a decoded pair that contradicts the
///   `real_sol = virtual_sol − 30 SOL` identity. That is a corroboration failure.
/// * a curve at exactly the seed reserve is fully decoded and fully confirmed; it
///   simply escrows nothing, because nobody has bought into it. That is an economic
///   fact, and `size_band` refuses it on `x_max == 0`.
///
/// Collapsing them cost a reject code's worth of information on every launch-depth
/// market the engine ever saw.
#[test]
fn unproven_depth_and_absent_depth_refuse_for_different_reasons() {
    let cfg = Config::dev_portable();
    let refuse = |depth: CurveDepth| {
        decide(
            &cand(Lane::ActiveMarketScalp),
            Some(Confirmation {
                depth,
                numeric: numeric_feats(),
            }),
            &cfg,
            cold_start(&cfg),
        )
    };
    // Cannot be seen at all.
    assert_eq!(
        refuse(CurveDepth::UNKNOWN),
        GateDecision::Reject(GateReject::NeedsOnchainConfirmation)
    );
    // A reserve the venue cannot produce is refused, NOT clamped into a thin market.
    assert_eq!(
        refuse(CurveDepth::derived(200_000_000)),
        GateDecision::Reject(GateReject::NeedsOnchainConfirmation)
    );
    // Seen, and empty: a curve nobody has bought into.
    assert_eq!(
        refuse(CurveDepth::derived(30_000_000_000)),
        GateDecision::Reject(GateReject::EconomicallyUnviable)
    );
}

/// **THE 30x OVERSTATEMENT, AT THE GATE.** Same market, same everything, except one
/// confirmation declares the PRICE reserve as its sellable depth (what every fixture
/// in this repo did before re-pin #27) and the other declares what the curve escrows.
/// The retired `sellable <= virtual_sol` law passed on both.
#[test]
fn the_price_reserve_can_no_longer_be_passed_off_as_capacity() {
    let mut cfg = Config::dev_portable();
    // A bankroll big enough that the sizing chain WANTS more than the pool holds, so
    // the cap is the binding constraint rather than a formality.
    cfg.apply("bankroll_initial_lamports", 200_000_000_000)
        .unwrap();
    let vsol = 31_000_000_000; // 1 SOL raised: the 30x row from the audit table
    let conf = Confirmation {
        depth: CurveDepth::derived(vsol),
        numeric: Features {
            liquidity_lamports: vsol,
            ..numeric_feats()
        },
    };
    let GateDecision::Admit(band) = decide(
        &cand(Lane::ActiveMarketScalp),
        Some(conf),
        &cfg,
        cold_start(&cfg),
    ) else {
        panic!("a 31 SOL curve with a floor clip must still admit");
    };
    assert_eq!(
        band.x_max, 1_000_000_000,
        "capacity is the 1 SOL the curve escrows, not the 31 SOL of price reserve"
    );
    // The number the old fixtures would have permitted, for the record.
    assert_eq!(vsol / band.x_max, 31);
}

/// **RE-PIN #29 — TP1 REACHABILITY (ArXiv:2606.08232 fat-tail capture design).**
///
/// The cost-aware TP ladder's TP1 sits at +10% (11_000 bps). If the calibrated
/// model estimates a realistic upside that can't reach TP1 after round-trip costs,
/// the gate must refuse — entering such a candidate means TP1 never fires, leaving
/// the position to rely entirely on the hard stop or trailing exit. That defeats the
/// ladder's purpose: locking profit early on fat-tail moonshots.
///
/// This test constructs a model-sourced PricedMove with a deliberately low estimate
/// (5_000 bps = +5%) that is below TP1 (11_000 bps) plus round-trip cost. The gate
/// must refuse with `Tp1Unreachable`.
#[test]
fn tp1_unreachable_refuses_low_model_estimate() {
    let cfg = Config::dev_portable();
    let conf = Confirmation {
        depth: CurveDepth::derived(REAL_CURVE_VSOL),
        numeric: numeric_feats(),
    };
    // Model estimate: +5% — below TP1's +10% even before round-trip costs.
    let model = MoveEstimate {
        bps: 5_000,
        base_bps: 5_000,
        lift_bps: 0,
        signals_applied: 0,
        n: 50,
        band: 0,
    };
    let pm = PricedMove::for_candidate(
        Some(&model),
        Lane::ActiveMarketScalp,
        0,
        0,
        cfg.gate_expected_move_bps,
        cfg.expectancy_min_lane_trades,
    );
    let d = decide(
        &cand(Lane::ActiveMarketScalp),
        Some(conf),
        &cfg,
        pm,
    );
    assert_eq!(
        d,
        GateDecision::Reject(GateReject::Tp1Unreachable),
        "a model estimate below TP1+cost must be refused, not admitted to rely on the hard stop"
    );
}

/// **RE-PIN #29 — TP1 REACHABILITY does NOT fire on cold-start candidates.**
///
/// Cold-start candidates use the population prior (gate_expected_move_bps = 3_400),
/// which is intentionally below TP1 (11_000 bps). The reachability check must NOT
/// fire on them because:
///   1. The model needs paper trades to calibrate — refusing all cold-start
///      candidates would starve it of evidence forever.
///   2. The cold-start prior is a POPULATION estimate, not a per-candidate estimate.
#[test]
fn tp1_reachability_does_not_fire_on_cold_start() {
    let cfg = Config::dev_portable();
    let conf = Confirmation {
        depth: CurveDepth::derived(REAL_CURVE_VSOL),
        numeric: numeric_feats(),
    };
    // Cold-start: no model, prior = 3_400 bps (below TP1 of 11_000 bps).
    let d = decide(
        &cand(Lane::ActiveMarketScalp),
        Some(conf),
        &cfg,
        cold_start(&cfg),
    );
    // Must ADMIT, not refuse with Tp1Unreachable — cold-start needs evidence.
    match d {
        GateDecision::Admit(band) => assert!(band.x_max > 0, "cold-start must admit for evidence"),
        GateDecision::Reject(GateReject::Tp1Unreachable) => {
            panic!("Tp1Unreachable must NOT fire on cold-start candidates")
        }
        other => panic!("expected admit for cold-start, got {other:?}"),
    }
}

/// **RE-PIN #29 — TP1 REACHABILITY admits model estimates that CAN reach TP1.**
///
/// A model estimate of +15% (15_000 bps) exceeds TP1 (11_000 bps) plus round-trip
/// cost (~450 bps), so the gate must admit.
#[test]
fn tp1_reachable_admits_high_model_estimate() {
    let cfg = Config::dev_portable();
    let conf = Confirmation {
        depth: CurveDepth::derived(REAL_CURVE_VSOL),
        numeric: numeric_feats(),
    };
    // Model estimate: +15% — above TP1's +10% plus round-trip cost.
    let model = MoveEstimate {
        bps: 15_000,
        base_bps: 15_000,
        lift_bps: 0,
        signals_applied: 0,
        n: 50,
        band: 0,
    };
    let pm = PricedMove::for_candidate(
        Some(&model),
        Lane::ActiveMarketScalp,
        0,
        0,
        cfg.gate_expected_move_bps,
        cfg.expectancy_min_lane_trades,
    );
    let d = decide(
        &cand(Lane::ActiveMarketScalp),
        Some(conf),
        &cfg,
        pm,
    );
    match d {
        GateDecision::Admit(band) => assert!(band.x_max > 0, "model estimate above TP1+cost must admit"),
        GateDecision::Reject(GateReject::Tp1Unreachable) => {
            panic!("Tp1Unreachable must NOT fire when model estimate exceeds TP1+cost")
        }
        other => panic!("expected admit for high model estimate, got {other:?}"),
    }
}
