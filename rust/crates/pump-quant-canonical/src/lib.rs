//! `pump_quant_canonical` — Provenance canonicalizer (Constitution §15).
//!
//! # Responsibility
//! Merge multiple provenance-tagged observations of **one** Solana transaction
//! (identified by its signature) into a single [`CanonicalTransaction`] that:
//!
//! * **Preserves feed disagreement** — the canonicalizer never silently adopts
//!   one provider's interpretation; every field on which sources disagree is
//!   retained in [`CanonicalTransaction::disagreements`] alongside the resolved
//!   canonical value and the authority class that determined it (§15).
//! * **Carries dual timelines** — observation truth (what this server saw and
//!   when, per source class and delivery mode) is kept strictly separate from
//!   canonical chain truth (slot / index / commitment progression). Timings are
//!   **never equated across source classes or delivery modes** (§15, §16, §18.6).
//! * **Carries fork status** — canonical vs dropped-fork state with provenance.
//! * **Carries full provenance** — every contributing observation is recorded.
//!
//! # Constitution discipline
//! * §22 — no floating-point anywhere in this crate; all quantities are integers
//!   (lamports, compute units, nanoseconds, basis-point-free counters) or
//!   fixed-width discriminants. Arithmetic that could overflow uses
//!   `saturating_*` / `checked_*` by contract.
//! * Deterministic reducer — no wall-clock, RNG, network, filesystem or float in
//!   logic. Any real I/O (live streaming / submission) is out of scope [S] and is
//!   modelled only as plain input data, never invoked here.
//! * Memory-bounded — the stateful [`Canonicalizer`] enforces an explicit cap on
//!   tracked signatures and observations per signature, with deterministic
//!   eviction.
//!
//! # Layout
//! * [`types`] — core identity, source-class, provider and status enums.
//! * [`observation`] — the provenance-tagged input record.
//! * [`canonical`] — the merged output and the pure merge reducer.
//! * [`reducer`] — the stateful, memory-bounded grouping reducer.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod canonical;
pub mod observation;
pub mod reducer;
pub mod types;

pub use canonical::{
    canonicalize_group, CanonicalFields, CanonicalTimeline, CanonicalTransaction, ClaimSource,
    FieldClaim, FieldDisagreement, ObservationTimeline, ProvenanceEntry, ResolvedField,
    TimelineKey,
};
pub use observation::{SourcedTime, TransactionObservation, TxClaim};
pub use reducer::{Canonicalizer, Evicted};
pub use types::{
    Commitment, DeliveryMode, FieldName, ForkStatus, Provider, Signature, SourceClass,
};
