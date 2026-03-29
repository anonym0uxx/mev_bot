//! Graduation arbitrage engine — production scaffolding (SPEC 4, Tasks 5-7).
//!
//! Detects migration events where a Pump.fun token graduates to Raydium AMM
//! or PumpSwap. The price dislocation between the bonding curve terminal price
//! and the DEX opening price creates an arbitrage opportunity.
//!
//! ## Architecture
//!
//! - `GradArbConfig` — parsed from EngineConfig graduation_arb fields
//! - `GradArbPosition` — live position with MFE/MAE tracking
//! - `GradArbClosedPosition` — completed trade for paper logging
//! - `GradArbStats` — atomic counters for real-time monitoring
//! - `GraduationArbEngine` — main engine struct, DashMap-backed positions
//!
//! ## Price Dislocation Math
//!
//! ```text
//! bc_terminal_price = vSol_terminal / vTokens_terminal
//! ray_opening_price = ray_reserve_sol / ray_reserve_tokens
//! spread_pct = (bc_terminal_price - ray_opening_price).abs() / bc_terminal_price * 100
//! ```

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;

use super::dedup::MigrationDedup;
use crate::feeds::MigrationSource;

// ── Config ───────────────────────────────────────────────────────────────────

/// Configuration for the graduation arbitrage engine.
/// Loaded from EngineConfig graduation_arb_* fields.
#[derive(Debug, Clone)]
pub struct GradArbConfig {
    /// Master toggle.
    pub enabled: bool,
    /// Paper mode — log trades but do not submit transactions.
    pub paper_mode: bool,
    /// Max position size in SOL.
    pub max_sol: f64,
    /// Minimum spread between BC terminal price and DEX opening price (%).
    pub min_spread_pct: f64,
    /// Take-profit target (fractional, e.g. 0.03 = 3%).
    pub tp_pct: f64,
    /// Stop-loss threshold (fractional, e.g. 0.02 = 2%).
    pub sl_pct: f64,
    /// Maximum hold time before forced exit (ms).
    pub max_hold_ms: u64,
    /// Jito tip for arb bundles (SOL).
    pub jito_tip_sol: f64,
    /// Dedup window — ignore duplicate migration events within this period (ms).
    pub dedup_ttl_ms: u64,
    /// RPC budget per arb attempt — timeout for pool reserve fetches (ms).
    pub rpc_timeout_ms: u64,
}

impl Default for GradArbConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            paper_mode: true,
            max_sol: 0.30,
            min_spread_pct: 3.0,
            tp_pct: 0.03,
            sl_pct: 0.02,
            max_hold_ms: 5_000,
            jito_tip_sol: 0.003,
            dedup_ttl_ms: 10_000,
            rpc_timeout_ms: 200,
        }
    }
}

impl GradArbConfig {
    /// Generate a config version string for paper trade logging.
    /// Format: `"grad-v{max_sol:.2}sol_{max_hold_ms}ms"`
    pub fn config_version(&self) -> String {
        format!("grad-v{:.2}sol_{}ms", self.max_sol, self.max_hold_ms)
    }
}

// ── Position Types ───────────────────────────────────────────────────────────

/// Type of DEX pool the token migrated to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolType {
    /// Raydium AMM V4 — traditional graduation target.
    RaydiumAmmV4,
    /// PumpSwap — Pump.fun's native DEX.
    PumpSwap,
}

impl PoolType {
    /// Serialization string for JSONL output.
    #[inline(always)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RaydiumAmmV4 => "raydium_amm_v4",
            Self::PumpSwap => "pump_swap",
        }
    }
}

/// Reason the graduation arb position was closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradArbExitReason {
    /// Hit take-profit target.
    TakeProfit,
    /// Hit stop-loss threshold.
    StopLoss,
    /// Exceeded max hold time.
    MaxHold,
    /// Spread below minimum threshold — no arb found.
    NoArbFound,
    /// Pool reserve fetch timed out within RPC budget.
    RpcTimeout,
    /// Could not resolve pool address from migration event.
    PoolNotFound,
}

impl GradArbExitReason {
    /// Serialization string for JSONL output.
    #[inline(always)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TakeProfit => "take_profit",
            Self::StopLoss => "stop_loss",
            Self::MaxHold => "max_hold",
            Self::NoArbFound => "no_arb_found",
            Self::RpcTimeout => "rpc_timeout",
            Self::PoolNotFound => "pool_not_found",
        }
    }
}

/// A live graduation arb position being tracked by the engine.
#[derive(Debug)]
pub struct GradArbPosition {
    /// Token mint address (32 bytes).
    pub mint: [u8; 32],
    /// DEX pool address.
    pub pool_address: [u8; 32],
    /// Type of pool (Raydium V4 or PumpSwap).
    pub pool_type: PoolType,
    /// Token price in lamports at entry.
    pub entry_price_lamports: u64,
    /// vSol reserves at migration (~85 SOL in lamports).
    pub entry_vsol_lamports: u64,
    /// SOL deployed in this position (lamports).
    pub size_lamports: u64,
    /// Bonding curve terminal price (SOL per token).
    pub bc_terminal_price: f64,
    /// Raydium/PumpSwap opening price (SOL per token).
    pub ray_opening_price: f64,
    /// Observed spread at entry (%).
    pub spread_pct: f64,
    /// Feed source that first detected the migration.
    pub detection_source: MigrationSource,
    /// Latency from migration tx to our detection (ms).
    pub detection_latency_ms: u64,
    /// Entry timestamp (epoch ms).
    pub entry_ts_ms: u64,
    /// Peak token price seen since entry (lamports) — for MFE tracking.
    pub peak_price_lamports: u64,
    /// Minimum token price seen since entry (lamports) — for MAE tracking.
    pub min_price_lamports: u64,
}

/// A completed graduation arb trade, ready for paper logging.
#[derive(Debug)]
pub struct GradArbClosedPosition {
    pub mint: [u8; 32],
    pub pool_address: [u8; 32],
    pub pool_type: PoolType,
    pub entry_price_lamports: u64,
    pub exit_price_lamports: u64,
    pub size_lamports: u64,
    pub bc_terminal_price: f64,
    pub ray_opening_price: f64,
    pub spread_pct: f64,
    pub detection_source: MigrationSource,
    pub detection_latency_ms: u64,
    pub entry_ts_ms: u64,
    pub exit_ts_ms: u64,
    pub hold_ms: u64,
    pub exit_reason: GradArbExitReason,
    /// Gross PnL in lamports (signed — can be negative).
    pub pnl_lamports: i64,
    /// Fee cost in lamports (Jito tip + priority fee).
    pub fee_lamports: u64,
    /// Net PnL in lamports (pnl_lamports - fee_lamports as i64).
    pub net_pnl_lamports: i64,
    /// Max favorable excursion in lamports.
    pub mfe_lamports: u64,
    /// Max adverse excursion in lamports.
    pub mae_lamports: u64,
}

// ── Stats ────────────────────────────────────────────────────────────────────

/// Lock-free atomic statistics for the graduation arb engine.
/// All counters use `Relaxed` ordering — eventual consistency is fine for stats.
pub struct GradArbStats {
    pub migrations_detected: AtomicU64,
    pub arb_entries: AtomicU64,
    pub arb_timeouts: AtomicU64,
    pub pool_not_found: AtomicU64,
    pub no_arb_spread: AtomicU64,
    pub exits_tp: AtomicU64,
    pub exits_sl: AtomicU64,
    pub exits_max_hold: AtomicU64,
    /// Gross PnL in lamports (signed via AtomicI64).
    pub gross_pnl_lamports: AtomicI64,
    /// Net PnL in lamports (signed via AtomicI64).
    pub net_pnl_lamports: AtomicI64,
}

impl GradArbStats {
    /// Create zeroed stats.
    pub fn new() -> Self {
        Self {
            migrations_detected: AtomicU64::new(0),
            arb_entries: AtomicU64::new(0),
            arb_timeouts: AtomicU64::new(0),
            pool_not_found: AtomicU64::new(0),
            no_arb_spread: AtomicU64::new(0),
            exits_tp: AtomicU64::new(0),
            exits_sl: AtomicU64::new(0),
            exits_max_hold: AtomicU64::new(0),
            gross_pnl_lamports: AtomicI64::new(0),
            net_pnl_lamports: AtomicI64::new(0),
        }
    }
}

impl Default for GradArbStats {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for GradArbStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GradArbStats")
            .field(
                "migrations_detected",
                &self.migrations_detected.load(Ordering::Relaxed),
            )
            .field("arb_entries", &self.arb_entries.load(Ordering::Relaxed))
            .field("arb_timeouts", &self.arb_timeouts.load(Ordering::Relaxed))
            .field(
                "pool_not_found",
                &self.pool_not_found.load(Ordering::Relaxed),
            )
            .field(
                "no_arb_spread",
                &self.no_arb_spread.load(Ordering::Relaxed),
            )
            .field("exits_tp", &self.exits_tp.load(Ordering::Relaxed))
            .field("exits_sl", &self.exits_sl.load(Ordering::Relaxed))
            .field(
                "exits_max_hold",
                &self.exits_max_hold.load(Ordering::Relaxed),
            )
            .field(
                "gross_pnl_lamports",
                &self.gross_pnl_lamports.load(Ordering::Relaxed),
            )
            .field(
                "net_pnl_lamports",
                &self.net_pnl_lamports.load(Ordering::Relaxed),
            )
            .finish()
    }
}

// ── Engine ───────────────────────────────────────────────────────────────────

/// Graduation arbitrage engine.
///
/// Evaluates migration events for arb opportunities between the bonding curve
/// terminal price and DEX opening price. Manages positions with TP/SL/MaxHold
/// exits and sends closed positions to the paper logger via crossbeam channel.
pub struct GraduationArbEngine {
    config: GradArbConfig,
    /// Live positions keyed by mint address.
    positions: DashMap<[u8; 32], GradArbPosition>,
    /// Migration event deduplicator.
    dedup: MigrationDedup,
    /// Shared atomic stats counters.
    stats: Arc<GradArbStats>,
    /// Channel sender for completed trades → paper logger thread.
    closed_tx: crossbeam_channel::Sender<GradArbClosedPosition>,
    /// Helius RPC URL for pool reserve fetches.
    helius_rpc_url: String,
}

impl GraduationArbEngine {
    /// Create a new graduation arb engine.
    pub fn new(
        config: GradArbConfig,
        stats: Arc<GradArbStats>,
        closed_tx: crossbeam_channel::Sender<GradArbClosedPosition>,
        helius_rpc_url: String,
    ) -> Self {
        let dedup_ttl_ms = config.dedup_ttl_ms;
        Self {
            config,
            positions: DashMap::with_capacity(16),
            dedup: MigrationDedup::new(dedup_ttl_ms),
            stats,
            closed_tx,
            helius_rpc_url,
        }
    }

    /// Get a reference to the engine config.
    pub fn config(&self) -> &GradArbConfig {
        &self.config
    }

    /// Get a reference to the shared stats.
    pub fn stats(&self) -> &Arc<GradArbStats> {
        &self.stats
    }

    /// Current number of open positions.
    pub fn position_count(&self) -> usize {
        self.positions.len()
    }

    /// Called when a migration event is detected from any feed source.
    ///
    /// Deduplicates across sources, then evaluates for arb opportunity.
    /// Currently a stub — full implementation in Task 9.
    pub async fn on_migration(
        &self,
        mint: [u8; 32],
        ts_ms: u64,
        source: MigrationSource,
        _sig: [u8; 32],
    ) {
        self.stats
            .migrations_detected
            .fetch_add(1, Ordering::Relaxed);

        if !self.config.enabled {
            return;
        }

        // Dedup: only process first detection within TTL window
        let _dedup_entry = match self.dedup.try_insert(mint, ts_ms, source) {
            Some(entry) => entry,
            None => return, // duplicate — already processing this migration
        };

        // TODO: Task 9 — pool resolution + spread calc + paper entry
        tracing::debug!(
            mint = %bs58::encode(mint).into_string(),
            source = source.as_str(),
            "[grad_arb] migration received (engine not yet fully implemented)"
        );
    }

    /// Called every tick (50ms) for position management.
    ///
    /// Checks all open positions for TP/SL/MaxHold exits.
    /// Currently a stub — full implementation in Task 10.
    #[inline(always)]
    pub fn on_tick(&self, _now_ms: u64) {
        // TODO: Task 10 — iterate positions, check MaxHold/TP/SL,
        // close expired positions and send via closed_tx
    }

    /// Get the Helius RPC URL (for pool reserve fetches in Task 9).
    pub fn helius_rpc_url(&self) -> &str {
        &self.helius_rpc_url
    }

    /// Get the closed position sender (for external close triggers).
    pub fn closed_tx(&self) -> &crossbeam_channel::Sender<GradArbClosedPosition> {
        &self.closed_tx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_config() -> GradArbConfig {
        GradArbConfig {
            enabled: true,
            paper_mode: true,
            max_sol: 0.30,
            min_spread_pct: 3.0,
            tp_pct: 0.03,
            sl_pct: 0.02,
            max_hold_ms: 5_000,
            jito_tip_sol: 0.003,
            dedup_ttl_ms: 10_000,
            rpc_timeout_ms: 200,
        }
    }

    fn make_test_engine() -> (GraduationArbEngine, crossbeam_channel::Receiver<GradArbClosedPosition>) {
        let config = make_test_config();
        let stats = Arc::new(GradArbStats::new());
        let (tx, rx) = crossbeam_channel::unbounded();
        let engine = GraduationArbEngine::new(
            config,
            stats,
            tx,
            "https://rpc.example.com".to_string(),
        );
        (engine, rx)
    }

    #[test]
    fn test_grad_arb_config_version() {
        let config = make_test_config();
        assert_eq!(config.config_version(), "grad-v0.30sol_5000ms");
    }

    #[test]
    fn test_grad_arb_config_version_custom() {
        let mut config = make_test_config();
        config.max_sol = 1.50;
        config.max_hold_ms = 10_000;
        assert_eq!(config.config_version(), "grad-v1.50sol_10000ms");
    }

    #[test]
    fn test_grad_arb_stats_default() {
        let stats = GradArbStats::new();
        assert_eq!(stats.migrations_detected.load(Ordering::Relaxed), 0);
        assert_eq!(stats.arb_entries.load(Ordering::Relaxed), 0);
        assert_eq!(stats.gross_pnl_lamports.load(Ordering::Relaxed), 0);
        assert_eq!(stats.net_pnl_lamports.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_grad_arb_engine_construction() {
        let (engine, _rx) = make_test_engine();
        assert!(engine.config().enabled);
        assert!(engine.config().paper_mode);
        assert_eq!(engine.position_count(), 0);
        assert_eq!(engine.helius_rpc_url(), "https://rpc.example.com");
    }

    #[test]
    fn test_grad_arb_engine_disabled_skips_processing() {
        let mut config = make_test_config();
        config.enabled = false;
        let stats = Arc::new(GradArbStats::new());
        let (tx, _rx) = crossbeam_channel::unbounded();
        let engine = GraduationArbEngine::new(config, stats.clone(), tx, String::new());

        // Run on_migration synchronously via tokio runtime
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            engine
                .on_migration([1u8; 32], 1000, MigrationSource::HeliusLogs, [0u8; 32])
                .await;
        });

        // Stats should increment even when disabled (for monitoring)
        assert_eq!(stats.migrations_detected.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_pool_type_as_str() {
        assert_eq!(PoolType::RaydiumAmmV4.as_str(), "raydium_amm_v4");
        assert_eq!(PoolType::PumpSwap.as_str(), "pump_swap");
    }

    #[test]
    fn test_exit_reason_as_str() {
        assert_eq!(GradArbExitReason::TakeProfit.as_str(), "take_profit");
        assert_eq!(GradArbExitReason::StopLoss.as_str(), "stop_loss");
        assert_eq!(GradArbExitReason::MaxHold.as_str(), "max_hold");
        assert_eq!(GradArbExitReason::NoArbFound.as_str(), "no_arb_found");
        assert_eq!(GradArbExitReason::RpcTimeout.as_str(), "rpc_timeout");
        assert_eq!(GradArbExitReason::PoolNotFound.as_str(), "pool_not_found");
    }

    #[test]
    fn test_closed_position_pnl_fields() {
        let cp = GradArbClosedPosition {
            mint: [1u8; 32],
            pool_address: [2u8; 32],
            pool_type: PoolType::RaydiumAmmV4,
            entry_price_lamports: 1_000_000,
            exit_price_lamports: 1_030_000,
            size_lamports: 300_000_000, // 0.3 SOL
            bc_terminal_price: 0.000001234,
            ray_opening_price: 0.000001176,
            spread_pct: 4.7,
            detection_source: MigrationSource::HeliusLogs,
            detection_latency_ms: 82,
            entry_ts_ms: 1_700_000_000_000,
            exit_ts_ms: 1_700_000_001_240,
            hold_ms: 1_240,
            exit_reason: GradArbExitReason::TakeProfit,
            pnl_lamports: 12_000_000,
            fee_lamports: 4_000_000,
            net_pnl_lamports: 8_000_000,
            mfe_lamports: 14_000_000,
            mae_lamports: 2_000_000,
        };
        assert_eq!(cp.hold_ms, 1_240);
        assert_eq!(cp.net_pnl_lamports, cp.pnl_lamports - cp.fee_lamports as i64);
    }

    #[test]
    fn test_grad_arb_stats_atomic_operations() {
        let stats = GradArbStats::new();
        stats.migrations_detected.fetch_add(5, Ordering::Relaxed);
        stats.arb_entries.fetch_add(3, Ordering::Relaxed);
        stats.gross_pnl_lamports.fetch_add(1_000_000, Ordering::Relaxed);
        stats.net_pnl_lamports.fetch_add(-500_000, Ordering::Relaxed);

        assert_eq!(stats.migrations_detected.load(Ordering::Relaxed), 5);
        assert_eq!(stats.arb_entries.load(Ordering::Relaxed), 3);
        assert_eq!(stats.gross_pnl_lamports.load(Ordering::Relaxed), 1_000_000);
        assert_eq!(stats.net_pnl_lamports.load(Ordering::Relaxed), -500_000);
    }

    #[test]
    fn test_on_tick_noop() {
        let (engine, _rx) = make_test_engine();
        // Should not panic — stub implementation
        engine.on_tick(1_700_000_000_000);
    }
}
