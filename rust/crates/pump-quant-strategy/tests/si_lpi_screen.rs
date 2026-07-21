#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_strategy::safety_integrity::*;

#[test]
fn impossible_move_is_fabricated() {
    // tiny inflow, deep market, huge appreciation -> unsupportable
    let flow = FlowWindow {
        net_inflow: 1,
        appreciation_bps: 50_000,
        age_secs: 0,
    };
    match lpi_score(&flow, 1_000_000, MarketPhase::Late) {
        LpiVerdict::Fabricated {
            covariate_bps,
            threshold_margin,
        } => {
            assert!(covariate_bps > 0); // non-zero at detection
            assert!(threshold_margin > 0); // beyond support
        }
        v => panic!("expected Fabricated, got {:?}", v),
    }
}
#[test]
fn depth_normalization_changes_verdict() {
    let flow = FlowWindow {
        net_inflow: 100,
        appreciation_bps: 5_000,
        age_secs: 0,
    };
    let shallow = lpi_score(&flow, 1, MarketPhase::Mid); // lots of support
    let deep = lpi_score(&flow, 10_000_000, MarketPhase::Mid); // little support
    assert!(matches!(shallow, LpiVerdict::Clean { .. }));
    assert!(matches!(deep, LpiVerdict::Fabricated { .. }));
}
#[test]
fn covariate_decays_with_age() {
    let young = FlowWindow {
        net_inflow: 1,
        appreciation_bps: 50_000,
        age_secs: 0,
    };
    let old = FlowWindow {
        net_inflow: 1,
        appreciation_bps: 50_000,
        age_secs: LPI_COVARIATE_HALF_LIFE_SECS,
    };
    let cy = match lpi_score(&young, 1_000_000, MarketPhase::Late) {
        LpiVerdict::Fabricated { covariate_bps, .. } => covariate_bps,
        _ => panic!(),
    };
    let co = match lpi_score(&old, 1_000_000, MarketPhase::Late) {
        LpiVerdict::Fabricated { covariate_bps, .. } => covariate_bps,
        _ => panic!(),
    };
    assert_eq!(co, cy / 2); // one half-life
    assert!(co < cy);
}
