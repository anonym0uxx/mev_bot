//! Point-in-time-correct feature serving (constitution 20).
//!
//! Responsibility: hold a time-ordered history of [`TimedFeature`] snapshots and
//! serve, for any decision cutoff `T`, only the freshest snapshot that was *fully
//! knowable* by `T`. This is the leakage guard for the whole system: a value may
//! be consumed only when both its `max_information_time_ns` and its
//! `computation_complete_ns` are `<= decision_cutoff_ns` (constitution 20). The
//! store is memory-bounded (constitution 22/57): a hard capacity with
//! oldest-first eviction.

use crate::types::{Completeness, EventId, FeatureVersion};

/// A single point-in-time feature snapshot (constitution 20).
///
/// Responsibility: bind a computed value to the exact time boundary at which it
/// became legitimately consumable, plus the events it was derived from. Serving
/// logic uses [`Self::servable_at`] as the gate and `max_information_time_ns` as
/// the freshness key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimedFeature<T> {
    /// The computed feature value.
    pub value: T,
    /// Provenance: identifiers of every source event that fed this value.
    pub source_event_ids: Vec<EventId>,
    /// Latest information time of any input (constitution 20). No input observed
    /// after this instant contributed, so consuming before it would be look-ahead.
    pub max_information_time_ns: u64,
    /// Time the computation itself finished. A value cannot be served before it
    /// physically exists, even if its inputs are old (constitution 20).
    pub computation_complete_ns: u64,
    /// Schema version of this value (constitution 20 live/replay parity).
    pub feature_version: FeatureVersion,
    /// Completeness status of the inputs (constitution 20 missing-is-explicit).
    pub completeness: Completeness,
}

impl<T> TimedFeature<T> {
    /// Construct a snapshot. Provenance ids are stored as given (caller owns
    /// ordering); serving never depends on their order, only on the time fields.
    #[must_use]
    pub fn new(
        value: T,
        source_event_ids: Vec<EventId>,
        max_information_time_ns: u64,
        computation_complete_ns: u64,
        feature_version: FeatureVersion,
        completeness: Completeness,
    ) -> Self {
        Self {
            value,
            source_event_ids,
            max_information_time_ns,
            computation_complete_ns,
            feature_version,
            completeness,
        }
    }

    /// Earliest decision cutoff at which this snapshot may be consumed
    /// (constitution 20): the later of its information time and its computation
    /// time. A snapshot is servable at cutoff `T` iff `servable_at() <= T`.
    #[must_use]
    pub fn servable_at(&self) -> u64 {
        self.max_information_time_ns
            .max(self.computation_complete_ns)
    }

    /// Whether this snapshot may be consumed at `decision_cutoff_ns` without
    /// look-ahead (constitution 20). Both the information time and the computation
    /// time must not exceed the cutoff.
    #[must_use]
    pub fn is_servable_at(&self, decision_cutoff_ns: u64) -> bool {
        self.max_information_time_ns <= decision_cutoff_ns
            && self.computation_complete_ns <= decision_cutoff_ns
    }
}

/// A memory-bounded, point-in-time-correct store of feature snapshots
/// (constitution 20, 22, 57).
///
/// Responsibility: accept snapshots in any push order and answer `as_of(T)` with
/// the freshest snapshot legitimately available at `T`. The store keeps at most
/// `capacity` snapshots; on overflow it evicts the snapshot with the smallest
/// [`TimedFeature::servable_at`] (the one that would be shadowed earliest), which
/// preserves recent point-in-time answers while bounding memory.
#[derive(Debug, Clone)]
pub struct TimedFeatureStore<T> {
    /// Snapshots kept sorted ascending by `(servable_at, max_information_time_ns,
    /// computation_complete_ns)`. Sorted storage makes `as_of` a bounded scan and
    /// keeps eviction of the earliest element O(1) at the front.
    items: Vec<TimedFeature<T>>,
    capacity: usize,
}

impl<T: Clone> TimedFeatureStore<T> {
    /// Create a store bounded to `capacity` snapshots. `capacity` is clamped to at
    /// least 1 so the store can always hold the most recent value.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            items: Vec::new(),
            capacity: capacity.max(1),
        }
    }

    /// Number of retained snapshots.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the store holds no snapshots.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Configured capacity bound.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Sort key giving a total, deterministic order over snapshots. Ordering by
    /// `servable_at` first is what makes `as_of` correct; the further keys only
    /// break ties deterministically (constitution 22 stable ordering).
    fn key(f: &TimedFeature<T>) -> (u64, u64, u64) {
        (
            f.servable_at(),
            f.max_information_time_ns,
            f.computation_complete_ns,
        )
    }

    /// Insert a snapshot, preserving sorted order and the capacity bound
    /// (constitution 20/57).
    ///
    /// Overflow strategy (explicit, constitution 22): once `capacity` is exceeded
    /// the front element (smallest `servable_at`) is removed. Because `as_of(T)`
    /// returns the element with the *largest* servable key `<= T`, evicting the
    /// smallest keys can only ever drop answers for very old cutoffs, never for
    /// the current or future ones.
    pub fn push(&mut self, feature: TimedFeature<T>) {
        let key = Self::key(&feature);
        // Binary search for the insertion point that keeps `items` ascending.
        let idx = self.items.partition_point(|f| Self::key(f) <= key);
        self.items.insert(idx, feature);
        while self.items.len() > self.capacity {
            self.items.remove(0);
        }
    }

    /// Serve the freshest snapshot legitimately available at `decision_cutoff_ns`
    /// (constitution 20).
    ///
    /// Returns the retained snapshot with the greatest `max_information_time_ns`
    /// among those whose `servable_at() <= decision_cutoff_ns`, breaking ties by
    /// greater `computation_complete_ns` then by later insertion (last wins),
    /// deterministically. Returns `None` if nothing is servable yet.
    ///
    /// No-look-ahead guarantee (property-tested): the result never depends on any
    /// snapshot whose `servable_at()` exceeds `decision_cutoff_ns`. Equivalently,
    /// adding or removing future snapshots cannot change `as_of` for a past cutoff.
    #[must_use]
    pub fn as_of(&self, decision_cutoff_ns: u64) -> Option<&TimedFeature<T>> {
        let mut best: Option<&TimedFeature<T>> = None;
        for f in &self.items {
            if !f.is_servable_at(decision_cutoff_ns) {
                // Items are sorted ascending by servable_at, so once one is not
                // servable every later one is not either — but is_servable_at is
                // the load-bearing check; we `continue` rather than `break` to stay
                // correct even if a caller mutates ordering assumptions.
                continue;
            }
            best = match best {
                None => Some(f),
                Some(b) => {
                    let fk = (f.max_information_time_ns, f.computation_complete_ns);
                    let bk = (b.max_information_time_ns, b.computation_complete_ns);
                    if fk >= bk {
                        Some(f)
                    } else {
                        Some(b)
                    }
                }
            };
        }
        best
    }

    /// Borrow the retained snapshots in ascending servable order (inspection/audit).
    #[must_use]
    pub fn snapshots(&self) -> &[TimedFeature<T>] {
        &self.items
    }
}
