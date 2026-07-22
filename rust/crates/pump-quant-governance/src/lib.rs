//! # pump-quant-governance
//!
//! Governance guards for the memecoin scalping bot. This crate is the
//! *enforcement spine* for the two-speed governance model (constitution
//! §56.2) and the provider-neutral source registry (§18.8), plus the
//! reproducible configuration hashing that lets every strategy version and
//! evaluator release be pinned and audited (§56.3, §44).
//!
//! ## Responsibility
//! Three self-contained, deterministic guards:
//!
//! * [`envelope`] — [`envelope::ParameterEnvelope`] bounds enforcement. Online
//!   ("fast path", §56.2) parameter changes are clamped or rejected against a
//!   registered `[min, max]` envelope so a deterministic controller can adapt a
//!   champion *inside* its validated ranges without a new experiment, while
//!   *crossing* the envelope is refused (crossing requires the full slow path).
//!
//! * [`lifecycle`] — the source-registry lifecycle finite-state machine
//!   (§18.8): `ACTIVE_PRIMARY, ACTIVE_REDUNDANT, TRANSITIONAL, DEGRADED,
//!   SUNSET_PENDING, DISABLED, RETIRED`. Encodes the legal transitions (e.g. a
//!   `TRANSITIONAL` Jito ShredStream adapter → `SUNSET_PENDING` → `RETIRED`
//!   with a recorded replacement) and refuses illegal ones. The immutable
//!   canonical authority class is fixed at construction and is never mutated by
//!   a transition (§18.8: measurements "may never change canonical authority").
//!
//! * [`hashing`] — reproducible [`hashing::StrategyHash`] and
//!   [`hashing::EvaluatorReleaseHash`]: a stable, domain-separated digest of a
//!   [`canonical::CanonicalValue`] configuration, built on a from-scratch,
//!   integer-only [`sha256`] and a canonical, injective, length-prefixed
//!   encoding ([`canonical`]) so byte-equivalent inputs always hash identically
//!   regardless of map insertion order.
//!
//! ## Constitutional invariants honored crate-wide
//! * **§22 — no floating point in outcome-controlling logic.** There is no
//!   `f32`/`f64` anywhere in this crate. Envelope values are integer
//!   fixed-point (`i128`; the caller chooses the scale — lamports, basis
//!   points, token base units). Hashing is `u32`/`u64` integer math.
//! * **Explicit overflow (§705).** Money/fixed-point arithmetic uses explicit
//!   `checked_*`/`saturating_*` operations; SHA-256's modular arithmetic uses
//!   `wrapping_*` *by contract* (SHA-256 is defined modulo 2^32), documented at
//!   each site.
//! * **Deterministic (§19, §22).** No wall-clock, no RNG, no network, no
//!   floating point. All ordering is stable (sorted keys / explicit `Ord`).
//! * **Memory-bounded where stateful (§57).** The registries carry explicit
//!   capacity bounds and the lifecycle transition audit log is a bounded ring
//!   buffer that evicts oldest entries rather than growing without limit.

#![forbid(unsafe_code)]

pub mod canonical;
pub mod envelope;
pub mod hashing;
pub mod lifecycle;
pub mod manifest;
pub mod sha256;
pub mod strategy_registry;
