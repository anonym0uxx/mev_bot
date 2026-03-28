//! pump-quant-core — Rust MEV engine entry point.
//!
//! Reads config from canary.json, wires feeds → joiner → engine hot-path.
//! Paper mode: logs all closed positions to SQLite.

use std::path::PathBuf;

use crossbeam_channel::bounded;
use tracing::info;
use tracing_subscriber::{fmt, EnvFilter};

use pump_quant_core::engine::config::load_config;
use pump_quant_core::engine::gates::GateStack;
use pump_quant_core::engine::hot_path::HotPath;
use pump_quant_core::engine::positions::{ClosedPosition, ExitReason, PositionManager};
use pump_quant_core::engine::scorer::Scorer;
use pump_quant_core::api::{ApiState, start_server};
use pump_quant_core::feeds::{
    event_joiner::EventJoiner,
    helius::{HeliusConfig, HeliusWsClient},
    shredstream::ShredStreamConfig,
    FeedEvent,
};
use pump_quant_core::persistence::sqlite::{SqliteLogger, TradeLogEntry};

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
        "pump-quant-core starting"
    );

    // ── Build engine components ─────────────────────────────────────
    let min_score = engine_config.gate.trigger_min_score;
    let gate_stack = GateStack::new(engine_config.gate);
    let scorer = Scorer::new(
        engine_config.score,
        engine_config.position.tp_tiers.first().map(|_| 0).unwrap_or(0) // placeholder
            .max(33_000_000_000), // use config min_vsol for scorer range
        engine_config.position.tp_tiers.first().map(|_| 0).unwrap_or(0)
            .max(43_000_000_000), // use config max_vsol for scorer range
    );

    // Channel for closed positions: position_manager → logger thread
    let (closed_tx, closed_rx) = bounded::<ClosedPosition>(256);

    let position_manager = PositionManager::new(engine_config.position, closed_tx);

    let mut hot_path = HotPath::new(
        gate_stack,
        scorer,
        position_manager,
        now_ms_system,
        paper_mode,
        min_score,
    );

    // ── Spawn logger thread ─────────────────────────────────────────
    let log_file = engine_config.log_file.clone();
    std::thread::Builder::new()
        .name("trade-logger".to_string())
        .spawn(move || {
            // Ensure data directory exists
            if let Some(parent) = PathBuf::from(&log_file).parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            let db_path = log_file.replace(".jsonl", ".sqlite");
            let logger = match SqliteLogger::new(&db_path) {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!("Failed to open SQLite logger: {e}");
                    return;
                }
            };

            let mut batch: Vec<TradeLogEntry> = Vec::with_capacity(32);
            let mut total_logged: u64 = 0;
            let mut cumulative_pnl: i64 = 0;

            loop {
                // Drain with a timeout to periodically flush
                match closed_rx.recv_timeout(std::time::Duration::from_secs(5)) {
                    Ok(cp) => {
                        cumulative_pnl += cp.net_pnl_sol;
                        total_logged += 1;

                        info!(
                            mint = %bs58::encode(&cp.mint).into_string(),
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

                        batch.push(TradeLogEntry {
                            mint: bs58::encode(&cp.mint).into_string(),
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
                            if let Err(e) = logger.log_trades_batch(&batch) {
                                tracing::error!("SQLite batch write failed: {e}");
                            }
                            batch.clear();
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                        // Flush any pending
                        if !batch.is_empty() {
                            if let Err(e) = logger.log_trades_batch(&batch) {
                                tracing::error!("SQLite batch write failed: {e}");
                            }
                            batch.clear();
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                        // Flush remaining
                        if !batch.is_empty() {
                            let _ = logger.log_trades_batch(&batch);
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

    // Spawn PumpPortal feed
    let pp_tx_clone = pp_tx.clone();
    let pp_shutdown_rx = shutdown_rx.clone();
    tokio::spawn(async move {
        pump_quant_core::feeds::pumpportal::run(pp_tx_clone, pp_shutdown_rx).await;
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

    // Spawn API server
    let api_state = ApiState::new();
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
                hot_path.on_trade(&trade);

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
                hot_path.on_prewarm(&prewarm);
            }
            Ok(FeedEvent::Tick { ts_ms }) => {
                hot_path.on_tick(ts_ms);
            }
            Ok(FeedEvent::CreatorSell { mint, ts_ms }) => {
                hot_path.on_creator_sell(&mint, ts_ms);
            }
            Ok(FeedEvent::Shutdown) => {
                info!("Shutdown signal received");
                let now = now_ms_system();
                hot_path.close_all(now);
                let _ = shutdown_tx.send(true);
                break;
            }
            Err(_) => {
                info!("Engine channel closed — shutting down");
                let now = now_ms_system();
                hot_path.close_all(now);
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
