//! Launch-discovery completeness auditor (constitution §62-M1 / criterion 73:
//! "complete-discovery-or-explicit-INCOMPLETE").
//!
//! Discovery must maximize recall (§14/§59) and M1 completion is blocked without
//! **proven or explicitly-INCOMPLETE** launch-universe coverage. This module is
//! the deterministic, laptop-buildable offline core of that proof: given a
//! known-universe fixture and the observed launch set, it computes recall /
//! coverage and emits `COMPLETE | INCOMPLETE` with the exact shortfall (the set
//! of known launches that were not observed). The live proof against a real
//! stream is the server half and is OUT OF SCOPE here.
//!
//! Distinct from the §21.5 `ActiveMarketUniverse` selector (criterion 90): this
//! audits *coverage of the launch feed*, it does not *select* active markets.
//!
//! # Constitution constraints (§22)
//!
//! Deterministic set arithmetic over integer launch ids, with `BTreeSet` for
//! stable ordering (no nondeterministic iteration). Recall is basis points
//! (integer), never a float.

use std::collections::BTreeSet;

/// Opaque launch identity (e.g. a creation-signature or mint hash).
pub type LaunchId = u64;

/// The measured coverage of an observed launch set against a known universe.
///
/// Responsibility: carry the raw counts, integer recall, and the exact
/// shortfall so an auditor can prove or refute completeness (§62-M1 /
/// criterion 73). Constitution §22: integer counts, bps recall.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CoverageAudit {
    /// Size of the known universe (deduplicated).
    pub known_count: u32,
    /// Size of the observed set (deduplicated).
    pub observed_count: u32,
    /// Known launches that were also observed (`|known ∩ observed|`).
    pub matched_count: u32,
    /// Recall in basis points: `matched * 10_000 / known` (10_000 = full
    /// recall). A vacuous empty known-universe is defined as full recall.
    pub recall_bps: u32,
    /// The shortfall: known launches NOT observed (`known \ observed`), sorted
    /// ascending for deterministic reporting.
    pub missing: Vec<LaunchId>,
    /// Observed launches NOT in the known universe (`observed \ known`), sorted
    /// ascending. Surfaced for diagnostics (fixture drift / over-collection);
    /// does not affect the recall verdict.
    pub unexpected: Vec<LaunchId>,
}

/// The completeness verdict emitted by the auditor.
///
/// Responsibility: the binary `COMPLETE | INCOMPLETE` decision plus the
/// explicit shortfall that criterion 73 requires. Constitution §22: integer bps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoverageVerdict {
    /// Every known launch was observed (recall == 10_000 bps).
    Complete,
    /// Coverage is incomplete; carries the missing launches and the recall.
    Incomplete {
        /// Known launches that were not observed, sorted ascending.
        shortfall: Vec<LaunchId>,
        /// Achieved recall in basis points (`< 10_000`).
        recall_bps: u32,
    },
}

/// Audit observed launch coverage against a known-universe fixture.
///
/// Recall `= |known ∩ observed| / |known|`, expressed in basis points. The
/// verdict is [`CoverageVerdict::Complete`] iff recall is exactly full
/// (`10_000` bps), i.e. every known launch was observed; otherwise
/// [`CoverageVerdict::Incomplete`] carrying the sorted shortfall. An empty known
/// universe is treated as vacuously [`CoverageVerdict::Complete`] with recall
/// `10_000` (there is nothing to miss).
///
/// Duplicates within either input are collapsed (a launch observed twice still
/// counts once), so the audit is robust to double-delivery.
///
/// Responsibility: the offline completeness core (§62-M1 / criterion 73).
/// Constitution §22: `BTreeSet` for deterministic ordering, integer bps recall,
/// `u128` widening on the recall multiply.
pub fn audit_launch_coverage(known: &[LaunchId], observed: &[LaunchId]) -> CoverageAudit {
    let known_set: BTreeSet<LaunchId> = known.iter().copied().collect();
    let observed_set: BTreeSet<LaunchId> = observed.iter().copied().collect();

    let missing: Vec<LaunchId> = known_set.difference(&observed_set).copied().collect();
    let unexpected: Vec<LaunchId> = observed_set.difference(&known_set).copied().collect();
    let matched_count = (known_set.len() - missing.len()) as u32;
    let known_count = known_set.len() as u32;

    let recall_bps = if known_count == 0 {
        10_000
    } else {
        ((matched_count as u128 * 10_000) / known_count as u128) as u32
    };

    CoverageAudit {
        known_count,
        observed_count: observed_set.len() as u32,
        matched_count,
        recall_bps,
        missing,
        unexpected,
    }
}

impl CoverageAudit {
    /// Derive the `COMPLETE | INCOMPLETE` verdict from this audit.
    ///
    /// Responsibility: turn measured coverage into the criterion-73 verdict.
    /// Constitution §22: integer comparison; explicit INCOMPLETE with shortfall.
    pub fn verdict(&self) -> CoverageVerdict {
        if self.recall_bps >= 10_000 {
            CoverageVerdict::Complete
        } else {
            CoverageVerdict::Incomplete {
                shortfall: self.missing.clone(),
                recall_bps: self.recall_bps,
            }
        }
    }
}
