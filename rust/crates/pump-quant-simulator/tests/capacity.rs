//! Leaf tests for `capacity`: capacity-curve harness over the §55 size grid.

use pump_quant_simulator::capacity::{capacity_curve, LandingModel, CAPACITY_GRID_LAMPORTS};
use pump_quant_simulator::fill::{
    CostModel, ExitImpairment, FillMode, ImpairmentLevel, MarketState,
};
use pump_quant_simulator::terminal_loss::TerminalLossPolicy;

fn base_market() -> MarketState {
    MarketState {
        notional_lamports: 0, // overridden per grid size
        move_bps: 5_000,
        depth_lamports: 1_000_000_000,
        impact_k_bps: 10_000,
    }
}

fn costs() -> CostModel {
    CostModel {
        entry_fee_bps: 100,
        exit_fee_bps: 100,
        entry_tip_lamports: 50_000,
        exit_tip_lamports: 50_000,
    }
}

fn imp() -> ExitImpairment {
    ExitImpairment {
        first_sell_penalty_bps: 200,
        retry_slippage_bps: 300,
        fee_escalation_bps: 50,
        retry_tip_lamports: 20_000,
        unexitable: false,
    }
}

fn landing() -> LandingModel {
    LandingModel {
        base_bps: 9_000,
        penalty_k_bps: 20_000,
    }
}

#[test]
fn curve_covers_the_mandated_grid_in_order() {
    let curve = capacity_curve(
        &base_market(),
        &costs(),
        &imp(),
        FillMode::Adversarial(ImpairmentLevel::Realistic),
        &TerminalLossPolicy::WriteToZero,
        &landing(),
    );
    assert_eq!(curve.len(), 7);
    let sizes: Vec<u64> = curve.iter().map(|p| p.size_lamports).collect();
    assert_eq!(sizes, CAPACITY_GRID_LAMPORTS.to_vec());
}

#[test]
fn anchored_points_match_hand_computed_fill() {
    let curve = capacity_curve(
        &base_market(),
        &costs(),
        &imp(),
        FillMode::Adversarial(ImpairmentLevel::Realistic),
        &TerminalLossPolicy::WriteToZero,
        &landing(),
    );
    // 0.01 SOL (index 0): entry_impact 100 bps, net 3_434_703 (hand-derived).
    assert_eq!(curve[0].price_impact_bps, 100);
    assert_eq!(curve[0].expectancy_lamports, 3_434_703);
    // 0.10 SOL (index 3): entry_impact 1000 bps, net 8_234_573 (matches fill leaf).
    assert_eq!(curve[3].price_impact_bps, 1000);
    assert_eq!(curve[3].expectancy_lamports, 8_234_573);
}

#[test]
fn price_impact_strictly_increases_with_size() {
    let curve = capacity_curve(
        &base_market(),
        &costs(),
        &imp(),
        FillMode::Adversarial(ImpairmentLevel::Realistic),
        &TerminalLossPolicy::WriteToZero,
        &landing(),
    );
    for w in curve.windows(2) {
        assert!(
            w[1].price_impact_bps > w[0].price_impact_bps,
            "impact must grow with size: {} !> {}",
            w[1].price_impact_bps,
            w[0].price_impact_bps
        );
    }
}

#[test]
fn per_unit_expectancy_strictly_decreases_nonlinearly() {
    // Scaling never assumes linear PnL: per-lamport net expectancy falls as size
    // rises because price impact grows super-proportionally in effect.
    let curve = capacity_curve(
        &base_market(),
        &costs(),
        &imp(),
        FillMode::Adversarial(ImpairmentLevel::Realistic),
        &TerminalLossPolicy::WriteToZero,
        &landing(),
    );
    for w in curve.windows(2) {
        // e0/size0 > e1/size1  <=>  e0*size1 > e1*size0  (all positive here).
        let lhs = w[0].expectancy_lamports * (w[1].size_lamports as i128);
        let rhs = w[1].expectancy_lamports * (w[0].size_lamports as i128);
        assert!(
            lhs > rhs,
            "per-unit expectancy must decrease with size ({lhs} !> {rhs})"
        );
    }
}

#[test]
fn landing_probability_is_nonincreasing() {
    let curve = capacity_curve(
        &base_market(),
        &costs(),
        &imp(),
        FillMode::Adversarial(ImpairmentLevel::Realistic),
        &TerminalLossPolicy::WriteToZero,
        &landing(),
    );
    for w in curve.windows(2) {
        assert!(w[1].landing_prob_bps <= w[0].landing_prob_bps);
    }
    // Hand check at 0.10 SOL: penalty = 100M*20000/1e9 = 2000 ; 9000-2000 = 7000.
    assert_eq!(curve[3].landing_prob_bps, 7_000);
}

#[test]
fn terminal_loss_exposure_reported_when_unexitable() {
    let mut bad = imp();
    bad.unexitable = true;
    let curve = capacity_curve(
        &base_market(),
        &costs(),
        &bad,
        FillMode::Adversarial(ImpairmentLevel::Realistic),
        &TerminalLossPolicy::WriteToZero,
        &landing(),
    );
    // Every point unexitable under WriteToZero: exposure == entry_cost (positive).
    for p in &curve {
        assert!(p.terminal_loss_exposure_lamports > 0);
        // exposure == entry_cost - proceeds(0) == entry_cost == size + entry_tip.
        assert_eq!(
            p.terminal_loss_exposure_lamports,
            p.size_lamports as i128 + 50_000
        );
    }
}
