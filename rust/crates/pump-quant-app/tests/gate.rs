//! The gate's corroboration + viability contract (§29, §18, §71).

use pump_quant_app::config::Config;
use pump_quant_app::gate::{decide, Confirmation, GateDecision, GateReject};
use pump_quant_watchlist::candidate::{Candidate, Features, Lane, Mint};

fn cand(lane: Lane) -> Candidate {
    Candidate::new(Mint::new([9u8; 32]), lane, 1_000, 1, Features::default())
}

/// DEPTH REALISM (re-pin #26): the gate's impact model is now DERIVED from the
/// market's own SOL-side reserve (`cost_model::impact_den_for`), so a fixture's
/// declared depth is a decision input rather than decoration. Real pump.fun virtual
/// reserves start at 30 SOL; the old 0.1 SOL declared here put the operator's 0.1 SOL
/// floor clip at 100% of the pool.
const REAL_CURVE_VSOL: u64 = 30_000_000_000;

fn numeric_feats() -> Features {
    Features {
        liquidity_lamports: REAL_CURVE_VSOL,
        buy_pressure_bp: 9_000,
        unique_buyers: 12,
        age_slots: 40,
    }
}

#[test]
fn no_confirmation_is_refused_even_for_loud_social() {
    let cfg = Config::dev_portable();
    // A social-lane candidate with a huge score but no on-chain confirmation.
    let d = decide(&cand(Lane::CreationSniper), None, &cfg, None);
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
        sellable_depth_lamports: REAL_CURVE_VSOL,
        numeric: Features::default(),
    };
    let d = decide(&cand(Lane::EarlyConfirmation), Some(conf), &cfg, None);
    assert_eq!(d, GateDecision::Reject(GateReject::NoNumericConfirmation));
}

#[test]
fn confirmed_and_viable_is_admitted() {
    let cfg = Config::dev_portable();
    let conf = Confirmation {
        sellable_depth_lamports: REAL_CURVE_VSOL,
        numeric: numeric_feats(),
    };
    let d = decide(&cand(Lane::ActiveMarketScalp), Some(conf), &cfg, None);
    match d {
        GateDecision::Admit(band) => {
            assert!(band.x_max > 0);
            assert!(band.x_min <= band.x_cost && band.x_cost <= band.x_max);
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
        sellable_depth_lamports: REAL_CURVE_VSOL,
        numeric: numeric_feats(),
    };
    let d = decide(&cand(Lane::ActiveMarketScalp), Some(conf), &cfg, None);
    assert_eq!(d, GateDecision::Reject(GateReject::EconomicallyUnviable));
}

#[test]
fn zero_depth_confirmation_is_treated_as_unconfirmed() {
    let cfg = Config::dev_portable();
    let conf = Confirmation {
        sellable_depth_lamports: 0,
        numeric: numeric_feats(),
    };
    let d = decide(&cand(Lane::ActiveMarketScalp), Some(conf), &cfg, None);
    assert_eq!(
        d,
        GateDecision::Reject(GateReject::NeedsOnchainConfirmation)
    );
}
