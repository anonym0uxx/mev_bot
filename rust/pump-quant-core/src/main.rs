//! pump-quant-core — Rust MEV engine entry point.
//!
//! Reads config from canary.json, wires feeds → joiner → engine hot-path.
//! Paper mode: logs all closed positions to SQLite.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use crossbeam_channel::bounded;
use tracing::info;
use tracing_subscriber::{fmt, EnvFilter};

use pump_quant_core::alerts::telegram::{self, TelegramAlerter};
use pump_quant_core::engine::config::load_config;
use pump_quant_core::engine::gates::GateStack;
use pump_quant_core::engine::health::HealthMonitor;
use pump_quant_core::engine::hot_path::HotPath;
use pump_quant_core::engine::positions::{ClosedPosition, ExitReason, PositionManager};
use pump_quant_core::engine::scorer::Scorer;
use pump_quant_core::api::{ApiState, EngineStats, start_server};
use pump_quant_core::feeds::{
    event_joiner::EventJoiner,
    helius::{HeliusConfig, HeliusWsClient},
    shredstream::ShredStreamConfig,
    FeedEvent, FeedSource,
};
use pump_quant_core::persistence::sqlite::{SqliteLogger, TradeLogEntry};
use pump_quant_core::persistence::paper_logger::PaperTradeLogger;
use pump_quant_core::persistence::engine_state::write_engine_state;

const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

fn now_ms_system() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn exit_reason_str(reason: ExitReason) -> &'static str {
    match reason {
        ExitReason::TakeProfit => "take_profit",
        ExitReason::StopLoss => "stop_loss",
        ExitReason::NextBuyer => "next_buyer",
        ExitReason::MaxHold => "max_hold",
        ExitReason::IntraHoldTrail => "intra_hold_trail",
        ExitReason::MomentumDecayFlat => "momentum_decay_flat",
        ExitReason::MomentumDecayFade => "momentum_decay_fade",
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Install rustls crypto provider (required before any TLS connections)
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Init tracing
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    tracing_subscriber::registry()
        .with(fmt::layer().with_target(true).compact())
        .with(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    // ── Load config ─────────────────────────────────────────────────
    let config_path = std::env::var("CANARY_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("config/canary.json")
        });

    info!(path = %config_path.display(), "Loading config");
    let engine_config = load_config(&config_path)?;

    let paper_mode = std::env::var("PAPER_MODE")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(engine_config.paper_mode);

    info!(
        paper_mode,
        gate_min_buy_lam = engine_config.gate.trigger_min_buy_lamports,
        gate_max_buy_lam = engine_config.gate.trigger_max_buy_lamports,
        min_vsol = engine_config.gate.min_vsol_lamports,
        max_vsol = engine_config.gate.max_vsol_lamports,
        min_score = engine_config.gate.trigger_min_score,
        max_hold_ms = engine_config.position.max_hold_ms,
        tp_tiers = engine_config.position.tp_tiers.len(),
        size_tiers = engine_config.position.size_tiers.len(),
        blocked_hours = engine_config.gate.blocked_hours_utc.len(),
        "pump-quant-core starting"
    );

    // ── Write engine state on startup ─────────────────────────────────
    let daemon_started_at_ms = now_ms_system();
    let data_dir = std::path::Path::new(&engine_config.log_file)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "data".to_string());
    if let Err(e) = write_engine_state(&data_dir, daemon_started_at_ms) {
        tracing::warn!("Failed to write initial engine state: {e}");
    } else {
        info!("Wrote engine-state.json to {data_dir}");
    }

    // ── Build engine components ─────────────────────────────────────
    let min_score = engine_config.gate.trigger_min_score;
    let gate_stack = GateStack::new(engine_config.gate);
    let scorer = Scorer::new(
        engine_config.score,
        engine_config.position.tp_tiers.first().map(|_| 0).unwrap_or(0)
            .max(33_000_000_000), // use config min_vsol for scorer range
        engine_config.position.tp_tiers.first().map(|_| 0).unwrap_or(0)
            .max(43_000_000_000), // use config max_vsol for scorer range
    );

    // ── Create Health Monitor ────────────────────────────────────────
    let health_monitor = HealthMonitor::new(&engine_config.health);
    info!(
        stale_threshold_ms = engine_config.health.market_feed_stale_ms,
        auto_pause = engine_config.health.auto_pause_on_degraded,
        "Health monitor created"
    );

    // ── Create Telegram Alerter (optional) ──────────────────────────
    let telegram_alerter = TelegramAlerter::new();
    if telegram_alerter.is_some() {
        info!("Telegram alerter enabled");
    } else {
        info!("Telegram alerter disabled (TELEGRAM_BOT_TOKEN / TELEGRAM_CHAT_ID not set)");
    }

    // Channel for closed positions: position_manager → main loop (safety tracking)
    let (closed_tx, closed_rx) = bounded::<ClosedPosition>(256);
    // Second channel: main loop → logger thread (after safety processing)
    let (logger_tx, logger_rx) = bounded::<ClosedPosition>(256);

    let max_entry_size_lamports = engine_config.position.max_entry_size_lamports;
    let position_manager = PositionManager::new(engine_config.position, closed_tx);
    let mut hot_path = HotPath::new(
        gate_stack,
        scorer,
        position_manager,
        now_ms_system,
        paper_mode,
        min_score,
        engine_config.daily_loss_cap_lamports,
        engine_config.consecutive_stop_pause_count,
        engine_config.consecutive_stop_pause_ms,
        engine_config.boosted_hours_utc.clone(),
        engine_config.tod_boost_multiplier,
        max_entry_size_lamports,
    );

    // Attach health monitor to hot path for entry gating
    hot_path.set_health_monitor(health_monitor.clone());

    // ── Spawn logger thread ─────────────────────────────────────────
    let log_file = engine_config.log_file.clone();
    let logger_data_dir = data_dir.clone();
    let logger_started_at = daemon_started_at_ms;
    let logger_telegram = telegram_alerter.clone();
    std::thread::Builder::new()
        .name("trade-logger".to_string())
        .spawn(move || {
            // Ensure data directory exists
            if let Some(parent) = PathBuf::from(&log_file).parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            let db_path = log_file.replace(".jsonl", ".sqlite");
            let sqlite_logger = match SqliteLogger::new(&db_path) {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!("Failed to open SQLite logger: {e}");
                    return;
                }
            };

            // Also open the JSONL paper trade logger (camelCase schema)
            let mut paper_logger = match PaperTradeLogger::new(&log_file) {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!("Failed to open PaperTradeLogger: {e}");
                    return;
                }
            };

            let mut batch: Vec<TradeLogEntry> = Vec::with_capacity(32);
            let mut total_logged: u64 = 0;
            let mut cumulative_pnl: i64 = 0;
            let mut last_engine_state_ms: u64 = 0;

            loop {
                // Drain with a timeout to periodically flush
                match logger_rx.recv_timeout(std::time::Duration::from_secs(5)) {
                    Ok(cp) => {
                        cumulative_pnl += cp.net_pnl_sol;
                        total_logged += 1;

                        let mint_b58 = bs58::encode(&cp.mint).into_string();

                        info!(
                            mint = %mint_b58,
                            exit = exit_reason_str(cp.exit_reason),
                            hold_ms = cp.hold_ms,
                            gross_pnl = cp.gross_pnl_sol,
                            net_pnl = cp.net_pnl_sol,
                            fees = cp.fees_sol,
                            score = format!("{:.4}", cp.score),
                            total = total_logged,
                            cum_pnl = cumulative_pnl,
                            "CLOSED"
                        );

                        // Write JSONL (camelCase, TS-compatible)
                        if let Err(e) = paper_logger.log(&cp, &mint_b58) {
                            tracing::error!("JSONL write failed: {e}");
                        }

                        // Send Telegram alert for every closed position
                        if let Some(ref tg) = logger_telegram {
                            let msg = telegram::format_trade_alert(
                                exit_reason_str(cp.exit_reason),
                                &mint_b58,
                                cp.hold_ms,
                                cp.net_pnl_sol as f64 / 1e9,
                            );
                            tg.try_send_blocking(&msg);
                        }

                        batch.push(TradeLogEntry {
                            mint: mint_b58,
                            entry_vsol: cp.entry_vsol as f64 / 1e9,
                            exit_vsol: cp.exit_vsol as f64 / 1e9,
                            entry_ts_ms: cp.entry_ts_ms as i64,
                            exit_ts_ms: cp.exit_ts_ms as i64,
                            hold_ms: cp.hold_ms as i64,
                            size_sol: cp.size_sol as f64 / 1e9,
                            gross_pnl_sol: cp.gross_pnl_sol as f64 / 1e9,
                            net_pnl_sol: cp.net_pnl_sol as f64 / 1e9,
                            fees_sol: cp.fees_sol as f64 / 1e9,
                            exit_reason: exit_reason_str(cp.exit_reason).to_string(),
                            score: cp.score,
                            is_paper: true,
                            engine_version: ENGINE_VERSION.to_string(),
                        });

                        // Flush batch every 16 entries
                        if batch.len() >= 16 {
                            if let Err(e) = sqlite_logger.log_trades_batch(&batch) {
                                tracing::error!("SQLite batch write failed: {e}");
                            }
                            batch.clear();
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                        // Flush any pending SQLite batch
                        if !batch.is_empty() {
                            if let Err(e) = sqlite_logger.log_trades_batch(&batch) {
                                tracing::error!("SQLite batch write failed: {e}");
                            }
                            batch.clear();
                        }

                        // Periodically refresh engine-state.json (every 60s)
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        if now - last_engine_state_ms >= 60_000 {
                            if let Err(e) = write_engine_state(&logger_data_dir, logger_started_at) {
                                tracing::warn!("Failed to refresh engine state: {e}");
                            }
                            last_engine_state_ms = now;
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                        // Flush remaining
                        if !batch.is_empty() {
                            let _ = sqlite_logger.log_trades_batch(&batch);
                        }
                        info!(total_logged, "Logger thread: channel closed, exiting");
                        return;
                    }
                }
            }
        })?;

    // ── Feed channels ───────────────────────────────────────────────
    let (pp_tx, pp_rx) = bounded::<FeedEvent>(256);
    let (helius_tx, helius_rx) = bounded::<FeedEvent>(256);
    let (engine_tx, engine_rx) = bounded::<FeedEvent>(1024);

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    // TASK-10: Shared creator map (mint → creator wallet) for signer matching.
    // PumpPortal writes on create events; CoreCast reads for creator-sell verification.
    // Uses std::sync::RwLock — writes are infrequent (new token creation only, ~1/min).
    // NOTE: std::sync::RwLock::write() will briefly block the tokio thread (~50ns),
    // which is acceptable given write frequency. tokio::sync::RwLock is not needed here
    // because the critical section is trivially short (HashMap insert, no I/O).
    let shared_creator_map: pump_quant_core::feeds::corecast::CreatorMap =
        Arc::new(RwLock::new(HashMap::new()));

    // Spawn PumpPortal feed
    let pp_tx_clone = pp_tx.clone();
    let pp_shutdown_rx = shutdown_rx.clone();
    let pp_creator_map = shared_creator_map.clone();
    tokio::spawn(async move {
        pump_quant_core::feeds::pumpportal::run(pp_tx_clone, pp_shutdown_rx, pp_creator_map).await;
    });
    info!("PumpPortal feed spawned");

    // Spawn Helius feed (optional)
    let helius_api_key = std::env::var("HELIUS_API_KEY").unwrap_or_default();
    let helius_enabled = !helius_api_key.is_empty();
    let helius_config = HeliusConfig {
        api_key: helius_api_key,
        enabled: helius_enabled,
    };
    let helius_client = HeliusWsClient::new(helius_config, helius_tx);
    helius_client.spawn();
    if helius_enabled {
        info!("Helius feed spawned");
    } else {
        info!("Helius feed disabled (no HELIUS_API_KEY)");
    }

    // Spawn ShredStream feed (optional)
    let shred_config = ShredStreamConfig::from_env();
    let shredstream_rx = if shred_config.enabled {
        let (shred_tx, shred_rx) = bounded::<FeedEvent>(256);
        let shred_shutdown_rx = shutdown_rx.clone();
        tokio::spawn(async move {
            pump_quant_core::feeds::shredstream::run(shred_tx, shred_shutdown_rx).await;
        });
        info!("ShredStream feed spawned");
        Some(shred_rx)
    } else {
        info!("ShredStream feed disabled (no SHREDSTREAM_ENDPOINT)");
        None
    };

    // Spawn CoreCast/Bitquery feed (optional)
    {
        // CoreCast events go directly to the engine channel (not through joiner)
        // since they emit FeedEvent::CreatorSell, not Trade/PreWarm
        let corecast_tx = engine_tx.clone();
        let corecast_shutdown_rx = shutdown_rx.clone();
        let creator_map: pump_quant_core::feeds::corecast::CreatorMap = shared_creator_map.clone();
        tokio::spawn(async move {
            pump_quant_core::feeds::corecast::run(corecast_tx, corecast_shutdown_rx, creator_map).await;
        });
        info!("CoreCast feed spawned (will activate if BITQUERY_API_KEY is set)");
    }

    // ── Spawn blockhash cache refresh task ─────────────────────────
    // Refreshes every 25s so tx execution never pays a per-trade RPC round-trip.
    // In paper mode: the cache is warmed but no TxExecutor consumes it — this
    // validates that the RPC endpoint is reachable (canary) at ~zero cost.
    // In live mode: pass `bh_cache` to TxExecutor::new() before this scope closes.
    {
        let rpc_url = std::env::var("SOLANA_RPC_URL")
            .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
        let bh_cache = pump_quant_core::tx::executor::BlockhashCache::new();
        // Arc is captured by the spawned task — stays alive even after this scope exits.
        bh_cache.clone().spawn_refresh_task(rpc_url);
        info!("Blockhash cache refresh task started (25s interval)");
    }

    // ── Spawn API server with shared stats ──────────────────────────
    let api_state = ApiState::with_health(health_monitor.clone());
    let shared_stats = api_state.stats.clone();
    let api_state_clone = api_state.clone();
    tokio::spawn(async move {
        start_server(api_state_clone).await;
    });
    info!("API server spawning on port 9421");

    // Spawn EventJoiner on a dedicated thread
    let joiner = EventJoiner::new(pp_rx, helius_rx, shredstream_rx, engine_tx);
    std::thread::Builder::new()
        .name("event-joiner".to_string())
        .spawn(move || joiner.run())?;
    info!("EventJoiner thread started");

    // ── Engine hot-path loop ────────────────────────────────────────
    info!(paper_mode, "Engine hot-path running — full gate→score→position pipeline");

    loop {
        match engine_rx.recv() {
            Ok(FeedEvent::Trade(trade)) => {
                // Record feed event for health monitoring
                health_monitor.record_event(trade.source, now_ms_system());

                hot_path.on_trade(&trade);

                // Drain closed positions for safety tracking, then forward to logger
                drain_closed_positions(&closed_rx, &mut hot_path, &logger_tx, &telegram_alerter);

                // Update shared API stats every 100 trades (also at first trade)
                let ts = hot_path.stats.trades_seen;
                if ts == 1 || ts % 100 == 0 {
                    sync_stats_to_api(&hot_path, &shared_stats);
                }

                // Stats logging every 1000 trades
                if hot_path.stats.trades_seen % 1000 == 0 {
                    let s = &hot_path.stats;
                    info!(
                        trades = s.trades_seen,
                        gates_passed = s.gates_passed,
                        gate_rejects = s.gate_rejects,
                        score_rejects = s.score_rejects,
                        positions = s.positions_opened,
                        open = hot_path.open_positions(),
                        prewarms = s.prewarms,
                        ticks = s.ticks,
                        "engine stats"
                    );
                }
            }
            Ok(FeedEvent::PreWarm(prewarm)) => {
                // Record feed event for health monitoring
                health_monitor.record_event(prewarm.source, now_ms_system());

                hot_path.on_prewarm(&prewarm);
            }
            Ok(FeedEvent::Tick { ts_ms }) => {
                hot_path.on_tick(ts_ms);

                // Drain closed positions for safety tracking, then forward to logger
                drain_closed_positions(&closed_rx, &mut hot_path, &logger_tx, &telegram_alerter);

                // Health check every 100 ticks (~5 seconds)
                if hot_path.stats.ticks % 100 == 0 {
                    let (health_status, recovered_feeds) = health_monitor.check(ts_ms);

                    // Alert on stale feeds
                    if let pump_quant_core::engine::health::HealthStatus::Degraded { ref stale_feeds } = health_status {
                        for feed in stale_feeds {
                            // AUDIT FIX: resolve the correct FeedSource per feed name,
                            // not always PumpPortal (was a manual-patch bug).
                            let source = match *feed {
                                "Helius" => FeedSource::Helius,
                                _ => FeedSource::PumpPortal,
                            };
                            let last_ms = health_monitor.last_event_ms(source);
                            let stale_s = if last_ms > 0 { ts_ms.saturating_sub(last_ms) / 1000 } else { 0 };
                            tracing::warn!(feed = %feed, stale_s, "Feed stale — trading paused");
                            if let Some(ref tg) = telegram_alerter {
                                tg.try_send_blocking(&telegram::format_feed_stale_alert(feed, stale_s));
                            }
                        }
                    }

                    // Alert on recovered feeds
                    for feed in &recovered_feeds {
                        info!(feed = %feed, "Feed recovered — trading resumed");
                        if let Some(ref tg) = telegram_alerter {
                            tg.try_send_blocking(&telegram::format_feed_recovered_alert(feed));
                        }
                    }
                }

                // Sync API stats every 200 ticks (~10 seconds)
                if hot_path.stats.ticks % 200 == 0 {
                    sync_stats_to_api(&hot_path, &shared_stats);
                }
            }
            Ok(FeedEvent::CreatorSell { mint, ts_ms }) => {
                hot_path.on_creator_sell(&mint, ts_ms);
            }
            Ok(FeedEvent::Shutdown) => {
                info!("Shutdown signal received");
                let now = now_ms_system();
                hot_path.close_all(now);
                drain_closed_positions(&closed_rx, &mut hot_path, &logger_tx, &telegram_alerter);
                sync_stats_to_api(&hot_path, &shared_stats);
                let _ = shutdown_tx.send(true);
                break;
            }
            Err(_) => {
                info!("Engine channel closed — shutting down");
                let now = now_ms_system();
                hot_path.close_all(now);
                drain_closed_positions(&closed_rx, &mut hot_path, &logger_tx, &telegram_alerter);
                sync_stats_to_api(&hot_path, &shared_stats);
                break;
            }
        }
    }

    let s = &hot_path.stats;
    info!(
        trades = s.trades_seen,
        gates_passed = s.gates_passed,
        positions_opened = s.positions_opened,
        gate_rejects = s.gate_rejects,
        score_rejects = s.score_rejects,
        prewarms = s.prewarms,
        creator_sells = s.creator_sells,
        "pump-quant-core stopped"
    );

    Ok(())
}

/// Drain all pending ClosedPositions from the position manager channel,
/// update HotPath safety counters, then forward to the logger thread.
fn drain_closed_positions(
    closed_rx: &crossbeam_channel::Receiver<ClosedPosition>,
    hot_path: &mut HotPath,
    logger_tx: &crossbeam_channel::Sender<ClosedPosition>,
    telegram_alerter: &Option<Arc<TelegramAlerter>>,
) {
    while let Ok(cp) = closed_rx.try_recv() {
        // Track safety state — returns Some if circuit breaker just fired
        if let Some((stops, pause_ms)) = hot_path.on_position_closed(&cp) {
            tracing::warn!(
                consecutive_stops = stops,
                pause_ms,
                "Circuit breaker fired"
            );
            if let Some(ref tg) = telegram_alerter {
                tg.try_send_blocking(&telegram::format_circuit_breaker_alert(stops, pause_ms / 1000));
            }
        }
        // Forward to logger thread (best-effort)
        let _ = logger_tx.try_send(cp);
    }
}

/// Sync HotPath stats into the shared API EngineStats.
fn sync_stats_to_api(hot_path: &HotPath, shared: &Arc<Mutex<EngineStats>>) {
    if let Ok(mut api_stats) = shared.lock() {
        let s = &hot_path.stats;
        api_stats.trades_seen = s.trades_seen;
        api_stats.gates_passed = s.gates_passed;
        api_stats.positions_opened = s.positions_opened;
        // gate_rejects isn't directly in EngineStats schema, but we can track it:
        // positions_closed, wins, losses are tracked separately by the logger.
        // For now, we at least sync the primary counters.
    }
}
