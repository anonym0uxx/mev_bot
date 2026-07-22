//! `pump_quant_simulator` — deterministic backtest / PAPER execution engine.
//!
//! Responsibility: run the same production decision core over *recorded* inputs
//! before any live capital is risked (constitution §38 execution simulator, §39
//! execution-calibration budget). Everything in this crate is a pure, deterministic
//! function of its inputs: no wall-clock, no RNG, no network, no filesystem, and
//! **no floating-point arithmetic in any outcome-controlling path** (§22). All
//! monetary quantities are integer lamports or fixed-point basis points; all
//! intermediate widening uses `u128`/`i128` with explicit checked/saturating
//! contracts. Live streaming and order submission are out of scope (server-side).
//!
//! Module map:
//! * [`fixed`] — integer / fixed-point primitives (§22).
//! * [`terminal_loss`] — predeclared terminal-loss accounting for unexitable
//!   positions (§38).
//! * [`fill`] — Modes A/B/C fill models with exit-impairment (§38).
//! * [`calibration`] — versioned, memory-bounded `CalibrationStore` and
//!   deterministic model application over recorded fills (§38/§39).
//! * [`capacity`] — capacity-curve harness over the §55 size grid.
//! * [`hazard`] — partial-pooled, phase-separated hazard estimator (§48).
#![warn(missing_docs)]
#![forbid(unsafe_code)]

pub mod calibration;
pub mod capacity;
pub mod fill;
pub mod fixed;
pub mod hazard;
pub mod terminal_loss;
