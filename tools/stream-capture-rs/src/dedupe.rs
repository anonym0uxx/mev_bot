//! Bounded cross-event dedupe by content id — copied verbatim from
//! `tools/social-ingest-https-rs/src/dedupe.rs` (§99-spirit bounding).
//!
//! The webhook lane keys the ring by transaction signature (Helius retries a
//! delivery up to 3× on slow/failed responses, and multiple webhooks can
//! overlap): first sighting emits, repeats are counted and skipped. An EMPTY
//! id is never deduplicated — an object without a signature is always emitted
//! (fail-open-as-absence: the edge never discards data it cannot classify).

use std::collections::HashSet;
use std::collections::VecDeque;

/// Default capacity for high-volume lanes.
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

    /// Number of distinct ids currently remembered.
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
