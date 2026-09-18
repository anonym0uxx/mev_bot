//! Search / retrieval over the watched-coin registry and its price cache.
//!
//! WHY THIS EXISTS. Qwen is given one bundle per decision, but a trencher also needs to *ask*
//! — "what else is worth looking at", "what is this mint's liquidity", "is this quote even
//! current". This module is that query surface. It is deliberately a pure function over the
//! existing [`WatchlistState`] plus [`PriceCache`]: no new state, no index to keep in sync, no
//! hidden ordering.
//!
//! Two properties are non-negotiable and are asserted in the tests:
//!
//! * **Deterministic order.** Hits are ranked by the registry's own rank (descending), ties
//!   broken by mint order. The same query over the same state always returns the same list,
//!   so a replay is reproducible (§22).
//! * **Freshness is reported, never assumed.** Each hit carries its [`PriceLookup`], and a
//!   caller that wants only currently-priced mints asks for that explicitly. A stale quote is
//!   returned *as stale* rather than silently dropped or silently trusted.

use crate::candidate::{Lane, Mint};
use crate::price_cache::{PriceCache, PriceLookup};
use crate::state::WatchlistState;

/// A retrieval request. Every field is optional-with-a-default: the zero value asks for
/// everything the registry holds, in rank order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchQuery {
    /// Restrict to one discovery lane.
    pub lane: Option<Lane>,
    /// Minimum raw discovery score.
    pub min_discovery_score: u64,
    /// Minimum pool liquidity at discovery, in lamports.
    pub min_liquidity_lamports: u64,
    /// Minimum cumulative ring volume at discovery, in lamports.
    pub min_volume_lamports: u64,
    /// Minimum distinct buyers observed at discovery.
    pub min_unique_buyers: u32,
    /// Maximum market age at discovery, in slots (None = no bound).
    pub max_age_slots: Option<u32>,
    /// Only return mints whose cached quote is currently fresh.
    pub require_fresh_price: bool,
    /// Maximum hits to return; `0` returns nothing (an explicit "no limit" is `usize::MAX`).
    pub limit: usize,
}

/// One retrieval result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    /// The mint.
    pub mint: Mint,
    /// The lane that surfaced it.
    pub lane: Lane,
    /// Its current registry rank (larger is stronger).
    pub rank: u64,
    /// Its raw discovery score.
    pub discovery_score: u64,
    /// The cached quote, with freshness attached.
    pub price: PriceLookup,
    /// Logical ticks since discovery.
    pub age_ticks: u64,
}

/// Run a query over the registry and its price cache, as of logical time `now`.
#[must_use]
pub fn search(
    state: &WatchlistState,
    cache: &PriceCache,
    q: &SearchQuery,
    now: u64,
) -> Vec<Hit> {
    if q.limit == 0 {
        return Vec::new();
    }
    let mut hits: Vec<Hit> = state
        .entries()
        .values()
        .filter(|c| {
            q.lane.map_or(true, |l| c.lane == l)
                && c.discovery_score >= q.min_discovery_score
                && c.features.liquidity_lamports >= q.min_liquidity_lamports
                && c.features.volume_lamports >= q.min_volume_lamports
                && c.features.unique_buyers >= q.min_unique_buyers
                && q.max_age_slots.map_or(true, |a| c.features.age_slots <= a)
        })
        .filter_map(|c| {
            let price = cache.lookup(&c.mint, now);
            if q.require_fresh_price && !price.is_fresh() {
                return None;
            }
            Some(Hit {
                mint: c.mint,
                lane: c.lane,
                rank: state.rank_of(c, now),
                discovery_score: c.discovery_score,
                price,
                age_ticks: now.saturating_sub(c.discovered_at),
            })
        })
        .collect();
    // Strongest first; ties by mint so the order never depends on map iteration or query
    // arrival order.
    hits.sort_by(|a, b| b.rank.cmp(&a.rank).then_with(|| a.mint.cmp(&b.mint)));
    hits.truncate(q.limit);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::{Candidate, Features};
    use crate::rank::{LaneWeights, RankParams};

    fn m(b: u8) -> Mint {
        Mint([b; 32])
    }

    fn feats(liquidity: u64, buyers: u32, volume: u64, age_slots: u32) -> Features {
        Features {
            liquidity_lamports: liquidity,
            unique_buyers: buyers,
            volume_lamports: volume,
            age_slots,
            ..Features::default()
        }
    }

    fn state() -> WatchlistState {
        WatchlistState::new(16, RankParams::new(1_000), LaneWeights::default())
    }

    fn insert(s: &mut WatchlistState, b: u8, lane: Lane, score: u64, f: Features) {
        s.insert(Candidate::new(m(b), lane, score, 100, f), 100);
    }

    #[test]
    fn a_zero_query_returns_everything_in_rank_order() {
        let mut s = state();
        insert(&mut s, 1, Lane::EarlyConfirmation, 500, feats(10, 3, 1_000, 5));
        insert(&mut s, 2, Lane::EarlyConfirmation, 900, feats(20, 4, 2_000, 6));
        let cache = PriceCache::new(4, 10);
        let q = SearchQuery { limit: usize::MAX, ..SearchQuery::default() };
        let hits = search(&s, &cache, &q, 100);
        assert_eq!(hits.len(), 2);
        assert!(hits[0].rank >= hits[1].rank, "descending rank");
    }

    #[test]
    fn order_is_deterministic_under_a_rank_tie() {
        let mut s = state();
        for b in [9u8, 3, 7] {
            insert(&mut s, b, Lane::EarlyConfirmation, 400, feats(1, 1, 1, 1));
        }
        let cache = PriceCache::new(4, 10);
        let q = SearchQuery { limit: usize::MAX, ..SearchQuery::default() };
        let hits = search(&s, &cache, &q, 100);
        let order: Vec<[u8; 32]> = hits.iter().map(|h| h.mint.bytes()).collect();
        assert_eq!(order, vec![[3u8; 32], [7u8; 32], [9u8; 32]], "ties break by mint");
    }

    #[test]
    fn filters_compose_without_widening_each_other() {
        let mut s = state();
        insert(&mut s, 1, Lane::EarlyConfirmation, 900, feats(50, 5, 9_000, 3));
        insert(&mut s, 2, Lane::GraduationTransition, 900, feats(50, 5, 9_000, 3));
        insert(&mut s, 3, Lane::EarlyConfirmation, 900, feats(1, 5, 9_000, 3));
        let cache = PriceCache::new(4, 10);
        let q = SearchQuery {
            lane: Some(Lane::EarlyConfirmation),
            min_liquidity_lamports: 10,
            limit: usize::MAX,
            ..SearchQuery::default()
        };
        let hits = search(&s, &cache, &q, 100);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].mint, m(1));
    }

    #[test]
    fn require_fresh_price_drops_unpriced_and_stale_candidates() {
        let mut s = state();
        insert(&mut s, 1, Lane::EarlyConfirmation, 900, feats(1, 1, 1, 1)); // fresh
        insert(&mut s, 2, Lane::EarlyConfirmation, 800, feats(1, 1, 1, 1)); // stale
        insert(&mut s, 3, Lane::EarlyConfirmation, 700, feats(1, 1, 1, 1)); // never priced
        let mut cache = PriceCache::new(4, 10);
        cache.on_tick(m(1), 1, crate::price_cache::PriceVenue::Amm, 100, 0);
        cache.on_tick(m(2), 1, crate::price_cache::PriceVenue::Amm, 0, 0);
        let q = SearchQuery { require_fresh_price: true, limit: usize::MAX, ..Default::default() };
        let hits = search(&s, &cache, &q, 100);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].mint, m(1));
        assert!(hits[0].price.is_fresh());
    }

    #[test]
    fn limit_caps_the_result_and_zero_means_zero() {
        let mut s = state();
        for b in 1..=5u8 {
            insert(&mut s, b, Lane::EarlyConfirmation, 100 * u64::from(b), feats(1, 1, 1, 1));
        }
        let cache = PriceCache::new(4, 10);
        let q = SearchQuery { limit: 2, ..Default::default() };
        assert_eq!(search(&s, &cache, &q, 100).len(), 2);
        let q0 = SearchQuery { limit: 0, ..Default::default() };
        assert!(search(&s, &cache, &q0, 100).is_empty());
    }
}
