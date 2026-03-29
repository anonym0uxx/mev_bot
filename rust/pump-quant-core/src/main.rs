//! pump-quant-core — Rust MEV engine entry point.
//!
//! Reads config from canary.json, wires feeds → joiner → engine hot-path.
//! Paper mode: logs all closed positions to SQLite.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

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
use pump_quant_core::arb::graduation::{
    GradArbConfig, GradArbClosedPosition, GradArbStats, GraduationArbEngine,
};
use pump_quant_core::feeds::{
    event_joiner::EventJoiner,
    helius::{HeliusConfig, HeliusWsClient},
    shredstream::ShredStreamConfig,
    FeedEvent, FeedSource,
};
use pump_quant_core::persistence::sqlite::{SqliteLogger, TradeLogEntry};
use pump_quant_core::persistence::paper_logger::PaperTradeLogger;
use pump_quant_core::persistence::grad_arb_logger::GradArbPaperLogger;
use pump_quant_core::persistence::engine_state::write_engine_state;

const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

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
    // Load .env file (relative to CWD, i.e. project root)
    // Silently ignore if .env doesn't exist — env vars may be set externally.
    let _ = dotenvy::dotenv();

    // Install rustls crypto provider (required before any TLS connections)
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Init tracing
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    tracing_subscriber::registry()
        .with(fmt::layer().with_target(true).compact())
        .with(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    // ── LATENCY: Anchor a monotonic clock to epoch time once at startup ──
    // All subsequent now_ms calls use Instant::elapsed() (~3ns) instead of
    // SystemTime::now() (~20ns syscall). One syscall at startup, zero in the loop.
    let epoch_offset_ms: u64 = {
        let sys = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        sys
    };
    let mono_start = Instant::now();

    // Inline helper: epoch ms from monotonic clock (no syscall).
    // Defined as a fn-like macro so it can be used anywhere without closure capture issues.
    macro_rules! now_ms_mono {
        () => {
            epoch_offset_ms + mono_start.elapsed().as_millis() as u64
        };
    }

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

    // ── Compute config version string for trade attribution ─────────
    let config_version = format!(
        "v{:.2}sol_{}ms_{}vsol",
        engine_config.gate.trigger_min_buy_lamports as f64 / 1_000_000_000.0,
        engine_config.position.max_hold_ms,
        engine_config.gate.min_vsol_lamports / 1_000_000_000
    );

    // ── Active config dump (single source of truth verification) ────
    info!(
        trigger_min_buy_sol = engine_config.gate.trigger_min_buy_lamports as f64 / 1e9,
        trigger_max_buy_sol = engine_config.gate.trigger_max_buy_lamports as f64 / 1e9,
        min_vsol = engine_config.gate.min_vsol_lamports as f64 / 1e9,
        max_vsol = engine_config.gate.max_vsol_lamports as f64 / 1e9,
        max_hold_ms = engine_config.position.max_hold_ms,
        min_hold_ms = engine_config.position.min_hold_before_exit_ms,
        trigger_min_score = engine_config.gate.trigger_min_score,
        pre_trigger_min_buys_1s = engine_config.gate.pre_trigger_min_buys_1s,
        pre_trigger_min_buys_2s = engine_config.gate.pre_trigger_min_buys_2s,
        pre_trigger_min_buys_5s = engine_config.gate.pre_trigger_min_buys_5s,
        pre_trigger_min_vsol_accel = engine_config.gate.pre_trigger_min_vsol_accel as f64 / 1e9,
        pre_trigger_min_volume_5s = engine_config.gate.pre_trigger_min_volume_5s_lamports as f64 / 1e9,
        max_trigger_isolation = engine_config.gate.max_trigger_isolation,
        max_token_age_ms = engine_config.gate.max_token_age_ms,
        max_concurrent_positions = engine_config.position.max_concurrent_positions,
        tp_tiers = engine_config.position.tp_tiers.len(),
        size_tiers = engine_config.position.size_tiers.len(),
        daily_loss_cap_sol = engine_config.daily_loss_cap_lamports as f64 / 1e9,
        consecutive_stop_pause_count = engine_config.consecutive_stop_pause_count,
        tod_gate_enabled = engine_config.gate.tod_gate_enabled,
        paper_mode = paper_mode,
        config_version = %config_version,
        "active config"
    );

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
        tod_gate_enabled = engine_config.gate.tod_gate_enabled,
        "pump-quant-core starting"
    );

    // ── Write engine state on startup ─────────────────────────────────
    let daemon_started_at_ms = now_ms_mono!();
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
        _now_ms_dummy, // LATENCY: HotPath uses quanta internally, not this fn
        paper_mode,
        min_score,
        engine_config.daily_loss_cap_lamports,
        engine_config.consecutive_stop_pause_count,
        engine_config.consecutive_stop_pause_ms,
        engine_config.boosted_hours_utc.clone(),
        engine_config.tod_boost_multiplier,
        max_entry_size_lamports,
        engine_config.randomizer.clone(),
    );

    // Attach health monitor to hot path for entry gating
    hot_path.set_health_monitor(health_monitor.clone());

    // ── Spawn logger thread ─────────────────────────────────────────
    let log_file = engine_config.log_file.clone();
    let logger_data_dir = data_dir.clone();
    let logger_started_at = daemon_started_at_ms;
    let logger_telegram = telegram_alerter.clone();
    let logger_config_version = config_version.clone();

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
            let mut paper_logger = match PaperTradeLogger::new(
                &log_file,
                paper_mode,
                logger_config_version,
            ) {
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

                        // Send Telegram alert for closed positions — LIVE MODE ONLY
                        // In paper mode, trade alerts are suppressed (too noisy at data collection volume)
                        if !paper_mode {
                            if let Some(ref tg) = logger_telegram {
                                let msg = telegram::format_trade_alert(
                                    exit_reason_str(cp.exit_reason),
                                    &mint_b58,
                                    cp.hold_ms,
                                    cp.net_pnl_sol as f64 / 1e9,
                                );
                                tg.try_send_blocking(&msg);
                            }
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
    // Set graduation arb flag in API stats at startup
    {
        let mut stats = api_state.stats.lock().unwrap();
        stats.graduation_arb_enabled = engine_config.graduation_arb_enabled;
    }
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

    // ── Graduation Arb Engine ───────────────────────────────────────
    let grad_arb_stats = Arc::new(GradArbStats::default());
    let (grad_closed_tx, grad_closed_rx) =
        crossbeam_channel::unbounded::<GradArbClosedPosition>();

    let helius_rpc_url = {
        let key = std::env::var("HELIUS_API_KEY").unwrap_or_default();
        if key.is_empty() {
            String::new()
        } else {
            format!("https://mainnet.helius-rpc.com/?api-key={}", key)
        }
    };

    let grad_arb_config = GradArbConfig {
        enabled: engine_config.graduation_arb_enabled,
        paper_mode: true, // always paper for now
        max_sol: engine_config.graduation_arb_max_sol,
        min_spread_pct: engine_config.graduation_arb_min_spread_pct,
        tp_pct: engine_config.graduation_arb_tp_pct,
        sl_pct: engine_config.graduation_arb_sl_pct,
        max_hold_ms: engine_config.graduation_arb_max_hold_ms,
        jito_tip_sol: engine_config.graduation_arb_jito_tip_sol,
        dedup_ttl_ms: 10_000,
        rpc_timeout_ms: 200,
    };

    let grad_arb_engine = Arc::new(GraduationArbEngine::new(
        grad_arb_config,
        grad_arb_stats.clone(),
        grad_closed_tx,
        helius_rpc_url,
    ));

    info!(
        enabled = engine_config.graduation_arb_enabled,
        paper_mode = true,
        max_sol = engine_config.graduation_arb_max_sol,
        min_spread_pct = engine_config.graduation_arb_min_spread_pct,
        max_hold_ms = engine_config.graduation_arb_max_hold_ms,
        "[grad_arb] engine initialized"
    );

    // ── Spawn graduation arb logger thread ──────────────────────────
    {
        let path = format!("{}/graduation_paper_trades.jsonl", data_dir);
        let config_version = format!(
            "grad-v{:.2}sol_{}ms",
            engine_config.graduation_arb_max_sol, engine_config.graduation_arb_max_hold_ms
        );
        std::thread::Builder::new()
            .name("grad-arb-logger".to_string())
            .spawn(move || {
                // Ensure data directory exists
                if let Some(parent) = std::path::Path::new(&path).parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let mut logger = match GradArbPaperLogger::new(&path, config_version) {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::error!("Failed to open graduation JSONL: {e}");
                        return;
                    }
                };
                for cp in grad_closed_rx {
                    let mint_b58 = bs58::encode(&cp.mint).into_string();
                    if let Err(e) = logger.log(&cp, &mint_b58) {
                        tracing::error!("Grad arb JSONL write failed: {e}");
                    }
                }
                info!("Grad arb logger thread exiting");
            })?;
    }

    // ── Engine hot-path loop ────────────────────────────────────────
    info!(paper_mode, "Engine hot-path running — full gate→score→position pipeline");

    loop {
        match engine_rx.recv() {
            Ok(FeedEvent::Trade(trade)) => {
                // LATENCY: use trade's own timestamp for health monitoring instead
                // of a redundant SystemTime::now() syscall. The trade timestamp comes
                // from PumpPortal's "timestamp" field (epoch ms), or fallback to
                // system time inside the feed parser. This eliminates one ~20ns
                // clock_gettime syscall per trade on the hot path.
                health_monitor.record_event(trade.source, trade.timestamp_ms);

                hot_path.on_trade(&trade);

                // Drain closed positions for safety tracking, then forward to logger
                drain_closed_positions(&closed_rx, &mut hot_path, &logger_tx, &telegram_alerter);

                // Update shared API stats every 100 trades (also at first trade)
                let ts = hot_path.stats.trades_seen;
                if ts == 1 || ts % 100 == 0 {
                    sync_stats_to_api(&hot_path, &shared_stats, &grad_arb_stats);
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
                        migrations = s.migrations,
                        lp_removals = s.lp_removals,
                        creator_sells = s.creator_sells,
                        helius_correlated = hot_path.helius_lead_count,
                        helius_avg_lead_ms = if hot_path.helius_lead_count > 0 {
                            hot_path.helius_lead_sum_ms / hot_path.helius_lead_count
                        } else { 0 },
                        "engine stats"
                    );
                }
            }
            Ok(FeedEvent::PreWarm(prewarm)) => {
                // LATENCY: use prewarm's own timestamp (same rationale as Trade)
                health_monitor.record_event(prewarm.source, prewarm.timestamp_ms);

                hot_path.on_prewarm(&prewarm);
            }
            Ok(FeedEvent::Tick { ts_ms }) => {
                hot_path.on_tick(ts_ms);

                // Graduation arb: check position exits (MaxHold)
                grad_arb_engine.on_tick(ts_ms);

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
                    sync_stats_to_api(&hot_path, &shared_stats, &grad_arb_stats);
                }
            }
            Ok(FeedEvent::TokenCreated(created)) => {
                hot_path.on_token_created(&created);
            }
            Ok(FeedEvent::CreatorSell { mint, ts_ms }) => {
                hot_path.on_creator_sell(&mint, ts_ms);
            }
            Ok(FeedEvent::Migration { mint, ts_ms, source, sig }) => {
                let mint_b58 = bs58::encode(&mint).into_string();
                let open_before = hot_path.open_positions();
                hot_path.on_migration(&mint, ts_ms);
                let open_after = hot_path.open_positions();
                let had_open_position = open_after < open_before;
                drain_closed_positions(&closed_rx, &mut hot_path, &logger_tx, &telegram_alerter);

                // Enhanced migration logging for graduation arb analysis
                info!(
                    mint = %mint_b58,
                    ts_ms = ts_ms,
                    source = source.as_str(),
                    open_position_closed = had_open_position,
                    "[grad_arb] graduation migration detected"
                );

                // Graduation arb: evaluate entry opportunity (async, non-blocking)
                if engine_config.graduation_arb_enabled {
                    let engine = Arc::clone(&grad_arb_engine);
                    tokio::spawn(async move {
                        engine.on_migration(mint, ts_ms, source, sig).await;
                    });
                }
            }
            Ok(FeedEvent::LpRemoval { mint, ts_ms }) => {
                hot_path.on_lp_removal(&mint, ts_ms);
                drain_closed_positions(&closed_rx, &mut hot_path, &logger_tx, &telegram_alerter);
            }

            Ok(FeedEvent::Shutdown) => {
                info!("Shutdown signal received");
                let now = now_ms_mono!();
                hot_path.close_all(now);
                drain_closed_positions(&closed_rx, &mut hot_path, &logger_tx, &telegram_alerter);
                sync_stats_to_api(&hot_path, &shared_stats, &grad_arb_stats);
                let _ = shutdown_tx.send(true);
                break;
            }
            Err(_) => {
                info!("Engine channel closed — shutting down");
                let now = now_ms_mono!();
                hot_path.close_all(now);
                drain_closed_positions(&closed_rx, &mut hot_path, &logger_tx, &telegram_alerter);
                sync_stats_to_api(&hot_path, &shared_stats, &grad_arb_stats);
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
        migrations = s.migrations,
        lp_removals = s.lp_removals,
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

/// Dummy clock fn for HotPath API compat — HotPath uses quanta internally.
fn _now_ms_dummy() -> u64 { 0 }

/// Sync HotPath stats + GradArb stats into the shared API EngineStats.
fn sync_stats_to_api(
    hot_path: &HotPath,
    shared: &Arc<Mutex<EngineStats>>,
    grad_arb_stats: &Arc<GradArbStats>,
) {
    if let Ok(mut api_stats) = shared.lock() {
        let s = &hot_path.stats;
        api_stats.trades_seen = s.trades_seen;
        api_stats.gates_passed = s.gates_passed;
        api_stats.positions_opened = s.positions_opened;
        // Stream event counters
        api_stats.migrations_seen = s.migrations;
        api_stats.lp_removals_seen = s.lp_removals;
        api_stats.creator_sells_seen = s.creator_sells;

        // Graduation arb stats — read from atomic counters
        api_stats.grad_arb_migrations =
            grad_arb_stats.migrations_detected.load(std::sync::atomic::Ordering::Relaxed);
        api_stats.grad_arb_entries =
            grad_arb_stats.arb_entries.load(std::sync::atomic::Ordering::Relaxed);
        api_stats.grad_arb_timeouts =
            grad_arb_stats.arb_timeouts.load(std::sync::atomic::Ordering::Relaxed);
        api_stats.grad_arb_pool_not_found =
            grad_arb_stats.pool_not_found.load(std::sync::atomic::Ordering::Relaxed);
        api_stats.grad_arb_no_spread =
            grad_arb_stats.no_arb_spread.load(std::sync::atomic::Ordering::Relaxed);
        api_stats.grad_arb_exits_tp =
            grad_arb_stats.exits_tp.load(std::sync::atomic::Ordering::Relaxed);
        api_stats.grad_arb_exits_sl =
            grad_arb_stats.exits_sl.load(std::sync::atomic::Ordering::Relaxed);
        api_stats.grad_arb_exits_max_hold =
            grad_arb_stats.exits_max_hold.load(std::sync::atomic::Ordering::Relaxed);
        api_stats.grad_arb_net_sol =
            grad_arb_stats.net_pnl_lamports.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e9;
        // Keep backward-compat fields updated too
        api_stats.graduation_arb_trades = api_stats.grad_arb_entries;
        api_stats.graduation_arb_net_sol = api_stats.grad_arb_net_sol;
    }
}
