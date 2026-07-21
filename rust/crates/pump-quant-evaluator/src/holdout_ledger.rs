//! `holdout_ledger` — deterministic holdout-access reuse accounting
//! (constitution §19, §53).
//!
//! Responsibility: make silent re-tuning against a holdout *detectable*. Each
//! holdout set is keyed by a content hash; the ledger records how many times it
//! has been touched and enforces an access budget. A second (or budget-exceeding)
//! access is flagged as reuse — the fingerprint of tuning a model against the
//! very data meant to validate it. The persistent store and governance response
//! live in the supervisor; the hash-and-count logic is this laptop leaf.
//!
//! Integer-only (constitution §22): counts are `u32`, the key is a 64-bit
//! content hash; no floats. The hash reuses the frozen-evaluator FNV-1a digest
//! (`evaluator_pin`) so the same bytes always key to the same slot.

use crate::evaluator_pin::fnv1a_64;
use std::collections::BTreeMap;

/// Content hash identifying a holdout set (constitution §19).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HoldoutHash(pub u64);

/// Deterministically hash a holdout set's member ids into a [`HoldoutHash`].
///
/// Responsibility (constitution §19): identical membership → identical key, so
/// re-presenting the same holdout under a different name still collides. The ids
/// are folded in *sorted, de-duplicated* order via a `BTreeSet` so that member
/// *order* does not change the key — the set is the identity, not the listing.
pub fn holdout_hash(member_ids: &[u64]) -> HoldoutHash {
    // Canonicalize: sort + dedup so the hash is a function of the set, not order.
    let set: std::collections::BTreeSet<u64> = member_ids.iter().copied().collect();
    let mut bytes = Vec::with_capacity(set.len() * 8);
    for id in set {
        bytes.extend_from_slice(&id.to_le_bytes());
    }
    HoldoutHash(fnv1a_64(&bytes))
}

/// Per-holdout access accounting record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessRecord {
    /// Number of accesses recorded so far.
    pub count: u32,
    /// Maximum accesses permitted before the budget is exceeded.
    pub budget: u32,
}

/// Outcome of attempting to access a holdout set (constitution §19).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessOutcome {
    /// Access granted within budget.
    Granted {
        /// The access number this call represents (1 for the first access).
        access_no: u32,
        /// Accesses remaining after this one.
        remaining: u32,
        /// True iff this is a repeat access (`access_no > 1`) — a reuse flag,
        /// benign only within budget but always worth surfacing.
        reused: bool,
    },
    /// The access budget has been exceeded — silent re-tuning suspected.
    BudgetExceeded {
        /// The access number this over-budget call represents.
        access_no: u32,
        /// The budget that was exceeded.
        budget: u32,
    },
    /// The holdout set was never registered — an unaccounted access.
    Unregistered,
}

/// Deterministic holdout-access ledger keyed by content hash.
///
/// Responsibility (constitution §19): track access counts against per-holdout
/// budgets so reuse is detectable. Backed by a `BTreeMap` for deterministic
/// iteration; contains no wall-clock, RNG, or floats.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HoldoutLedger {
    records: BTreeMap<HoldoutHash, AccessRecord>,
}

impl HoldoutLedger {
    /// An empty ledger.
    pub fn new() -> Self {
        HoldoutLedger {
            records: BTreeMap::new(),
        }
    }

    /// Register a holdout set with an access budget.
    ///
    /// Responsibility (constitution §19): declare a holdout and how many times
    /// it may legitimately be touched (typically once). Re-registering the same
    /// hash resets its budget and zeroes its count — an explicit governance act,
    /// distinct from a silent access. Returns the prior record if one existed.
    pub fn register(&mut self, hash: HoldoutHash, budget: u32) -> Option<AccessRecord> {
        self.records.insert(hash, AccessRecord { count: 0, budget })
    }

    /// Look up the current record for a holdout set, if registered.
    pub fn record(&self, hash: HoldoutHash) -> Option<AccessRecord> {
        self.records.get(&hash).copied()
    }

    /// Record one access against a holdout set.
    ///
    /// Responsibility (constitution §19): increment the access count and decide
    /// the outcome. An unregistered hash yields [`AccessOutcome::Unregistered`]
    /// and is *not* counted (there is no budget to charge it against). A
    /// registered access increments `count` (saturating by contract — the count
    /// is diagnostic and must never wrap) and yields [`AccessOutcome::Granted`]
    /// while `count ≤ budget`, else [`AccessOutcome::BudgetExceeded`]. The
    /// `reused` flag on a granted access marks any repeat touch.
    pub fn record_access(&mut self, hash: HoldoutHash) -> AccessOutcome {
        let Some(rec) = self.records.get_mut(&hash) else {
            return AccessOutcome::Unregistered;
        };
        rec.count = rec.count.saturating_add(1);
        let access_no = rec.count;
        if access_no > rec.budget {
            AccessOutcome::BudgetExceeded {
                access_no,
                budget: rec.budget,
            }
        } else {
            AccessOutcome::Granted {
                access_no,
                remaining: rec.budget - access_no,
                reused: access_no > 1,
            }
        }
    }
}
