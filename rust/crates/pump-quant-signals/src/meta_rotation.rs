//! MetaRotationState time-safe category-assignment validator (constitution
//! §21.4, criterion 81 — the pure, fixture-testable time-safety core).
//!
//! The full MetaRotationState feature family is research/governance (the
//! supervisor half). The piece that is pure logic — and the piece this module
//! provides — is the time-safety core of category assignment:
//!
//! - a **versioned-taxonomy resolver**: an assignment's category must exist in
//!   the taxonomy, and its `taxonomy_version` must be pinned to the taxonomy
//!   that was active at assignment time (§21.4: "taxonomy changes are versioned
//!   like the feature schema");
//! - a **monotonic-timestamp guard**: every `CategoryAssignment` is timestamped
//!   and **never retroactive** — an assignment is only visible/valid at an
//!   observation time on or after it was made (`assignment_ts <=
//!   observation_ts`). A future-dated assignment consumed at an earlier
//!   observation is look-ahead leakage and is rejected.
//!
//! # Constitution constraints (§22)
//!
//! Deterministic, integer-only, no wall-clock (timestamps are inputs).
//! `BTreeSet` gives stable membership iteration. No floats.

use std::collections::BTreeSet;

/// Opaque token identity (mint-hash), abstract to avoid a chain dependency.
pub type TokenId = u64;

/// A versioned narrative-category taxonomy snapshot (§21.4a).
///
/// Responsibility: the set of valid category ids at a pinned `version`.
/// Categories emerge and die, so the version is load-bearing. Constitution §22:
/// integer version, `BTreeSet` for deterministic membership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Taxonomy {
    /// Monotonic taxonomy version.
    pub version: u32,
    /// Category ids valid at this version.
    pub categories: BTreeSet<u32>,
}

impl Taxonomy {
    /// Build a taxonomy from a version and an iterator of category ids.
    ///
    /// Responsibility: convenience constructor (§21.4). Constitution §22: pure.
    pub fn new(version: u32, categories: impl IntoIterator<Item = u32>) -> Self {
        Taxonomy {
            version,
            categories: categories.into_iter().collect(),
        }
    }

    /// Whether `category_id` is defined in this taxonomy version.
    ///
    /// Responsibility: category-existence check (§21.4a). Constitution §22: pure.
    #[inline]
    pub fn contains(&self, category_id: u32) -> bool {
        self.categories.contains(&category_id)
    }
}

/// A timestamped, version-pinned category assignment (§21.4a/b).
///
/// Responsibility: the record the time-safety guard validates. `assignment_ts_ms`
/// is when the assignment was made; `observation_ts_ms` is the point-in-time at
/// which it is being consumed. `taxonomy_version` is the version pinned at
/// assignment time. Constitution §22: integer timestamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CategoryAssignment {
    /// Token being categorized.
    pub token: TokenId,
    /// Assigned category id.
    pub category_id: u32,
    /// Taxonomy version pinned at assignment time.
    pub taxonomy_version: u32,
    /// When the assignment was made (milliseconds).
    pub assignment_ts_ms: u64,
    /// The point-in-time at which the assignment is consumed (milliseconds).
    pub observation_ts_ms: u64,
}

/// Verdict of the time-safe category-assignment validator.
///
/// Responsibility: enumerate accept + every rejection reason so violations are
/// explicit and auditable (§21.4, criterion 81). Constitution §22: data only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignmentVerdict {
    /// Valid: category exists, version pinned correctly, and not retroactive.
    Accepted,
    /// `assignment_ts_ms > observation_ts_ms`: a future-dated assignment
    /// consumed earlier = look-ahead leakage (retroactive assignment rejected).
    RejectedRetroactive,
    /// `taxonomy_version` does not match the active taxonomy at assignment time.
    RejectedTaxonomyUnpinned,
    /// The category id is not defined in the pinned taxonomy version.
    RejectedUnknownCategory,
}

/// Validate a category assignment for time-safety and versioned-taxonomy
/// correctness (§21.4, criterion 81).
///
/// `active_taxonomy` is the taxonomy that was in force at the assignment's
/// `assignment_ts_ms`. Checks, in priority order:
///
/// 1. **Version pinning** — `assignment.taxonomy_version == active_taxonomy.version`,
///    else [`AssignmentVerdict::RejectedTaxonomyUnpinned`].
/// 2. **Category existence** — the category is defined in the taxonomy, else
///    [`AssignmentVerdict::RejectedUnknownCategory`].
/// 3. **Monotonic timestamp** — `assignment_ts_ms <= observation_ts_ms`, else
///    [`AssignmentVerdict::RejectedRetroactive`].
///
/// Passing all three yields [`AssignmentVerdict::Accepted`].
///
/// Responsibility: the pure time-safety core of MetaRotationState assignment
/// (§21.4). Constitution §22: integer comparisons, deterministic, no wall-clock.
pub fn validate_assignment(
    assignment: &CategoryAssignment,
    active_taxonomy: &Taxonomy,
) -> AssignmentVerdict {
    if assignment.taxonomy_version != active_taxonomy.version {
        return AssignmentVerdict::RejectedTaxonomyUnpinned;
    }
    if !active_taxonomy.contains(assignment.category_id) {
        return AssignmentVerdict::RejectedUnknownCategory;
    }
    if assignment.assignment_ts_ms > assignment.observation_ts_ms {
        return AssignmentVerdict::RejectedRetroactive;
    }
    AssignmentVerdict::Accepted
}
