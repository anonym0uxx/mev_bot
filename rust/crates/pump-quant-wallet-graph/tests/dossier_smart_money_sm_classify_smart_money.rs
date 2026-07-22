// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'smart_money' component (leaf 'sm_classify_smart_money').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_wallet_graph::smart_money::*;

#[test]
fn sm_classify_props() {
    let pass = PnlScreenResult {
        components: PnlComponents {
            total_realized: 700,
            top_removed_realized: 200,
            top_token_pnl: 500,
            token_count: 2,
            profitable_token_count: 2,
            self_dealt_token_count: 0,
            self_dealt_realized: 0,
        },
        reason: PnlFailReason::None,
    };
    let followable = LaggedShadowResult {
        wallet_net: 225_500,
        control_net: -130_900,
        wallet_actions_simulated: 1,
        wallet_actions_skipped: 0,
    };
    let unfollowable = LaggedShadowResult {
        wallet_net: -50_000,
        control_net: 0,
        wallet_actions_simulated: 1,
        wallet_actions_skipped: 0,
    };
    assert!(followable.is_followable());
    assert!(!unfollowable.is_followable());

    // Passing PnL + followable shadow + private -> SmartMoneyFollowable.
    assert_eq!(
        classify_smart_money(&pass, &followable, Legibility::Private, false),
        WalletQualityState::SmartMoneyFollowable
    );
    // Legibility escalation.
    assert_eq!(
        classify_smart_money(&pass, &followable, Legibility::PreLegibility, false),
        WalletQualityState::PreLegibilityCandidate
    );
    assert_eq!(
        classify_smart_money(&pass, &followable, Legibility::PublicBurned, false),
        WalletQualityState::PublicBurned
    );
    // Bait dominates even a passing, followable wallet.
    assert_eq!(
        classify_smart_money(&pass, &followable, Legibility::Private, true),
        WalletQualityState::CopyBaitSuspect
    );
    // Passing PnL but shadow not followable -> InsiderTimingNonreplicable.
    assert_eq!(
        classify_smart_money(&pass, &unfollowable, Legibility::Private, false),
        WalletQualityState::InsiderTimingNonreplicable
    );
    // PnL-failure reasons map through even with a failing shadow.
    let sd = PnlScreenResult {
        components: pass.components.clone(),
        reason: PnlFailReason::SelfDealing,
    };
    assert_eq!(
        classify_smart_money(&sd, &unfollowable, Legibility::Private, false),
        WalletQualityState::SelfDealingPnl
    );
    let lucky = PnlScreenResult {
        components: pass.components.clone(),
        reason: PnlFailReason::LuckyConcentrated,
    };
    assert_eq!(
        classify_smart_money(&lucky, &unfollowable, Legibility::Private, false),
        WalletQualityState::LuckyConcentratedPnl
    );
    let insufficient = PnlScreenResult {
        components: pass.components.clone(),
        reason: PnlFailReason::InsufficientSample,
    };
    assert_eq!(
        classify_smart_money(&insufficient, &unfollowable, Legibility::Private, false),
        WalletQualityState::InsufficientSample
    );
}
