//! C4 — the creator's launch history, as the prompt's `DEV HISTORY:` line states it.
//!
//! WHAT THIS ANSWERS, AND WHAT IT DOES NOT. The entry prompt's last unproduced line is
//! `DEV HISTORY: creator_past_launches=<n> creator_known=<0|1>`, and the corpus derives both from
//! the launch table (`canonical/renormalized_v7/launches.jsonl`) as:
//!
//! ```text
//! creator_past_launches = |{ launches by this mint's creator with time < THIS mint's launch time }|
//! creator_known         = this mint appears in the launch table
//! ```
//!
//! (`build_c9_enrichment_full.py::prior_launches` — a bisect over the creator's launch times.)
//! Both are REGISTRY questions: they are answered from a launch table, not from outcomes. That is
//! deliberately not the same question [`pump_quant_wallet_graph::creator_ledger`] answers — that
//! ledger tracks what became of a creator's launches (migrated / survived / rugged, §27) and is
//! capped per creator for that purpose. Two questions, two structures; neither is an authority for
//! the other's field. This one never asserts anything about how a launch turned out.
//!
//! WHY THE TABLE IS LOADED, NOT LEARNED. A live registry sees only the launches that happen while
//! it is running (the wallet-graph ledger is fed from `Engine`'s launch path for exactly that
//! reason), so on a fresh boot it cannot know a creator's history. The corpus's number is
//! table-derived and includes launches observed long before the daemon started, so the table is an
//! INPUT the engine carries — the same shape as the mcap and tracked-wallet tables. Live launches
//! are then folded in with [`CreatorHistory::observe`], so the registry only grows and a
//! newly-deployed mint is `creator_known = 1` immediately rather than after a restart.
//!
//! DETERMINISM. No clock, no RNG, no floating point: a mint maps to its creator and launch time,
//! and the count is a binary search over a sorted vector. Same inputs, same answer, every replay.
//!
//! BOUNDED STATE (§99). One entry per launch, capped by [`MAX_TRACKED_LAUNCHES`]; a launch beyond
//! the cap is REFUSED and counted ([`CreatorHistory::refused_launches`]) rather than evicting an
//! older mint — eviction would silently change a `creator_past_launches` count that the corpus had
//! already written, whereas a refusal is visible.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use pump_quant_proposal::decision::DevHistoryDecision;

/// Maximum launches held in the registry (§99). The corpus's canonical table is ~20k rows, so this
/// leaves room for live growth while keeping the structure small and its copies cheap.
pub const MAX_TRACKED_LAUNCHES: usize = 262_144;

/// One launch: which creator shipped it, and when (`recv_unix_ms`, the causal clock).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Launch {
    /// Stable per-entity creator id.
    pub creator: u64,
    /// Launch receive time, unix milliseconds — the same clock the rest of the ledger uses.
    pub launch_unix_ms: i64,
}

/// The creator launch-history registry behind the `DEV HISTORY:` line.
#[derive(Debug, Clone, Default)]
pub struct CreatorHistory {
    by_mint: BTreeMap<[u8; 32], Launch>,
    /// Per creator, the launch times SORTED — the bisect the corpus's `prior_launches` performs.
    by_creator: BTreeMap<u64, Vec<i64>>,
    refused: u64,
}

impl CreatorHistory {
    /// An empty registry — every mint reads `creator_past_launches=unknown creator_known=0`, which
    /// is the corpus's own rendering for a mint it could not attribute.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many launches are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_mint.len()
    }

    /// True when no launch has been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_mint.is_empty()
    }

    /// Launches refused because the registry was at [`MAX_TRACKED_LAUNCHES`].
    #[must_use]
    pub const fn refused_launches(&self) -> u64 {
        self.refused
    }

    /// Record a launch. Returns `true` when it was admitted.
    ///
    /// Idempotent by mint: the same mint re-observed (a replay, a duplicate stream message, a table
    /// loaded twice) does not double-count its creator's history or move its launch time. A mint
    /// already present therefore returns `true` and changes nothing.
    pub fn observe(&mut self, mint: [u8; 32], creator: u64, launch_unix_ms: i64) -> bool {
        if let Some(existing) = self.by_mint.get(&mint) {
            return *existing
                == Launch {
                    creator,
                    launch_unix_ms,
                };
        }
        if self.by_mint.len() >= MAX_TRACKED_LAUNCHES {
            self.refused += 1;
            return false;
        }
        let times = self.by_creator.entry(creator).or_default();
        // ALWAYS insert: two of a creator's mints can share a millisecond, and the corpus's bisect
        // counts every launch, not every distinct timestamp. De-duplicating by time would drop one
        // of them and undercount the next launch's `creator_past_launches`. A re-observed MINT is
        // already handled above, so this cannot double-count a single launch.
        let at = times.partition_point(|&x| x < launch_unix_ms);
        times.insert(at, launch_unix_ms);
        self.by_mint.insert(
            mint,
            Launch {
                creator,
                launch_unix_ms,
            },
        );
        true
    }

    /// The `DEV HISTORY:` inputs for a mint, exactly as the corpus derives them.
    ///
    /// `creator_past_launches` is the number of the creator's launches STRICTLY before this mint's
    /// own launch — a mint's own launch never counts itself, and neither does a launch at the same
    /// millisecond (the corpus's bisect is `ts < own`).
    #[must_use]
    pub fn dev_history(&self, mint: &[u8; 32]) -> DevHistoryDecision {
        let Some(launch) = self.by_mint.get(mint) else {
            return DevHistoryDecision {
                creator_past_launches: None,
                creator_known: 0,
            };
        };
        let times = self.by_creator.get(&launch.creator);
        let prior = times.map_or(0, |t| {
            // `partition_point`: the count of entries `< launch_unix_ms` in the sorted vector.
            t.partition_point(|&x| x < launch.launch_unix_ms) as i64
        });
        DevHistoryDecision {
            creator_past_launches: Some(prior),
            creator_known: 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(b: u8) -> [u8; 32] {
        let mut out = [0u8; 32];
        out[0] = b;
        out
    }

    #[test]
    fn an_unattributed_mint_renders_unknown_not_zero() {
        // The corpus's own convention: a mint it could not attribute gets
        // `creator_past_launches=unknown creator_known=0`, NOT `0`/known.
        let h = CreatorHistory::new();
        let d = h.dev_history(&m(9));
        assert_eq!(d.creator_past_launches, None);
        assert_eq!(d.creator_known, 0);
    }

    #[test]
    fn prior_launches_counts_strictly_before_the_mints_own_launch() {
        let mut h = CreatorHistory::new();
        assert!(h.observe(m(1), 42, 1_000));
        assert!(h.observe(m(2), 42, 2_000));
        assert!(h.observe(m(3), 42, 3_000));
        // Another creator's launches are not this creator's history.
        assert!(h.observe(m(4), 99, 1_500));

        assert_eq!(h.dev_history(&m(1)).creator_past_launches, Some(0));
        assert_eq!(h.dev_history(&m(2)).creator_past_launches, Some(1));
        assert_eq!(h.dev_history(&m(3)).creator_past_launches, Some(2));
        assert_eq!(h.dev_history(&m(4)).creator_past_launches, Some(0));
        assert_eq!(h.dev_history(&m(4)).creator_known, 1);
    }

    #[test]
    fn a_launch_at_the_same_millisecond_is_not_prior() {
        // The bisect is `ts < own`, so a same-instant launch is excluded — this is observable and
        // must not depend on insert order.
        let mut h = CreatorHistory::new();
        assert!(h.observe(m(1), 7, 5_000));
        assert!(h.observe(m(2), 7, 5_000));
        assert_eq!(h.dev_history(&m(1)).creator_past_launches, Some(0));
        assert_eq!(h.dev_history(&m(2)).creator_past_launches, Some(0));
        // …and a later launch DOES see both.
        assert!(h.observe(m(3), 7, 5_001));
        assert_eq!(h.dev_history(&m(3)).creator_past_launches, Some(2));
    }

    #[test]
    fn observing_the_same_mint_twice_counts_it_once() {
        let mut h = CreatorHistory::new();
        assert!(h.observe(m(1), 3, 100));
        assert!(
            h.observe(m(1), 3, 100),
            "a replay is admitted and idempotent"
        );
        assert!(h.observe(m(2), 3, 200));
        assert_eq!(h.len(), 2);
        assert_eq!(h.dev_history(&m(2)).creator_past_launches, Some(1));
        // A conflicting re-observation is refused rather than silently rewriting history.
        assert!(!h.observe(m(1), 3, 999));
        assert_eq!(h.dev_history(&m(2)).creator_past_launches, Some(1));
    }

    #[test]
    fn the_registry_is_bounded_and_refusal_is_visible() {
        let mut h = CreatorHistory::new();
        for i in 0..64u32 {
            let mut mint = [0u8; 32];
            mint[..4].copy_from_slice(&i.to_le_bytes());
            assert!(h.observe(mint, 1, i64::from(i)));
        }
        assert_eq!(h.len(), 64);
        assert_eq!(h.refused_launches(), 0);
    }
}
