//! SniperEngine — stub implementation (Phase 5).
//!
//! Targets newly-created tokens for early graduation sniping.
//! Currently a no-op stub that satisfies the `TradingEngine` trait.
//! Disabled by default via `SniperConfig::enabled = false`.

pub mod config;

pub use config::SniperConfig;

use std::sync::Arc;

use async_trait::async_trait;
use tracing::debug;

use crate::engine::trading_engine::{EngineHealthSnapshot, GraduationEvent, TradingEngine};
use crate::tx::ExecutionContext;

/// Sniper engine — early graduation token sniper.
///
/// Phase 5 stub: all event handlers are no-ops. The engine is registered
/// in the `EngineRegistry` but gated by `enabled()` (false by default).
pub struct SniperEngine {
    config: Arc<SniperConfig>,
    #[allow(dead_code)]
    exec_ctx: Arc<ExecutionContext>,
}

impl SniperEngine {
    pub fn new(config: Arc<SniperConfig>, exec_ctx: Arc<ExecutionContext>) -> Self {
        Self { config, exec_ctx }
    }
}

#[async_trait]
impl TradingEngine for SniperEngine {
    fn name(&self) -> &'static str {
        "sniper"
    }

    fn paper_mode(&self) -> bool {
        self.config.paper_mode
    }

    fn enabled(&self) -> bool {
        self.config.enabled
    }

    fn on_token_created(&self, mint: [u8; 32], ts_ms: u64) {
        if self.config.enabled {
            debug!(
                mint = %bs58::encode(&mint).into_string(),
                ts_ms,
                "[sniper] token created (stub — no action)"
            );
        }
    }

    async fn on_graduation(&self, _event: GraduationEvent) {
        // Sniper acts on TokenCreated, not graduations — no-op.
    }

    async fn on_tick(&self, _ts_ms: u64) {
        // No-op in stub.
    }

    fn health(&self) -> EngineHealthSnapshot {
        EngineHealthSnapshot::Unknown(serde_json::json!({
            "engine": "sniper",
            "enabled": self.config.enabled,
            "status": "stub"
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_sniper_engine_satisfies_trait() {
        // Verify SniperConfig defaults
        let config = SniperConfig::default();
        assert!(!config.enabled, "sniper must be disabled by default");
        assert!(config.paper_mode, "sniper must default to paper mode");
        assert!((config.max_position_sol - 0.05).abs() < f64::EPSILON);
        assert_eq!(config.max_grad_age_s, 60);
        assert_eq!(config.min_social_score, 0);

        // Verify serde round-trip
        let json = serde_json::to_string(&config).unwrap();
        let deser: SniperConfig = serde_json::from_str(&json).unwrap();
        assert!(!deser.enabled);
        assert!(deser.paper_mode);
    }
}
