//! Core trait for all trading engines.
//!
//! Object-safe via `async_trait`. Each engine is wrapped in `Arc` and
//! dispatched to by FeedRouter. Engines receive events in parallel (fan-out).

use async_trait::async_trait;
use crate::feeds::MigrationSource;
use crate::momentum::types::GradEnrichment;

/// Snapshot of engine health/stats for the API layer.
#[derive(Debug, serde::Serialize)]
#[serde(tag = "engine")]
pub enum EngineHealthSnapshot {
    Momentum(crate::momentum::MomentumStats),
    Sniper(serde_json::Value),
    Unknown(serde_json::Value),
}

/// Unified graduation event — wraps both `Migration` and `PumpSwapGraduationDirect`.
/// Engines get one method (`on_graduation`) instead of two.
#[derive(Debug, Clone)]
pub struct GraduationEvent {
    pub mint: [u8; 32],
    pub sig: [u8; 64],
    pub ts_ms: u64,
    pub source: MigrationSource,
    pub enrichment: GradEnrichment,
    /// Pre-extracted vault accounts from Helius Enhanced WS.
    /// `None` for legacy `Migration` events (engine resolves via RPC).
    pub pumpswap_vaults: Option<PumpSwapVaults>,
}

#[derive(Debug, Clone, Copy)]
pub struct PumpSwapVaults {
    pub coin_vault: [u8; 32],
    pub pc_vault: [u8; 32],
}

#[async_trait]
pub trait TradingEngine: Send + Sync + 'static {
    /// Human-readable name, unique across all registered engines.
    fn name(&self) -> &'static str;

    /// Whether this engine is in paper mode (log trades, no real TXs).
    fn paper_mode(&self) -> bool;

    /// Whether this engine is enabled. Checked by registry before dispatch.
    fn enabled(&self) -> bool;

    /// Called on every `FeedEvent::TokenCreated`. Non-async, must be fast.
    fn on_token_created(&self, mint: [u8; 32], ts_ms: u64);

    /// Called on every graduation event. Cold path (~10-50/day for momentum).
    /// Spawned in tokio::spawn by registry — may do RPC calls.
    async fn on_graduation(&self, event: GraduationEvent);

    /// Called every tick (~50ms). Hot path — must return quickly.
    async fn on_tick(&self, ts_ms: u64);

    /// Engine health/stats snapshot for API.
    fn health(&self) -> EngineHealthSnapshot;

    /// Post-startup recovery hook. Called once, 5s after init. Default: no-op.
    async fn on_startup_recovery(&self) {}

    /// Graceful shutdown. Default: no-op.
    async fn shutdown(&self) {}
}
