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

pub mod scorer;
pub mod velocity;
