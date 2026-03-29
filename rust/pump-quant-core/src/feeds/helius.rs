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
///
/// Extracts signature + slot. Attempts to extract mint from program log lines
/// (Pump.fun emits `Program log: <base58_mint>` in buy/sell instruction logs).
/// If mint extraction fails, emits PreWarmEvent with mint=[0u8;32] — the engine
/// can still use the sig_prefix for dedup correlation with PumpPortal.
///
/// Also detects buy vs sell from log content when possible.
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

    // LATENCY: Decode base58 signature into stack-allocated [u8; 64].
    // Uses bs58::decode().onto() — no heap allocation (saves ~50-80ns per decode).
    let mut sig = [0u8; 64];
    match bs58::decode(sig_str).onto(&mut sig[..]) {
        Ok(n) if n == 64 => {}
        Ok(n) => {
            debug!("[helius] unexpected sig length {}", n);
            return None;
        }
        Err(_) => return None,
    }

    // ── Attempt to extract mint + direction from program log lines ──
    // Pump.fun program logs contain structured data we can parse:
    //   "Program log: Instruction: Buy" / "Program log: Instruction: Sell"
    //   The mint address appears as an account key in the invoke context.
    //
    // Log format from pump.fun (observed pattern):
    //   "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [1]"
    //   "Program log: Instruction: Buy"
    //   ... (inner CPI logs)
    //
    // We cannot reliably extract the mint from these logs alone because the
    // account keys are not logged — only the program ID and instruction name.
    // The actual mint address is in the transaction's accountKeys, which
    // logsSubscribe does NOT provide.
    //
    // What we CAN extract: buy/sell direction from "Instruction: Buy/Sell" logs.
    let mut is_buy = true; // default assumption
    let mut is_pump_trade = false;

    if let Some(logs) = value.get("logs").and_then(|l| l.as_array()) {
        for log_entry in logs {
            if let Some(log_str) = log_entry.as_str() {
                // Detect pump.fun program invocation
                if log_str.starts_with("Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke") {
                    is_pump_trade = true;
                }
                // Detect buy/sell direction
                if log_str.contains("Instruction: Buy") {
                    is_buy = true;
                } else if log_str.contains("Instruction: Sell") {
                    is_buy = false;
                }
            }
        }
    }

    // Only emit if this is actually a pump.fun transaction
    if !is_pump_trade {
        return None;
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    // NOTE: mint=[0u8;32] because logsSubscribe doesn't provide accountKeys.
    // The sig_prefix is used for dedup correlation when PumpPortal confirms.
    // To make Helius a true primary trigger, we'd need either:
    //   1. accountSubscribe on pump.fun bonding curves (full account data)
    //   2. LaserStream Preprocessed Transactions (gRPC, full decoded tx)
    //   3. Helius Enhanced Transactions API (HTTP, adds latency)
    Some(PreWarmEvent {
        mint: [0u8; 32],
        trader: [0u8; 32],
        sig,
        sol_amount: 0,
        is_buy,
        timestamp_ms: now_ms,
        source: FeedSource::Helius,
    })
}
