//! pump-quant-core — Momentum graduation engine.
//!
//! Reads config from canary.json, wires feeds → event loop → momentum engine.
//! The sole trading path: post-graduation momentum on PumpSwap/Raydium.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use crossbeam_channel::bounded;
use tracing::info;
use tracing_subscriber::{fmt, EnvFilter};

use pump_quant_core::alerts::telegram::{self, TelegramAlerter};
use pump_quant_core::engine::config::load_config;
use pump_quant_core::engine::health::HealthMonitor;
use pump_quant_core::api::{ApiState, EngineStats, start_server};
use pump_quant_core::momentum::MomentumEngine;
use pump_quant_core::momentum::types::GradEnrichment;
use pump_quant_core::feeds::{
    event_joiner::EventJoiner,
    helius::{HeliusConfig, HeliusPumpSwapClient, HeliusWsClient},
    shredstream::ShredStreamConfig,
    FeedEvent, FeedSource,
};
use pump_quant_core::persistence::engine_state::write_engine_state;

const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env file (relative to CWD, i.e. project root)
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

    // ── LATENCY: Anchor monotonic clock to epoch time once at startup ──
    let epoch_offset_ms: u64 = {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    };
    let mono_start = Instant::now();

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

    let paper_mode = engine_config.momentum.paper_mode;

    info!(
        paper_mode,
        momentum_enabled = engine_config.momentum.enabled,
        min_grad_score = engine_config.momentum.min_grad_score,
        position_size_sol = engine_config.momentum.position_size_sol,
        "pump-quant-core starting (momentum-only)"
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

    // ── Feed channels ───────────────────────────────────────────────
    let (pp_tx, pp_rx) = bounded::<FeedEvent>(256);
    let (helius_tx, helius_rx) = bounded::<FeedEvent>(256);
    let (engine_tx, engine_rx) = bounded::<FeedEvent>(1024);

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    // Shared creator map (mint → creator wallet) for signer matching.
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
        api_key: helius_api_key.clone(),
        enabled: helius_enabled,
    };
    let helius_client = HeliusWsClient::new(helius_config, helius_tx);
    helius_client.spawn();
    if helius_enabled {
        info!("Helius feed spawned");
    } else {
        info!("Helius feed disabled (no HELIUS_API_KEY)");
    }

    // Spawn Helius PumpSwap graduation detector
    let helius_pumpswap_config = HeliusConfig {
        api_key: helius_api_key.clone(),
        enabled: helius_enabled,
    };
    let helius_pumpswap_tx = engine_tx.clone();
    let helius_pumpswap_client =
        HeliusPumpSwapClient::new(helius_pumpswap_config, helius_pumpswap_tx);
    helius_pumpswap_client.spawn();
    if helius_enabled {
        info!("Helius PumpSwap transactionSubscribe feed spawned");
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
        let corecast_tx = engine_tx.clone();
        let corecast_shutdown_rx = shutdown_rx.clone();
        let creator_map: pump_quant_core::feeds::corecast::CreatorMap = shared_creator_map.clone();
        tokio::spawn(async move {
            pump_quant_core::feeds::corecast::run(corecast_tx, corecast_shutdown_rx, creator_map).await;
        });
        info!("CoreCast feed spawned (will activate if BITQUERY_API_KEY is set)");
    }

    let public_rpc_url = std::env::var("PUBLIC_RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());

    // ── Spawn API server ────────────────────────────────────────────
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

    // ── Momentum Engine ──────────────────────────────────────────────
    let momentum_config = Arc::new(engine_config.momentum.clone());
    let momentum_rpc_url = Arc::new(
        std::env::var("SOLANA_RPC_URL")
            .ok()
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| {
                std::env::var("HELIUS_API_KEY")
                    .ok()
                    .filter(|k| !k.is_empty())
                    .map(|k| format!("https://mainnet.helius-rpc.com/?api-key={}", k))
                    .unwrap_or_default()
            }),
    );
    let momentum_wss_url = std::env::var("SOLANA_WS_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| {
            std::env::var("HELIUS_API_KEY")
                .ok()
                .filter(|k| !k.is_empty())
                .map(|k| format!("wss://mainnet.helius-rpc.com/?api-key={}", k))
                .unwrap_or_else(|| "wss://invalid.example.com".to_string())
        });
    let momentum_log_path = format!("{}/momentum_paper_trades.jsonl", data_dir);

    // ── Build ExecutionContext (shared infra: Jito, Nozomi, blockhash, wallet, RPC, tips) ──
    let exec_ctx = {
        // Blockhash cache
        let bh_cache = pump_quant_core::tx::executor::BlockhashCache::new();
        {
            let rpc_for_bh = public_rpc_url.clone();
            bh_cache.clone().spawn_refresh_task(rpc_for_bh);
        }

        // Jito gRPC client (live mode only)
        let jito_grpc_client: Option<std::sync::Arc<pump_quant_core::tx::jito_grpc::JitoGrpcClient>> =
            if !paper_mode {
                let cfg = pump_quant_core::tx::jito_grpc::JitoGrpcConfig::default();
                match pump_quant_core::tx::jito_grpc::JitoGrpcClient::new(cfg).await {
                    Ok(client) => {
                        let _ = client.warmup().await;
                        Some(std::sync::Arc::new(client))
                    }
                    Err(e) => {
                        tracing::warn!(err = ?e, "JitoGrpcClient init failed — live sells disabled");
                        None
                    }
                }
            } else {
                None
            };

        // Nozomi client (live mode only)
        let nozomi_client: Option<std::sync::Arc<pump_quant_core::tx::nozomi::NozomiClient>> =
            if !paper_mode {
                std::env::var("NOZOMI_API_KEY")
                    .ok()
                    .filter(|k| !k.is_empty())
                    .map(|key| {
                        std::sync::Arc::new(pump_quant_core::tx::nozomi::NozomiClient::new(
                            std::env::var("NOZOMI_ENDPOINT").unwrap_or_else(|_| {
                                "https://ewr1.nozomi.temporal.xyz".to_string()
                            }),
                            key,
                        ))
                    })
            } else {
                None
            };

        // Wallet keypair (pre-loaded once — replaces per-trade fs::read)
        let wallet = if !paper_mode {
            let kp_path = std::env::var("WALLET_KEYPAIR_PATH").unwrap_or_default();
            pump_quant_core::tx::execution_context::WalletKeys::load_from_path(&kp_path)
        } else {
            None
        };

        // Tip engine
        let tip_engine = std::sync::Arc::new(parking_lot::Mutex::new(
            pump_quant_core::tx::tip_engine::TipEngine::new(
                pump_quant_core::tx::tip_engine::TipConfig::default(),
            ),
        ));

        // Helius HTTPS RPC URL for getProgramAccounts
        let helius_rpc_url = std::sync::Arc::new(
            std::env::var("HELIUS_API_KEY")
                .ok()
                .filter(|k| !k.is_empty())
                .map(|k| format!("https://mainnet.helius-rpc.com/?api-key={}", k))
                .unwrap_or_else(|| momentum_rpc_url.to_string()),
        );

        // RPC fallback client + URL
        let rpc_fallback_url = std::sync::Arc::new(
            std::env::var("SOLANA_RPC_URL")
                .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string()),
        );
        let rpc_fallback_client = reqwest::Client::new();

        // RPC primary sender with circuit breaker
        let rpc_sender_config = pump_quant_core::momentum::rpc_sender::RpcSenderConfig::from_momentum_config(
            &momentum_config.rpc_sender,
        );
        let rpc_sender = std::sync::Arc::new(pump_quant_core::momentum::rpc_sender::RpcSender::new(
            helius_rpc_url.to_string(),
            rpc_sender_config,
        ));

        let public_rpc = std::sync::Arc::new(public_rpc_url.clone());

        std::sync::Arc::new(pump_quant_core::tx::ExecutionContext {
            jito_grpc: jito_grpc_client,
            nozomi_client,
            blockhash_cache: bh_cache,
            wallet,
            tip_engine,
            rpc_sender,
            rpc_fallback_client,
            rpc_fallback_url,
            helius_rpc_url,
            public_rpc_url: public_rpc,
        })
    };

    let (momentum_engine, _scored_token_tx, _momentum_ws_handle, _momentum_logger_handle) = MomentumEngine::new(
        momentum_config.clone(),
        momentum_rpc_url,
        momentum_wss_url,
        &momentum_log_path,
        exec_ctx,
    );
    let momentum_engine = Arc::new(momentum_engine);

    info!(
        enabled = engine_config.momentum.enabled,
        paper_mode,
        entry_delay_ms = engine_config.momentum.entry_delay_ms,
        min_grad_score = engine_config.momentum.min_grad_score,
        position_size_sol = engine_config.momentum.position_size_sol,
        "[momentum] engine initialized"
    );

    // Set momentum enabled flag in API stats
    {
        let mut stats = api_state.stats.lock().unwrap();
        stats.momentum_enabled = engine_config.momentum.enabled;
    }

    // ── Orphan position recovery ─────────────────────────────────────
    {
        let recovery_engine = Arc::clone(&momentum_engine);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            recovery_engine.recover_orphan_positions().await;
        });
    }

    // ── Counters for stats logging ──────────────────────────────────
    let mut trades_seen: u64 = 0;
    let mut ticks: u64 = 0;
    let mut migrations: u64 = 0;
    let mut creator_sells: u64 = 0;

    // ── Main event loop — momentum only ─────────────────────────────
    info!(paper_mode, "Momentum engine running");

    loop {
        match engine_rx.recv() {
            Ok(FeedEvent::Trade(trade)) => {
                health_monitor.record_event(trade.source, trade.timestamp_ms);
                trades_seen += 1;

                // Stats logging every 1000 trades
                if trades_seen % 1000 == 0 {
                    info!(
                        trades = trades_seen,
                        ticks,
                        migrations,
                        creator_sells,
                        "engine stats"
                    );
                }
            }
            Ok(FeedEvent::PreWarm(prewarm)) => {
                health_monitor.record_event(prewarm.source, prewarm.timestamp_ms);
            }
            Ok(FeedEvent::Tick { ts_ms }) => {
                ticks += 1;

                // Momentum engine: check pending entries + active positions
                momentum_engine.on_tick(ts_ms).await;

                // Health check every 100 ticks (~5 seconds)
                if ticks % 100 == 0 {
                    let (health_status, recovered_feeds) = health_monitor.check(ts_ms);

                    if let pump_quant_core::engine::health::HealthStatus::Degraded { ref stale_feeds } = health_status {
                        for feed in stale_feeds {
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

                    for feed in &recovered_feeds {
                        info!(feed = %feed, "Feed recovered — trading resumed");
                        if let Some(ref tg) = telegram_alerter {
                            tg.try_send_blocking(&telegram::format_feed_recovered_alert(feed));
                        }
                    }
                }

                // Sync API stats every 200 ticks (~10 seconds)
                if ticks % 200 == 0 {
                    if let Ok(mut api_stats) = shared_stats.lock() {
                        api_stats.trades_seen = trades_seen;
                        api_stats.migrations_seen = migrations;
                        api_stats.creator_sells_seen = creator_sells;
                    }
                }
            }
            Ok(FeedEvent::TokenCreated(tc)) => {
                // Record creation timestamp for real grad_speed_s computation
                momentum_engine.record_token_created(tc.mint, tc.ts_ms);
            }
            Ok(FeedEvent::CreatorSell { mint: _, ts_ms }) => {
                health_monitor.record_event(FeedSource::CoreCast, ts_ms);
                creator_sells += 1;
            }
            Ok(FeedEvent::Migration { mint, ts_ms, source, sig }) => {
                if matches!(source, pump_quant_core::feeds::MigrationSource::CoreCastStream2) {
                    health_monitor.record_event(FeedSource::CoreCast, ts_ms);
                }
                migrations += 1;

                let mint_b58 = bs58::encode(&mint).into_string();
                let enrichment = GradEnrichment::UNKNOWN;

                info!(
                    mint = %mint_b58,
                    ts_ms,
                    source = source.as_str(),
                    "[momentum] graduation migration detected"
                );

                if engine_config.momentum.enabled {
                    let momentum = Arc::clone(&momentum_engine);
                    tokio::spawn(async move {
                        momentum.on_migration(mint, ts_ms, sig, enrichment).await;
                    });
                }
            }
            Ok(FeedEvent::PumpSwapGraduationDirect { mint, sig, ts_ms, coin_vault, pc_vault, source }) => {
                let mint_b58 = bs58::encode(&mint).into_string();
                migrations += 1;

                let enrichment = GradEnrichment::UNKNOWN;

                info!(
                    mint = %mint_b58,
                    ts_ms,
                    source = source.as_str(),
                    "[momentum] PumpSwap graduation direct detected"
                );

                if engine_config.momentum.enabled {
                    let momentum = Arc::clone(&momentum_engine);
                    tokio::spawn(async move {
                        momentum.on_pumpswap_graduation_direct(
                            mint, sig, ts_ms, coin_vault, pc_vault, source, enrichment,
                        ).await;
                    });
                }
            }
            Ok(FeedEvent::LpRemoval { mint: _, ts_ms: _ }) => {
                // No-op: momentum engine handles exits internally
            }
            Ok(FeedEvent::Shutdown) => {
                info!("Shutdown signal received");
                let _ = shutdown_tx.send(true);
                break;
            }
            Err(_) => {
                info!("Engine channel closed — shutting down");
                break;
            }
        }
    }

    info!(
        trades = trades_seen,
        ticks,
        migrations,
        creator_sells,
        "pump-quant-core stopped"
    );

    Ok(())
}
