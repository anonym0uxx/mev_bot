// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'active_market_universe' component (leaf 'select_pipeline').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_signals::active_market_universe::*;

#[test]
fn select_pipeline() {
    fn analysis() -> AnalysisConfig {
        AnalysisConfig {
            liquidity_ref_lamports: 100_000_000_000,
            volume_ref_lamports: 50_000_000_000,
            traders_ref: 100,
            swaps_ref: 500,
            w_liquidity_bps: 2_500,
            w_volume_bps: 2_500,
            w_breadth_bps: 2_000,
            w_activity_bps: 1_000,
            w_spread_bps: 1_000,
            w_concentration_bps: 1_000,
        }
    }
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
    fn cfg() -> UniverseConfig {
        UniverseConfig {
            screen: screen(),
            analysis: analysis(),
            min_priority_score: 1,
            capacity: 10,
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

    let mut strong = obs(7);
    strong.liquidity_lamports = 100_000_000_000;
    strong.volume_lamports_window = 50_000_000_000;
    strong.unique_traders_window = 100;
    strong.swap_count_window = 500;
    strong.spread_bps = 0;
    strong.top_holder_concentration_bps = 0;
    let mid = obs(3);
    let mut weak = obs(9);
    weak.liquidity_lamports = 12_000_000_000;
    weak.volume_lamports_window = 6_000_000_000;
    weak.unique_traders_window = 12;
    weak.swap_count_window = 25;

    let out = select_active_market_universe(&[mid, weak, strong], &cfg());

    assert_eq!(out.len(), 3);
    assert_eq!(out[0].token_id, 7);
    for (i, c) in out.iter().enumerate() {
        assert_eq!(c.rank, i as u32);
        assert_eq!(
            c.discovery_source,
            DiscoverySource::ActiveMarketQualification
        );
    }
    for w in out.windows(2) {
        assert!(w[0].priority_score >= w[1].priority_score);
    }

    let mut bad = obs(1);
    bad.liquidity_lamports = 100;
    let good = select_active_market_universe(&[bad, obs(2)], &cfg());
    assert_eq!(good.len(), 1);
    assert_eq!(good[0].token_id, 2);

    let mut floored = cfg();
    floored.min_priority_score = 100_000;
    assert!(select_active_market_universe(&[obs(1), obs(2)], &floored).is_empty());

    let mut capped = cfg();
    capped.capacity = 2;
    let many: Vec<MarketObservation> = (1..=5).map(obs).collect();
    let bounded = select_active_market_universe(&many, &capped);
    assert_eq!(bounded.len(), 2);
    assert_eq!(bounded[0].rank, 0);
    assert_eq!(bounded[1].rank, 1);
}
