#![allow(unused_imports)]
use pump_quant_strategy::safety_integrity::*;

#[test]
fn failed_sell_distinct_from_zero_close() {
    let o = RawOutcome { entry_lamports: 1000, exit_lamports: None, fixed_cost_lamports: 50,
        sell_landed: false, inactivity_timeout: false };
    let t = classify_terminal(&o);
    assert_eq!(t, TerminalState::FailedSell);
    assert_ne!(t, TerminalState::Closed { net_lamports: 0 });
    // full loss includes fixed cost
    assert_eq!(t.net_lamports(&o), -(1000 + 50));
}
#[test]
fn terminal_loss_distinct_and_full_loss() {
    let o = RawOutcome { entry_lamports: 2000, exit_lamports: None, fixed_cost_lamports: 30,
        sell_landed: false, inactivity_timeout: true };
    let t = classify_terminal(&o);
    assert_eq!(t, TerminalState::TerminalLoss);
    assert_ne!(t, TerminalState::Closed { net_lamports: 0 });
    assert_eq!(t.net_lamports(&o), -(2000 + 30));
}
#[test]
fn completed_round_trip_closed() {
    let o = RawOutcome { entry_lamports: 1000, exit_lamports: Some(1200), fixed_cost_lamports: 50,
        sell_landed: true, inactivity_timeout: false };
    assert_eq!(classify_terminal(&o), TerminalState::Closed { net_lamports: 150 });
}
