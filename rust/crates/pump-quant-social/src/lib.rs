//! `pump_quant_social` — the SocialSourceQualityLedger, the "alpha-vs-trash" system
//! (constitution §29.8, memory/reflection integration §29.9).
//!
//! # Responsibility
//! Reconcile every attributable social call (account × token × timestamp × content
//! hash) against reconstructed market state and score it on the ten §29.8
//! determinants (D1–D10), each stored *decomposed* with sample size, confidence,
//! and time decay. From those decomposed scores, classify a source into exactly one
//! of the eight §29.8 states, cross-channel copy-echo is detected, and the
//! amplification graph is edge-scored so echo (reach) is never mistaken for alpha.
//!
//! # Constitution discipline (binding)
//! * **§22 determinism / integer money.** No floating point anywhere in the
//!   outcome-controlling logic. Every score is basis-point (`bps`, scale 10_000)
//!   fixed-point in `i64`, every accumulation uses `i128`, and overflow is handled
//!   explicitly (checked/saturating/clamped by contract — never silent wraparound;
//!   `overflow-checks` are also forced on in every profile). No wall-clock, RNG,
//!   network, filesystem, or float. Any age/latency the scorers need is supplied as
//!   an already-measured `u64` nanosecond argument — the module never reads a clock.
//! * **Fade-first (PUBLIC_BURNED presumption).** Classification checks the
//!   disqualifying (fade) states *before* any positive tier; a source earns
//!   `PRE_FLOW_ALPHA` only by beating the D3 state-at-call selection control, and low
//!   sample resolves to `INSUFFICIENT_SAMPLE`, never to trust.
//! * **Real algorithms.** Every scorer computes from its inputs; nothing is
//!   hardcoded to a fixed answer. Thresholds are the only constants and live in
//!   explicit config structs.
//!
//! # Scope boundary
//! Live streaming, capture, and submission are OUT OF SCOPE (server, `[S]`). This
//! crate is the deterministic reducer math only; any I/O would sit behind a trait
//! and is never called here.

pub mod amplification;
pub mod classification;
pub mod copy_echo;
pub mod determinants;
pub mod fixedpoint;
pub mod ledger;
pub mod types;
