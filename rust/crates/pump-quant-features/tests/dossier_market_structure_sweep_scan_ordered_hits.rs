// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'market_structure' component (leaf 'sweep_scan_ordered_hits').
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
fn sweep_scan_ordered_hits() {
    let sup = 100;
    let res = 200;
    let action = [
        mk(101, 105, 95, 103),  // idx0 bullish sweep of support (low<sup, close>sup)
        mk(150, 160, 145, 155), // idx1 nothing
        mk(198, 210, 195, 197), // idx2 bearish sweep of resistance (high>res, close<res)
    ];
    assert_eq!(
        sweep_scan(&action, sup, res),
        vec![
            (0, SweepKind::SweptLowReclaimed),
            (2, SweepKind::SweptHighRejected),
        ]
    );
    // Single-bar predicates agree with the scan.
    assert!(is_bullish_sweep(&action[0], sup));
    assert!(!is_bearish_sweep(&action[0], res));
    assert!(is_bearish_sweep(&action[2], res));
    assert!(!is_bullish_sweep(&action[2], sup));

    // Output indices are non-decreasing (ascending scan order).
    let hits = sweep_scan(&action, sup, res);
    for w in hits.windows(2) {
        assert!(w[0].0 <= w[1].0);
    }

    // Edge: a bar touching but not piercing the level is not a sweep -> empty.
    let calm = [mk(100, 200, 100, 150)];
    assert!(sweep_scan(&calm, sup, res).is_empty());
    assert!(!is_bullish_sweep(&calm[0], sup));
    assert!(!is_bearish_sweep(&calm[0], res));
}
