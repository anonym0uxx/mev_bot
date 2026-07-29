//! `pump_quant_narrative` — attention-velocity narrative engine.
//!
//! Responsibility: the deterministic, research-plane feature layer that turns
//! timestamp-safe attention observations into narrative-state features for the
//! StrategyRuntime's corroboration tier. It implements the ten narrative leaves
//! of the constitution's attention/narrative specification (§29 Narrative
//! interpretation stack — `AttentionStateReducer`, `AttentionDecayModel`,
//! `SocialCatalystClassifier`; §21.4 `MetaRotationState`; §29.7/§46
//! Signal-Horizon Matching Law; §783 pre-legibility doctrine), aggregated here
//! as the attention-velocity engine.
//!
//! Hard invariants (constitution):
//! * §22 — NO `f32`/`f64` anywhere on the outcome path. All quantities are
//!   integer or fixed-point over [`FP_ONE`]. Overflow is explicit: every
//!   arithmetic step that can overflow widens to `i128`/`u128` and then
//!   saturates back by contract (documented at each site).
//! * Deterministic — no wall-clock, no RNG, no network, no float in logic. All
//!   time enters as caller-supplied integer instants/windows; live capture and
//!   submission are out of scope (server side).
//! * Corroboration-tier, fade-first — narrative alone never authorizes a trade;
//!   [`narrative::nv_candidate_score`] hard-caps any unconfirmed narrative.
//! * §29.5 — absence of data is valid; `Unknown` never becomes false sentiment
//!   (modeled with `Option`/dedicated `NoData` variants, never fabricated).
//!
//! Memory-bounded: every function is a pure fold over caller-owned slices; the
//! crate holds no growing state of its own.
//!
//! ## Two narrative axes (do not conflate them)
//! * [`narrative::NarrativeClass`] — how a narrative *behaves*: decay speed and
//!   reach ceiling (trend / news / tech / culture). Consumed by
//!   [`narrative::nv_narrative_ceiling`].
//! * [`narrative_family::NarrativeFamily`] — what a token is *about*: the
//!   subject-matter family (animal / political / celebrity / tech / derivative /
//!   stream / seasonal), classified only from deterministic lexical and
//!   launch-metadata evidence, and `Unclassified` whenever that evidence is
//!   absent (§6.4 — under-claiming beats fabricating).

// SAFETY POLICY (added 2026-07-29): this crate is narrative scoring feeding admission,
// and it contained zero `unsafe` when this was added. `forbid` makes that a
// property the compiler holds rather than one a reviewer has to re-verify —
// and unlike `deny` it cannot be locally overridden by an `#[allow]`.
// Constitution §24(b): an `unsafe` block requires a dossier-registered,
// property-tested safety argument. There is no such dossier entry for this
// crate, so there is no `unsafe` this attribute could legitimately block.
#![forbid(unsafe_code)]

pub mod attention_decay;
pub mod attention_state;
pub mod catalyst_classifier;
pub mod narrative;
pub mod narrative_family;

pub use narrative_family::{
    nv_family_classify, nv_family_classify_default, FamilyClassification, FamilyEvidence,
    FamilyEvidenceLane, FamilyLexicon, MatchMode, NarrativeFamily, Needle,
    FAMILY_DERIVATIVE_SIMILARITY_BPS, FAMILY_LEXICON_V1, FAMILY_LEXICON_VERSION,
};

pub use narrative::{
    nv_attention_money_divergence, nv_attention_series, nv_candidate_score, nv_class_classify,
    nv_lifecycle_stage, nv_meta_emergence, nv_narrative_ceiling, nv_platform_lead,
    nv_pre_legibility, nv_virality_coeff, AttentionMoneyDivergence, AttentionSeries, ClassFeatures,
    LifecycleStage, MetaEmergence, NarrativeClass, PlatformLead, FP_ONE,
};

pub use catalyst_classifier::{
    classify as nv_catalyst_classify, CatalystFeatures, CatalystThresholds, SocialCatalyst,
};

pub use attention_decay::{
    nv_attention_decay, AttentionDecayModel, AttentionEvent, DecayInputs, EventKind,
};

pub use attention_state::{
    nv_attention_distinction, nv_attention_state, AttentionDistinction, AttentionState, Mention,
    MAX_TRACKED,
};
