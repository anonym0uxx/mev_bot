// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'market_structure' component (leaf 'range_state_ratio_classification').
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
fn range_state_ratio_classification() {
    // Baseline range 100 each, recent 20 each -> 20% -> compression.
    let comp = vec![
        mk(0, 100, 0, 50),
        mk(0, 100, 0, 50),
        mk(0, 20, 0, 10),
        mk(0, 20, 0, 10),
    ];
    assert_eq!(
        range_state(&comp, 2, 2, 6_000, 15_000),
        Some(RangeState::Compression)
    );

    // Recent 200 vs baseline 100 -> 200% -> expansion.
    let exp = vec![
        mk(0, 100, 0, 50),
        mk(0, 100, 0, 50),
        mk(0, 200, 0, 100),
        mk(0, 200, 0, 100),
    ];
    assert_eq!(
        range_state(&exp, 2, 2, 6_000, 15_000),
        Some(RangeState::Expansion)
    );

    // Recent == baseline -> neutral.
    let neu = vec![mk(0, 100, 0, 50); 4];
    assert_eq!(
        range_state(&neu, 2, 2, 6_000, 15_000),
        Some(RangeState::Neutral)
    );

    // Boundary: recent 60% of baseline == contraction_bps 6000 -> inclusive compression.
    let boundary = vec![
        mk(0, 100, 0, 50),
        mk(0, 100, 0, 50),
        mk(0, 60, 0, 30),
        mk(0, 60, 0, 30),
    ];
    assert_eq!(
        range_state(&boundary, 2, 2, 6_000, 15_000),
        Some(RangeState::Compression)
    );

    // Rejection: fewer than recent+baseline bars.
    assert_eq!(range_state(&comp, 3, 2, 6_000, 15_000), None);
    // Rejection: zero window length.
    assert_eq!(range_state(&neu, 0, 2, 6_000, 15_000), None);
    // Rejection: zero baseline range -> ratio undefined.
    let flat = vec![
        mk(50, 50, 50, 50),
        mk(50, 50, 50, 50),
        mk(0, 20, 0, 10),
        mk(0, 20, 0, 10),
    ];
    assert_eq!(range_state(&flat, 2, 2, 6_000, 15_000), None);
}
