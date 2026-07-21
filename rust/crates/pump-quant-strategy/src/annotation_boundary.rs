//! # annotation_boundary — human-annotation boundary guard (criterion 46)
//!
//! Human annotations are **advisory only**. This module provides the deterministic
//! boundary analogous to the model-fact boundary: a [`HumanAnnotation`] can only
//! ever land in an [`AdvisoryNote`] ([`annotation_to_advisory`]); it can never be
//! admitted into factual/reducer state ([`admit_annotation_as_fact`] always
//! rejects); and it can never short-circuit a risk / economic / signing gate
//! ([`gate_with_annotation`] returns the gate's own verdict unchanged, so a
//! "positive" note cannot flip a failing gate to pass).
//!
//! ## Constitution
//! §30 (annotations may never bypass automated risk controls, authorize live
//! trades, or override thesis invalidation / wallet survival / sellability /
//! economic gates / replay). Pure and deterministic.

/// A timestamped, seal-immutable human annotation. It carries no numeric factual
/// payload the reducer could consume — only a free-form note and metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HumanAnnotation {
    /// Free-form annotation text.
    pub note: String,
    /// Annotator id.
    pub author: u32,
    /// Seal timestamp (ns); immutable after sealing.
    pub sealed_at_ns: u64,
}

impl HumanAnnotation {
    /// A deterministic fixture used by the property tests.
    pub fn test() -> Self {
        HumanAnnotation {
            note: "looks like a strong launch".to_string(),
            author: 1,
            sealed_at_ns: 1_000,
        }
    }
}

/// An advisory-only output — the sole destination for an annotation. It may
/// generate research hypotheses but is never a production feature or fact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdvisoryNote {
    /// The advisory text.
    pub note: String,
    /// Annotator id (provenance).
    pub author: u32,
}

/// Why an annotation was refused fact admission — it is always refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnnotationFactError {
    /// Annotations are advisory-only and can never become factual state.
    AdvisoryOnly,
}

/// Route an annotation to its only legal destination: advisory (leaf **ab_guard**).
///
/// This is the *only* function that consumes an annotation, and it produces an
/// [`AdvisoryNote`], never a fact.
pub fn annotation_to_advisory(a: &HumanAnnotation) -> AdvisoryNote {
    AdvisoryNote {
        note: a.note.clone(),
        author: a.author,
    }
}

/// The fact-admission boundary for annotations (leaf **ab_guard**).
///
/// Always returns `Err(AnnotationFactError::AdvisoryOnly)` — there is no path by
/// which a human annotation becomes factual/reducer state. The `Ok` type is `()`
/// only to keep the signature `Result`-shaped; it is never produced.
pub fn admit_annotation_as_fact(_a: &HumanAnnotation) -> Result<(), AnnotationFactError> {
    Err(AnnotationFactError::AdvisoryOnly)
}

/// Whether an annotation may ever enter factual state. Always `false`.
#[inline]
pub fn annotation_admissible_as_fact() -> bool {
    false
}

/// Apply an annotation at a risk/economic/signing gate (leaf **ab_guard**).
///
/// Returns the gate's own boolean verdict **unchanged**, regardless of the
/// annotation's content: an annotation can neither turn a failing gate into a pass
/// nor a passing gate into a fail. This is the deterministic proof that
/// annotations cannot short-circuit a gate.
#[inline]
pub fn gate_with_annotation(gate_pass: bool, _a: &HumanAnnotation) -> bool {
    gate_pass
}
