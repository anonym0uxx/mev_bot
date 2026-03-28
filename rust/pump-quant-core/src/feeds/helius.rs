//! Helius WebSocket feed — processed-commitment log subscriptions on the
//! pump.fun program (`6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P`).
//!
//! Pre-warms mint trade history BEFORE PumpPortal confirms.
//! Emits `PreWarmEvent` only — no vSol, no reserves, no trigger logic.

use crossbeam_channel::Sender;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

use crate::feeds::{FeedEvent, FeedSource, PreWarmEvent};

const PUMP_PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
const MAX_BACKOFF_MS: u64 = 30_000;

pub struct HeliusConfig {
    pub api_key: String,
    pub enabled: bool,
}

pub struct HeliusWsClient {
    config: HeliusConfig,
    engine_tx: Sender<FeedEvent>,
}

impl HeliusWsClient {
    pub fn new(config: HeliusConfig, engine_tx: Sender<FeedEvent>) -> Self {
        Self { config, engine_tx }
    }

    /// Spawn a tokio task that connects, subscribes, and forwards PreWarm events.
    /// Reconnects on disconnect with exponential backoff (1s → 2s → 4s → max 30s).
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            self.run_loop().await;
        })
    }

    /// Connect, subscribe, and forward PreWarm events in a loop.
    /// Reconnects on disconnect with exponential backoff.
    /// This is an async method — call from a tokio task.
    pub async fn run_loop(self) {
        if !self.config.enabled || self.config.api_key.is_empty() {
            info!("[helius] disabled or no API key — skipping");
            return;
        }

        let url = format!(
            "wss://mainnet.helius-rpc.com/?api-key={}",
            self.config.api_key
        );

        let sub_msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "logsSubscribe",
            "params": [
                { "mentions": [PUMP_PROGRAM_ID] },
                { "commitment": "processed" }
            ]
        })
        .to_string();

        let mut backoff_ms: u64 = 1_000;

        loop {
            info!("[helius] connecting");

            match connect_async(&url).await {
                Err(e) => {
                    warn!("[helius] connect failed: {e} — retrying in {backoff_ms}ms");
                }
                Ok((ws_stream, _)) => {
                    backoff_ms = 1_000; // reset on successful connect

                    let (mut write, mut read) = ws_stream.split();

                    // Send subscription
                    if let Err(e) = write.send(Message::Text(sub_msg.clone().into())).await {
                        error!("[helius] subscribe send failed: {e}");
                        continue;
                    }

                    info!("[helius] connected and subscribed");

                    // Read loop
                    loop {
                        match read.next().await {
                            Some(Ok(Message::Text(text))) => {
                                if let Some(event) = parse_helius_log(&text) {
                                    if self.engine_tx.send(FeedEvent::PreWarm(event)).is_err() {
                                        info!("[helius] engine channel closed — exiting");
                                        return;
                                    }
                                }
                            }
                            Some(Ok(Message::Ping(data))) => {
                                let _ = write.send(Message::Pong(data)).await;
                            }
                            Some(Ok(Message::Close(_))) => {
                                warn!("[helius] server sent close frame");
                                break;
                            }
                            Some(Err(e)) => {
                                warn!("[helius] ws error: {e}");
                                break;
                            }
                            Some(Ok(_)) => {} // Binary, Pong, Frame — ignore
                            None => {
                                warn!("[helius] stream ended");
                                break;
                            }
                        }
                    }

                    warn!("[helius] disconnected — retrying in {backoff_ms}ms");
                }
            }

            // Exponential backoff
            tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
            backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
        }
    }
}

/// Parse a Helius `logsNotification` message.
/// Extracts signature + slot only — mint is not reliably derivable from logs.
fn parse_helius_log(text: &str) -> Option<PreWarmEvent> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;

    // Must be a logsNotification
    if v.get("method")?.as_str()? != "logsNotification" {
        return None;
    }

    let value = v.pointer("/params/result/value")?;

    // Skip failed transactions
    let err = value.get("err")?;
    if !err.is_null() {
        return None;
    }

    let sig_str = value.get("signature")?.as_str()?;
    let _slot = v
        .pointer("/params/result/context/slot")
        .and_then(|s| s.as_u64())
        .unwrap_or(0);

    // Decode base58 signature → [u8; 64]
    let sig_bytes = bs58::decode(sig_str).into_vec().ok()?;
    if sig_bytes.len() != 64 {
        debug!("[helius] unexpected sig length {}", sig_bytes.len());
        return None;
    }
    let mut sig = [0u8; 64];
    sig.copy_from_slice(&sig_bytes);

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    Some(PreWarmEvent {
        mint: [0u8; 32],
        trader: [0u8; 32],
        sig,
        sol_amount: 0,
        is_buy: true,
        timestamp_ms: now_ms,
        source: FeedSource::Helius,
    })
}
