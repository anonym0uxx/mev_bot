//! `pump_quant_protocol` — pump.fun / PumpSwap decoders + tx-instruction data builders.
//!
//! This crate ports the *pure, deterministic* portion of the legacy TypeScript
//! MEV stack (`pump-tx-builder.ts`, `bonding-curve-sim.ts`) into idiomatic,
//! std-only Rust. It contains four responsibilities, one per module:
//!
//! * [`decode`]  — strict, bounds-checked little-endian account decoders.
//! * [`curve`]   — integer bonding-curve / constant-product output math.
//! * [`ix`]      — instruction **data** serialization (discriminator + args).
//! * [`registry`]— versioned protocol-registry identifiers.
//!
//! # Constitution
//! * §22 — NO `f32`/`f64` on any outcome-controlling path. Every calculation
//!   in this crate is integer / fixed-point (lamports as `u64`/`u128`, ratios
//!   in basis points). There is not a single float in the crate.
//! * Overflow is always explicit (`checked_*` / `saturating_*`), never silent.
//! * Deterministic: identical inputs always yield identical outputs. No
//!   wall-clock, RNG, network or floating point participates in any result.
//! * Live I/O (RPC / submit / streams) is out of scope; only the data layout
//!   and math are ported here.

#![forbid(unsafe_code)]

pub mod curve;
pub mod decode;
pub mod ix;
pub mod registry;
