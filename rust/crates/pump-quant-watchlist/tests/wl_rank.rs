//! `wl_rank` leaf tests: recency decay + fixed-point rank composition, with
//! expectations computed independently of the implementation.

use pump_quant_watchlist::candidate::{Candidate, Features, Lane, Mint};
use pump_quant_watchlist::rank::{
    recency_factor, score_rank, LaneWeights, RankParams, RECENCY_ONE, WEIGHT_ONE,
};

fn cand(score: u64, discovered_at: u64, lane: Lane) -> Candidate {
    let mut b = [0u8; 32];
    b[0] = 1;
    Candidate::new(
        Mint::new(b),
        lane,
        score,
        discovered_at,
        Features::default(),
    )
}

#[test]
fn recency_is_full_at_zero_age_and_zero_at_ttl() {
    assert_eq!(recency_factor(0, 0, 100), RECENCY_ONE);
    assert_eq!(recency_factor(50, 50, 100), RECENCY_ONE); // age 0
    assert_eq!(recency_factor(0, 100, 100), 0); // age == ttl
    assert_eq!(recency_factor(0, 250, 100), 0); // age > ttl
}

#[test]
fn recency_linear_decay_matches_hand_computation() {
    // ttl = 1000, age = 250 => RECENCY_ONE * (1000-250)/1000 = 1e6 * 0.75.
    let r = recency_factor(0, 250, 1000);
    assert_eq!(r, 750_000);
    // age = 900 => 1e6 * 100/1000 = 100_000.
    assert_eq!(recency_factor(100, 1000, 1000), 100_000);
    // Monotonic non-increasing across the horizon.
    let mut prev = u64::MAX;
    for age in 0..=1000u64 {
        let r = recency_factor(0, age, 1000);
        assert!(r <= prev, "recency must not increase with age at {age}");
        prev = r;
    }
}

#[test]
fn recency_zero_ttl_is_disabled() {
    assert_eq!(recency_factor(0, 0, 0), 0);
}

#[test]
fn future_discovery_is_treated_as_brand_new() {
    // now < discovered_at => saturating age 0 => full recency, never negative.
    assert_eq!(recency_factor(500, 100, 1000), RECENCY_ONE);
}

#[test]
fn rank_composition_matches_hand_computation() {
    let weights = LaneWeights::from_defaults();
    let params = RankParams::new(1000);
    // ActiveMarketScalp weight = WEIGHT_ONE (10_000 => 1.0x).
    let c = cand(1_000_000, 0, Lane::ActiveMarketScalp);
    // age 250 => recency 750_000.
    // after_recency = 1_000_000 * 750_000 / 1_000_000 = 750_000.
    // after_weight  = 750_000 * 10_000 / 10_000 = 750_000.
    assert_eq!(score_rank(&c, 250, params, &weights), 750_000);

    // EarlyConfirmation weight = 12_000 => 1.2x. Fresh (age 0 => recency 1e6).
    let c2 = cand(1_000_000, 0, Lane::EarlyConfirmation);
    // after_recency = 1_000_000; after_weight = 1_000_000 * 12_000 / 10_000 = 1_200_000.
    assert_eq!(score_rank(&c2, 0, params, &weights), 1_200_000);

    // CreationSniper 0.8x, half-decayed.
    let c3 = cand(2_000_000, 0, Lane::CreationSniper);
    // age 500 => recency 500_000. after_recency = 2_000_000*500_000/1_000_000 = 1_000_000.
    // after_weight = 1_000_000 * 8_000 / 10_000 = 800_000.
    assert_eq!(score_rank(&c3, 500, params, &weights), 800_000);
}

#[test]
fn rank_is_zero_when_decayed_out() {
    let weights = LaneWeights::from_defaults();
    let params = RankParams::new(100);
    let c = cand(9_999_999, 0, Lane::EarlyConfirmation);
    assert_eq!(score_rank(&c, 100, params, &weights), 0);
    assert_eq!(score_rank(&c, 5000, params, &weights), 0);
}

#[test]
fn rank_saturates_into_u64_without_wrapping() {
    // Force the u128 product above u64::MAX and confirm the ceiling clamp.
    let mut weights = LaneWeights::from_defaults();
    weights.set(Lane::ActiveMarketScalp, 60_000); // 6x
    let params = RankParams::new(10);
    let c = cand(u64::MAX, 0, Lane::ActiveMarketScalp);
    // age 0 => recency 1e6 => after_recency = u64::MAX. after_weight = u64::MAX*6 > u64::MAX.
    assert_eq!(score_rank(&c, 0, params, &weights), u64::MAX);
}

#[test]
fn lane_weights_override_and_default_roundtrip() {
    let mut w = LaneWeights::from_defaults();
    assert_eq!(w.get(Lane::CreationSniper), 8_000);
    w.set(Lane::CreationSniper, WEIGHT_ONE);
    assert_eq!(w.get(Lane::CreationSniper), WEIGHT_ONE);
    // Other lanes unaffected.
    assert_eq!(w.get(Lane::EarlyConfirmation), 12_000);
}
