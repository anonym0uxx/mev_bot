//! Deterministic ordering of equal-timestamp events (§19 "Tie-breaking").
//!
//! Responsibility: when two events carry the *same* replay timestamp, the
//! replay engine must still order them identically on every run, or
//! `DecisionRecord`s would not be byte-equivalent across replays (§19's
//! reproducibility contract). The full constitution ordering is
//! `replay timestamp → source sequence → connection epoch → slot →
//! transaction index → signature → observation ID`; this crate implements the
//! stable leading key `(ts_ns, source, seq)` that the strategy seam needs,
//! where `seq` is the per-source monotonic sequence number that already folds
//! in the finer distinctions for a single source.
//!
//! No floating point, no allocation in the comparator, total order (§22).

use std::cmp::Ordering;

/// A monotonically increasing identifier for one observation source /
/// connection (e.g. a specific LaserStream connection or the earliest-source
/// feed).
///
/// Responsibility: give the tie-break a stable, integer secondary key so that
/// two events sharing a timestamp are ordered by *which source produced them*
/// before falling back to per-source sequence. Wrapped in a newtype so it
/// cannot be accidentally compared against a `seq` or a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceId(pub u16);

/// The sort key for a replay event, ordered exactly `(ts_ns, source, seq)`.
///
/// Responsibility: be the single source of truth for event ordering under
/// equal timestamps (§19). The derived `Ord` is lexicographic over the fields
/// **in declaration order**, which is precisely `ts_ns` then `source` then
/// `seq` — do not reorder the fields without updating the ordering contract.
///
/// All fields are integers (§22: no floating point in outcome-controlling
/// logic). The key is `Copy` and comparison allocates nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventKey {
    /// Replay timestamp in nanoseconds — the primary ordering key (§19).
    pub ts_ns: u64,
    /// The producing source / connection — first tie-breaker when timestamps
    /// collide.
    pub source: SourceId,
    /// Per-source monotonic sequence number — final tie-breaker, guaranteeing
    /// a total order among events from the same source at the same instant.
    pub seq: u64,
}

impl EventKey {
    /// Construct an event key from its three integer components.
    ///
    /// Responsibility: ergonomic constructor kept `pub` for tests (§22).
    #[must_use]
    pub const fn new(ts_ns: u64, source: u16, seq: u64) -> Self {
        Self {
            ts_ns,
            source: SourceId(source),
            seq,
        }
    }
}

/// The canonical tie-break comparator: order two events by
/// `(ts_ns, source, seq)` (§19).
///
/// Responsibility: provide a named, total-order comparison usable directly as
/// a `sort_by` closure so call sites never re-implement (and drift from) the
/// ordering. Equivalent to `a.cmp(b)` on [`EventKey`]; exposed as a free
/// function to make the ordering contract explicit and greppable.
#[must_use]
pub fn tie_break_cmp(a: &EventKey, b: &EventKey) -> Ordering {
    a.cmp(b)
}

/// Sort `events` in place into deterministic replay order.
///
/// Responsibility: apply [`tie_break_cmp`] with a **stable** sort, so that any
/// two elements comparing equal (identical `ts_ns`, `source`, and `seq` —
/// e.g. genuine duplicates the caller has not yet deduplicated) retain their
/// original relative order. Because the sort is stable, ordering is fully
/// determined by the input contents *and* input order, which is exactly the
/// reproducibility §19 requires. `EventKey` is `Copy`, so this reorders in
/// place with no per-element allocation.
pub fn stable_tie_break_sort(events: &mut [EventKey]) {
    events.sort_by(tie_break_cmp);
}
