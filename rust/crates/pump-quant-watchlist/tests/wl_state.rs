//! `wl_state` leaf tests: capacity bound, rank-based eviction, TTL pruning,
//! same-mint merge, deterministic ranking.

use pump_quant_watchlist::candidate::{Candidate, Features, Lane, Mint};
use pump_quant_watchlist::rank::{LaneWeights, RankParams};
use pump_quant_watchlist::state::WatchlistState;

fn mint(tag: u8) -> Mint {
    let mut b = [0u8; 32];
    b[0] = tag;
    Mint::new(b)
}

fn c(tag: u8, lane: Lane, score: u64, at: u64) -> Candidate {
    Candidate::new(mint(tag), lane, score, at, Features::default())
}

fn state(cap: usize, ttl: u64) -> WatchlistState {
    WatchlistState::new(cap, RankParams::new(ttl), LaneWeights::from_defaults())
}

#[test]
fn respects_capacity_and_never_exceeds_it() {
    let mut s = state(3, 1000);
    assert!(s.insert(c(1, Lane::ActiveMarketScalp, 100, 0), 0));
    assert!(s.insert(c(2, Lane::ActiveMarketScalp, 200, 0), 0));
    assert!(s.insert(c(3, Lane::ActiveMarketScalp, 300, 0), 0));
    assert_eq!(s.len(), 3);
    // A weaker fourth candidate cannot displace anyone; rejected.
    assert!(!s.insert(c(4, Lane::ActiveMarketScalp, 50, 0), 0));
    assert_eq!(s.len(), 3);
    assert!(!s.contains(&mint(4)));
}

#[test]
fn stronger_candidate_evicts_weakest_when_full() {
    let mut s = state(2, 1000);
    s.insert(c(1, Lane::ActiveMarketScalp, 100, 0), 0);
    s.insert(c(2, Lane::ActiveMarketScalp, 200, 0), 0);
    // Insert a stronger one at now=0: weakest is mint 1 (rank 100). New rank 500 > 100.
    assert!(s.insert(c(3, Lane::ActiveMarketScalp, 500, 0), 0));
    assert_eq!(s.len(), 2);
    assert!(!s.contains(&mint(1)));
    assert!(s.contains(&mint(2)));
    assert!(s.contains(&mint(3)));
}

#[test]
fn equal_rank_does_not_evict_incumbent() {
    let mut s = state(1, 1000);
    s.insert(c(1, Lane::ActiveMarketScalp, 100, 0), 0);
    // Same rank => new does not evict (ties keep incumbent).
    assert!(!s.insert(c(2, Lane::ActiveMarketScalp, 100, 0), 0));
    assert!(s.contains(&mint(1)));
    assert!(!s.contains(&mint(2)));
}

#[test]
fn eviction_uses_decayed_rank_at_now() {
    // Capacity 2. mint1 is old (will have decayed), mint2 fresh.
    let mut s = state(2, 100);
    s.insert(c(1, Lane::ActiveMarketScalp, 1000, 0), 0); // discovered at 0
    s.insert(c(2, Lane::ActiveMarketScalp, 1000, 90), 90); // discovered at 90
                                                           // At now=95: mint1 age 95 => recency (100-95)/100 = 5% => rank 50.
                                                           //            mint2 age 5  => recency 95% => rank 950.
                                                           // New candidate mint3 rank at now=95, fresh: score 100 => rank 100 > 50 => evicts mint1.
    assert!(s.insert(c(3, Lane::ActiveMarketScalp, 100, 95), 95));
    assert!(!s.contains(&mint(1)));
    assert!(s.contains(&mint(2)));
    assert!(s.contains(&mint(3)));
}

#[test]
fn prune_removes_expired_by_ttl() {
    let mut s = state(10, 100);
    s.insert(c(1, Lane::ActiveMarketScalp, 100, 0), 0);
    s.insert(c(2, Lane::ActiveMarketScalp, 100, 50), 50);
    s.insert(c(3, Lane::ActiveMarketScalp, 100, 90), 90);
    // At now=150: ages are 150, 100, 60. TTL=100 => expire when age>=100.
    // mint1 (150) and mint2 (100) expire; mint3 (60) survives.
    let evicted = s.prune(150);
    assert_eq!(evicted, 2);
    assert_eq!(s.len(), 1);
    assert!(s.contains(&mint(3)));
}

#[test]
fn same_mint_insert_keeps_stronger_evidence_without_growing() {
    let mut s = state(5, 1000);
    // First: CreationSniper score 1000 => evidence 8_000_000.
    s.insert(c(1, Lane::CreationSniper, 1000, 0), 0);
    assert_eq!(s.len(), 1);
    // Stronger evidence for same mint: EarlyConfirmation score 1000 => 12_000_000.
    assert!(s.insert(c(1, Lane::EarlyConfirmation, 1000, 0), 0));
    assert_eq!(s.len(), 1);
    assert_eq!(
        s.entries().get(&mint(1)).unwrap().lane,
        Lane::EarlyConfirmation
    );
    // Weaker re-observation must NOT overwrite; still size 1, still EarlyConf.
    assert!(!s.insert(c(1, Lane::CreationSniper, 500, 0), 0));
    assert_eq!(s.len(), 1);
    assert_eq!(
        s.entries().get(&mint(1)).unwrap().lane,
        Lane::EarlyConfirmation
    );
}

#[test]
fn ranked_is_sorted_desc_with_deterministic_tiebreak() {
    let mut s = state(5, 1000);
    s.insert(c(1, Lane::ActiveMarketScalp, 300, 0), 0);
    s.insert(c(2, Lane::ActiveMarketScalp, 100, 0), 0);
    s.insert(c(3, Lane::ActiveMarketScalp, 200, 0), 0);
    let r = s.ranked(0);
    let ranks: Vec<u64> = r.iter().map(|(rk, _)| *rk).collect();
    assert_eq!(ranks, vec![300, 200, 100]);
    assert_eq!(r[0].1.mint, mint(1));
    assert_eq!(r[2].1.mint, mint(2));

    // Equal rank tie-break by mint ascending.
    let mut s2 = state(5, 1000);
    s2.insert(c(9, Lane::ActiveMarketScalp, 100, 0), 0);
    s2.insert(c(2, Lane::ActiveMarketScalp, 100, 0), 0);
    let r2 = s2.ranked(0);
    assert_eq!(r2[0].1.mint, mint(2));
    assert_eq!(r2[1].1.mint, mint(9));
}

#[test]
fn zero_capacity_accepts_nothing() {
    let mut s = state(0, 1000);
    assert!(!s.insert(c(1, Lane::ActiveMarketScalp, 100, 0), 0));
    assert_eq!(s.len(), 0);
    assert!(s.is_empty());
}
