//! # pump-quant-domain
//!
//! Shared **domain vocabulary** for the pump-quant memecoin scalping system: the
//! canonical newtypes and stable enums that every other crate imports so that
//! identifiers, money, venues, and evidence provenance mean exactly one thing
//! across ingestion, the deterministic `StrategyRuntime`, execution, and replay.
//!
//! ## Responsibility
//! This crate is *foundational and dependency-free*. It owns no behaviour beyond
//! the vocabulary itself: it defines the wire-stable types other crates share and
//! the small, total, integer-only helpers that operate on them. It deliberately
//! contains no I/O, no clock, no network, no floating point, and no strategy
//! logic — those live in downstream crates that import this one.
//!
//! ## Constitution alignment
//! * **Section 17 (Required event schemas):** the newtypes and enums here are the
//!   neutral vocabulary that `RawObservation`, `CanonicalTransaction`,
//!   `DecisionRecord`, and `CandidateRecord` are built from — no provider-specific
//!   SDK type is ever exposed.
//! * **Section 22 (Deterministic strategy core):** all money and rate arithmetic
//!   is integer / fixed-point with *explicit* overflow discipline
//!   (checked / saturating), never floating point in outcome-controlling logic.
//! * **Section 18 / 16:** [`Venue`], [`EvidenceStage`], [`DeliveryMode`],
//!   [`DatasetFidelity`], and [`SourceLifecycleStatus`] encode the provider-neutral
//!   source and provenance model.
//! * **Section 23 (Candidate lifecycle):** [`CandidateLifecycleState`].
//!
//! Everything the tests touch is `pub` by contract.

// SAFETY POLICY (added 2026-07-29): this crate is the shared money and identity types,
// and it contained zero `unsafe` when this was added. `forbid` makes that a
// property the compiler holds rather than one a reviewer has to re-verify —
// and unlike `deny` it cannot be locally overridden by an `#[allow]`.
// Constitution §24(b): an `unsafe` block requires a dossier-registered,
// property-tested safety argument. There is no such dossier entry for this
// crate, so there is no `unsafe` this attribute could legitimately block.
#![forbid(unsafe_code)]

#![deny(missing_docs)]

pub mod evidence;
pub mod ids;
pub mod lifecycle;
pub mod market;
pub mod money;

// Flat re-exports so downstream crates can `use pump_quant_domain::Lamports;` etc.
pub use evidence::{DatasetFidelity, DeliveryMode, EvidenceStage, SourceLifecycleStatus};
pub use ids::{Mint, ParseMintError, ProviderId, Slot, SourceId, TradeId};
pub use lifecycle::CandidateLifecycleState;
pub use market::{Lane, Side, Venue};
pub use money::{BasisPoints, Lamports, SignedLamports, TokenAmount};
