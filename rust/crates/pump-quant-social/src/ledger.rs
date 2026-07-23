//! The SocialSourceQualityLedger aggregate (constitution §29.8 / §29.9).
//!
//! # Responsibility
//! Hold the current classification per source with a bounded memory footprint, and
//! fold a fresh determinant bundle into a stored, decaying classification. This is
//! the research-plane ledger surface (§29.9 `source_quality_ledger` table analogue);
//! it is deterministic and integer, and it is a *research system* — its output
//! reaches production only through admission (§29.8), never as trade authority here.
//!
//! Two bounded per-source surfaces live here:
//! * [`SourceQualityLedger`] — *what kind of source is this*: the classification
//!   (alpha vs trash) folded from the determinant bundle.
//! * [`SourceOutcomeLedger`] — *did the source actually earn net SOL*: realized
//!   net-SOL attribution keyed on a fully-qualified [`SourceRef`] (kind + id), so a
//!   paid Discord ALPHA room is graded on the SOL IT earned, distinct from every
//!   other source (§74 net-SOL, §71 reflection integrity). This is the mechanism
//!   the engine later feeds so reflection can up/down-weight or retire a room.
//!
//! Memory-boundedness (§22 "memory-bounded where stateful"): each ledger keeps at
//! most `capacity` per-source entries; on overflow the least-recently-updated source
//! is evicted (deterministic by update order), so the structure never grows unbounded.

use crate::classification::{classify, Classification, ClassificationConfig, DeterminantBundle};
use crate::types::SourceRef;

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

/// One stored per-source realized-outcome entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceOutcomeEntry {
    /// The fully-qualified source (kind + id).
    pub source: SourceRef,
    /// Realized net SOL attributed to this source, in signed lamports (§74).
    pub net_sol_lamports: i64,
    /// Number of reconciled realized outcomes folded in.
    pub trade_count: u64,
    /// Monotonic update sequence for deterministic LRU eviction — not a clock.
    pub update_seq: u64,
}

/// A bounded, deterministic per-source realized-net-SOL ledger (§29.8 / §71 / §74).
///
/// Where [`SourceQualityLedger`] answers *what kind of source is this*, this answers
/// *did the source actually earn net SOL*. It is keyed on the fully-qualified
/// [`SourceRef`] (kind + id), so a Discord alpha room accrues its OWN realized net —
/// never lumped with an X account that happens to share a numeric id — which is the
/// seam reflection uses to up/down-weight or retire a paid room. Not `Default`: a
/// capacity of zero is meaningless, so construction is explicit.
///
/// Bounded exactly like [`SourceQualityLedger`]: at most `capacity` sources, LRU
/// eviction of the least-recently-updated on overflow (§22 memory-bounded).
///
/// Overflow (§22, explicit): net-SOL uses **saturating** signed add — a lamport
/// total beyond `i64` range is physically impossible (Solana total supply is far
/// below `i64::MAX` lamports), so saturation is a safe-by-contract clamp that can
/// never silently wrap money. The trade count saturates on `u64` likewise.
#[derive(Debug, Clone)]
pub struct SourceOutcomeLedger {
    capacity: usize,
    entries: Vec<SourceOutcomeEntry>,
    next_seq: u64,
}

impl SourceOutcomeLedger {
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

    /// The stored entry for a source, if tracked.
    #[must_use]
    pub fn get(&self, source: SourceRef) -> Option<SourceOutcomeEntry> {
        self.entries.iter().find(|e| e.source == source).copied()
    }

    /// Realized net SOL attributed to a source, in signed lamports. `0` when the
    /// source is untracked (no evidence, never a loss).
    #[must_use]
    pub fn net_sol(&self, source: SourceRef) -> i64 {
        self.get(source).map_or(0, |e| e.net_sol_lamports)
    }

    /// Number of reconciled realized outcomes recorded for a source (`0` when
    /// untracked).
    #[must_use]
    pub fn trade_count(&self, source: SourceRef) -> u64 {
        self.get(source).map_or(0, |e| e.trade_count)
    }

    /// Record (reconcile) one realized trade outcome for `source`: `net_sol_lamports`
    /// is the signed net result in lamports (negative for a loss). Accumulates with
    /// saturating add (safe-by-contract, see type docs), evicting the
    /// least-recently-updated source if a NEW source would exceed capacity. §74.
    pub fn record(&mut self, source: SourceRef, net_sol_lamports: i64) {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        if let Some(existing) = self.entries.iter_mut().find(|e| e.source == source) {
            existing.net_sol_lamports = existing.net_sol_lamports.saturating_add(net_sol_lamports);
            existing.trade_count = existing.trade_count.saturating_add(1);
            existing.update_seq = seq;
            return;
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
        self.entries.push(SourceOutcomeEntry {
            source,
            net_sol_lamports,
            trade_count: 1,
            update_seq: seq,
        });
    }

    /// Total realized net SOL across all tracked sources, in signed lamports.
    /// Saturating fold (safe-by-contract). §22 / §74.
    #[must_use]
    pub fn total_net_sol(&self) -> i64 {
        self.entries
            .iter()
            .fold(0i64, |acc, e| acc.saturating_add(e.net_sol_lamports))
    }
}
