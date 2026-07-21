//! `wl_lane_ingest` leaf tests: union intake + strongest-evidence dedup,
//! order-independent and deterministic.

use pump_quant_watchlist::candidate::{Candidate, Features, Lane, Mint};
use pump_quant_watchlist::lane_ingest::ingest_union;
use pump_quant_watchlist::rank::LaneWeights;

fn mint(tag: u8) -> Mint {
    let mut b = [0u8; 32];
    b[0] = tag;
    Mint::new(b)
}

fn c(tag: u8, lane: Lane, score: u64, at: u64) -> Candidate {
    Candidate::new(mint(tag), lane, score, at, Features::default())
}

#[test]
fn distinct_mints_all_survive() {
    let w = LaneWeights::from_defaults();
    let out = ingest_union(
        [
            c(1, Lane::CreationSniper, 100, 0),
            c(2, Lane::EarlyConfirmation, 100, 0),
            c(3, Lane::ActiveMarketScalp, 100, 0),
        ],
        &w,
    );
    assert_eq!(out.len(), 3);
    assert!(out.contains_key(&mint(1)));
    assert!(out.contains_key(&mint(2)));
    assert!(out.contains_key(&mint(3)));
}

#[test]
fn dedup_keeps_higher_evidence_strength() {
    let w = LaneWeights::from_defaults();
    // Same mint, two lanes.
    // A: CreationSniper score 2000 => strength 2000*8000 = 16_000_000.
    // B: ActiveMarketScalp score 1500 => strength 1500*10000 = 15_000_000.
    // A wins on strength despite the weaker lane weight.
    let a = c(7, Lane::CreationSniper, 2000, 5);
    let b = c(7, Lane::ActiveMarketScalp, 1500, 6);
    let out = ingest_union([a, b], &w);
    assert_eq!(out.len(), 1);
    assert_eq!(out.get(&mint(7)).copied(), Some(a));
}

#[test]
fn dedup_is_order_independent() {
    let w = LaneWeights::from_defaults();
    let a = c(7, Lane::CreationSniper, 2000, 5); // strength 16_000_000
    let b = c(7, Lane::ActiveMarketScalp, 1500, 6); // strength 15_000_000
    let forward = ingest_union([a, b], &w);
    let backward = ingest_union([b, a], &w);
    assert_eq!(forward, backward);
    assert_eq!(forward.get(&mint(7)).copied(), Some(a));
}

#[test]
fn strength_tie_breaks_by_lane_weight_then_earliest() {
    let w = LaneWeights::from_defaults();
    // Equal strength: EarlyConf score 1000 (1000*12000=12_000_000) vs
    // CreationSniper score 1500 (1500*8000=12_000_000). Tie on strength =>
    // higher lane weight (EarlyConfirmation, 12_000) wins.
    let early = c(4, Lane::EarlyConfirmation, 1000, 20);
    let snipe = c(4, Lane::CreationSniper, 1500, 10);
    let out = ingest_union([snipe, early], &w);
    assert_eq!(out.get(&mint(4)).copied(), Some(early));
}

#[test]
fn full_strength_and_weight_tie_breaks_by_earliest_discovery() {
    let w = LaneWeights::from_defaults();
    // Same lane, same score => equal strength and equal weight. Earliest wins.
    let older = c(5, Lane::ActiveMarketScalp, 1000, 3);
    let newer = c(5, Lane::ActiveMarketScalp, 1000, 99);
    let out = ingest_union([newer, older], &w);
    assert_eq!(out.get(&mint(5)).copied(), Some(older));
}

#[test]
fn three_lane_union_collapses_to_single_strongest() {
    let w = LaneWeights::from_defaults();
    let a = c(8, Lane::CreationSniper, 1000, 0); // 8_000_000
    let b = c(8, Lane::EarlyConfirmation, 1000, 0); // 12_000_000  <- strongest
    let d = c(8, Lane::ActiveMarketScalp, 1000, 0); // 10_000_000
    let out = ingest_union([a, d, b], &w);
    assert_eq!(out.len(), 1);
    assert_eq!(out.get(&mint(8)).copied(), Some(b));
}
