pub mod bayesian_signal;
pub mod bonding_curve;
pub mod config;
pub mod entry_engine;
pub mod exit_v4;
pub mod kelly_sizing;
pub mod entry_randomizer;
pub mod gates;
pub mod health;
pub mod hot_path;
pub mod latency;
pub mod positions;
pub mod regime;
pub mod ride_state;
pub mod risk_manager;
pub mod watchlist;
pub mod scorer;
pub mod scoring;

#[cfg(test)]
mod integration_tests;
