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
//! - [`ex_bundle_assemble`] — Jito bundle ordering / validation.
//! - [`ex_circuit_breaker`] — RPC circuit-breaker backoff state machine.
//! - [`si_incident_gate`] — incident-branch remediation admission gate
//!   (model output must pass sell-simulation + signing before reaching chain;
//!   the deterministic exit path is proven model-independent).

pub mod ex_blockhash_cache;
pub mod ex_bundle_assemble;
pub mod ex_circuit_breaker;
pub mod ex_reconcile_fill;
pub mod ex_route_policy;
pub mod ex_sell_ladder_escalate;
pub mod ex_sell_ladder_state;
pub mod ex_tip_compute;
pub mod si_incident_gate;
