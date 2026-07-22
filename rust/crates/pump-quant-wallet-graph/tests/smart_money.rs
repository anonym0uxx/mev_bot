//! Leaf tests for Section 28 smart-money authentication: the family-netted
//! self-dealing-excluded luck-filtered PnL screen, the follower-executable
//! lagged-shadow simulator, and the combined classifier.
//!
//! All expected values are computed independently by hand from the trade lists,
//! prices, and config, across multiple inputs including edge cases.

use std::collections::HashMap;

use pump_quant_wallet_graph::smart_money::{
    classify_smart_money, lagged_shadow, simulate_action, LaggedShadowResult, Legibility,
    PnlComponents, PnlFailReason, PnlScreen, PnlScreenConfig, PnlScreenResult, PriceOracle,
    ShadowConfig, Trade, WalletAction, WalletQualityState,
};
use pump_quant_wallet_graph::{FamilyId, TokenId};

/// Deterministic in-memory price series behind the `PriceOracle` trait.
struct MapOracle {
    prices: HashMap<(u64, u64), u64>,
}
impl MapOracle {
    fn new() -> Self {
        Self {
            prices: HashMap::new(),
        }
    }
    fn set(&mut self, token: u64, slot: u64, price_scaled: u64) {
        self.prices.insert((token, slot), price_scaled);
    }
}
impl PriceOracle for MapOracle {
    fn price_scaled(&self, token: TokenId, slot: u64) -> Option<u64> {
        self.prices.get(&(token.0, slot)).copied()
    }
}

fn trade(token: u64, is_buy: bool, units: u64, sol: u64, self_dealt: bool) -> Trade {
    Trade {
        family: FamilyId(1),
        token: TokenId(token),
        is_buy,
        units,
        sol_lamports: sol,
        self_dealt,
        slot: 0,
    }
}

// ---------------------------------------------------------------------------
// PnL screen
// ---------------------------------------------------------------------------

#[test]
fn pnl_screen_passes_clean_two_token_profit() {
    // Token A: buy 100u/1000L, sell 100u/1500L -> pnl 500.
    // Token B: buy 50u/500L,  sell 50u/700L  -> pnl 200.
    let trades = [
        trade(1, true, 100, 1000, false),
        trade(1, false, 100, 1500, false),
        trade(2, true, 50, 500, false),
        trade(2, false, 50, 700, false),
    ];
    let s = PnlScreen::new(PnlScreenConfig { min_tokens: 2 });
    let r = s.evaluate(&trades);
    assert_eq!(r.reason, PnlFailReason::None);
    assert!(r.passed());
    assert_eq!(
        r.components,
        PnlComponents {
            total_realized: 700,
            top_removed_realized: 200,
            top_token_pnl: 500,
            token_count: 2,
            profitable_token_count: 2,
            self_dealt_token_count: 0,
            self_dealt_realized: 0,
        }
    );
}

#[test]
fn pnl_screen_flags_lucky_concentration() {
    // Token A pnl +500, Token B pnl -100. total 400, top 500,
    // top-removed = -100 <= 0 -> LuckyConcentrated.
    let trades = [
        trade(1, true, 100, 1000, false),
        trade(1, false, 100, 1500, false),
        trade(2, true, 50, 500, false),
        trade(2, false, 50, 400, false),
    ];
    let s = PnlScreen::new(PnlScreenConfig { min_tokens: 2 });
    let r = s.evaluate(&trades);
    assert_eq!(r.reason, PnlFailReason::LuckyConcentrated);
    assert_eq!(r.components.total_realized, 400);
    assert_eq!(r.components.top_removed_realized, -100);
}

#[test]
fn pnl_screen_excludes_and_flags_self_dealing() {
    // Self-dealt token A pnl +500 (excluded). Non-self-dealt B +200, C +100.
    let trades = [
        trade(1, true, 100, 1000, true),
        trade(1, false, 100, 1500, true),
        trade(2, true, 50, 500, false),
        trade(2, false, 50, 700, false),
        trade(3, true, 50, 500, false),
        trade(3, false, 50, 600, false),
    ];
    let s = PnlScreen::new(PnlScreenConfig { min_tokens: 2 });
    let r = s.evaluate(&trades);
    assert_eq!(r.reason, PnlFailReason::SelfDealing);
    assert_eq!(r.components.self_dealt_token_count, 1);
    assert_eq!(r.components.self_dealt_realized, 500);
    // Excluded from skill PnL: only B + C counted.
    assert_eq!(r.components.total_realized, 300);
    assert_eq!(r.components.token_count, 2);
}

#[test]
fn pnl_screen_insufficient_sample() {
    // Only one realized token but min_tokens = 2.
    let trades = [
        trade(1, true, 100, 1000, false),
        trade(1, false, 100, 1500, false),
    ];
    let s = PnlScreen::new(PnlScreenConfig { min_tokens: 2 });
    let r = s.evaluate(&trades);
    assert_eq!(r.reason, PnlFailReason::InsufficientSample);
    assert_eq!(r.components.token_count, 1);
}

#[test]
fn pnl_screen_non_positive_realized() {
    // Two losing tokens: A -100, B -50. total -150.
    let trades = [
        trade(1, true, 100, 1000, false),
        trade(1, false, 100, 900, false),
        trade(2, true, 50, 500, false),
        trade(2, false, 50, 450, false),
    ];
    let s = PnlScreen::new(PnlScreenConfig { min_tokens: 2 });
    let r = s.evaluate(&trades);
    assert_eq!(r.reason, PnlFailReason::NonPositiveRealized);
    assert_eq!(r.components.total_realized, -150);
}

#[test]
fn pnl_screen_partial_realization_uses_prorated_cost_basis() {
    // Token A: buy 100u/1000L, sell 60u/900L.
    //   realized_units = 60. cost_of_sold = 1000*60/100 = 600.
    //   proceeds_of_sold = 900*60/60 = 900. pnl = 300.
    // Token B: buy 40u/400L, sell 40u/500L -> pnl 100.
    let trades = [
        trade(1, true, 100, 1000, false),
        trade(1, false, 60, 900, false),
        trade(2, true, 40, 400, false),
        trade(2, false, 40, 500, false),
    ];
    let s = PnlScreen::new(PnlScreenConfig { min_tokens: 2 });
    let r = s.evaluate(&trades);
    assert_eq!(r.reason, PnlFailReason::None);
    assert_eq!(r.components.total_realized, 400);
    assert_eq!(r.components.top_token_pnl, 300);
}

#[test]
fn pnl_screen_open_position_not_realized() {
    // A token only bought, never sold, contributes no realized PnL.
    let trades = [
        trade(1, true, 100, 1000, false),
        // realized token B to satisfy... but min_tokens=1 for this edge test.
    ];
    let s = PnlScreen::new(PnlScreenConfig { min_tokens: 1 });
    let r = s.evaluate(&trades);
    assert_eq!(r.components.token_count, 0);
    assert_eq!(r.reason, PnlFailReason::InsufficientSample);
}

// ---------------------------------------------------------------------------
// Lagged-shadow simulator
// ---------------------------------------------------------------------------

fn shadow_cfg() -> ShadowConfig {
    ShadowConfig {
        latency_slots: 2,
        horizon_slots: 5,
        tp_bps: 2000, // +20%
        sl_bps: 1000, // -10%
        size_lamports: 1_000_000,
        fee_bps: 100, // 1%
        tip_lamports: 1000,
    }
}

#[test]
fn simulate_action_take_profit_hit() {
    // Action slot 10 -> entry slot 12. Entry price_scaled 100_000_000
    //   => lamports/unit = 100. units = 1e6 * 1e6 / 1e8 = 10_000.
    // tp_price = 100_000_000 * 12000/10000 = 120_000_000.
    // slot13=110M, slot14=115M, slot15=125M (>=TP) -> exit 125M.
    // proceeds = 10_000 * 125_000_000 / 1_000_000 = 1_250_000.
    // entry_fee = 1_000_000*1% = 10_000. exit_fee = 1_250_000*1% = 12_500.
    // tips = 2*1000 = 2000.
    // net = (1_250_000-1_000_000) - 10_000 - 12_500 - 2000 = 225_500.
    let mut o = MapOracle::new();
    o.set(1, 12, 100_000_000);
    o.set(1, 13, 110_000_000);
    o.set(1, 14, 115_000_000);
    o.set(1, 15, 125_000_000);
    let net = simulate_action(
        &o,
        WalletAction {
            token: TokenId(1),
            action_slot: 10,
        },
        &shadow_cfg(),
    );
    assert_eq!(net, Some(225_500));
}

#[test]
fn simulate_action_stop_loss_hit() {
    // Entry 100M, slot13=95M, slot14=89M (<= SL 90M) -> exit 89M.
    // proceeds = 10_000 * 89_000_000/1_000_000 = 890_000.
    // net = (890_000-1_000_000) - 10_000 - (890_000*1%=8_900) - 2000 = -130_900.
    let mut o = MapOracle::new();
    o.set(2, 12, 100_000_000);
    o.set(2, 13, 95_000_000);
    o.set(2, 14, 89_000_000);
    let net = simulate_action(
        &o,
        WalletAction {
            token: TokenId(2),
            action_slot: 10,
        },
        &shadow_cfg(),
    );
    assert_eq!(net, Some(-130_900));
}

#[test]
fn simulate_action_horizon_exit_last_price() {
    // No TP/SL touch; exit at last observed price within horizon.
    // Entry 100M. Prices 105M..108M, last observed at slot17 = 108M.
    // proceeds = 10_000 * 108_000_000/1_000_000 = 1_080_000.
    // net = (1_080_000-1_000_000) -10_000 -(1_080_000*1%=10_800) -2000 = 57_200.
    let mut o = MapOracle::new();
    o.set(3, 12, 100_000_000);
    o.set(3, 13, 105_000_000);
    o.set(3, 14, 106_000_000);
    o.set(3, 15, 107_000_000);
    o.set(3, 16, 107_500_000);
    o.set(3, 17, 108_000_000);
    let net = simulate_action(
        &o,
        WalletAction {
            token: TokenId(3),
            action_slot: 10,
        },
        &shadow_cfg(),
    );
    assert_eq!(net, Some(57_200));
}

#[test]
fn simulate_action_skips_when_no_entry_price() {
    let o = MapOracle::new(); // empty
    let net = simulate_action(
        &o,
        WalletAction {
            token: TokenId(9),
            action_slot: 10,
        },
        &shadow_cfg(),
    );
    assert_eq!(net, None);
}

#[test]
fn simulate_action_skips_when_no_exit_price() {
    // Entry price present, but no price anywhere in the horizon window.
    let mut o = MapOracle::new();
    o.set(4, 12, 100_000_000);
    let net = simulate_action(
        &o,
        WalletAction {
            token: TokenId(4),
            action_slot: 10,
        },
        &shadow_cfg(),
    );
    assert_eq!(net, None);
}

#[test]
fn lagged_shadow_wallet_beats_matched_control() {
    // Wallet action = TP winner (+225_500). Control action = SL loser (-130_900).
    let mut o = MapOracle::new();
    o.set(1, 12, 100_000_000);
    o.set(1, 13, 110_000_000);
    o.set(1, 14, 115_000_000);
    o.set(1, 15, 125_000_000);
    o.set(2, 12, 100_000_000);
    o.set(2, 13, 95_000_000);
    o.set(2, 14, 89_000_000);
    let wallet = [WalletAction {
        token: TokenId(1),
        action_slot: 10,
    }];
    let control = [WalletAction {
        token: TokenId(2),
        action_slot: 10,
    }];
    let res = lagged_shadow(&o, &wallet, &control, &shadow_cfg());
    assert_eq!(res.wallet_net, 225_500);
    assert_eq!(res.control_net, -130_900);
    assert_eq!(res.wallet_actions_simulated, 1);
    assert_eq!(res.wallet_actions_skipped, 0);
    assert!(res.is_followable());
}

#[test]
fn lagged_shadow_not_followable_when_control_matches() {
    // Both wallet and control take the same winning setup -> wallet_net equals
    // control_net -> not strictly better -> not followable.
    let mut o = MapOracle::new();
    for tok in [1u64, 2u64] {
        o.set(tok, 12, 100_000_000);
        o.set(tok, 13, 110_000_000);
        o.set(tok, 14, 115_000_000);
        o.set(tok, 15, 125_000_000);
    }
    let wallet = [WalletAction {
        token: TokenId(1),
        action_slot: 10,
    }];
    let control = [WalletAction {
        token: TokenId(2),
        action_slot: 10,
    }];
    let res = lagged_shadow(&o, &wallet, &control, &shadow_cfg());
    assert_eq!(res.wallet_net, res.control_net);
    assert!(!res.is_followable());
}

// ---------------------------------------------------------------------------
// Combined classifier
// ---------------------------------------------------------------------------

fn passing_pnl() -> PnlScreenResult {
    PnlScreenResult {
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
    }
}
fn followable_shadow() -> LaggedShadowResult {
    LaggedShadowResult {
        wallet_net: 225_500,
        control_net: -130_900,
        wallet_actions_simulated: 1,
        wallet_actions_skipped: 0,
    }
}
fn unfollowable_shadow() -> LaggedShadowResult {
    LaggedShadowResult {
        wallet_net: -50_000,
        control_net: 0,
        wallet_actions_simulated: 1,
        wallet_actions_skipped: 0,
    }
}

#[test]
fn classify_smart_money_followable_private() {
    let st = classify_smart_money(
        &passing_pnl(),
        &followable_shadow(),
        Legibility::Private,
        false,
    );
    assert_eq!(st, WalletQualityState::SmartMoneyFollowable);
}

#[test]
fn classify_smart_money_pre_legibility_and_public_burned() {
    assert_eq!(
        classify_smart_money(
            &passing_pnl(),
            &followable_shadow(),
            Legibility::PreLegibility,
            false
        ),
        WalletQualityState::PreLegibilityCandidate
    );
    assert_eq!(
        classify_smart_money(
            &passing_pnl(),
            &followable_shadow(),
            Legibility::PublicBurned,
            false
        ),
        WalletQualityState::PublicBurned
    );
}

#[test]
fn classify_smart_money_bait_dominates_everything() {
    // Even a passing PnL + followable shadow is CopyBaitSuspect when the bait
    // screen fires.
    let st = classify_smart_money(
        &passing_pnl(),
        &followable_shadow(),
        Legibility::Private,
        true,
    );
    assert_eq!(st, WalletQualityState::CopyBaitSuspect);
}

#[test]
fn classify_smart_money_insider_timing_when_shadow_fails() {
    let st = classify_smart_money(
        &passing_pnl(),
        &unfollowable_shadow(),
        Legibility::Private,
        false,
    );
    assert_eq!(st, WalletQualityState::InsiderTimingNonreplicable);
}

#[test]
fn classify_smart_money_maps_pnl_failure_reasons() {
    let bad_shadow = unfollowable_shadow();
    let self_dealing = PnlScreenResult {
        components: passing_pnl().components,
        reason: PnlFailReason::SelfDealing,
    };
    assert_eq!(
        classify_smart_money(&self_dealing, &bad_shadow, Legibility::Private, false),
        WalletQualityState::SelfDealingPnl
    );
    let lucky = PnlScreenResult {
        components: passing_pnl().components,
        reason: PnlFailReason::LuckyConcentrated,
    };
    assert_eq!(
        classify_smart_money(&lucky, &bad_shadow, Legibility::Private, false),
        WalletQualityState::LuckyConcentratedPnl
    );
    let insufficient = PnlScreenResult {
        components: passing_pnl().components,
        reason: PnlFailReason::InsufficientSample,
    };
    assert_eq!(
        classify_smart_money(&insufficient, &bad_shadow, Legibility::Private, false),
        WalletQualityState::InsufficientSample
    );
}
