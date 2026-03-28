//! pump-quant-core — Rust MEV engine entry point.
//!
//! Reads config from env, wires feeds → joiner → engine hot-path.
//! Paper mode: logs all trade decisions, no live tx submission.

use crossbeam_channel::bounded;
use tracing::info;
use tracing_subscriber::{fmt, EnvFilter};

use pump_quant_core::feeds::{
    event_joiner::EventJoiner,
    helius::{HeliusConfig, HeliusWsClient},
    FeedEvent,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Init tracing
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    tracing_subscriber::registry()
        .with(fmt::layer().with_target(true).compact())
        .with(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let paper_mode = std::env::var("PAPER_MODE")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(true);

    info!(paper_mode, "pump-quant-core starting");

    // Channels: feeds → joiner (bounded, drop oldest on overload)
    let (pp_tx, pp_rx) = bounded::<FeedEvent>(256);
    let (helius_tx, helius_rx) = bounded::<FeedEvent>(256);

    // Channel: joiner → engine hot-path (bounded)
    let (engine_tx, engine_rx) = bounded::<FeedEvent>(1024);

    // Shutdown signal for PumpPortal feed
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    // Spawn PumpPortal feed task
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

    // Spawn EventJoiner on a dedicated thread
    let joiner = EventJoiner::new(pp_rx, helius_rx, engine_tx);
    std::thread::Builder::new()
        .name("event-joiner".to_string())
        .spawn(move || joiner.run())?;
    info!("EventJoiner thread started");

    // Engine hot-path loop (placeholder — full gate/score/position logic goes here)
    info!(paper_mode, "Engine hot-path running");
    let mut trade_count = 0u64;
    let mut prewarm_count = 0u64;

    loop {
        match engine_rx.recv() {
            Ok(FeedEvent::Trade(t)) => {
                trade_count += 1;
                if trade_count % 100 == 0 {
                    info!(
                        trade_count,
                        prewarm_count,
                        mint = %bs58::encode(&t.mint).into_string(),
                        sol_lamports = t.sol_amount,
                        vsol_lamports = t.vsol_reserves,
                        is_buy = t.is_buy,
                        source = ?t.source,
                        "trade [sample]"
                    );
                }
            }
            Ok(FeedEvent::PreWarm(p)) => {
                prewarm_count += 1;
                tracing::debug!(source = ?p.source, "prewarm");
            }
            Ok(FeedEvent::Tick { ts_ms }) => {
                tracing::debug!(ts_ms, "tick");
            }
            Ok(FeedEvent::Shutdown) => {
                info!("Shutdown signal received");
                let _ = shutdown_tx.send(true);
                break;
            }
            Ok(FeedEvent::CreatorSell { mint, ts_ms }) => {
                info!(
                    mint = %bs58::encode(&mint).into_string(),
                    ts_ms,
                    "creator sell detected"
                );
            }
            Err(_) => {
                info!("Engine channel closed — shutting down");
                break;
            }
        }
    }

    info!(trade_count, prewarm_count, "pump-quant-core stopped");
    Ok(())
}
