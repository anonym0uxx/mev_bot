// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'safety_integrity' component (leaf 'si_failed_sell_state').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_strategy::safety_integrity::*;

#[test]
fn failed_sell_distinct_from_zero_close() {
    let o = RawOutcome {
        entry_lamports: 1000,
        exit_lamports: None,
        fixed_cost_lamports: 50,
        sell_landed: false,
        inactivity_timeout: false,
    };
    let t = classify_terminal(&o);
    assert_eq!(t, TerminalState::FailedSell);
    assert_ne!(t, TerminalState::Closed { net_lamports: 0 });
    // full loss includes fixed cost
    assert_eq!(t.net_lamports(&o), -(1000 + 50));
}
#[test]
fn terminal_loss_distinct_and_full_loss() {
    let o = RawOutcome {
        entry_lamports: 2000,
        exit_lamports: None,
        fixed_cost_lamports: 30,
        sell_landed: false,
        inactivity_timeout: true,
    };
    let t = classify_terminal(&o);
    assert_eq!(t, TerminalState::TerminalLoss);
    assert_ne!(t, TerminalState::Closed { net_lamports: 0 });
    assert_eq!(t.net_lamports(&o), -(2000 + 30));
}
#[test]
fn completed_round_trip_closed() {
    let o = RawOutcome {
        entry_lamports: 1000,
        exit_lamports: Some(1200),
        fixed_cost_lamports: 50,
        sell_landed: true,
        inactivity_timeout: false,
    };
    assert_eq!(
        classify_terminal(&o),
        TerminalState::Closed { net_lamports: 150 }
    );
}
