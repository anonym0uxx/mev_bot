//! Feed event router — single dispatch authority for all engine events.
//!
//! Owns health monitoring, stats sync, and Telegram alerting.
//! Routes each `FeedEvent` variant to the appropriate engine method
//! via `EngineRegistry`. Returns `false` on `FeedEvent::Shutdown` to
//! signal the caller to break the event loop.

use std::sync::{Arc, Mutex};

use tracing::info;

use crate::alerts::telegram::{self, TelegramAlerter};
use crate::api::EngineStats;
use crate::engine::health::{HealthMonitor, HealthStatus};
use crate::engine::registry::EngineRegistry;
use crate::engine::trading_engine::{GraduationEvent, PumpSwapVaults};
use crate::feeds::{FeedEvent, FeedSource};
use crate::momentum::types::GradEnrichment;

/// Internal event counters for stats logging and API sync.
struct EventCounters {
    trades_seen: u64,
    ticks: u64,
    migrations: u64,
    creator_sells: u64,
}

impl EventCounters {
    fn new() -> Self {
        Self {
            trades_seen: 0,
            ticks: 0,
            migrations: 0,
            creator_sells: 0,
        }
    }
}

/// Routes `FeedEvent`s to engines via `EngineRegistry`, manages health checks
/// and stats sync on tick boundaries.
pub struct FeedRouter {
    health_monitor: Arc<HealthMonitor>,
    shared_stats: Arc<Mutex<EngineStats>>,
    telegram_alerter: Option<Arc<TelegramAlerter>>,
    counters: EventCounters,
    health_check_interval: u64,
    stats_sync_interval: u64,
    stats_log_interval: u64,
}

impl FeedRouter {
    /// Create a new `FeedRouter`.
    ///
    /// - `health_monitor` — shared health monitor (also used by API)
    /// - `shared_stats` — API-visible stats (synced every 200 ticks)
    /// - `telegram_alerter` — optional Telegram alert sender
    pub fn new(
        health_monitor: Arc<HealthMonitor>,
        shared_stats: Arc<Mutex<EngineStats>>,
        telegram_alerter: Option<Arc<TelegramAlerter>>,
    ) -> Self {
        Self {
            health_monitor,
            shared_stats,
            telegram_alerter,
            counters: EventCounters::new(),
            health_check_interval: 100,  // ~5s at 50ms ticks
            stats_sync_interval: 200,    // ~10s at 50ms ticks
            stats_log_interval: 1000,    // every 1000 trades
        }
    }

    /// Dispatch a single feed event. Returns `false` on `Shutdown` (caller should break).
    ///
    /// This is the single dispatch authority — all routing logic lives here.
    /// The main event loop is just:
    /// ```ignore
    /// loop {
    ///     match engine_rx.recv() {
    ///         Ok(event) => { if !router.dispatch(event, &registry).await { break; } }
    ///         Err(_) => break,
    ///     }
    /// }
    /// ```
    pub async fn dispatch(&mut self, event: FeedEvent, registry: &EngineRegistry) -> bool {
        match event {
            FeedEvent::Trade(trade) => {
                self.health_monitor.record_event(trade.source, trade.timestamp_ms);
                self.counters.trades_seen += 1;

                if self.counters.trades_seen % self.stats_log_interval == 0 {
                    info!(
                        trades = self.counters.trades_seen,
                        ticks = self.counters.ticks,
                        migrations = self.counters.migrations,
                        creator_sells = self.counters.creator_sells,
                        "engine stats"
                    );
                }
            }

            FeedEvent::PreWarm(prewarm) => {
                self.health_monitor.record_event(prewarm.source, prewarm.timestamp_ms);
            }

            FeedEvent::Tick { ts_ms } => {
                self.counters.ticks += 1;

                // Engine tick dispatch (sequential — engines self-throttle)
                registry.dispatch_tick(ts_ms).await;

                // Health check every 100 ticks (~5s)
                if self.counters.ticks % self.health_check_interval == 0 {
                    let (health_status, recovered_feeds) = self.health_monitor.check(ts_ms);
                    self.handle_health_status(&health_status, &recovered_feeds, ts_ms);
                }

                // Stats sync every 200 ticks (~10s)
                if self.counters.ticks % self.stats_sync_interval == 0 {
                    if let Ok(mut stats) = self.shared_stats.lock() {
                        stats.trades_seen = self.counters.trades_seen;
                        stats.migrations_seen = self.counters.migrations;
                        stats.creator_sells_seen = self.counters.creator_sells;
                    }
                }
            }

            FeedEvent::TokenCreated(tc) => {
                registry.dispatch_token_created(tc.mint, tc.ts_ms);
            }

            FeedEvent::CreatorSell { mint: _, ts_ms } => {
                self.health_monitor.record_event(FeedSource::CoreCast, ts_ms);
                self.counters.creator_sells += 1;
            }

            FeedEvent::Migration { mint, ts_ms, source, sig } => {
                if matches!(source, crate::feeds::MigrationSource::CoreCastStream2) {
                    self.health_monitor.record_event(FeedSource::CoreCast, ts_ms);
                }
                self.counters.migrations += 1;

                let mint_b58 = bs58::encode(&mint).into_string();
                info!(
                    mint = %mint_b58,
                    ts_ms,
                    source = source.as_str(),
                    "[momentum] graduation migration detected"
                );

                registry.dispatch_graduation(GraduationEvent {
                    mint,
                    sig,
                    ts_ms,
                    source,
                    enrichment: GradEnrichment::UNKNOWN,
                    pumpswap_vaults: None,
                });
            }

            FeedEvent::PumpSwapGraduationDirect {
                mint, sig, ts_ms, coin_vault, pc_vault, source,
            } => {
                self.counters.migrations += 1;

                let mint_b58 = bs58::encode(&mint).into_string();
                info!(
                    mint = %mint_b58,
                    ts_ms,
                    source = source.as_str(),
                    "[momentum] PumpSwap graduation direct detected"
                );

                registry.dispatch_graduation(GraduationEvent {
                    mint,
                    sig,
                    ts_ms,
                    source,
                    enrichment: GradEnrichment::UNKNOWN,
                    pumpswap_vaults: Some(PumpSwapVaults { coin_vault, pc_vault }),
                });
            }

            FeedEvent::LpRemoval { .. } => {
                // No-op: engines handle exits internally
            }

            FeedEvent::Shutdown => {
                info!("Shutdown signal received");
                return false;
            }
        }

        true
    }

    /// Handle health status changes: log stale feeds and send Telegram alerts.
    fn handle_health_status(
        &self,
        status: &HealthStatus,
        recovered: &[&'static str],
        ts_ms: u64,
    ) {
        if let HealthStatus::Degraded { ref stale_feeds } = status {
            for feed in stale_feeds {
                let source = match *feed {
                    "Helius" => FeedSource::Helius,
                    _ => FeedSource::PumpPortal,
                };
                let last_ms = self.health_monitor.last_event_ms(source);
                let stale_s = if last_ms > 0 {
                    ts_ms.saturating_sub(last_ms) / 1000
                } else {
                    0
                };
                tracing::warn!(feed = %feed, stale_s, "Feed stale — trading paused");
                if let Some(ref tg) = self.telegram_alerter {
                    tg.try_send_blocking(&telegram::format_feed_stale_alert(feed, stale_s));
                }
            }
        }

        for feed in recovered {
            info!(feed = %feed, "Feed recovered — trading resumed");
            if let Some(ref tg) = self.telegram_alerter {
                tg.try_send_blocking(&telegram::format_feed_recovered_alert(feed));
            }
        }
    }

    /// Accessor for final stats logging on shutdown.
    pub fn trades_seen(&self) -> u64 {
        self.counters.trades_seen
    }

    pub fn ticks(&self) -> u64 {
        self.counters.ticks
    }

    pub fn migrations(&self) -> u64 {
        self.counters.migrations
    }

    pub fn creator_sells(&self) -> u64 {
        self.counters.creator_sells
    }
}
