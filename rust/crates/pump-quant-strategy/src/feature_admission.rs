//! # feature_admission — causal-hypothesis admission guard (criterion 41)
//!
//! A deterministic guard ([`admit_feature`]) that refuses any feature which lacks
//! a stated causal hypothesis, a registered experiment, or a defeated-baseline
//! result. No feature enters production solely because it correlates
//! (constitution §46): it must answer *why* it should causally influence future
//! outcomes, be tied to a registered experiment, and have beaten its baseline.
//!
//! This is distinct from the specific safety-integrity gates already built: it is
//! the generic pre-production feature-admission predicate.
//!
//! ## Constitution
//! §46: causal rationale + experiment + baseline-defeat are mandatory. Pure and
//! deterministic; no I/O.

/// A candidate feature's admission record.
///
/// The two ids are `Option` because an absent id (not merely a zero) is exactly
/// the failure this guard catches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeatureAdmissionRequest {
    /// Stable feature id being considered for production.
    pub feature_id: u32,
    /// Registered causal-hypothesis id, or `None` if the feature has none.
    pub causal_hypothesis_id: Option<u64>,
    /// Registered experiment id, or `None` if unregistered.
    pub experiment_id: Option<u64>,
    /// Whether the feature beat its baseline out of sample.
    pub defeated_baseline: bool,
}

/// A feature that has passed admission — no public constructor other than
/// [`admit_feature`], so an un-admitted feature cannot masquerade as production.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmittedFeature {
    /// The admitted feature id.
    pub feature_id: u32,
    /// The causal hypothesis backing it.
    pub causal_hypothesis_id: u64,
    /// The experiment that validated it.
    pub experiment_id: u64,
}

/// Why a feature was refused production admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionReject {
    /// No stateable causal hypothesis → research-only.
    MissingCausalHypothesis,
    /// No registered experiment backing the feature.
    MissingExperiment,
    /// The feature did not beat its baseline out of sample.
    BaselineNotDefeated,
}

/// Deterministic feature-admission guard (leaf **fa_admit**).
///
/// Admits iff the request carries a causal-hypothesis id, an experiment id, and a
/// defeated-baseline result; the checks are ordered so the reject reason is
/// stable (hypothesis → experiment → baseline). Pure.
pub fn admit_feature(req: &FeatureAdmissionRequest) -> Result<AdmittedFeature, AdmissionReject> {
    let causal_hypothesis_id = req
        .causal_hypothesis_id
        .ok_or(AdmissionReject::MissingCausalHypothesis)?;
    let experiment_id = req
        .experiment_id
        .ok_or(AdmissionReject::MissingExperiment)?;
    if !req.defeated_baseline {
        return Err(AdmissionReject::BaselineNotDefeated);
    }
    Ok(AdmittedFeature {
        feature_id: req.feature_id,
        causal_hypothesis_id,
        experiment_id,
    })
}
