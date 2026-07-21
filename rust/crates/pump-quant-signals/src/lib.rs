//! `pump_quant_signals` -- entry-signal scoring for the memecoin scalping bot.
//!
//! This crate ports the ENTRY-SIGNAL scoring logic from the legacy proven
//! production code (`pump-quant-core::momentum::scorer` and
//! `pump-quant-core::momentum::velocity`), adapted to clean, idiomatic,
//! std-only Rust.
//!
//! # Constitution constraints (§22 -- integer-only outcome paths)
//!
//! - **No `f32`/`f64` anywhere in an outcome-controlling path.** All money is
//!   in lamports (`u64`/`u128`), all prices are fixed-point integers, and all
//!   ratios/rates are expressed in basis points (bps).
//! - **Overflow is explicit** -- every arithmetic step that could overflow uses
//!   `checked_*`, `saturating_*`, or a wider intermediate type (`u128`/`i128`)
//!   by contract. There is no silent wrapping.
//! - **Deterministic** -- identical inputs always produce identical outputs.
//!   No wall-clock, RNG, network, or floating point in the logic.
//!
//! # Modules
//!
//! - [`scorer`] -- the 8-dimension graduation entry scorer plus the individual
//!   component scorers (speed, volume tier, velocity, buy/sell ratio gate,
//!   entry discount, LP reserve, pre-entry momentum).
//! - [`velocity`] -- signed two-point price velocity in bps/sec.
//! - [`microstructure`] -- AMM order-flow / microstructure feature catalog
//!   (§21.7, criterion 95): CVD + divergence, breadth-decomposed OFI,
//!   trade-size distribution, absorption/exhaustion, anchored VWAP, executable
//!   price-impact, and swap-arrival burst signatures.
//! - [`launch_trajectory`] -- launch-sale-trajectory + creation-window
//!   competition feature families (§21.7, criterion 104).
//! - [`attention_spend`] -- versioned paid-attention-spend computation and the
//!   Tier-0 no-self-promotion guard (§29.10, criterion 110).
//! - [`discovery_audit`] -- launch-discovery completeness auditor emitting
//!   `COMPLETE | INCOMPLETE` with the shortfall (§62-M1, criterion 73).
//! - [`meta_rotation`] -- MetaRotationState time-safe category-assignment
//!   validator (§21.4, criterion 81).

pub mod attention_spend;
pub mod discovery_audit;
pub mod launch_trajectory;
pub mod meta_rotation;
pub mod microstructure;
pub mod scorer;
pub mod velocity;
