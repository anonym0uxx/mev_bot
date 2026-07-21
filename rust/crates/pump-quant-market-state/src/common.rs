//! Shared primitives for the market-state reducers.
//!
//! ## Responsibility
//! Fixed-point ratio helpers and memory-bounded collection wrappers used by
//! every reducer in this crate. Centralizing them keeps the §22 no-float and
//! memory-bound invariants in one auditable place.

use std::collections::{BTreeMap, BTreeSet};

/// Opaque, caller-resolved entity identifier (a wallet, token account, fee
/// payer, funding root, cluster, creator, or narrative category).
///
/// ## Responsibility
/// This crate never parses pubkeys or hashes; upstream decoding resolves raw
/// on-chain identities to stable `u64` ids so the reducers stay pure integer
/// math (§22). Distinct real-world entities MUST map to distinct ids and the
/// same entity MUST map to the same id for the reducer counts to be meaningful.
pub type EntityId = u64;

/// Completeness of a derived value, mirroring the constitution's
/// UNKNOWN / INCOMPLETE labeling (§6.4): a reducer that has hit its memory
/// bound reports [`Completeness::Incomplete`] instead of silently under- or
/// over-counting.
///
/// Constitution: §6.4 ("When raw data is incomplete, label the result UNKNOWN,
/// INCOMPLETE, or UNRESOLVED. Never silently infer missing truth."), §99
/// (capacity-bounded structures with defined behavior at the bound).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Completeness {
    /// Every observed event was retained within capacity; counts are exact.
    Complete,
    /// A capacity bound was exceeded; some distinct entities could not be
    /// tracked, so counts are lower bounds and must be treated as INCOMPLETE.
    Incomplete,
}

impl Completeness {
    /// Merge two completeness states: the result is [`Completeness::Incomplete`]
    /// if either input is incomplete. Used when a snapshot aggregates several
    /// bounded structures.
    #[must_use]
    pub fn merge(self, other: Completeness) -> Completeness {
        match (self, other) {
            (Completeness::Complete, Completeness::Complete) => Completeness::Complete,
            _ => Completeness::Incomplete,
        }
    }
}

/// Compute an unsigned ratio in basis points (parts per 10 000) using a `u128`
/// intermediate so count/lamport ratios never overflow in practice.
///
/// Returns `None` only when `denominator == 0` (an undefined ratio, reported as
/// UNKNOWN by callers). When the exact result would exceed `u64::MAX` it
/// saturates to `u64::MAX` — documented, deterministic, and never a float
/// (§22). The `numerator * 10_000` step uses `saturating_mul`, so pathological
/// inputs saturate rather than wrap.
///
/// Constitution: §6.4 (derived ratios), §22 (integer/fixed-point only).
#[must_use]
pub fn ratio_bps(numerator: u128, denominator: u128) -> Option<u64> {
    if denominator == 0 {
        return None;
    }
    let scaled = numerator.saturating_mul(10_000);
    let bps = scaled / denominator;
    Some(u64::try_from(bps).unwrap_or(u64::MAX))
}

/// Compute a signed ratio in basis points, e.g. buy/sell imbalance
/// `(buys - sells) / (buys + sells)`.
///
/// Returns `None` when `denominator == 0`. Saturates to `i64::MIN`/`i64::MAX`
/// at the extremes. Uses an `i128` intermediate. No float (§22).
///
/// Constitution: §21.3 (market-wide buy/sell imbalance component).
#[must_use]
pub fn signed_ratio_bps(numerator: i128, denominator: i128) -> Option<i64> {
    if denominator == 0 {
        return None;
    }
    let scaled = numerator.saturating_mul(10_000);
    let bps = scaled / denominator;
    Some(i64::try_from(bps).unwrap_or(if bps.is_negative() {
        i64::MIN
    } else {
        i64::MAX
    }))
}

/// A memory-bounded set of [`EntityId`]s with a hard capacity.
///
/// ## Responsibility
/// Backs the "distinct X" counters in the reducers while honoring the §99
/// memory-bound law: once `capacity` distinct ids are held, further *new* ids
/// are rejected and the structure is flagged overflowed (its
/// [`Completeness`] becomes [`Completeness::Incomplete`]). Insertion order is
/// irrelevant to the count, and the backing [`BTreeSet`] gives deterministic
/// iteration for any debug/inspection use (§ multi-dim inspectability, criterion
/// 47).
#[derive(Clone, Debug)]
pub struct BoundedSet {
    inner: BTreeSet<EntityId>,
    capacity: usize,
    overflowed: bool,
}

impl BoundedSet {
    /// Create an empty bounded set holding at most `capacity` distinct ids.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        BoundedSet {
            inner: BTreeSet::new(),
            capacity,
            overflowed: false,
        }
    }

    /// Insert `id`. Returns `true` if the id is newly present (i.e. it was
    /// admitted and had not been seen before). A brand-new id that cannot be
    /// admitted because the set is full sets the overflow flag and returns
    /// `false`.
    pub fn insert(&mut self, id: EntityId) -> bool {
        if self.inner.contains(&id) {
            return false;
        }
        if self.inner.len() >= self.capacity {
            self.overflowed = true;
            return false;
        }
        self.inner.insert(id);
        true
    }

    /// Number of distinct ids currently held (a lower bound on the true count
    /// once [`Self::overflowed`] is `true`).
    #[must_use]
    pub fn len(&self) -> u32 {
        u32::try_from(self.inner.len()).unwrap_or(u32::MAX)
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Whether `id` is currently held.
    #[must_use]
    pub fn contains(&self, id: EntityId) -> bool {
        self.inner.contains(&id)
    }

    /// Whether capacity was ever exceeded.
    #[must_use]
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// Completeness derived from the overflow flag.
    #[must_use]
    pub fn completeness(&self) -> Completeness {
        if self.overflowed {
            Completeness::Incomplete
        } else {
            Completeness::Complete
        }
    }
}

/// A memory-bounded map from [`EntityId`] to a per-entity accumulator `V`.
///
/// ## Responsibility
/// Backs per-wallet / per-category running aggregates. Like [`BoundedSet`] it
/// caps the number of tracked keys (§99); a new key that would exceed capacity
/// is dropped and the map is flagged overflowed. Existing keys are always
/// updatable, so already-tracked entities never lose fidelity.
#[derive(Clone, Debug)]
pub struct BoundedMap<V> {
    inner: BTreeMap<EntityId, V>,
    capacity: usize,
    overflowed: bool,
}

impl<V> BoundedMap<V> {
    /// Create an empty bounded map holding at most `capacity` keys.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        BoundedMap {
            inner: BTreeMap::new(),
            capacity,
            overflowed: false,
        }
    }

    /// Get a mutable reference to the accumulator for `key`, inserting
    /// `default()` first if absent. Returns `None` (and flags overflow) only
    /// when `key` is new and the map is already at capacity.
    pub fn get_or_insert_with<F: FnOnce() -> V>(
        &mut self,
        key: EntityId,
        default: F,
    ) -> Option<&mut V> {
        if !self.inner.contains_key(&key) {
            if self.inner.len() >= self.capacity {
                self.overflowed = true;
                return None;
            }
            self.inner.insert(key, default());
        }
        self.inner.get_mut(&key)
    }

    /// Immutable reference to the accumulator for `key`, if tracked.
    #[must_use]
    pub fn get(&self, key: EntityId) -> Option<&V> {
        self.inner.get(&key)
    }

    /// Number of tracked keys.
    #[must_use]
    pub fn len(&self) -> u32 {
        u32::try_from(self.inner.len()).unwrap_or(u32::MAX)
    }

    /// Whether the map is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Deterministic iterator over `(key, value)` in ascending key order.
    pub fn iter(&self) -> impl Iterator<Item = (&EntityId, &V)> {
        self.inner.iter()
    }

    /// Deterministic iterator over the accumulator values in ascending key
    /// order.
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.inner.values()
    }

    /// Whether capacity was ever exceeded.
    #[must_use]
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// Completeness derived from the overflow flag.
    #[must_use]
    pub fn completeness(&self) -> Completeness {
        if self.overflowed {
            Completeness::Incomplete
        } else {
            Completeness::Complete
        }
    }
}
