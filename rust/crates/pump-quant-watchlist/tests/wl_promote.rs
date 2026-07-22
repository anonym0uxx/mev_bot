//! `wl_promote` leaf tests: top-k selection with a min-rank floor, decayed
//! candidates filtered, deterministic ordering, `k` bound.

use pump_quant_watchlist::candidate::{Candidate, Features, Lane, Mint};
use pump_quant_watchlist::promote::promote_top;
use pump_quant_watchlist::rank::{LaneWeights, RankParams};
use pump_quant_watchlist::state::WatchlistState;

fn mint(tag: u8) -> Mint {
    let mut b = [0u8; 32];
    b[0] = tag;
    Mint::new(b)
}

fn c(tag: u8, score: u64, at: u64) -> Candidate {
    Candidate::new(
        mint(tag),
        Lane::ActiveMarketScalp,
        score,
        at,
        Features::default(),
    )
}

fn state(cap: usize, ttl: u64) -> WatchlistState {
    WatchlistState::new(cap, RankParams::new(ttl), LaneWeights::from_defaults())
}

#[test]
fn promotes_top_k_strongest_first() {
    let mut s = state(10, 1000);
    s.insert(c(1, 100, 0), 0);
    s.insert(c(2, 500, 0), 0);
    s.insert(c(3, 300, 0), 0);
    s.insert(c(4, 400, 0), 0);
    let top = promote_top(&s, 0, 2, 0);
    assert_eq!(top.len(), 2);
    assert_eq!(top[0].mint, mint(2)); // 500
    assert_eq!(top[1].mint, mint(4)); // 400
}

#[test]
fn min_rank_floor_filters_weak_candidates() {
    let mut s = state(10, 1000);
    s.insert(c(1, 100, 0), 0);
    s.insert(c(2, 500, 0), 0);
    s.insert(c(3, 250, 0), 0);
    // Floor 300 => only mint2 (500) qualifies; mint3 (250) and mint1 (100) excluded.
    let top = promote_top(&s, 0, 10, 300);
    assert_eq!(top.len(), 1);
    assert_eq!(top[0].mint, mint(2));
}

#[test]
fn decayed_candidates_are_excluded_by_positive_floor() {
    let mut s = state(10, 100);
    s.insert(c(1, 1_000_000, 0), 0);
    // At now=100 the candidate has decayed to rank 0.
    let top = promote_top(&s, 100, 10, 1);
    assert!(top.is_empty());
    // With floor 0, a rank-0 candidate is still included.
    let top0 = promote_top(&s, 100, 10, 0);
    assert_eq!(top0.len(), 1);
}

#[test]
fn k_zero_promotes_nothing() {
    let mut s = state(10, 1000);
    s.insert(c(1, 500, 0), 0);
    assert!(promote_top(&s, 0, 0, 0).is_empty());
}

#[test]
fn k_larger_than_set_returns_all_qualifying() {
    let mut s = state(10, 1000);
    s.insert(c(1, 100, 0), 0);
    s.insert(c(2, 200, 0), 0);
    let top = promote_top(&s, 0, 50, 0);
    assert_eq!(top.len(), 2);
    assert_eq!(top[0].mint, mint(2));
    assert_eq!(top[1].mint, mint(1));
}

#[test]
fn promote_does_not_mutate_state() {
    let mut s = state(10, 1000);
    s.insert(c(1, 100, 0), 0);
    s.insert(c(2, 200, 0), 0);
    let before = s.len();
    let _ = promote_top(&s, 0, 1, 0);
    assert_eq!(s.len(), before);
    assert!(s.contains(&mint(1)));
    assert!(s.contains(&mint(2)));
}
