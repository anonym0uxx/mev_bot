//! Cross-poll dedupe by content id — the Rust twin of the Python adapters'
//! `seen: set[str]`, hardened with a bound.
//!
//! The Python twins keep an unbounded `set` for the life of the process; a
//! long-running watch against a firehose would grow it forever. Here the set
//! is paired with an insertion-order ring: past [`DedupeRing::cap`] distinct
//! ids the oldest is evicted (§99-spirit bounding). Behavior is identical to
//! the Python until the cap is reached — and the cap (65 536 ids) is far
//! beyond any realistic overlap window between polls.
//!
//! Python semantics preserved exactly: an EMPTY id is never deduplicated
//! (`if tid and tid in seen: continue`) — a vendor object without an id is
//! always emitted, and never occupies ring space.

use std::collections::HashSet;
use std::collections::VecDeque;

/// Default capacity used by every subcommand.
pub const DEFAULT_CAP: usize = 65_536;

/// Bounded seen-set with FIFO eviction. Pure state machine (§22): no clock.
pub struct DedupeRing {
    cap: usize,
    seen: HashSet<String>,
    order: VecDeque<String>,
}

impl DedupeRing {
    /// A ring remembering at most `cap` distinct ids (`cap` >= 1).
    #[must_use]
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            seen: HashSet::new(),
            order: VecDeque::new(),
        }
    }

    /// Record `id`; returns `true` when the event should be EMITTED (first
    /// sighting, or an empty id which is never deduplicated).
    pub fn insert(&mut self, id: &str) -> bool {
        if id.is_empty() {
            return true;
        }
        if self.seen.contains(id) {
            return false;
        }
        if self.order.len() == self.cap {
            if let Some(oldest) = self.order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
        self.seen.insert(id.to_string());
        self.order.push_back(id.to_string());
        true
    }

    /// Number of distinct ids currently remembered — mirrors the Python
    /// `len(seen)` in the watch diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// True when nothing has been recorded yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sighting_emits_duplicate_skips() {
        let mut r = DedupeRing::new(8);
        assert!(r.insert("a"));
        assert!(!r.insert("a"));
        assert!(r.insert("b"));
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn empty_id_always_emits_and_never_occupies_space() {
        let mut r = DedupeRing::new(2);
        assert!(r.insert(""));
        assert!(r.insert(""));
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn eviction_is_fifo_and_bounded() {
        let mut r = DedupeRing::new(2);
        assert!(r.insert("a"));
        assert!(r.insert("b"));
        assert!(r.insert("c")); // evicts "a"
        assert_eq!(r.len(), 2);
        assert!(r.insert("a"), "evicted id is fresh again");
        assert!(!r.insert("c"), "recent id still deduplicated");
    }
}
