//! `wl_lane_performance` leaf tests: realized net-SOL accumulation per lane,
//! signed losses, per-trade mean, totals, and saturating overflow contract.

use pump_quant_watchlist::candidate::Lane;
use pump_quant_watchlist::lane_performance::LanePerformance;

#[test]
fn records_net_sol_and_counts_per_lane() {
    let mut lp = LanePerformance::new();
    lp.record(Lane::ActiveMarketScalp, 1_000);
    lp.record(Lane::ActiveMarketScalp, 2_500);
    lp.record(Lane::EarlyConfirmation, 400);
    // Independent sums.
    assert_eq!(lp.net_sol(Lane::ActiveMarketScalp), 3_500);
    assert_eq!(lp.trade_count(Lane::ActiveMarketScalp), 2);
    assert_eq!(lp.net_sol(Lane::EarlyConfirmation), 400);
    assert_eq!(lp.trade_count(Lane::EarlyConfirmation), 1);
    // Untouched lane stays zero.
    assert_eq!(lp.net_sol(Lane::CreationSniper), 0);
    assert_eq!(lp.trade_count(Lane::CreationSniper), 0);
}

#[test]
fn losses_are_signed_and_can_net_negative() {
    let mut lp = LanePerformance::new();
    lp.record(Lane::GraduationTransition, 500);
    lp.record(Lane::GraduationTransition, -1_200);
    // 500 + (-1200) = -700.
    assert_eq!(lp.net_sol(Lane::GraduationTransition), -700);
    assert_eq!(lp.trade_count(Lane::GraduationTransition), 2);
}

#[test]
fn net_sol_per_trade_truncates_toward_zero() {
    let mut lp = LanePerformance::new();
    lp.record(Lane::ActiveMarketScalp, 1_000);
    lp.record(Lane::ActiveMarketScalp, 1_000);
    lp.record(Lane::ActiveMarketScalp, 500);
    // total 2_500 / 3 trades = 833 (truncated).
    assert_eq!(lp.net_sol_per_trade(Lane::ActiveMarketScalp), Some(833));
    // Negative mean truncates toward zero: -2500/3 = -833.
    let mut lp2 = LanePerformance::new();
    lp2.record(Lane::ActiveMarketScalp, -1_000);
    lp2.record(Lane::ActiveMarketScalp, -1_000);
    lp2.record(Lane::ActiveMarketScalp, -500);
    assert_eq!(lp2.net_sol_per_trade(Lane::ActiveMarketScalp), Some(-833));
    // No trades => None.
    assert_eq!(lp.net_sol_per_trade(Lane::CreationSniper), None);
}

#[test]
fn total_net_sol_sums_all_lanes() {
    let mut lp = LanePerformance::new();
    lp.record(Lane::CreationSniper, 100);
    lp.record(Lane::EarlyConfirmation, 200);
    lp.record(Lane::GraduationTransition, -50);
    lp.record(Lane::ActiveMarketScalp, 1_000);
    // 100 + 200 - 50 + 1000 = 1250.
    assert_eq!(lp.total_net_sol(), 1_250);
}

#[test]
fn net_sol_add_saturates_instead_of_wrapping() {
    let mut lp = LanePerformance::new();
    lp.record(Lane::ActiveMarketScalp, i64::MAX);
    // Another large positive must clamp at i64::MAX, never wrap to negative.
    lp.record(Lane::ActiveMarketScalp, i64::MAX);
    assert_eq!(lp.net_sol(Lane::ActiveMarketScalp), i64::MAX);
    // Symmetric on the negative side.
    let mut lp2 = LanePerformance::new();
    lp2.record(Lane::ActiveMarketScalp, i64::MIN);
    lp2.record(Lane::ActiveMarketScalp, i64::MIN);
    assert_eq!(lp2.net_sol(Lane::ActiveMarketScalp), i64::MIN);
}

#[test]
fn default_matches_new() {
    assert_eq!(LanePerformance::default(), LanePerformance::new());
}
