// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'active_market_universe' component (leaf 'analyze_candidate_composite').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_signals::active_market_universe::*;

#[test]
fn analyze_candidate_composite() {
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

    let a = analysis();

    assert_eq!(analyze_candidate(&obs(1), &a), 5_790);

    assert_eq!(
        analyze_candidate(&obs(1), &a),
        analyze_candidate(&obs(2), &a)
    );

    let mut best = obs(1);
    best.liquidity_lamports = u128::MAX;
    best.volume_lamports_window = u128::MAX;
    best.unique_traders_window = u32::MAX;
    best.swap_count_window = u32::MAX;
    best.spread_bps = 0;
    best.top_holder_concentration_bps = 0;
    assert_eq!(analyze_candidate(&best, &a), 10_000);

    let mut low = obs(1);
    low.liquidity_lamports = 0;
    low.volume_lamports_window = 0;
    low.unique_traders_window = 0;
    low.swap_count_window = 0;
    low.spread_bps = 0;
    low.top_holder_concentration_bps = 0;
    let low_score = analyze_candidate(&low, &a);
    assert_eq!(low_score, 2_000);
    let mut more_liq = low;
    more_liq.liquidity_lamports = 100_000_000_000;
    assert!(analyze_candidate(&more_liq, &a) >= low_score);
}
