//! `pump_quant_wallet_graph` — wallet intelligence for the pump-quant memecoin
//! scalping bot.
//!
//! This crate implements the deterministic, integer-only wallet-intelligence
//! primitives required by the constitution:
//!
//! * [`tier1_hot_summary`] — Section 28 Tier-1 *bounded production summaries*:
//!   deterministic, memory-bounded hot-path-eligible reducers (same-block
//!   co-buy counts, first-N buyer co-occurrence, cluster-adjusted breadth,
//!   synchronized-sell risk).
//! * [`tier2_wallet_graph`] — Section 28 Tier-2 *research and anti-leakage
//!   infrastructure*: offline connected-components / union-find over a typed,
//!   discovery-time-stamped edge set; creator / funding / operator family
//!   grouping; Section 53 family-holdout generation; Section 46
//!   activity-matched placebo cohorts.
//! * [`smart_money`] — Section 28 smart-money authentication: family-netted,
//!   self-dealing-excluded, luck-filtered realized-PnL screen plus the
//!   follower-executable *lagged-shadow* simulator (the only admissible
//!   definition of "smart money").
//! * [`deployer_credibility`] — Section 27 / §70.9 point-in-time
//!   deployer-credibility features (prior-CA count, serial-deploy flag,
//!   key / mutual-follower reach, verified-partnership vs self-claimed).
//!
//! # Determinism and arithmetic law (Section 22)
//!
//! No floating-point arithmetic appears anywhere in the outcome-controlling
//! logic. All money is in lamports (`u64` / `i128`), all ratios are in basis
//! points, and all price fixed-point uses [`PRICE_SCALE`]. Every multiplication
//! that could overflow is widened to `u128` first; conversions that could
//! truncate use saturating semantics by contract. There is no wall-clock, RNG,
//! network, or floating-point dependency; the one external dependency (a price
//! series for the lagged-shadow simulator) is modelled behind a trait and never
//! called by this crate.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

pub mod deployer_credibility;
pub mod smart_money;
pub mod tier1_hot_summary;
pub mod tier2_wallet_graph;

/// Basis-point denominator (100% = 10 000 bps).
pub const BPS_DENOM: u64 = 10_000;

/// Fixed-point scale for prices expressed as "scaled lamports per token base
/// unit" (a price of `p` lamports/unit is stored as `p * PRICE_SCALE`). Chosen
/// so that sub-lamport per-unit prices remain representable as integers.
pub const PRICE_SCALE: u128 = 1_000_000;

/// A wallet identity. Upstream ingestion assigns each 32-byte pubkey a stable
/// `u64` handle deterministically; this crate only ever compares handles, so
/// the mapping never affects outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WalletId(pub u64);

/// A token / mint identity (stable upstream-assigned handle).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TokenId(pub u64);

/// A funding / operator family identity — the Tier-2 graph unit at which
/// smart-money PnL is netted (Section 28 "operator-family level").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FamilyId(pub u64);

/// Compute `a * b / c` widening through `u128` to avoid intermediate overflow.
///
/// Returns `None` when `c == 0`. The result is truncated toward zero (integer
/// division), which is deterministic and identical on every platform.
#[must_use]
pub fn mul_div_u128(a: u128, b: u128, c: u128) -> Option<u128> {
    if c == 0 {
        return None;
    }
    // a and b are each <= u128::MAX by type; their product may exceed u128, so
    // callers must keep operands within a range whose product fits. All callers
    // in this crate operate on lamports (<= ~10^18) and bps / scale factors
    // (<= 10^6), whose products fit comfortably in u128.
    Some(a.saturating_mul(b) / c)
}

/// Integer median of a slice of `i128` values.
///
/// For an even count the mean of the two central elements is returned using
/// `i128` arithmetic (no float). Returns `None` for an empty slice. The input
/// is copied and sorted, so the caller's slice is left untouched and the result
/// is order-independent (deterministic).
#[must_use]
pub fn integer_median(values: &[i128]) -> Option<i128> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_unstable();
    let n = v.len();
    if n % 2 == 1 {
        Some(v[n / 2])
    } else {
        // Average of the two central elements; sum may be up to 2*i128::MAX in
        // magnitude conceptually, but for our lamport-scale PnL values it is far
        // inside range. Divide toward zero.
        Some((v[n / 2 - 1] + v[n / 2]) / 2)
    }
}

/// A deterministic, memory-bounded set of `u64` identities kept in sorted order.
///
/// Insertions beyond the configured capacity are refused and counted in
/// [`BoundedIdSet::overflow`], so the structure never grows past its bound
/// (Section 28 Tier-1 "bounded production summaries"; Section 57 memory law).
#[derive(Debug, Clone)]
pub struct BoundedIdSet {
    ids: Vec<u64>,
    cap: usize,
    overflow: u64,
}

impl BoundedIdSet {
    /// Create an empty set that will hold at most `cap` distinct ids.
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            ids: Vec::with_capacity(cap),
            cap,
            overflow: 0,
        }
    }

    /// Insert `id`. Returns `true` iff `id` was newly stored. When the set is
    /// already at capacity and `id` is not present, the id is dropped and the
    /// overflow counter is incremented (conservative: dropped ids may or may
    /// not have been duplicates, so [`Self::len`] is a lower bound on the true
    /// distinct count once overflow is non-zero).
    pub fn insert(&mut self, id: u64) -> bool {
        match self.ids.binary_search(&id) {
            Ok(_) => false,
            Err(pos) => {
                if self.ids.len() >= self.cap {
                    self.overflow = self.overflow.saturating_add(1);
                    false
                } else {
                    self.ids.insert(pos, id);
                    true
                }
            }
        }
    }

    /// Whether `id` is currently stored.
    #[must_use]
    pub fn contains(&self, id: u64) -> bool {
        self.ids.binary_search(&id).is_ok()
    }

    /// Number of distinct ids currently stored (bounded by capacity).
    #[must_use]
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Count of insertions refused because the set was at capacity.
    #[must_use]
    pub fn overflow(&self) -> u64 {
        self.overflow
    }

    /// Configured maximum number of distinct ids.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Sorted view of the stored ids.
    #[must_use]
    pub fn as_slice(&self) -> &[u64] {
        &self.ids
    }
}
