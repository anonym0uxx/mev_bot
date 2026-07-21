//! `wl_state` leaf — bounded, ranked, decaying working set.
//!
//! Responsibility: hold the live watchlist. It is a max-`capacity` set of
//! candidates keyed by mint, with TTL expiry and rank-based eviction so it can
//! run forever without unbounded growth. This is the memory-safety guarantee
//! the "always-scanning eye" needs.
//!
//! Bounds & eviction (§99):
//! - **Capacity:** never holds more than `capacity` candidates. Backed by a
//!   single `BTreeMap<Mint, Candidate>`; no auxiliary structure can outgrow it.
//! - **TTL:** [`WatchlistState::prune`] drops candidates whose age exceeds the
//!   configured `ttl_ticks`.
//! - **Eviction on overflow:** inserting into a full set keeps the top-`capacity`
//!   by rank — a new candidate that outranks the current weakest evicts it;
//!   otherwise the new candidate is rejected. Never allocates beyond capacity.
//! - **Decay:** candidates are never mutated in place; their effective rank
//!   decays via recency at query time ([`crate::rank`]).
//!
//! Same-mint re-discovery keeps the strongest lane evidence, exactly as
//! [`crate::lane_ingest`] does, so state and ingest agree.
//!
//! Constitution: §22 (deterministic, integer), §99 (bounded + eviction), §102.

use crate::candidate::{Candidate, Mint};
use crate::lane_ingest;
use crate::rank::{score_rank, LaneWeights, RankParams};
use std::collections::BTreeMap;

/// The bounded, ranked watchlist working set.
///
/// Responsibility: the single owner of live candidate memory (`wl_state`). §99.
#[derive(Clone, Debug)]
pub struct WatchlistState {
    entries: BTreeMap<Mint, Candidate>,
    capacity: usize,
    params: RankParams,
    weights: LaneWeights,
}

impl WatchlistState {
    /// Create an empty watchlist.
    ///
    /// `capacity` is the hard maximum number of candidates (§99). `params`
    /// governs recency/TTL; `weights` the per-lane ranking multipliers.
    /// A `capacity` of 0 yields a set that accepts nothing.
    #[must_use]
    pub fn new(capacity: usize, params: RankParams, weights: LaneWeights) -> Self {
        Self {
            entries: BTreeMap::new(),
            capacity,
            params,
            weights,
        }
    }

    /// Hard capacity (maximum live candidates). §99.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Current number of live candidates. Always `<= capacity()`.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the watchlist holds no candidates.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether a mint is currently present.
    #[must_use]
    pub fn contains(&self, mint: &Mint) -> bool {
        self.entries.contains_key(mint)
    }

    /// The lane weights this state ranks with.
    #[must_use]
    pub fn weights(&self) -> &LaneWeights {
        &self.weights
    }

    /// The rank parameters this state ranks with.
    #[must_use]
    pub fn params(&self) -> RankParams {
        self.params
    }

    /// Rank of a candidate under this state's params/weights at time `now`.
    #[must_use]
    pub fn rank_of(&self, candidate: &Candidate, now: u64) -> u64 {
        score_rank(candidate, now, self.params, &self.weights)
    }

    /// Insert (or merge) one candidate at logical time `now`.
    ///
    /// Behaviour (deterministic, §22):
    /// - If the mint is already present, keep whichever record has the stronger
    ///   lane evidence (same rule as [`crate::lane_ingest`]); size is unchanged.
    /// - If the mint is new and there is room, insert it.
    /// - If the mint is new and the set is full, insert only if it outranks the
    ///   current weakest entry, evicting that weakest entry (§99). Ties (equal
    ///   rank) do **not** evict — the incumbent is retained.
    ///
    /// Returns `true` iff `candidate` is present in the set after the call.
    pub fn insert(&mut self, candidate: Candidate, now: u64) -> bool {
        if let Some(existing) = self.entries.get(&candidate.mint) {
            // Merge: keep the stronger lane evidence for this mint.
            let keep_new = lane_ingest::ingest_union([*existing, candidate], &self.weights)
                .get(&candidate.mint)
                .copied()
                == Some(candidate);
            if keep_new {
                self.entries.insert(candidate.mint, candidate);
            }
            return self.entries.get(&candidate.mint) == Some(&candidate);
        }

        if self.capacity == 0 {
            return false;
        }

        if self.entries.len() < self.capacity {
            self.entries.insert(candidate.mint, candidate);
            return true;
        }

        // Full: find the current weakest entry by rank at `now`.
        let new_rank = self.rank_of(&candidate, now);
        let weakest = self
            .entries
            .values()
            .map(|c| (self.rank_of(c, now), c.mint))
            .min_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

        match weakest {
            Some((weak_rank, weak_mint)) if new_rank > weak_rank => {
                self.entries.remove(&weak_mint);
                self.entries.insert(candidate.mint, candidate);
                true
            }
            _ => false,
        }
    }

    /// Drop every candidate whose age at `now` exceeds the TTL horizon.
    ///
    /// `age = now.saturating_sub(discovered_at)`; a candidate is expired when
    /// `age >= ttl_ticks` (matching [`crate::rank::recency_factor`], which
    /// returns 0 there). Returns the number evicted. §99.
    pub fn prune(&mut self, now: u64) -> usize {
        let ttl = self.params.ttl_ticks;
        let before = self.entries.len();
        self.entries.retain(|_, c| {
            let age = now.saturating_sub(c.discovered_at);
            ttl != 0 && age < ttl
        });
        before - self.entries.len()
    }

    /// All live candidates ranked at `now`, strongest first.
    ///
    /// Deterministic tie-break: equal rank orders by mint ascending (§22).
    /// Allocates a `Vec` bounded by `len()` (§99).
    #[must_use]
    pub fn ranked(&self, now: u64) -> Vec<(u64, Candidate)> {
        let mut out: Vec<(u64, Candidate)> = self
            .entries
            .values()
            .map(|c| (self.rank_of(c, now), *c))
            .collect();
        out.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.mint.cmp(&b.1.mint)));
        out
    }

    /// Read-only view of the live entries by mint.
    #[must_use]
    pub fn entries(&self) -> &BTreeMap<Mint, Candidate> {
        &self.entries
    }
}
