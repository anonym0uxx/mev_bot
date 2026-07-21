//! Configuration for the SniperEngine.
//!
//! Sniper targets newly-created tokens (TokenCreated events) that graduate
//! quickly. Disabled by default — stub only in Phase 5.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SniperConfig {
    #[serde(default)]
    pub enabled: bool, // default: false

    #[serde(default = "sniper_default_paper_mode")]
    pub paper_mode: bool, // default: true

    #[serde(default = "sniper_default_position_sol")]
    pub max_position_sol: f64, // default: 0.05

    #[serde(default = "sniper_default_grad_age")]
    pub max_grad_age_s: u32, // default: 60

    #[serde(default)]
    pub min_social_score: u8, // default: 0
}

impl Default for SniperConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            paper_mode: true,
            max_position_sol: 0.05,
            max_grad_age_s: 60,
            min_social_score: 0,
        }
    }
}

fn sniper_default_paper_mode() -> bool {
    true
}
fn sniper_default_position_sol() -> f64 {
    0.05
}
fn sniper_default_grad_age() -> u32 {
    60
}
