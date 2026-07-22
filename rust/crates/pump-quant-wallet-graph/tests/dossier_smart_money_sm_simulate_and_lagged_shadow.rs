// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'smart_money' component (leaf 'sm_simulate_and_lagged_shadow').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_wallet_graph::smart_money::*;

#[test]
fn sm_simulate_action_props() {
    use std::collections::HashMap;
    struct O {
        p: HashMap<(u64, u64), u64>,
    }
    impl PriceOracle for O {
        fn price_scaled(&self, token: pump_quant_wallet_graph::TokenId, slot: u64) -> Option<u64> {
            self.p.get(&(token.0, slot)).copied()
        }
    }
    let cfg = ShadowConfig {
        latency_slots: 2,
        horizon_slots: 5,
        tp_bps: 2000,
        sl_bps: 1000,
        size_lamports: 1_000_000,
        fee_bps: 100,
        tip_lamports: 1000,
    };
    let act = |t: u64| WalletAction {
        token: pump_quant_wallet_graph::TokenId(t),
        action_slot: 10,
    };

    // Take-profit touch: entry@100M, exit@125M -> net +225_500.
    let mut m = HashMap::new();
    m.insert((1, 12), 100_000_000u64);
    m.insert((1, 13), 110_000_000u64);
    m.insert((1, 14), 115_000_000u64);
    m.insert((1, 15), 125_000_000u64);
    assert_eq!(simulate_action(&O { p: m }, act(1), &cfg), Some(225_500));

    // Stop-loss touch: entry@100M, exit@89M -> net -130_900.
    let mut m3 = HashMap::new();
    m3.insert((2, 12), 100_000_000u64);
    m3.insert((2, 13), 95_000_000u64);
    m3.insert((2, 14), 89_000_000u64);
    assert_eq!(simulate_action(&O { p: m3 }, act(2), &cfg), Some(-130_900));

    // No entry price -> unshadowable.
    assert_eq!(
        simulate_action(&O { p: HashMap::new() }, act(9), &cfg),
        None
    );

    // Entry price present but no exit anywhere in the horizon -> None.
    let mut m4 = HashMap::new();
    m4.insert((4, 12), 100_000_000u64);
    assert_eq!(simulate_action(&O { p: m4 }, act(4), &cfg), None);
}

#[test]
fn sm_lagged_shadow_aggregates_and_followable() {
    use std::collections::HashMap;
    struct O {
        p: HashMap<(u64, u64), u64>,
    }
    impl PriceOracle for O {
        fn price_scaled(&self, token: pump_quant_wallet_graph::TokenId, slot: u64) -> Option<u64> {
            self.p.get(&(token.0, slot)).copied()
        }
    }
    let cfg = ShadowConfig {
        latency_slots: 2,
        horizon_slots: 5,
        tp_bps: 2000,
        sl_bps: 1000,
        size_lamports: 1_000_000,
        fee_bps: 100,
        tip_lamports: 1000,
    };
    let mut m = HashMap::new();
    // token 1: TP winner (+225_500)
    m.insert((1, 12), 100_000_000u64);
    m.insert((1, 13), 110_000_000u64);
    m.insert((1, 14), 115_000_000u64);
    m.insert((1, 15), 125_000_000u64);
    // token 2: SL loser (-130_900)
    m.insert((2, 12), 100_000_000u64);
    m.insert((2, 13), 95_000_000u64);
    m.insert((2, 14), 89_000_000u64);
    let o = O { p: m };
    let wallet = [WalletAction {
        token: pump_quant_wallet_graph::TokenId(1),
        action_slot: 10,
    }];
    let control = [WalletAction {
        token: pump_quant_wallet_graph::TokenId(2),
        action_slot: 10,
    }];
    let res = lagged_shadow(&o, &wallet, &control, &cfg);
    assert_eq!(res.wallet_net, 225_500);
    assert_eq!(res.control_net, -130_900);
    assert_eq!(res.wallet_actions_simulated, 1);
    assert_eq!(res.wallet_actions_skipped, 0);
    assert!(res.is_followable());
}
