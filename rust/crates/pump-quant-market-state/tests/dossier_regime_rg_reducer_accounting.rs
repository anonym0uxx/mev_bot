// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'regime' component (leaf 'rg_reducer_accounting').
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
use pump_quant_market_state::regime::*;

#[test]
fn rg_reducer_accounting() {
    let mut r = MarketRegimeReducer::new();
    for _ in 0..10 {
        r.ingest(&MarketEvent::Launch);
    }
    for _ in 0..3 {
        r.ingest(&MarketEvent::Graduation);
    }
    for _ in 0..7 {
        r.ingest(&MarketEvent::Buy);
    }
    for _ in 0..3 {
        r.ingest(&MarketEvent::Sell);
    }
    for _ in 0..2 {
        r.ingest(&MarketEvent::Rug);
    }
    r.ingest(&MarketEvent::RouteAttempt { succeeded: true });
    r.ingest(&MarketEvent::RouteAttempt { succeeded: false });
    r.ingest(&MarketEvent::RouteAttempt { succeeded: true });
    r.ingest(&MarketEvent::RouteAttempt { succeeded: false });
    r.ingest(&MarketEvent::RouteAttempt { succeeded: true });
    r.set_live_markets(100);

    let obs = r.observation();
    assert_eq!(obs.launches, 10);
    assert_eq!(obs.graduations, 3);
    assert_eq!(obs.buys, 7);
    assert_eq!(obs.sells, 3);
    assert_eq!(obs.rugs, 2);
    assert_eq!(obs.route_attempts, 5);
    assert_eq!(obs.route_failures, 2);
    assert_eq!(obs.live_markets, Some(100));
    assert_eq!(obs.sol_price_change_bps, None);
    assert_eq!(obs.median_priority_fee, None);
    assert_eq!(obs.slot_fullness_bps, None);
    assert_eq!(obs.liquidity_index, None);

    let th = RegimeThresholds::default();
    assert_eq!(r.classify(&th), classify(&obs, &th));
    assert_eq!(r.classify(&th).launch_velocity, RegimeLevel::Normal);
}
