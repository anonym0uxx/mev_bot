pub mod bonding_curve;
pub mod config;
pub mod entry_engine;
pub mod entry_randomizer;
// RIDE-only engine: exit_machine is dead code, kept until positions.rs is cleaned up
pub mod exit_machine;
pub mod gates;
pub mod health;
pub mod hot_path;
pub mod latency;
pub mod positions;
pub mod regime;
pub mod ride_state;
pub mod risk_manager;
pub mod scorer;
pub mod scoring;
pub mod signal_engine;

#[cfg(test)]
#[cfg(test)]
mod integration_tests;
