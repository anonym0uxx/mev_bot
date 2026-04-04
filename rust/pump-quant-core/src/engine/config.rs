//! Configuration loader for the momentum engine.
//!
//! Reads canary.json, extracts the `momentum` and `health` sections.

use std::path::Path;

use anyhow::{Context, Result};

use super::health::HealthConfig;

/// Minimal engine config — momentum + sniper engines.
/// Backrunner fields (gate, score, position, ride, entry_engine, risk) removed.
pub struct EngineConfig {
    pub health: HealthConfig,
    pub log_file: String,
    /// Post-graduation momentum engine configuration.
    pub momentum: crate::momentum::MomentumConfig,
    /// Sniper engine configuration (disabled by default).
    pub sniper: crate::sniper::SniperConfig,
}

/// Load canary.json from the given path and return a minimal `EngineConfig`.
/// Only the `momentum` and `health` sections are parsed.
pub fn load_config(path: &Path) -> Result<EngineConfig> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config at {}", path.display()))?;

    let root: serde_json::Value =
        serde_json::from_str(&raw).context("failed to parse canary.json as JSON")?;

    // ── Build HealthConfig from top-level `health` section ──────────
    let health = if let Some(health_val) = root.get("health") {
        let market_feed_stale_s: u64 = health_val
            .get("market_feed_stale_s")
            .and_then(|v| v.as_u64())
            .unwrap_or(45);
        let auto_pause_on_degraded: bool = health_val
            .get("auto_pause_on_degraded")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        HealthConfig {
            market_feed_stale_ms: market_feed_stale_s * 1000,
            auto_pause_on_degraded,
        }
    } else {
        HealthConfig::default()
    };

    // ── Load momentum config from top-level `momentum` section ──────
    let momentum: crate::momentum::MomentumConfig = root
        .get("momentum")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    // ── Load sniper config from top-level `sniper` section ────────
    let sniper: crate::sniper::SniperConfig = root
        .get("sniper")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    // ── Log file path from `mev.log_file` (backward compat) ────────
    let log_file = root
        .get("mev")
        .and_then(|mev| mev.get("log_file"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "data/backrun_paper_trades.jsonl".to_string());

    Ok(EngineConfig {
        health,
        log_file,
        momentum,
        sniper,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_defaults() {
        let health = HealthConfig::default();
        assert_eq!(health.market_feed_stale_ms, 45_000);
        assert!(health.auto_pause_on_degraded);
    }
}
