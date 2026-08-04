//! # pump-quant-execution
//!
//! Submission-logic crate for the pump-quant memecoin scalping bot. This crate
//! ports the *decision* layer of the legacy production execution path — the
//! deterministic state machines and integer math that decide *what* to submit
//! and *how* — while explicitly leaving all live I/O (RPC sends, Jito bundle
//! submission, websocket streams) out of scope.
//!
//! ## Constitution references
//! - **§22 (no floats in outcome-controlling paths):** every value that affects
//!   an on-chain decision is integer / fixed-point. Lamports are `u64` / `u128`
//!   / `i128`; ratios are basis points (`bps`, 1/10_000).
//! - **Explicit overflow:** all arithmetic that can overflow uses `checked_*`,
//!   `saturating_*`, or widened intermediates (`u128` / `i128`).
//! - **Determinism:** no wall-clock reads, no RNG, no network, no floats. All
//!   time is passed in explicitly as caller-supplied `*_ms` / `*_slot` inputs,
//!   so identical inputs always yield identical outputs.
//!
//! ## Leaves
//! - [`ex_sell_ladder_state`] — 5-level sell escalation state machine.
//! - [`ex_sell_ladder_escalate`] — second-scale deterministic escalation trigger.
//! - [`ex_reconcile_fill`] — on-chain fill reconciliation math.
//! - [`ex_tip_compute`] — priority / Jito tip sizing from congestion inputs.
//! - [`ex_blockhash_cache`] — blockhash validity-window logic.
//! - [`ex_route_policy`] — MEV-aware route selection.
//! - [`ex_route_health`] — observed landing outcomes folded into the health
//!   inputs [`ex_route_policy`] needs. Bounded window, integer EWMA, and an
//!   under-sampled route reports NO measurement rather than good health.
//! - [`ex_sender_route`] — Helius Sender tier selection, tip budgeting against
//!   expected edge, and composition with [`ex_route_policy`] into a single
//!   submission plan. Purely additive: it reads [`ex_route_policy`] types and
//!   changes none of them.
//! - [`ex_promotion_gate`] — whether paper evidence justifies arming live
//!   capital. A pre-registered test with a stated threshold, requiring
//!   statistical separation from zero rather than merely a positive total, and
//!   refusing outright on a gappy feed.
//! - [`ex_live_arming`] — the live-capital arming gate and risk envelope. One
//!   operator arming action authorises autonomous trading within stated limits;
//!   after it, no entry or exit needs approval. Exits are admitted even when
//!   disarmed, because a kill switch that blocks selling traps capital instead
//!   of protecting it.
//! - [`ex_bundle_assemble`] — Jito bundle ordering / validation.
//! - [`ex_circuit_breaker`] — RPC circuit-breaker backoff state machine.
//! - [`ex_builder_quarantine`] — builder-quarantine circuit breaker (criterion 78):
//!   folds §36 classified failures and quarantines a builder after N construction
//!   strikes; the gate a submitter must consult before any build/live use.
//! - [`ex_construction_gate`] — construction validation gate (criteria 77/113):
//!   deterministic fixture-parity + decode-round-trip rungs, live-state
//!   simulation deferred to Phase-B behind a trait.
//! - [`si_incident_gate`] — incident-branch remediation admission gate
//!   (model output must pass sell-simulation + signing before reaching chain;
//!   the deterministic exit path is proven model-independent).
//!
//! ## The live-capital chain
//! Three gates in series, each able to refuse:
//! [`ex_promotion_gate`] decides whether the evidence justifies arming at all,
//! [`ex_live_arming`] decides whether a given entry fits the authorised
//! envelope, and [`ex_builder_quarantine`] decides whether a submitter may be
//! used. None of them asks a human to approve a trade.

// SAFETY POLICY (added 2026-07-29): this crate is the code that builds, sizes, routes and reconciles what is submitted to chain,
// and it contained zero `unsafe` when this was added. `forbid` makes that a
// property the compiler holds rather than one a reviewer has to re-verify —
// and unlike `deny` it cannot be locally overridden by an `#[allow]`.
// Constitution §24(b): an `unsafe` block requires a dossier-registered,
// property-tested safety argument. There is no such dossier entry for this
// crate, so there is no `unsafe` this attribute could legitimately block.
#![forbid(unsafe_code)]

pub mod ex_blockhash_cache;
pub mod ex_builder_quarantine;
pub mod ex_bundle_assemble;
pub mod ex_circuit_breaker;
pub mod ex_construction_gate;
pub mod ex_live_arming;
pub mod ex_outbound_sink;
pub mod ex_promotion_gate;
pub mod ex_reconcile_fill;
pub mod ex_route_health;
pub mod ex_route_policy;
pub mod ex_rpc_simulator;
pub mod ex_sell_ladder_escalate;
pub mod ex_sell_ladder_state;
pub mod ex_sender_route;
pub mod ex_tip_compute;
pub mod ex_fee_calibration;
pub mod ex_live_sink;
pub mod si_incident_gate;
pub mod remediation_paths;