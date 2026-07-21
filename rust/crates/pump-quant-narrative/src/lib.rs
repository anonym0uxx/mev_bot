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

pub mod narrative;

pub use narrative::{
    nv_attention_money_divergence, nv_attention_series, nv_candidate_score, nv_class_classify,
    nv_lifecycle_stage, nv_meta_emergence, nv_narrative_ceiling, nv_platform_lead,
    nv_pre_legibility, nv_virality_coeff, AttentionMoneyDivergence, AttentionSeries, ClassFeatures,
    LifecycleStage, MetaEmergence, NarrativeClass, PlatformLead, FP_ONE,
};
