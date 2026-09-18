//! The cached price feed — microsecond lookups for the mints Qwen is watching.
//!
//! WHY THIS EXISTS. Every price question the model asks (or that the proposal builder needs
//! to fill `CURVE STATE` / `AMM POOL STATE` / `PRICE UNITS`) would otherwise be an RPC
//! round-trip inside the decision deadline. LaserStream already carries the tick, so the
//! price is cached at ingest and read here.
//!
//! THREE RULES THIS MODULE ENFORCES
//!
//! 1. **A stale price is never a price.** Every lookup returns [`PriceLookup`], which
//!    distinguishes `Fresh` from `Stale` from `Missing`. There is no `get() -> Option<u64>`
//!    that would let a caller read a two-hour-old quote as if it were current: the same
//!    silent-drift class as a wrong prompt field.
//! 2. **Integer, tick-driven, deterministic (§22).** No float, no wall clock, no RNG. Age is
//!    `now - tick` in caller-supplied logical ticks, so the same tape replays identically.
//! 3. **Bounded and honestly bounded.** Capacity is explicit; eviction is least-recently-
//!    touched with a deterministic tie-break, and the eviction count is exported so a
//!    capacity that is too small shows up as telemetry instead of as mysterious misses.

use crate::candidate::Mint;
use std::collections::BTreeMap;

/// Which market a quote came from.
///
/// Carried per quote, not per mint: a mint graduates mid-life, and a caller that cannot tell
/// a bonding-curve price from an AMM price cannot apply the right venue fee (94.63 vs 30
/// bp/side — a 3x cost difference on the same number).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PriceVenue {
    /// Still on the bonding curve.
    BondingCurve,
    /// Graduated: the quote came from the PumpSwap AMM pool.
    Amm,
}

/// One observed price, with the logical time it was observed at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Quote {
    /// Price in the caller's fixed-point units (lamports per raw token, matching the c11
    /// `price_lamports_per_raw_token` field — never a float).
    pub price_fp: u64,
    /// Which venue printed it.
    pub venue: PriceVenue,
    /// The logical tick the price was observed at.
    pub tick: u64,
    /// The chain slot the price was observed at (for provenance; not used for ageing).
    pub slot: u64,
}

/// The answer to a price question. The three cases are never collapsed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PriceLookup {
    /// Observed within the freshness bound.
    Fresh(Quote),
    /// Observed, but older than the freshness bound — usable only with that fact attached.
    Stale(Quote),
    /// Never observed (or invalidated).
    Missing,
}

impl PriceLookup {
    /// The quote, whichever kind it is.
    #[must_use]
    pub fn quote(self) -> Option<Quote> {
        match self {
            PriceLookup::Fresh(q) | PriceLookup::Stale(q) => Some(q),
            PriceLookup::Missing => None,
        }
    }

    /// Whether the lookup is fresh (the only state a decision should be priced from).
    #[must_use]
    pub fn is_fresh(self) -> bool {
        matches!(self, PriceLookup::Fresh(_))
    }
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    quote: Quote,
    /// Logical time this entry was last written or read — the LRU key.
    touched: u64,
}

/// A bounded, tick-driven, deterministic price cache for the watched set.
#[derive(Debug)]
pub struct PriceCache {
    capacity: usize,
    max_staleness_ticks: u64,
    entries: BTreeMap<Mint, Entry>,
    evictions: u64,
}

impl PriceCache {
    /// Build a cache holding at most `capacity` mints, treating a quote older than
    /// `max_staleness_ticks` as stale.
    ///
    /// `capacity == 0` is legal and means "cache nothing" — every lookup is `Missing`, every
    /// insert is refused. That is a valid configuration for a shadow run that must not hold
    /// state, and it is explicit rather than a panic.
    #[must_use]
    pub fn new(capacity: usize, max_staleness_ticks: u64) -> Self {
        Self { capacity, max_staleness_ticks, entries: BTreeMap::new(), evictions: 0 }
    }

    /// Maximum number of mints held.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// The freshness bound, in logical ticks.
    #[must_use]
    pub fn max_staleness_ticks(&self) -> u64 {
        self.max_staleness_ticks
    }

    /// Number of mints currently cached.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many entries have been evicted for capacity since construction (telemetry: a
    /// non-zero, growing count means the cache is too small for the watched set).
    #[must_use]
    pub fn evictions(&self) -> u64 {
        self.evictions
    }

    /// Record a tick for `mint`. Returns `true` when the quote is now held.
    ///
    /// Refreshing an existing mint never evicts: it is the common path and allocates nothing.
    /// When a new mint arrives at capacity, the least-recently-touched entry is dropped, ties
    /// broken by mint order so the choice is a pure function of the sequence of ticks.
    pub fn on_tick(
        &mut self,
        mint: Mint,
        price_fp: u64,
        venue: PriceVenue,
        tick: u64,
        slot: u64,
    ) -> bool {
        if self.capacity == 0 {
            return false;
        }
        let quote = Quote { price_fp, venue, tick, slot };
        if let Some(e) = self.entries.get_mut(&mint) {
            e.quote = quote;
            e.touched = tick;
            return true;
        }
        if self.entries.len() >= self.capacity {
            // Least-recently-touched; ties by mint order (BTreeMap iteration is sorted, so
            // `min_by_key` over (touched, mint) is deterministic without extra state).
            if let Some(victim) = self
                .entries
                .iter()
                .min_by_key(|(m, e)| (e.touched, **m))
                .map(|(m, _)| *m)
            {
                self.entries.remove(&victim);
                self.evictions += 1;
            }
        }
        self.entries.insert(mint, Entry { quote, touched: tick });
        true
    }

    /// Look up a price as of logical time `now`. Reads are not touches: a read must not
    /// change what a later eviction does, or the cache's contents would depend on query
    /// order rather than on the tape.
    #[must_use]
    pub fn lookup(&self, mint: &Mint, now: u64) -> PriceLookup {
        match self.entries.get(mint) {
            None => PriceLookup::Missing,
            Some(e) => {
                let age = now.saturating_sub(e.quote.tick);
                if age <= self.max_staleness_ticks {
                    PriceLookup::Fresh(e.quote)
                } else {
                    PriceLookup::Stale(e.quote)
                }
            }
        }
    }

    /// Age of the cached quote for `mint` as of `now`, if any is held.
    #[must_use]
    pub fn age_ticks(&self, mint: &Mint, now: u64) -> Option<u64> {
        self.entries.get(mint).map(|e| now.saturating_sub(e.quote.tick))
    }

    /// Drop a mint's quote (e.g. the watchlist dropped it, or its market resolved).
    /// Returns whether anything was dropped.
    pub fn invalidate(&mut self, mint: &Mint) -> bool {
        self.entries.remove(mint).is_some()
    }

    /// Drop everything (a re-org / feed reset), keeping the configuration.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// The mints currently held, in deterministic order.
    #[must_use]
    pub fn mints(&self) -> Vec<Mint> {
        self.entries.keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(b: u8) -> Mint {
        Mint([b; 32])
    }

    #[test]
    fn fresh_stale_and_missing_are_never_collapsed() {
        let mut c = PriceCache::new(4, 10);
        assert_eq!(c.lookup(&m(1), 100), PriceLookup::Missing);
        c.on_tick(m(1), 7_000, PriceVenue::Amm, 100, 5);
        assert_eq!(
            c.lookup(&m(1), 105),
            PriceLookup::Fresh(Quote { price_fp: 7_000, venue: PriceVenue::Amm, tick: 100, slot: 5 })
        );
        assert!(c.lookup(&m(1), 110).is_fresh(), "age == bound is still fresh");
        match c.lookup(&m(1), 111) {
            PriceLookup::Stale(q) => assert_eq!(q.price_fp, 7_000),
            other => panic!("expected Stale, got {other:?}"),
        }
        assert_eq!(c.age_ticks(&m(1), 130), Some(30));
        assert_eq!(c.age_ticks(&m(9), 130), None);
    }

    #[test]
    fn a_refresh_updates_price_venue_and_recency_without_evicting() {
        let mut c = PriceCache::new(2, 10);
        c.on_tick(m(1), 100, PriceVenue::BondingCurve, 1, 1);
        assert!(c.on_tick(m(1), 260, PriceVenue::Amm, 5, 9), "refresh accepted");
        assert_eq!(c.len(), 1);
        assert_eq!(c.evictions(), 0, "a refresh is not an eviction");
        let q = c.lookup(&m(1), 5).quote().unwrap();
        assert_eq!((q.price_fp, q.venue, q.slot), (260, PriceVenue::Amm, 9));
    }

    #[test]
    fn eviction_is_least_recently_touched_and_deterministic() {
        let mut c = PriceCache::new(2, u64::MAX);
        c.on_tick(m(1), 1, PriceVenue::Amm, 10, 0);
        c.on_tick(m(2), 2, PriceVenue::Amm, 20, 0);
        // m(1) is the oldest; a third mint must displace it, not m(2).
        c.on_tick(m(3), 3, PriceVenue::Amm, 30, 0);
        assert_eq!(c.evictions(), 1);
        assert!(matches!(c.lookup(&m(1), 30), PriceLookup::Missing));
        assert!(c.lookup(&m(2), 30).is_fresh());
        assert!(c.lookup(&m(3), 30).is_fresh());
        assert_eq!(c.mints(), vec![m(2), m(3)]);
    }

    #[test]
    fn ties_evict_the_lowest_mint_so_replay_is_reproducible() {
        let mut c = PriceCache::new(1, u64::MAX);
        c.on_tick(m(7), 1, PriceVenue::Amm, 10, 0);
        c.on_tick(m(3), 2, PriceVenue::Amm, 10, 0); // same touch tick: 3 < 7 wins the slot
        assert_eq!(c.mints(), vec![m(3)]);
        assert!(matches!(c.lookup(&m(7), 10), PriceLookup::Missing));
    }

    #[test]
    fn a_read_does_not_change_what_evicts_next() {
        let mut c = PriceCache::new(2, u64::MAX);
        c.on_tick(m(1), 1, PriceVenue::Amm, 10, 0);
        c.on_tick(m(2), 2, PriceVenue::Amm, 20, 0);
        let _ = c.lookup(&m(1), 20); // reading the oldest must NOT protect it
        c.on_tick(m(3), 3, PriceVenue::Amm, 30, 0);
        assert!(matches!(c.lookup(&m(1), 30), PriceLookup::Missing));
    }

    #[test]
    fn zero_capacity_holds_nothing_and_says_so() {
        let mut c = PriceCache::new(0, 10);
        assert!(!c.on_tick(m(1), 5, PriceVenue::Amm, 1, 1));
        assert!(c.is_empty());
        assert_eq!(c.lookup(&m(1), 1), PriceLookup::Missing);
        assert_eq!(c.evictions(), 0);
    }

    #[test]
    fn invalidate_and_clear_drop_quotes() {
        let mut c = PriceCache::new(4, 10);
        c.on_tick(m(1), 1, PriceVenue::Amm, 1, 1);
        c.on_tick(m(2), 2, PriceVenue::Amm, 1, 1);
        assert!(c.invalidate(&m(1)));
        assert!(!c.invalidate(&m(1)), "second invalidate is a no-op");
        assert_eq!(c.mints(), vec![m(2)]);
        c.clear();
        assert!(c.is_empty());
        assert_eq!(c.capacity(), 4, "clear keeps configuration");
    }
}
