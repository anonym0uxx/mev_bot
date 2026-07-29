//! `pump_quant_ingest` — the provider-parsing boundary crate.
//!
//! This crate ports the *payload parsers* of the legacy Helius and PumpPortal
//! WebSocket feeds into pure, deterministic, std-only functions that turn raw
//! provider bytes into the crate-local canonical transaction type. No live
//! subscription, no network, no wall-clock, no floating point in any
//! outcome-controlling path (constitution §22). Live I/O (RPC / streams /
//! submission) is explicitly OUT OF SCOPE and would sit behind a trait at the
//! edge (ARCHITECTURE rule 4).
//!
//! Modules:
//! - [`base58`]  — manual base58 (Bitcoin alphabet) decoder, no `bs58` dep.
//! - [`json`]    — minimal std-only JSON scanner (no `serde`/`simd_json`);
//!   numbers are kept as raw text so no float is ever produced.
//! - [`canonical`] — canonical output types (`CanonicalTx`, deltas in signed
//!   lamports / base units).
//! - [`helius_parse`] — port of `feeds/helius.rs` decode logic (leaf
//!   `in_helius_parse`).
//! - [`pumpportal_parse`] — port of `feeds/pumpportal.rs` decode logic (leaf
//!   `in_pumpportal_parse`); the legacy `f64` SOL→lamports conversion is
//!   reimplemented in integer fixed-point per §22 / named defect (8).
//! - [`source_registry`] — source classification + observation-source-mix
//!   labels (leaf `in_source_registry`; §14.5, §15, §16).
//! - [`submission_surface`] — Jito submission-surface (Block Engine / bundles /
//!   tips) lifecycle tracked independently of the ShredStream data-feed sunset
//!   (leaf `in_submission_surface`; §18.3.1, §18.8; criterion 76).

// SAFETY POLICY (added 2026-07-29): this crate is the decode boundary every canonical fact enters through,
// and it contained zero `unsafe` when this was added. `forbid` makes that a
// property the compiler holds rather than one a reviewer has to re-verify —
// and unlike `deny` it cannot be locally overridden by an `#[allow]`.
// Constitution §24(b): an `unsafe` block requires a dossier-registered,
// property-tested safety argument. There is no such dossier entry for this
// crate, so there is no `unsafe` this attribute could legitimately block.
#![forbid(unsafe_code)]

pub mod base58;
pub mod canonical;
pub mod helius_parse;
pub mod json;
pub mod pumpportal_parse;
pub mod social_parse;
pub mod social_source;
pub mod source_registry;
pub mod submission_surface;
pub mod token_metadata_parse;
