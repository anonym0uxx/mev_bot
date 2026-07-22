//! Leaf tests for `fill`: Modes A/B/C fill models with exit impairment (§38).
//! Every numeric expectation is derived by hand from the model in `fill.rs`.

use pump_quant_simulator::fill::{
    simulate_fill, CostModel, ExitImpairment, FillMode, ImpairmentLevel, MarketState,
};
use pump_quant_simulator::terminal_loss::TerminalLossPolicy;

fn market() -> MarketState {
    MarketState {
        notional_lamports: 100_000_000, // 0.1 SOL
        move_bps: 5_000,                // +50%
        depth_lamports: 1_000_000_000,  // 1 SOL depth
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

// Shared entry-side facts (independent of mode):
//   entry_impact = 100M*10000/1e9 = 1000 bps
//   after_entry_fee = 100M - 1% = 99_000_000
//   entry_value = 99_000_000 - 10% = 89_100_000
//   entry_cost = 100_000_000 + 50_000 = 100_050_000
//   mark = 89_100_000 * 1.5 = 133_650_000
//   exit_impact(133_650_000) = floor(1336.5) = 1336 bps

#[test]
fn mode_a_signal_replay_is_not_claimable() {
    let r = simulate_fill(
        &market(),
        &costs(),
        &imp(),
        FillMode::SignalReplay,
        &TerminalLossPolicy::WriteToZero,
    );
    assert!(!r.claimable, "Mode A must not assert a profitability claim");
    assert_eq!(r.entry_impact_bps, 1000);
    assert_eq!(r.entry_value_lamports, 89_100_000);
    assert_eq!(r.entry_cost_lamports, 100_050_000);
    // Signal-side proceeds are the raw mark, no exit costs.
    assert_eq!(r.exit_proceeds_lamports, 133_650_000);
    assert_eq!(r.net_pnl_lamports, 133_650_000 - 100_050_000);
    assert_eq!(r.exit_impact_bps, 0);
    assert!(!r.impaired);
}

#[test]
fn mode_b_optimistic_ceiling() {
    let r = simulate_fill(
        &market(),
        &costs(),
        &imp(),
        FillMode::OptimisticCeiling,
        &TerminalLossPolicy::WriteToZero,
    );
    assert!(r.claimable);
    assert_eq!(r.entry_impact_bps, 1000);
    assert_eq!(r.exit_impact_bps, 1336);
    // after_exit_fee = 133_650_000 - 1% = 132_313_500
    // after_impact   = 132_313_500 - floor(17_677_083.6) = 114_636_417
    // proceeds       = 114_636_417 - 50_000 = 114_586_417
    assert_eq!(r.exit_proceeds_lamports, 114_586_417);
    assert_eq!(r.net_pnl_lamports, 14_536_417);
    assert!(!r.impaired);
}

#[test]
fn mode_c_realistic_applies_impairment() {
    let r = simulate_fill(
        &market(),
        &costs(),
        &imp(),
        FillMode::Adversarial(ImpairmentLevel::Realistic),
        &TerminalLossPolicy::WriteToZero,
    );
    assert!(r.claimable);
    assert!(r.impaired);
    assert!(!r.unexitable);
    // exit_fee_total = 100 + 50 = 150 ; impair = (200+300) = 500 ; retry_tip = 20_000
    // after_exit_fee = 133_650_000 - 150bps = 131_645_250
    // after_impact   = 131_645_250 - 1336bps(floor 17_587_805) = 114_057_445
    // after_impair   = 114_057_445 - 500bps(floor 5_702_872) = 108_354_573
    // proceeds       = 108_354_573 - 50_000 - 20_000 = 108_284_573
    assert_eq!(r.exit_proceeds_lamports, 108_284_573);
    assert_eq!(r.net_pnl_lamports, 8_234_573);
}

#[test]
fn mode_c_pessimistic_is_harsher_than_realistic() {
    let realistic = simulate_fill(
        &market(),
        &costs(),
        &imp(),
        FillMode::Adversarial(ImpairmentLevel::Realistic),
        &TerminalLossPolicy::WriteToZero,
    );
    let pess = simulate_fill(
        &market(),
        &costs(),
        &imp(),
        FillMode::Adversarial(ImpairmentLevel::Pessimistic),
        &TerminalLossPolicy::WriteToZero,
    );
    // exit_fee_total = 100 + 100 = 200 ; impair = 1000 ; retry_tip = 40_000
    // after_exit_fee = 133_650_000 - 200bps = 130_977_000
    // after_impact   = 130_977_000 - 1336bps(floor 17_498_527) = 113_478_473
    // after_impair   = 113_478_473 - 1000bps(floor 11_347_847) = 102_130_626
    // proceeds       = 102_130_626 - 50_000 - 40_000 = 102_040_626
    assert_eq!(pess.exit_proceeds_lamports, 102_040_626);
    assert_eq!(pess.net_pnl_lamports, 1_990_626);
    // Ordering property: pessimistic <= realistic net PnL.
    assert!(pess.net_pnl_lamports < realistic.net_pnl_lamports);
}

#[test]
fn mode_ordering_b_ge_c_realistic_ge_c_pessimistic() {
    let b = simulate_fill(
        &market(),
        &costs(),
        &imp(),
        FillMode::OptimisticCeiling,
        &TerminalLossPolicy::WriteToZero,
    );
    let cr = simulate_fill(
        &market(),
        &costs(),
        &imp(),
        FillMode::Adversarial(ImpairmentLevel::Realistic),
        &TerminalLossPolicy::WriteToZero,
    );
    let cp = simulate_fill(
        &market(),
        &costs(),
        &imp(),
        FillMode::Adversarial(ImpairmentLevel::Pessimistic),
        &TerminalLossPolicy::WriteToZero,
    );
    // Optimistic ceiling is an upper bound on the adversarial modes.
    assert!(b.net_pnl_lamports > cr.net_pnl_lamports);
    assert!(cr.net_pnl_lamports > cp.net_pnl_lamports);
}

#[test]
fn mode_c_unexitable_uses_predeclared_terminal_loss_not_mark() {
    let mut bad = imp();
    bad.unexitable = true;
    // WriteToZero: proceeds 0, full loss of entry cost.
    let zero = simulate_fill(
        &market(),
        &costs(),
        &bad,
        FillMode::Adversarial(ImpairmentLevel::Realistic),
        &TerminalLossPolicy::WriteToZero,
    );
    assert!(zero.unexitable);
    assert_eq!(zero.exit_proceeds_lamports, 0);
    assert_eq!(zero.net_pnl_lamports, -100_050_000);
    // The mark (133_650_000) is never used for an unexitable position.
    assert!(zero.exit_proceeds_lamports < 133_650_000);

    // Predeclared 20% residual of the *basis* (entry_value 89_100_000): 17_820_000.
    let residual = simulate_fill(
        &market(),
        &costs(),
        &bad,
        FillMode::Adversarial(ImpairmentLevel::Realistic),
        &TerminalLossPolicy::ResidualBps(2_000),
    );
    assert_eq!(residual.exit_proceeds_lamports, 17_820_000);
    assert_eq!(residual.net_pnl_lamports, 17_820_000 - 100_050_000);
}

#[test]
fn zero_depth_forces_full_impact() {
    let mut m = market();
    m.depth_lamports = 0;
    let r = simulate_fill(
        &m,
        &costs(),
        &imp(),
        FillMode::OptimisticCeiling,
        &TerminalLossPolicy::WriteToZero,
    );
    // Full 100% entry impact -> entry_value = 99_000_000 reduced by 100% = 0.
    assert_eq!(r.entry_impact_bps, 10_000);
    assert_eq!(r.entry_value_lamports, 0);
}
