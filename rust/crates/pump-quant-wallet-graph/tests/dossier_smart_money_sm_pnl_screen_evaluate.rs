// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'smart_money' component (leaf 'sm_pnl_screen_evaluate').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(
    unused_imports,
    dead_code,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_wallet_graph::smart_money::*;

#[test]
fn sm_pnl_evaluate_props() {
    fn tr(token: u64, is_buy: bool, units: u64, sol: u64, self_dealt: bool) -> Trade {
        Trade {
            family: pump_quant_wallet_graph::FamilyId(1),
            token: pump_quant_wallet_graph::TokenId(token),
            is_buy,
            units,
            sol_lamports: sol,
            self_dealt,
            slot: 0,
        }
    }
    let s = PnlScreen::new(PnlScreenConfig { min_tokens: 2 });

    // Clean two-token profit passes with exact components.
    let clean = [
        tr(1, true, 100, 1000, false),
        tr(1, false, 100, 1500, false),
        tr(2, true, 50, 500, false),
        tr(2, false, 50, 700, false),
    ];
    let r = s.evaluate(&clean);
    assert_eq!(r.reason, PnlFailReason::None);
    assert!(r.passed());
    assert_eq!(r.components.total_realized, 700);
    assert_eq!(r.components.top_removed_realized, 200);
    assert_eq!(r.components.top_token_pnl, 500);
    assert_eq!(r.components.token_count, 2);
    assert_eq!(r.components.profitable_token_count, 2);

    // Self-dealt profit is excluded from skill PnL and rejected.
    let sd = [
        tr(1, true, 100, 1000, true),
        tr(1, false, 100, 1500, true),
        tr(2, true, 50, 500, false),
        tr(2, false, 50, 700, false),
        tr(3, true, 50, 500, false),
        tr(3, false, 50, 600, false),
    ];
    let r2 = s.evaluate(&sd);
    assert_eq!(r2.reason, PnlFailReason::SelfDealing);
    assert!(!r2.passed());
    assert_eq!(r2.components.self_dealt_token_count, 1);
    assert_eq!(r2.components.self_dealt_realized, 500);
    assert_eq!(r2.components.total_realized, 300);
    assert_eq!(r2.components.token_count, 2);

    // One jackpot (+500) plus a loser (-100): concentration screen fires.
    let lucky = [
        tr(1, true, 100, 1000, false),
        tr(1, false, 100, 1500, false),
        tr(2, true, 50, 500, false),
        tr(2, false, 50, 400, false),
    ];
    let r3 = s.evaluate(&lucky);
    assert_eq!(r3.reason, PnlFailReason::LuckyConcentrated);
    assert_eq!(r3.components.total_realized, 400);
    assert_eq!(r3.components.top_removed_realized, -100);

    // Partial realization pro-rates cost basis: buy 100u/1000L, sell 60u/900L -> pnl 300.
    let partial = [
        tr(1, true, 100, 1000, false),
        tr(1, false, 60, 900, false),
        tr(2, true, 40, 400, false),
        tr(2, false, 40, 500, false),
    ];
    let r4 = s.evaluate(&partial);
    assert_eq!(r4.reason, PnlFailReason::None);
    assert_eq!(r4.components.top_token_pnl, 300);
    assert_eq!(r4.components.total_realized, 400);
}
