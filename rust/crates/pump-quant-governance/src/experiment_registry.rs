//! Narrative/social/meta feature-experiment registry + admission binding
//! (constitution §46; criterion 84).
//!
//! ## Why this exists
//! The generic feature-admission guard (`pump_quant_strategy::feature_admission`)
//! requires that a feature carry *some* registered experiment id, but criterion 84
//! is stricter for the narrative/social/meta feature families: they may not reach
//! shadow until the **two specific attention-alpha experiments** are registered
//! *and passing*:
//!
//! * **Experiment #2 — meta-rotation predictiveness.** Hypothesis: the
//!   cross-narrative meta-rotation state (which narrative cohort attention is
//!   rotating *into*) has out-of-sample predictive value for forward returns of
//!   tokens in the inflowing cohort, beyond per-token momentum.
//! * **Experiment #3 — source-tier value.** Hypothesis: the source *tier* of an
//!   attention signal (which class of venue/author surfaced it) carries
//!   differential, separable predictive value — a tier-1 surface is not
//!   interchangeable with a tier-3 surface at equal raw velocity.
//!
//! This module seeds those two experiments as constants (id + hypothesis), states
//! which feature kinds are bound to them ([`requires_experiments`]), and provides
//! the fail-closed admission predicate ([`admit_to_shadow`]) the engine consults
//! before a narrative-scoped feature may enter shadow. The generic per-feature
//! guard stays where it is; this is the *family-scoped* binding on top of it.
//!
//! ## Constitution constraints (§19, §22)
//! Pure, total, deterministic. No floating point, no wall-clock, no RNG, no I/O.
//! Required-experiment order is fixed (ascending [`ExperimentId`]) so the reject
//! reason for a given registry state is always identical.

/// Stable identifier for a registered research experiment (§46 / §56.3).
///
/// A newtype over `u16` so an experiment id can never be confused with a feature
/// id, causal-hypothesis id, or lane. Ordering is the numeric order and drives
/// the deterministic evaluation order of [`admit_to_shadow`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExperimentId(pub u16);

/// **Experiment #2 — meta-rotation predictiveness** (criterion 84).
pub const EXPERIMENT_META_ROTATION_PREDICTIVENESS: ExperimentId = ExperimentId(2);

/// **Experiment #3 — source-tier value** (criterion 84).
pub const EXPERIMENT_SOURCE_TIER_VALUE: ExperimentId = ExperimentId(3);

/// A seeded experiment: its stable id and the exact causal hypothesis it tests.
///
/// Seeding the hypothesis text as a constant (not a free-form runtime string)
/// makes the registry reproducible — the same build always declares the same two
/// experiments with the same hypotheses (§56.3 reproducibility).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExperimentSeed {
    /// Stable experiment id.
    pub id: ExperimentId,
    /// The stated causal hypothesis under test (§46: no feature without a
    /// hypothesis; no experiment without one either).
    pub hypothesis: &'static str,
}

/// The registry seed: the two attention-alpha experiments criterion 84 binds the
/// narrative/social/meta families to. Ordered by ascending id.
pub const SEEDED_EXPERIMENTS: [ExperimentSeed; 2] = [
    ExperimentSeed {
        id: EXPERIMENT_META_ROTATION_PREDICTIVENESS,
        hypothesis: "Cross-narrative meta-rotation state (which cohort attention \
                     is rotating into) has out-of-sample predictive value for \
                     forward returns of inflowing-cohort tokens, beyond per-token \
                     momentum.",
    },
    ExperimentSeed {
        id: EXPERIMENT_SOURCE_TIER_VALUE,
        hypothesis: "The source tier of an attention signal carries differential, \
                     separable predictive value: a tier-1 surface is not \
                     interchangeable with a tier-3 surface at equal raw velocity.",
    },
];

/// Look up the seeded hypothesis for an experiment id, if it is one of the
/// criterion-84 seeds.
pub fn seeded_hypothesis(id: ExperimentId) -> Option<&'static str> {
    SEEDED_EXPERIMENTS
        .iter()
        .find(|s| s.id == id)
        .map(|s| s.hypothesis)
}

/// The feature family a candidate feature belongs to.
///
/// The narrative-scoped families ([`FeatureKind::Narrative`],
/// [`FeatureKind::Social`], [`FeatureKind::Meta`]) are the ones criterion 84
/// binds to Experiments #2 and #3; every other kind carries no extra
/// family-level experiment requirement (the generic per-feature guard still
/// applies to it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FeatureKind {
    /// A narrative-attention feature (narrative velocity/acceleration, cohort
    /// state). Bound to Experiments #2 and #3.
    Narrative,
    /// A social-attention feature (per-source social velocity, author signals).
    /// Bound to Experiments #2 and #3.
    Social,
    /// A meta-rotation feature (cross-narrative rotation). Bound to Experiments
    /// #2 and #3.
    Meta,
    /// A microstructure feature — no family-level experiment binding here.
    Microstructure,
    /// A creator-reputation feature — no family-level experiment binding here.
    Creator,
    /// Any other feature family — no family-level experiment binding here.
    Other,
}

impl FeatureKind {
    /// Whether this kind is one of the narrative-scoped families criterion 84
    /// binds to the attention-alpha experiments.
    pub fn is_narrative_scoped(&self) -> bool {
        matches!(
            self,
            FeatureKind::Narrative | FeatureKind::Social | FeatureKind::Meta
        )
    }
}

/// The experiments the narrative-scoped families require, in ascending id order.
const NARRATIVE_REQUIRED: [ExperimentId; 2] = [
    EXPERIMENT_META_ROTATION_PREDICTIVENESS,
    EXPERIMENT_SOURCE_TIER_VALUE,
];

/// The empty requirement set for non-narrative-scoped kinds.
const NONE_REQUIRED: [ExperimentId; 0] = [];

/// The experiments `kind` must have registered-and-passing before it may enter
/// shadow (criterion 84).
///
/// Returns `{#2, #3}` (ascending) for the narrative/social/meta families and an
/// empty slice for every other kind. Deterministic and allocation-free — the
/// returned slice borrows a `'static` table.
pub fn requires_experiments(kind: FeatureKind) -> &'static [ExperimentId] {
    if kind.is_narrative_scoped() {
        &NARRATIVE_REQUIRED
    } else {
        &NONE_REQUIRED
    }
}

/// The registered state of a single experiment as the registry knows it.
///
/// Fail-closed: an experiment the registry has never heard of is neither
/// registered nor passing, so absence can never satisfy admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExperimentState {
    id: ExperimentId,
    passing: bool,
}

/// Maximum experiments the registry retains (§57 bound). The criterion-84
/// binding needs exactly two; the slack lets adjacent experiments share the
/// registry without unbounded growth.
pub const EXPERIMENT_REGISTRY_CAPACITY: usize = 32;

/// Why a narrative-scoped feature was refused admission to shadow (criterion 84).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExperimentAdmissionReject {
    /// A required experiment has not been registered at all. Carries the first
    /// (lowest-id) missing experiment.
    ExperimentNotRegistered(ExperimentId),
    /// A required experiment is registered but not yet passing. Carries the
    /// first (lowest-id) registered-but-failing experiment.
    ExperimentNotPassing(ExperimentId),
    /// The registry is full and the experiment could not be registered (§57).
    RegistryFull,
}

/// A bounded registry of experiment states the admission predicate consults.
///
/// §57: capacity-bounded (never grows past [`EXPERIMENT_REGISTRY_CAPACITY`]).
/// §22: no float, no wall-clock. Entries are kept sorted by id so lookups and the
/// admission scan are deterministic regardless of registration order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExperimentRegistry {
    entries: Vec<ExperimentState>,
}

impl ExperimentRegistry {
    /// An empty registry — nothing registered, so every narrative-scoped feature
    /// is refused (fail-closed).
    pub fn new() -> Self {
        ExperimentRegistry {
            entries: Vec::new(),
        }
    }

    /// A registry pre-seeded with the criterion-84 experiments, each in the
    /// given `passing` state. Convenience for the common "both #2 and #3
    /// registered" case; pass `passing = false` to register them not-yet-passing.
    pub fn seeded(passing: bool) -> Self {
        let mut r = ExperimentRegistry::new();
        for s in SEEDED_EXPERIMENTS.iter() {
            // Capacity is 32 and there are two seeds — this cannot overflow.
            let _ = r.register(s.id, passing);
        }
        r
    }

    /// Register (or update) an experiment's passing state.
    ///
    /// Idempotent on the id: re-registering the same id updates its passing
    /// state in place rather than duplicating it. Entries stay sorted by id.
    /// Returns [`ExperimentAdmissionReject::RegistryFull`] only when a *new* id
    /// would exceed the §57 capacity bound.
    pub fn register(
        &mut self,
        id: ExperimentId,
        passing: bool,
    ) -> Result<(), ExperimentAdmissionReject> {
        match self.entries.binary_search_by(|e| e.id.cmp(&id)) {
            Ok(idx) => {
                self.entries[idx].passing = passing;
                Ok(())
            }
            Err(idx) => {
                if self.entries.len() >= EXPERIMENT_REGISTRY_CAPACITY {
                    return Err(ExperimentAdmissionReject::RegistryFull);
                }
                self.entries.insert(idx, ExperimentState { id, passing });
                Ok(())
            }
        }
    }

    /// Whether an experiment is registered (regardless of passing state).
    pub fn is_registered(&self, id: ExperimentId) -> bool {
        self.entries.binary_search_by(|e| e.id.cmp(&id)).is_ok()
    }

    /// Whether an experiment is registered *and* passing.
    pub fn is_passing(&self, id: ExperimentId) -> bool {
        self.entries
            .binary_search_by(|e| e.id.cmp(&id))
            .map(|idx| self.entries[idx].passing)
            .unwrap_or(false)
    }

    /// Number of registered experiments.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Fail-closed admission predicate: may a feature of `kind` enter shadow given
/// the current experiment `registry`? (criterion 84.)
///
/// For a narrative-scoped kind, *every* experiment in
/// [`requires_experiments`] must be registered **and** passing; the scan is in
/// ascending id order and reports the first offending experiment, so the reject
/// reason for a given registry state is deterministic:
///
/// 1. the first required experiment that is not registered →
///    [`ExperimentAdmissionReject::ExperimentNotRegistered`], else
/// 2. the first required experiment registered but not passing →
///    [`ExperimentAdmissionReject::ExperimentNotPassing`].
///
/// A non-narrative-scoped kind carries no family-level requirement and is
/// admitted (the generic per-feature guard still governs it elsewhere).
pub fn admit_to_shadow(
    kind: FeatureKind,
    registry: &ExperimentRegistry,
) -> Result<(), ExperimentAdmissionReject> {
    for &id in requires_experiments(kind) {
        if !registry.is_registered(id) {
            return Err(ExperimentAdmissionReject::ExperimentNotRegistered(id));
        }
    }
    for &id in requires_experiments(kind) {
        if !registry.is_passing(id) {
            return Err(ExperimentAdmissionReject::ExperimentNotPassing(id));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_are_experiments_2_and_3_with_hypotheses() {
        assert_eq!(SEEDED_EXPERIMENTS.len(), 2);
        assert_eq!(SEEDED_EXPERIMENTS[0].id, ExperimentId(2));
        assert_eq!(SEEDED_EXPERIMENTS[1].id, ExperimentId(3));
        // Every seed states a non-empty causal hypothesis (§46).
        for s in SEEDED_EXPERIMENTS.iter() {
            assert!(!s.hypothesis.is_empty());
        }
        assert_eq!(
            seeded_hypothesis(EXPERIMENT_META_ROTATION_PREDICTIVENESS),
            Some(SEEDED_EXPERIMENTS[0].hypothesis)
        );
        assert_eq!(
            seeded_hypothesis(EXPERIMENT_SOURCE_TIER_VALUE),
            Some(SEEDED_EXPERIMENTS[1].hypothesis)
        );
        assert_eq!(seeded_hypothesis(ExperimentId(999)), None);
    }

    #[test]
    fn narrative_scoped_kinds_require_experiments_2_and_3() {
        for kind in [
            FeatureKind::Narrative,
            FeatureKind::Social,
            FeatureKind::Meta,
        ] {
            assert!(kind.is_narrative_scoped());
            assert_eq!(
                requires_experiments(kind),
                &[
                    EXPERIMENT_META_ROTATION_PREDICTIVENESS,
                    EXPERIMENT_SOURCE_TIER_VALUE
                ]
            );
        }
    }

    #[test]
    fn non_narrative_kinds_require_no_family_experiments() {
        for kind in [
            FeatureKind::Microstructure,
            FeatureKind::Creator,
            FeatureKind::Other,
        ] {
            assert!(!kind.is_narrative_scoped());
            assert!(requires_experiments(kind).is_empty());
            // ...and are admitted with an empty registry.
            assert_eq!(admit_to_shadow(kind, &ExperimentRegistry::new()), Ok(()));
        }
    }

    #[test]
    fn empty_registry_refuses_narrative_feature_on_first_missing() {
        let reg = ExperimentRegistry::new();
        // Nothing registered → refused on the lowest-id required experiment (#2).
        assert_eq!(
            admit_to_shadow(FeatureKind::Narrative, &reg),
            Err(ExperimentAdmissionReject::ExperimentNotRegistered(
                EXPERIMENT_META_ROTATION_PREDICTIVENESS
            ))
        );
    }

    #[test]
    fn only_experiment_2_registered_reports_experiment_3_missing() {
        let mut reg = ExperimentRegistry::new();
        reg.register(EXPERIMENT_META_ROTATION_PREDICTIVENESS, true)
            .unwrap();
        // #2 present+passing, #3 absent → refused on #3 (registration precedes
        // passing in the scan order).
        assert_eq!(
            admit_to_shadow(FeatureKind::Social, &reg),
            Err(ExperimentAdmissionReject::ExperimentNotRegistered(
                EXPERIMENT_SOURCE_TIER_VALUE
            ))
        );
    }

    #[test]
    fn both_registered_but_one_failing_refuses_on_passing() {
        let mut reg = ExperimentRegistry::new();
        reg.register(EXPERIMENT_META_ROTATION_PREDICTIVENESS, true)
            .unwrap();
        reg.register(EXPERIMENT_SOURCE_TIER_VALUE, false).unwrap();
        // Both registered, but #3 not passing → ExperimentNotPassing(#3).
        assert_eq!(
            admit_to_shadow(FeatureKind::Meta, &reg),
            Err(ExperimentAdmissionReject::ExperimentNotPassing(
                EXPERIMENT_SOURCE_TIER_VALUE
            ))
        );
        // #2 failing is reported first (lower id) when both fail.
        reg.register(EXPERIMENT_META_ROTATION_PREDICTIVENESS, false)
            .unwrap();
        assert_eq!(
            admit_to_shadow(FeatureKind::Meta, &reg),
            Err(ExperimentAdmissionReject::ExperimentNotPassing(
                EXPERIMENT_META_ROTATION_PREDICTIVENESS
            ))
        );
    }

    #[test]
    fn both_registered_and_passing_admits() {
        let reg = ExperimentRegistry::seeded(true);
        assert!(reg.is_registered(EXPERIMENT_META_ROTATION_PREDICTIVENESS));
        assert!(reg.is_registered(EXPERIMENT_SOURCE_TIER_VALUE));
        assert!(reg.is_passing(EXPERIMENT_META_ROTATION_PREDICTIVENESS));
        assert!(reg.is_passing(EXPERIMENT_SOURCE_TIER_VALUE));
        for kind in [
            FeatureKind::Narrative,
            FeatureKind::Social,
            FeatureKind::Meta,
        ] {
            assert_eq!(admit_to_shadow(kind, &reg), Ok(()));
        }
    }

    #[test]
    fn register_is_idempotent_and_updates_passing_in_place() {
        let mut reg = ExperimentRegistry::new();
        reg.register(EXPERIMENT_META_ROTATION_PREDICTIVENESS, false)
            .unwrap();
        reg.register(EXPERIMENT_META_ROTATION_PREDICTIVENESS, true)
            .unwrap();
        assert_eq!(reg.len(), 1); // not duplicated
        assert!(reg.is_passing(EXPERIMENT_META_ROTATION_PREDICTIVENESS));
    }

    #[test]
    fn registry_is_bounded_and_refuses_overflow() {
        let mut reg = ExperimentRegistry::new();
        for i in 0..EXPERIMENT_REGISTRY_CAPACITY as u16 {
            reg.register(ExperimentId(1000 + i), true).unwrap();
        }
        assert_eq!(reg.len(), EXPERIMENT_REGISTRY_CAPACITY);
        // A new id past capacity is refused (§57).
        assert_eq!(
            reg.register(ExperimentId(9999), true),
            Err(ExperimentAdmissionReject::RegistryFull)
        );
        // But updating an already-registered id still works at capacity.
        assert_eq!(reg.register(ExperimentId(1000), false), Ok(()));
    }

    #[test]
    fn admission_is_deterministic_and_order_independent() {
        // Registering #3 before #2 yields the same admit result as the reverse.
        let mut a = ExperimentRegistry::new();
        a.register(EXPERIMENT_SOURCE_TIER_VALUE, true).unwrap();
        a.register(EXPERIMENT_META_ROTATION_PREDICTIVENESS, true)
            .unwrap();
        let mut b = ExperimentRegistry::new();
        b.register(EXPERIMENT_META_ROTATION_PREDICTIVENESS, true)
            .unwrap();
        b.register(EXPERIMENT_SOURCE_TIER_VALUE, true).unwrap();
        assert_eq!(a, b);
        assert_eq!(
            admit_to_shadow(FeatureKind::Narrative, &a),
            admit_to_shadow(FeatureKind::Narrative, &b)
        );
        assert_eq!(admit_to_shadow(FeatureKind::Narrative, &a), Ok(()));
    }
}
