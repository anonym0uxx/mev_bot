// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'micro' component (leaf 'mc_flow_accounting').
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
use pump_quant_features::micro::*;

fn te(
    id: u64,
    ts: u64,
    price: i128,
    base: u64,
    quote: u64,
    buy: bool,
) -> pump_quant_features::types::TradeEvent {
    pump_quant_features::types::TradeEvent {
        event_id: id,
        ts_ns: ts,
        price_fp: to_price_fp(price),
        base_qty: base,
        quote_qty: quote,
        side: if buy {
            pump_quant_features::types::Side::Buy
        } else {
            pump_quant_features::types::Side::Sell
        },
    }
}

#[test]
fn mc_flow_accounting_props() {
    let s = [
        te(1, 1, 10, 10, 100, true),
        te(2, 2, 11, 4, 44, false),
        te(3, 3, 10, 6, 60, true),
    ];
    // CVD = +100 - 44 + 60 = 116.
    assert_eq!(cumulative_volume_delta(&s), 116);
    // OFI base = +10 - 4 + 6 = 12.
    assert_eq!(order_flow_imbalance_base(&s), 12);
    // buy base 16, sell base 4, net 12, total 20 -> 6000 bps.
    assert_eq!(order_flow_imbalance_bps(&s), Some(6000));

    // Empty slice: CVD/OFI are 0; bps undefined -> None.
    assert_eq!(cumulative_volume_delta(&[]), 0);
    assert_eq!(order_flow_imbalance_base(&[]), 0);
    assert_eq!(order_flow_imbalance_bps(&[]), None);

    // All-buy: bps saturates at +10_000; all-sell: -10_000.
    let all_buy = [te(1, 1, 10, 5, 50, true), te(2, 2, 10, 3, 30, true)];
    assert_eq!(order_flow_imbalance_bps(&all_buy), Some(10_000));
    let all_sell = [te(1, 1, 10, 5, 50, false), te(2, 2, 10, 3, 30, false)];
    assert_eq!(order_flow_imbalance_bps(&all_sell), Some(-10_000));

    // Perfectly balanced base volume -> 0 bps (net zero, total nonzero).
    let balanced = [te(1, 1, 10, 7, 70, true), te(2, 2, 10, 7, 70, false)];
    assert_eq!(order_flow_imbalance_bps(&balanced), Some(0));

    // bps is always within [-10_000, 10_000] and sign matches CVD's base net.
    assert!(order_flow_imbalance_bps(&s).unwrap().abs() <= 10_000);
}
