//! The SocialSourceQualityLedger aggregate (constitution §29.8 / §29.9).
//!
//! # Responsibility
//! Hold the current classification per source with a bounded memory footprint, and
//! fold a fresh determinant bundle into a stored, decaying classification. This is
//! the research-plane ledger surface (§29.9 `source_quality_ledger` table analogue);
//! it is deterministic and integer, and it is a *research system* — its output
//! reaches production only through admission (§29.8), never as trade authority here.
//!
//! Memory-boundedness (§22 "memory-bounded where stateful"): the ledger keeps at most
//! `capacity` per-source entries; on overflow the least-recently-updated source is
//! evicted (deterministic by update order), so the structure never grows unbounded.

use crate::classification::{classify, Classification, ClassificationConfig, DeterminantBundle};

/// One stored per-source ledger entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceLedgerEntry {
    /// The source (account) id.
    pub source_id: u64,
    /// The most recent classification computed for the source.
    pub classification: Classification,
    /// A monotonically increasing update sequence number (assigned by the ledger),
    /// used purely for deterministic LRU eviction — not a wall-clock.
    pub update_seq: u64,
}

/// A bounded, deterministic per-source quality ledger.
///
/// Not `Default`: a capacity of zero is meaningless, so construction is explicit.
#[derive(Debug, Clone)]
pub struct SourceQualityLedger {
    capacity: usize,
    entries: Vec<SourceLedgerEntry>,
    next_seq: u64,
}

impl SourceQualityLedger {
    /// Create a ledger holding at most `capacity` sources (min 1).
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: Vec::new(),
            next_seq: 0,
        }
    }

    /// Number of sources currently tracked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the ledger tracks no sources.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Maximum number of sources retained.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Look up the current classification for a source, if tracked.
    #[must_use]
    pub fn get(&self, source_id: u64) -> Option<Classification> {
        self.entries
            .iter()
            .find(|e| e.source_id == source_id)
            .map(|e| e.classification)
    }

    /// Classify `bundle` under `cfg`, store the result for `source_id`, evicting the
    /// least-recently-updated source if capacity would be exceeded, and return the
    /// fresh classification.
    ///
    /// Fade-first semantics are inherited entirely from
    /// [`crate::classification::classify`]; the ledger adds only bounded storage.
    pub fn fold(
        &mut self,
        source_id: u64,
        bundle: &DeterminantBundle,
        cfg: &ClassificationConfig,
    ) -> Classification {
        let classification = classify(bundle, cfg);
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        if let Some(existing) = self.entries.iter_mut().find(|e| e.source_id == source_id) {
            existing.classification = classification;
            existing.update_seq = seq;
            return classification;
        }
        if self.entries.len() >= self.capacity {
            // Evict least-recently-updated (smallest update_seq). Deterministic.
            if let Some((idx, _)) = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.update_seq)
            {
                self.entries.swap_remove(idx);
            }
        }
        self.entries.push(SourceLedgerEntry {
            source_id,
            classification,
            update_seq: seq,
        });
        classification
    }
}
