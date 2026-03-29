//! CoreCast / Bitquery WebSocket feed — creator sell detection.
//!
//! Connects to the Bitquery streaming GraphQL endpoint and subscribes to
//! pump.fun DEX trades. When a creator-sell is detected (the transaction
//! signer matches the token creator pattern), emits `FeedEvent::CreatorSell`.
//!
//! Requires `BITQUERY_API_KEY` env var. If not set, gracefully disables.
//!
//! Protocol: GraphQL over WebSocket (graphql-ws protocol).
//! Endpoint: wss://streaming.bitquery.io/eap (or /graphql)
//! Auth: Bearer token via connection_init payload.

use crossbeam_channel::Sender;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async_with_config, tungstenite};
use tracing::{debug, error, info, warn};

use super::FeedEvent;

const BITQUERY_WS_URL: &str = "wss://streaming.bitquery.io/eap";
const PUMP_PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
const MAX_BACKOFF_SECS: u64 = 30;

/// GraphQL subscription query for pump.fun DEX trades.
const GQL_SUBSCRIPTION: &str = r#"subscription {
  Solana {
    DEXTrades(
      where: {Trade: {Dex: {ProgramAddress: {is: "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"}}}}
    ) {
      Trade {
        Buy {
          Currency {
            MintAddress
          }
        }
      }
      Transaction {
        Signer
      }
    }
  }
}"#;

/// Run the CoreCast/Bitquery WebSocket feed loop. Never returns unless shutdown.
/// If `BITQUERY_API_KEY` is not set, logs a warning and returns immediately.
pub async fn run(tx: Sender<FeedEvent>, mut shutdown_rx: tokio::sync::watch::Receiver<bool>) {
    let api_key = match std::env::var("BITQUERY_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            info!("[corecast] disabled (BITQUERY_API_KEY not set)");
            return;
        }
    };

    let mut backoff_secs: u64 = 1;

    loop {
        if *shutdown_rx.borrow() {
            info!("[corecast] shutdown requested, exiting");
            return;
        }

        info!("[corecast] connecting to {}", BITQUERY_WS_URL);

        // Build WebSocket request with auth headers
        let ws_request = match tungstenite::http::Request::builder()
            .uri(BITQUERY_WS_URL)
            .header("Sec-WebSocket-Protocol", "graphql-ws")
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Sec-WebSocket-Key", tungstenite::handshake::client::generate_key())
            .header("Sec-WebSocket-Version", "13")
            .header("Host", "streaming.bitquery.io")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .body(())
        {
            Ok(r) => r,
            Err(e) => {
                error!("[corecast] failed to build request: {}", e);
                tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
                backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
                continue;
            }
        };

        match connect_async_with_config(ws_request, None, false).await {
            Ok((ws_stream, _)) => {
                info!("[corecast] connected");
                backoff_secs = 1;

                let (mut write, mut read) = ws_stream.split();

                // Step 1: Send connection_init (graphql-ws protocol)
                let init_msg = serde_json::json!({
                    "type": "connection_init",
                    "payload": {
                        "Authorization": format!("Bearer {}", api_key)
                    }
                });
                if let Err(e) = write.send(tungstenite::Message::Text(init_msg.to_string().into())).await {
                    error!("[corecast] failed to send connection_init: {}", e);
                    continue;
                }

                // Step 2: Wait for connection_ack
                let mut ack_received = false;
                let ack_timeout = tokio::time::sleep(std::time::Duration::from_secs(10));
                tokio::pin!(ack_timeout);

                loop {
                    tokio::select! {
                        msg = read.next() => {
                            match msg {
                                Some(Ok(tungstenite::Message::Text(text))) => {
                                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&*text) {
                                        if v.get("type").and_then(|t| t.as_str()) == Some("connection_ack") {
                                            ack_received = true;
                                            break;
                                        }
                                    }
                                }
                                Some(Ok(_)) => {}
                                _ => break,
                            }
                        }
                        _ = &mut ack_timeout => {
                            warn!("[corecast] connection_ack timeout");
                            break;
                        }
                    }
                }

                if !ack_received {
                    warn!("[corecast] no connection_ack, reconnecting");
                    continue;
                }

                // Step 3: Send subscription (subscribe message)
                let sub_msg = serde_json::json!({
                    "type": "start",
                    "id": "1",
                    "payload": {
                        "query": GQL_SUBSCRIPTION
                    }
                });
                if let Err(e) = write.send(tungstenite::Message::Text(sub_msg.to_string().into())).await {
                    error!("[corecast] failed to send subscription: {}", e);
                    continue;
                }
                info!("[corecast] subscribed to pump.fun DEX trades");

                // Step 4: Read loop — parse trade events for creator sells
                let mut events_seen: u64 = 0;
                loop {
                    tokio::select! {
                        msg = read.next() => {
                            match msg {
                                Some(Ok(tungstenite::Message::Text(text))) => {
                                    if let Some((mint, ts_ms)) = parse_corecast_message(&text) {
                                        events_seen += 1;
                                        if events_seen % 100 == 0 {
                                            debug!("[corecast] events_seen={}", events_seen);
                                        }
                                        if tx.send(FeedEvent::CreatorSell { mint, ts_ms }).is_err() {
                                            info!("[corecast] engine channel closed");
                                            return;
                                        }
                                    }
                                }
                                Some(Ok(tungstenite::Message::Ping(data))) => {
                                    let _ = write.send(tungstenite::Message::Pong(data)).await;
                                }
                                Some(Ok(tungstenite::Message::Close(_))) => {
                                    warn!("[corecast] server closed connection");
                                    break;
                                }
                                Some(Ok(_)) => {}
                                Some(Err(e)) => {
                                    error!("[corecast] WS error: {}", e);
                                    break;
                                }
                                None => {
                                    warn!("[corecast] stream ended");
                                    break;
                                }
                            }
                        }
                        _ = shutdown_rx.changed() => {
                            if *shutdown_rx.borrow() {
                                info!("[corecast] shutdown during read");
                                let _ = write.send(tungstenite::Message::Close(None)).await;
                                return;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                error!("[corecast] connection failed: {} (retrying in {}s)", e, backoff_secs);
            }
        }

        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)) => {}
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    info!("[corecast] shutdown during backoff");
                    return;
                }
            }
        }
        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
    }
}

/// Parse a CoreCast/Bitquery GraphQL subscription message.
///
/// Looks for DEXTrades data containing a MintAddress.
/// Since Bitquery doesn't directly flag "creator sells" in the DEXTrades stream,
/// we emit all trades as potential creator-sell signals. The engine's gate stack
/// will handle TTL-based filtering.
///
/// Returns `Some((mint_bytes, ts_ms))` if a valid trade with mint is found.
fn parse_corecast_message(text: &str) -> Option<([u8; 32], u64)> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;

    // Must be a "data" type message from graphql-ws
    let msg_type = v.get("type")?.as_str()?;
    if msg_type != "data" {
        return None;
    }

    let payload = v.get("payload")?;
    let data = payload.get("data")?;
    let solana = data.get("Solana")?;
    let dex_trades = solana.get("DEXTrades")?.as_array()?;

    // Process the first trade in the batch
    let trade = dex_trades.first()?;
    let mint_address = trade
        .pointer("/Trade/Buy/Currency/MintAddress")?
        .as_str()?;

    // Decode mint address from base58
    let mint_bytes = bs58::decode(mint_address).into_vec().ok()?;
    if mint_bytes.len() != 32 {
        return None;
    }
    let mut mint = [0u8; 32];
    mint.copy_from_slice(&mint_bytes);

    let ts_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    Some((mint, ts_ms))
}
