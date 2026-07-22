// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'active_market_universe' component (leaf 'progressive_filter_band').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_signals::active_market_universe::*;

#[test]
fn progressive_filter_band() {
    fn screen() -> ScreenCriteria {
        ScreenCriteria {
            min_liquidity_lamports: 10_000_000_000,
            min_volume_lamports: 5_000_000_000,
            min_swap_count: 20,
            min_unique_traders: 10,
            max_spread_bps: 300,
            max_concentration_bps: 5_000,
            min_age_ms: 60_000,
            max_age_ms: 86_400_000,
        }
    }
    fn obs(id: u64) -> MarketObservation {
        MarketObservation {
            token_id: id,
            liquidity_lamports: 50_000_000_000,
            volume_lamports_window: 25_000_000_000,
            swap_count_window: 250,
            unique_traders_window: 50,
            age_ms: 600_000,
            spread_bps: 100,
            top_holder_concentration_bps: 2_000,
        }
    }

    let c = screen();

    assert!(passes_progressive_filter(&obs(1), &c));

    let mut edge = obs(1);
    edge.spread_bps = c.max_spread_bps;
    edge.top_holder_concentration_bps = c.max_concentration_bps;
    edge.age_ms = c.min_age_ms;
    assert!(passes_progressive_filter(&edge, &c));
    let mut edge_hi = obs(1);
    edge_hi.age_ms = c.max_age_ms;
    assert!(passes_progressive_filter(&edge_hi, &c));

    let mut wide = obs(1);
    wide.spread_bps = c.max_spread_bps + 1;
    assert!(!passes_progressive_filter(&wide, &c));
    let mut conc = obs(1);
    conc.top_holder_concentration_bps = c.max_concentration_bps + 1;
    assert!(!passes_progressive_filter(&conc, &c));

    let mut young = obs(1);
    young.age_ms = c.min_age_ms - 1;
    assert!(!passes_progressive_filter(&young, &c));
    let mut stale = obs(1);
    stale.age_ms = c.max_age_ms + 1;
    assert!(!passes_progressive_filter(&stale, &c));
}
