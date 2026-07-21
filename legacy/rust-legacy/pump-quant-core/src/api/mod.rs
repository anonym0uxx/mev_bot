//! HTTP control/metrics API for the pump-quant Rust engine.
//!
//! Exposes health, stats, control (pause/resume), and positions endpoints
//! on port 9421 (avoids conflict with TypeScript daemon on 9420).

pub mod server;

pub use server::{ApiState, EngineStats, start_server};
