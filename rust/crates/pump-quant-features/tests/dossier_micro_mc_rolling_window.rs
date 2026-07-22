// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'micro' component (leaf 'mc_rolling_window').
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
fn mc_rolling_window_props() {
    // Zero window or zero capacity is rejected.
    assert_eq!(
        RollingFlowWindow::new(0, 10).unwrap_err(),
        pump_quant_features::types::FeatureError::InvalidConfiguration
    );
    assert_eq!(
        RollingFlowWindow::new(100, 0).unwrap_err(),
        pump_quant_features::types::FeatureError::InvalidConfiguration
    );

    // Time eviction: pushing at ts=100 with window 100 evicts everything ts<=0.
    let mut w = RollingFlowWindow::new(100, 10).unwrap();
    w.push(te(1, 0, 10, 10, 100, true)).unwrap();
    w.push(te(2, 50, 11, 4, 44, false)).unwrap();
    w.push(te(3, 100, 10, 6, 60, true)).unwrap();
    assert_eq!(w.len(), 2);
    // Retained {sell 44, buy 60}: cvd = 16, ofi_base = 6-4 = 2.
    assert_eq!(w.cvd(), 16);
    assert_eq!(w.ofi_base(), 2);
    assert_eq!(w.buy_base(), 6);
    assert_eq!(w.sell_base(), 4);
    assert_eq!(w.ofi_bps(), Some(2000));
    assert!(!w.is_empty());

    // Non-monotonic timestamp is rejected and leaves state unchanged.
    let mut w2 = RollingFlowWindow::new(100, 10).unwrap();
    w2.push(te(1, 10, 10, 1, 10, true)).unwrap();
    let err = w2.push(te(2, 9, 10, 1, 10, true)).unwrap_err();
    assert_eq!(
        err,
        pump_quant_features::types::FeatureError::NonMonotonicTimestamp {
            previous_ns: 10,
            offending_ns: 9,
        }
    );
    assert_eq!(w2.len(), 1);
    assert_eq!(w2.cvd(), 10);

    // Capacity eviction: hard cap 2 drops the oldest even when in-window.
    let mut w3 = RollingFlowWindow::new(1_000_000, 2).unwrap();
    w3.push(te(1, 1, 10, 10, 100, true)).unwrap();
    w3.push(te(2, 2, 10, 5, 50, false)).unwrap();
    w3.push(te(3, 3, 10, 7, 70, true)).unwrap();
    assert_eq!(w3.len(), 2);
    // Retained {sell 50, buy 70}: cvd = 20.
    assert_eq!(w3.cvd(), 20);

    // Empty window has zero aggregates.
    let w4 = RollingFlowWindow::new(100, 10).unwrap();
    assert!(w4.is_empty());
    assert_eq!(w4.len(), 0);
    assert_eq!(w4.cvd(), 0);
    assert_eq!(w4.ofi_bps(), None);
}
