// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'market_structure' component (leaf 'bar_range_fp_equals_high_minus_low').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_features::market_structure::*;

fn mk(open: i128, high: i128, low: i128, close: i128) -> pump_quant_features::bar::Bar {
    pump_quant_features::bar::Bar {
        open_time_ns: 0,
        close_time_ns: 0,
        open_fp: open,
        high_fp: high,
        low_fp: low,
        close_fp: close,
        base_volume: 1,
        quote_volume: 1,
        buy_base_volume: 1,
        sell_base_volume: 0,
        trade_count: 1,
        first_event_id: 0,
        last_event_id: 0,
    }
}

#[test]
fn bar_range_fp_equals_high_minus_low() {
    let b = mk(100, 130, 90, 110);
    assert_eq!(bar_range_fp(&b), 40);
    // Edge: zero-width bar.
    assert_eq!(bar_range_fp(&mk(50, 50, 50, 50)), 0);
    // Property: for high >= low the range equals high-low and is non-negative,
    // across a signed grid including negative fixed-point prices.
    let mut h = -20i128;
    while h <= 20 {
        let mut w = 0i128;
        while w <= 40 {
            let r = bar_range_fp(&mk(0, h, h - w, 0));
            assert_eq!(r, w);
            assert!(r >= 0);
            w += 1;
        }
        h += 1;
    }
}
