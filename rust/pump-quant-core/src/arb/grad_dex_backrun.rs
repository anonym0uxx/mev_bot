//! Graduation→DEX Backrun Engine (v1)
//!
//! When a Pump.fun token graduates to Raydium AMM, we skip the BC→DEX price
//! dislocation arb (requires sub-100ms we can't achieve with current RPC latency)
//! and instead watch the newly-created DEX pool for large opening buyers.
//!
//! ## Strategy
//!
//! Opening buyers on a freshly-graduated token are typically retail/bots without
//! MEV protection. Large first buys (>0.5 SOL) create immediate price impact that
//! we can backrun within the same or next slot via Jito.
//!
//! ## Architecture
//!
//! ```text
//! MigrationEvent → on_migration() → MonitorState inserted into DashMap
//! DEX trade event → on_dex_trade() → check trigger → open position → manage exit
//! Periodic tick → prune_stale() → remove timed-out monitors
//! ```
//!
//! ## Paper Mode
//!
//! In paper mode, all trades are simulated and logged to
//! `data/grad_dex_backrun_paper_trades.jsonl` without any on-chain transactions.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use serde_json::json;

use super::graduation::PoolType;

// ── Config ────────────────────────────────────────────────────────────────────

/// Configuration for the graduation→DEX backrun engine.
#[derive(Debug, Clone)]
pub struct GradDexBackrunConfig {
    /// Master toggle. Default: false.
    pub enabled: bool,
    /// Paper mode — log trades but do not submit transactions. Default: true.
    pub paper_mode: bool,
    /// Number of Solana slots to monitor after graduation (1 slot ≈ 400ms).
    /// Default: 5 slots ≈ 2000ms.
    pub monitor_slots: u8,
    /// Minimum trigger buy size in SOL to qualify for a backrun entry.
    /// Filters small retail buys with insufficient price impact. Default: 0.5 SOL.
    pub min_trigger_buy_sol: f64,
    /// Our position size in SOL. Default: 0.1 SOL.
    pub entry_size_sol: f64,
    /// Take-profit threshold (fractional). Default: 0.02 (2%).
    pub take_profit_pct: f64,
    /// Stop-loss threshold (fractional). Default: 0.015 (1.5%).
    pub stop_loss_pct: f64,
    /// Jito tip per backrun bundle in SOL. Default: 0.002 SOL.
    pub jito_tip_sol: f64,
    /// Maximum monitoring window in ms before abandoning. Default: 2000ms.
    pub monitor_timeout_ms: u64,
    /// Maximum concurrent active monitors. Default: 10.
    pub max_concurrent_monitors: usize,
    /// Path for paper trade JSONL log.
    pub log_path: String,
}

impl Default for GradDexBackrunConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            paper_mode: true,
            monitor_slots: 5,
            min_trigger_buy_sol: 0.5,
            entry_size_sol: 0.1,
            take_profit_pct: 0.02,
            stop_loss_pct: 0.015,
            jito_tip_sol: 0.002,
            monitor_timeout_ms: 2_000,
            max_concurrent_monitors: 10,
            log_path: "data/grad_dex_backrun_paper_trades.jsonl".to_string(),
        }
    }
}

// ── Stats ─────────────────────────────────────────────────────────────────────

/// Atomic statistics for the graduation→DEX backrun engine.
/// All counters use relaxed ordering — only used for monitoring, not synchronization.
#[derive(Default)]
pub struct GradDexBackrunStats {
    /// Total migration events received.
    pub migrations_seen: AtomicU64,
    /// Monitors spawned (graduation→DEX backrun candidates).
    pub monitors_spawned: AtomicU64,
    /// Entries taken (backrun positions opened).
    pub entries_taken: AtomicU64,
    /// Take-profit exits.
    pub take_profits: AtomicU64,
    /// Stop-loss exits.
    pub stop_losses: AtomicU64,
    /// Monitors that timed out without a triggering trade.
    pub timeouts: AtomicU64,
    /// Gross PnL in lamports (signed, stored as i64 bits in u64).
    pub gross_pnl_lamports: AtomicI64,
    /// Total fees paid in lamports.
    pub fees_lamports: AtomicU64,
}

impl GradDexBackrunStats {
    pub fn net_pnl_sol(&self) -> f64 {
        let gross = self.gross_pnl_lamports.load(Ordering::Relaxed) as f64 / 1e9;
        let fees = self.fees_lamports.load(Ordering::Relaxed) as f64 / 1e9;
        gross - fees
    }
}

// ── Internal State ────────────────────────────────────────────────────────────

/// Per-graduation monitor state.
#[derive(Debug, Clone)]
struct MonitorState {
    mint: [u8; 32],
    pool_type: PoolType,
    start_ms: u64,
    /// Entry price in lamports-per-token (None = not yet entered).
    entry_price: Option<f64>,
    /// Entry timestamp ms.
    entry_time_ms: u64,
    /// Position size in lamports.
    entry_size_lamports: u64,
    /// Fee paid for this trade in lamports.
    fee_lamports: u64,
    /// Maximum favorable excursion in lamports.
    mfe_lamports: i64,
    /// Maximum adverse excursion in lamports.
    mae_lamports: i64,
}

// ── Engine ────────────────────────────────────────────────────────────────────

/// Graduation→DEX backrun engine.
///
/// Thread-safe: uses DashMap for concurrent access from the event loop.
pub struct GradDexBackrunEngine {
    config: GradDexBackrunConfig,
    active_monitors: DashMap<[u8; 32], MonitorState>,
    stats: Arc<GradDexBackrunStats>,
    log_file: Option<std::sync::Mutex<std::fs::File>>,
}

impl GradDexBackrunEngine {
    /// Create a new engine. Opens the paper log file if enabled and in paper mode.
    pub fn new(config: GradDexBackrunConfig) -> Self {
        let log_file = if config.enabled && config.paper_mode {
            match OpenOptions::new()
                .create(true)
                .append(true)
                .open(&config.log_path)
            {
                Ok(f) => {
                    tracing::info!("[grad_dex_backrun] paper log: {}", config.log_path);
                    Some(std::sync::Mutex::new(f))
                }
                Err(e) => {
                    tracing::error!("[grad_dex_backrun] failed to open log: {}", e);
                    None
                }
            }
        } else {
            None
        };

        Self {
            config,
            active_monitors: DashMap::with_capacity(16),
            stats: Arc::new(GradDexBackrunStats::default()),
            log_file,
        }
    }

    /// Returns a clone of the stats handle for external monitoring.
    pub fn stats(&self) -> Arc<GradDexBackrunStats> {
        Arc::clone(&self.stats)
    }

    /// Called when a migration event fires for a token.
    ///
    /// Starts monitoring the new DEX pool for large opening buyers.
    /// Skips PumpSwap graduations (pump.fun controls migration price, no backrun alpha).
    /// Non-blocking — monitoring is driven by subsequent `on_dex_trade()` calls.
    pub fn on_migration(&self, mint: [u8; 32], pool_type: PoolType, now_ms: u64) {
        if !self.config.enabled {
            return;
        }
        // PumpSwap: pump.fun controls migration price → no structural backrun opportunity
        if pool_type == PoolType::PumpSwap {
            return;
        }
        // Cap concurrent monitors to bound memory usage
        if self.active_monitors.len() >= self.config.max_concurrent_monitors {
            tracing::debug!("[grad_dex_backrun] monitor cap reached, skipping migration");
            return;
        }

        self.stats.migrations_seen.fetch_add(1, Ordering::Relaxed);
        self.stats.monitors_spawned.fetch_add(1, Ordering::Relaxed);

        let fee_lamports = (self.config.jito_tip_sol * 1e9) as u64
            + 5_000 // base tx fee (5000 lamports)
            + 2_100; // Jito bundle overhead estimate

        let state = MonitorState {
            mint,
            pool_type,
            start_ms: now_ms,
            entry_price: None,
            entry_time_ms: 0,
            entry_size_lamports: (self.config.entry_size_sol * 1e9) as u64,
            fee_lamports,
            mfe_lamports: 0,
            mae_lamports: 0,
        };
        self.active_monitors.insert(mint, state);

        tracing::debug!(
            mint = %bs58::encode(mint).into_string(),
            "[grad_dex_backrun] monitoring graduation"
        );
    }

    /// Called for every DEX trade on monitored mints.
    ///
    /// In paper mode: simulates entry/exit and logs the result.
    /// In live mode: would submit Jito backrun bundle (TODO).
    ///
    /// # Arguments
    /// * `mint` — token mint address
    /// * `buy_sol` — SOL size of this trade (0 if sell)
    /// * `price` — current price (lamports per token atom)
    /// * `is_buy` — true if this is a buy trade
    /// * `now_ms` — current epoch ms
    pub fn on_dex_trade(
        &self,
        mint: &[u8; 32],
        buy_sol: f64,
        price: f64,
        is_buy: bool,
        now_ms: u64,
    ) {
        let mut entry = match self.active_monitors.get_mut(mint) {
            Some(e) => e,
            None => return,
        };

        // Check timeout
        if now_ms.saturating_sub(entry.start_ms) > self.config.monitor_timeout_ms {
            let mint_copy = entry.mint;
            drop(entry);
            self.stats.timeouts.fetch_add(1, Ordering::Relaxed);
            self.active_monitors.remove(&mint_copy);
            return;
        }

        if entry.entry_price.is_none() {
            // Not yet entered — look for a large opening buy to backrun
            if is_buy && buy_sol >= self.config.min_trigger_buy_sol && price > 0.0 {
                entry.entry_price = Some(price);
                entry.entry_time_ms = now_ms;
                self.stats.entries_taken.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    mint = %bs58::encode(mint).into_string(),
                    buy_sol = buy_sol,
                    price = price,
                    "[grad_dex_backrun] entry triggered"
                );
            }
            return;
        }

        // In position — manage exit
        let entry_price = entry.entry_price.unwrap();
        if entry_price <= 0.0 {
            return;
        }

        let pnl_pct = (price - entry_price) / entry_price;
        let pnl_lamports = (pnl_pct * entry.entry_size_lamports as f64) as i64;

        // Track MFE/MAE
        if pnl_lamports > entry.mfe_lamports {
            entry.mfe_lamports = pnl_lamports;
        }
        if pnl_lamports < entry.mae_lamports {
            entry.mae_lamports = pnl_lamports;
        }

        let exit_reason = if pnl_pct >= self.config.take_profit_pct {
            Some("take_profit")
        } else if pnl_pct <= -self.config.stop_loss_pct {
            Some("stop_loss")
        } else {
            None
        };

        if let Some(reason) = exit_reason {
            let mint_copy = entry.mint;
            let pool_type = entry.pool_type;
            let entry_price_copy = entry_price;
            let entry_time_ms = entry.entry_time_ms;
            let entry_size_lamports = entry.entry_size_lamports;
            let fee_lamports = entry.fee_lamports;
            let mfe = entry.mfe_lamports;
            let mae = entry.mae_lamports;
            drop(entry);

            self.close_position(
                mint_copy,
                pool_type,
                entry_price_copy,
                price,
                entry_time_ms,
                now_ms,
                entry_size_lamports,
                fee_lamports,
                mfe,
                mae,
                reason,
            );
        }
    }

    /// Close a position: update stats and log paper trade.
    #[allow(clippy::too_many_arguments)]
    fn close_position(
        &self,
        mint: [u8; 32],
        pool_type: PoolType,
        entry_price: f64,
        exit_price: f64,
        entry_time_ms: u64,
        exit_time_ms: u64,
        size_lamports: u64,
        fee_lamports: u64,
        mfe_lamports: i64,
        mae_lamports: i64,
        exit_reason: &'static str,
    ) {
        self.active_monitors.remove(&mint);

        let pnl_lamports =
            ((exit_price - entry_price) / entry_price * size_lamports as f64) as i64;
        let net_lamports = pnl_lamports - fee_lamports as i64;
        let size_sol = size_lamports as f64 / 1e9;
        let pnl_sol = pnl_lamports as f64 / 1e9;
        let fee_sol = fee_lamports as f64 / 1e9;
        let net_sol = net_lamports as f64 / 1e9;
        let hold_ms = exit_time_ms.saturating_sub(entry_time_ms);

        // Update stats
        self.stats
            .gross_pnl_lamports
            .fetch_add(pnl_lamports, Ordering::Relaxed);
        self.stats
            .fees_lamports
            .fetch_add(fee_lamports, Ordering::Relaxed);
        match exit_reason {
            "take_profit" => {
                self.stats.take_profits.fetch_add(1, Ordering::Relaxed);
            }
            "stop_loss" => {
                self.stats.stop_losses.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }

        tracing::info!(
            mint = %bs58::encode(mint).into_string(),
            exit_reason = exit_reason,
            pnl_sol = pnl_sol,
            net_sol = net_sol,
            hold_ms = hold_ms,
            "[grad_dex_backrun] position closed"
        );

        // Log paper trade
        if self.config.paper_mode {
            let mint_b58 = bs58::encode(mint).into_string();
            let record = json!({
                "mint": mint_b58,
                "poolType": pool_type.as_str(),
                "entryPrice": entry_price,
                "exitPrice": exit_price,
                "entryTimestampMs": entry_time_ms,
                "exitTimestampMs": exit_time_ms,
                "holdMs": hold_ms,
                "sizeSol": size_sol,
                "pnlSol": pnl_sol,
                "feesSol": fee_sol,
                "netPnlSol": net_sol,
                "mfeSol": mfe_lamports as f64 / 1e9,
                "maeSol": mae_lamports as f64 / 1e9,
                "exitReason": exit_reason,
                "engineVersion": "grad-dex-backrun-v1",
                "is_paper": true,
                "recordedAt": exit_time_ms,
            });

            let mut line = record.to_string();
            line.push('\n');

            if let Some(ref log_mutex) = self.log_file {
                if let Ok(mut file) = log_mutex.lock() {
                    let _ = file.write_all(line.as_bytes());
                }
            }
        }
    }

    /// Prune monitors that have exceeded their timeout window.
    /// Call periodically (e.g., every 5 seconds) from the engine loop.
    pub fn prune_stale(&self, now_ms: u64) {
        let timeout = self.config.monitor_timeout_ms;
        let timeouts = self.active_monitors.iter().filter(|e| {
            now_ms.saturating_sub(e.value().start_ms) > timeout
                && e.value().entry_price.is_none()
        }).count() as u64;

        if timeouts > 0 {
            self.stats.timeouts.fetch_add(timeouts, Ordering::Relaxed);
        }

        self.active_monitors.retain(|_, state| {
            now_ms.saturating_sub(state.start_ms) <= timeout
                || state.entry_price.is_some() // keep active positions even if past timeout
        });
    }

    /// Returns true if this engine is enabled and running.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Returns current count of active monitors.
    pub fn active_monitor_count(&self) -> usize {
        self.active_monitors.len()
    }
}
