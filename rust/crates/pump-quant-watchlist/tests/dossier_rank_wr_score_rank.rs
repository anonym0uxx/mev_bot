// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'rank' component (leaf 'wr_score_rank').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_watchlist::rank::*;

fn mk_cand(
    score: u64,
    discovered_at: u64,
    lane: pump_quant_watchlist::candidate::Lane,
) -> pump_quant_watchlist::candidate::Candidate {
    pump_quant_watchlist::candidate::Candidate::new(
        pump_quant_watchlist::candidate::Mint::new([0u8; 32]),
        lane,
        score,
        discovered_at,
        pump_quant_watchlist::candidate::Features::default(),
    )
}

#[test]
fn wr_score_rank_props() {
    let weights = LaneWeights::from_defaults();
    let params = RankParams::new(1000);

    // ActiveMarketScalp = 1.0x. age 250 => recency 750_000.
    // 1_000_000 * 750_000 / 1e6 * 10_000 / 10_000 = 750_000.
    let c = mk_cand(
        1_000_000,
        0,
        pump_quant_watchlist::candidate::Lane::ActiveMarketScalp,
    );
    assert_eq!(score_rank(&c, 250, params, &weights), 750_000);

    // EarlyConfirmation = 1.2x, fresh (recency 1e6): 1_000_000 * 12_000 / 10_000 = 1_200_000.
    let c2 = mk_cand(
        1_000_000,
        0,
        pump_quant_watchlist::candidate::Lane::EarlyConfirmation,
    );
    assert_eq!(score_rank(&c2, 0, params, &weights), 1_200_000);

    // CreationSniper = 0.8x, half decayed. score 2_000_000, age 500 => recency 500_000.
    // 2_000_000 * 500_000 / 1e6 = 1_000_000; * 8_000 / 10_000 = 800_000.
    let c3 = mk_cand(
        2_000_000,
        0,
        pump_quant_watchlist::candidate::Lane::CreationSniper,
    );
    assert_eq!(score_rank(&c3, 500, params, &weights), 800_000);

    // Decayed to 0 => rank 0 regardless of score.
    let params2 = RankParams::new(100);
    let c4 = mk_cand(
        9_999_999,
        0,
        pump_quant_watchlist::candidate::Lane::EarlyConfirmation,
    );
    assert_eq!(score_rank(&c4, 100, params2, &weights), 0);
    assert_eq!(score_rank(&c4, 5000, params2, &weights), 0);

    // Saturates into u64::MAX without wrapping.
    let mut w2 = LaneWeights::from_defaults();
    w2.set(
        pump_quant_watchlist::candidate::Lane::ActiveMarketScalp,
        60_000,
    ); // 6x
    let params3 = RankParams::new(10);
    let big = mk_cand(
        u64::MAX,
        0,
        pump_quant_watchlist::candidate::Lane::ActiveMarketScalp,
    );
    assert_eq!(score_rank(&big, 0, params3, &w2), u64::MAX);
}
