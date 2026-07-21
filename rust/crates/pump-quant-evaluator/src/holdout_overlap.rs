//! `holdout_overlap` — creator/cluster Tier-2 family-holdout leakage checker
//! (constitution §17, §53).
//!
//! Responsibility: prove, on the frozen-evaluator side, that no creator/cluster
//! *family* appears in both the training set and the holdout set. Tokens from
//! the same creator or wallet-cluster share hidden structure; if a family
//! straddles the split, the holdout is contaminated and out-of-sample metrics
//! are fiction. This is pure set logic over family identifiers — fully
//! laptop-testable and independent of the research harness that assembled the
//! sets.
//!
//! Integer-only (constitution §22): family ids are opaque `u64`; no floats.

use std::collections::BTreeSet;

/// Opaque creator/cluster family identifier. Ordering drives deterministic
/// output order only, never a statistic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FamilyId(pub u64);

/// Result of the train/holdout family-overlap check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overlap {
    /// True iff the split is clean (no family in both sets).
    pub is_clean: bool,
    /// Families that leaked across the split, in ascending [`FamilyId`] order.
    pub leaked: Vec<FamilyId>,
}

impl Overlap {
    /// Number of leaked families.
    pub fn leak_count(&self) -> usize {
        self.leaked.len()
    }
}

/// Assert zero family overlap between the train and holdout sets.
///
/// Responsibility (constitution §17, §53): compute the set intersection of
/// `train_family_ids` and `holdout_family_ids`. The result is
/// [`Overlap::is_clean`]` == true` iff the intersection is empty; any shared
/// families are reported in deterministic ascending order. Because both inputs
/// are `BTreeSet`s the intersection walk is ordered and the output is a pure,
/// deterministic function of the inputs.
pub fn holdout_overlap(
    train_family_ids: &BTreeSet<FamilyId>,
    holdout_family_ids: &BTreeSet<FamilyId>,
) -> Overlap {
    let leaked: Vec<FamilyId> = train_family_ids
        .intersection(holdout_family_ids)
        .copied()
        .collect();
    Overlap {
        is_clean: leaked.is_empty(),
        leaked,
    }
}
