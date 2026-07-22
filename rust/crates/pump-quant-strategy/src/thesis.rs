//! # thesis — deterministic per-entry thesis + invalidation (criteria 43, 44)
//!
//! Every entry stores an explicit, machine-readable [`Thesis`]: the set of
//! must-remain-true (`required`) predicates and the set of if-triggered
//! (`invalidation`) predicates, compiled deterministically at entry from the
//! decision-time inputs ([`build_thesis`]) and serializable for storage
//! ([`Thesis::canonical_bytes`]).
//!
//! [`evaluate_thesis`] is the deterministic invalidation predicate (criterion
//! 44): given the current reducer/feature state it returns
//! [`ThesisVerdict::Holds`] or [`ThesisVerdict::Invalidated`], and the result
//! feeds a forced hold/exit action ([`forced_action`]). There is **no** model /
//! LLM override input to this function — a high entry score or a language model
//! cannot flip an `Invalidated` verdict, because the boundary does not exist in
//! the type signature.
//!
//! ## Constitution
//! §22: no floats; feature values are `i64` fixed-point, completeness/confidence
//! in bps, freshness in ns. Deterministic: identical inputs → identical thesis
//! and identical verdict. Missing/stale/incomplete evidence for a required
//! condition is conservatively treated as *not satisfied* (never defaulted to a
//! passing number), consistent with the missingness law.

use crate::strategy_id::fnv1a_64;

// ===========================================================================
// Condition model
// ===========================================================================

/// Required direction of a thesis condition relative to its threshold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// The observed feature value must be `>=` the threshold.
    AtLeast,
    /// The observed feature value must be `<=` the threshold.
    AtMost,
}

impl Direction {
    /// Canonical discriminant byte.
    #[inline]
    pub fn tag(self) -> u8 {
        match self {
            Direction::AtLeast => 0,
            Direction::AtMost => 1,
        }
    }
}

/// One compiled thesis condition over the registered feature schema.
///
/// A condition is *satisfied* by an observation iff the feature is fresh enough,
/// complete enough, and its value meets the direction/threshold. Ad-hoc
/// predicates are impossible: every condition names a `feature_id` from the
/// schema and a fixed comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThesisCondition {
    /// Registered feature-schema id this condition reads.
    pub feature_id: u32,
    /// Required direction relative to `threshold_fp`.
    pub direction: Direction,
    /// Comparison threshold, fixed-point.
    pub threshold_fp: i64,
    /// Minimum completeness (bps) for the observation to count.
    pub min_completeness_bps: u32,
    /// Maximum age (ns) beyond which the observation is stale.
    pub freshness_bound_ns: u64,
}

impl ThesisCondition {
    /// Canonical byte encoding of this condition (little-endian, fixed layout).
    fn write_canonical(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.feature_id.to_le_bytes());
        out.push(self.direction.tag());
        out.extend_from_slice(&self.threshold_fp.to_le_bytes());
        out.extend_from_slice(&self.min_completeness_bps.to_le_bytes());
        out.extend_from_slice(&self.freshness_bound_ns.to_le_bytes());
    }

    /// Whether `obs` satisfies this condition at `now_ns`.
    ///
    /// Requires freshness (`now − obs_ts <= bound`), completeness (`>= min`), and
    /// the direction/threshold relation. A missing observation is *not*
    /// satisfied (handled by [`evaluate_thesis`]).
    pub fn satisfied_by(&self, obs: &FeatureObservation, now_ns: u64) -> bool {
        let age = now_ns.saturating_sub(obs.observed_ts_ns);
        if age > self.freshness_bound_ns {
            return false;
        }
        if obs.completeness_bps < self.min_completeness_bps {
            return false;
        }
        match self.direction {
            Direction::AtLeast => obs.value_fp >= self.threshold_fp,
            Direction::AtMost => obs.value_fp <= self.threshold_fp,
        }
    }
}

// ===========================================================================
// Thesis type + deterministic construction (leaf: th_build)
// ===========================================================================

/// The deterministic entry inputs a thesis is compiled from.
///
/// Everything a [`Thesis`] contains is derived from this record, so identical
/// inputs always produce an identical (and identically-hashing) thesis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThesisInputs {
    /// Entry mode id (part of the thesis identity).
    pub entry_mode: u16,
    /// Setup archetype id (part of the thesis identity).
    pub archetype: u16,
    /// Entry timestamp (ns).
    pub entry_ts_ns: u64,
    /// Compiled must-remain-true conditions (schema-derived, order significant).
    pub required: Vec<ThesisCondition>,
    /// Compiled if-triggered invalidation conditions (order significant).
    pub invalidation: Vec<ThesisCondition>,
    /// Evidence reference ids available at entry.
    pub evidence_refs: Vec<u64>,
}

/// A deterministic per-entry thesis (criterion 43).
///
/// The `thesis_id` is the canonical digest of the compiled thesis, so two
/// entries with identical inputs share an id and any change changes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thesis {
    /// Canonical digest identity of this thesis.
    pub thesis_id: u64,
    /// Entry mode id.
    pub entry_mode: u16,
    /// Setup archetype id.
    pub archetype: u16,
    /// Entry timestamp (ns).
    pub created_at_ns: u64,
    /// Must-remain-true conditions.
    pub required: Vec<ThesisCondition>,
    /// If-triggered invalidation conditions.
    pub invalidation: Vec<ThesisCondition>,
    /// Evidence references captured at entry.
    pub evidence_refs: Vec<u64>,
}

impl Thesis {
    /// Canonical, length-framed byte serialization for storage / hashing.
    ///
    /// Excludes `thesis_id` itself (which is derived from these bytes). Fully
    /// deterministic and unambiguous (length prefixes on every vector).
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&self.entry_mode.to_le_bytes());
        out.extend_from_slice(&self.archetype.to_le_bytes());
        out.extend_from_slice(&self.created_at_ns.to_le_bytes());
        out.extend_from_slice(&(self.required.len() as u64).to_le_bytes());
        for c in &self.required {
            c.write_canonical(&mut out);
        }
        out.extend_from_slice(&(self.invalidation.len() as u64).to_le_bytes());
        for c in &self.invalidation {
            c.write_canonical(&mut out);
        }
        out.extend_from_slice(&(self.evidence_refs.len() as u64).to_le_bytes());
        for r in &self.evidence_refs {
            out.extend_from_slice(&r.to_le_bytes());
        }
        out
    }
}

/// Compile a deterministic [`Thesis`] from entry inputs (leaf **th_build**).
///
/// Pure and fixture-testable: identical [`ThesisInputs`] yield a byte-identical
/// thesis with the same `thesis_id`. No wall-clock — `created_at_ns` is the
/// caller-provided entry timestamp.
pub fn build_thesis(inputs: &ThesisInputs) -> Thesis {
    let mut thesis = Thesis {
        thesis_id: 0,
        entry_mode: inputs.entry_mode,
        archetype: inputs.archetype,
        created_at_ns: inputs.entry_ts_ns,
        required: inputs.required.clone(),
        invalidation: inputs.invalidation.clone(),
        evidence_refs: inputs.evidence_refs.clone(),
    };
    thesis.thesis_id = fnv1a_64(&thesis.canonical_bytes());
    thesis
}

// ===========================================================================
// State + deterministic invalidation predicate (leaf: th_evaluate)
// ===========================================================================

/// A single feature observation from the reducer / feature engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeatureObservation {
    /// Registered feature-schema id.
    pub feature_id: u32,
    /// Observed value, fixed-point.
    pub value_fp: i64,
    /// Completeness in bps.
    pub completeness_bps: u32,
    /// Timestamp the value was observed (ns).
    pub observed_ts_ns: u64,
}

/// The current reducer/feature state a thesis is evaluated against.
///
/// A thin, deterministic lookup over a slice of observations — no map ordering
/// nondeterminism, first match by `feature_id` wins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThesisState<'a> {
    /// The current observations, keyed by `feature_id` on lookup.
    pub observations: &'a [FeatureObservation],
}

impl<'a> ThesisState<'a> {
    /// Look up the first observation for `feature_id`, if present.
    pub fn get(&self, feature_id: u32) -> Option<&FeatureObservation> {
        self.observations
            .iter()
            .find(|o| o.feature_id == feature_id)
    }
}

/// The deterministic thesis verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThesisVerdict {
    /// Every required condition holds and no invalidation condition triggered.
    Holds,
    /// A required condition failed (or its evidence is missing/stale) **or** an
    /// invalidation condition triggered — the position must act.
    Invalidated,
}

/// The forced action a verdict compels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForcedAction {
    /// Continue holding — thesis intact.
    Hold,
    /// Force an exit — thesis invalidated, non-overridable.
    ForceExit,
}

/// The deterministic invalidation predicate (leaf **th_evaluate**, criterion 44).
///
/// Returns [`ThesisVerdict::Invalidated`] iff **any** required condition is not
/// satisfied by the current state (including because its evidence is missing,
/// stale, or incomplete) **or** **any** invalidation condition is triggered
/// (its predicate is met). Otherwise [`ThesisVerdict::Holds`].
///
/// There is deliberately no model/score/override parameter: a language model or a
/// high entry score cannot change the outcome, satisfying the
/// "LLM-cannot-override-invalidation" boundary by construction.
pub fn evaluate_thesis(thesis: &Thesis, state: &ThesisState, now_ns: u64) -> ThesisVerdict {
    // Every must-remain-true condition must be satisfied by fresh, complete
    // evidence; a missing observation is not satisfied.
    for cond in &thesis.required {
        match state.get(cond.feature_id) {
            Some(obs) if cond.satisfied_by(obs, now_ns) => {}
            _ => return ThesisVerdict::Invalidated,
        }
    }
    // Any triggered invalidation condition invalidates the thesis.
    for cond in &thesis.invalidation {
        if let Some(obs) = state.get(cond.feature_id) {
            if cond.satisfied_by(obs, now_ns) {
                return ThesisVerdict::Invalidated;
            }
        }
    }
    ThesisVerdict::Holds
}

/// Map a thesis verdict to its forced action (non-overridable).
#[inline]
pub fn forced_action(verdict: ThesisVerdict) -> ForcedAction {
    match verdict {
        ThesisVerdict::Holds => ForcedAction::Hold,
        ThesisVerdict::Invalidated => ForcedAction::ForceExit,
    }
}
