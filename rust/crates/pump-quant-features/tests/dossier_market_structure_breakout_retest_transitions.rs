// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'market_structure' component (leaf 'breakout_retest_transitions').
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
fn breakout_retest_transitions() {
    let level = 100;
    // Never closes strictly above -> None.
    let none = [mk(90, 95, 85, 92), mk(92, 99, 90, 95)];
    assert_eq!(breakout_retest_state(&none, level), BreakoutState::None);
    // Closes above, no retest touch -> Broken.
    let broke = [mk(95, 105, 94, 104), mk(104, 110, 103, 108)];
    assert_eq!(breakout_retest_state(&broke, level), BreakoutState::Broken);
    // Break out then dip to level (low<=100) closing back above -> RetestHeld.
    let held = [mk(95, 106, 94, 104), mk(104, 105, 98, 101)];
    assert_eq!(
        breakout_retest_state(&held, level),
        BreakoutState::RetestHeld
    );
    // Break out then close below -> Failed.
    let failed = [mk(95, 106, 94, 104), mk(104, 105, 90, 95)];
    assert_eq!(breakout_retest_state(&failed, level), BreakoutState::Failed);
    // Failed dominates an earlier held retest.
    let held_then_fail = [
        mk(95, 106, 94, 104),
        mk(104, 105, 98, 101),
        mk(101, 102, 88, 90),
    ];
    assert_eq!(
        breakout_retest_state(&held_then_fail, level),
        BreakoutState::Failed
    );
    // Edge: empty action slice -> None (never armed).
    assert_eq!(breakout_retest_state(&[], level), BreakoutState::None);
}
