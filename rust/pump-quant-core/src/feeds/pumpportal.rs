//! PumpPortal WebSocket feed.
//!
//! Protocol flow:
//! 1. Connect to wss://pumpportal.fun/api/data
//! 2. Send `{"method":"subscribeNewToken"}` → receives new token creation events
//! 3. On each new token, immediately send `{"method":"subscribeTokenTrade","keys":["<mint>"]}` 
//!    → starts receiving buy/sell trade events for that mint
//! 4. Parse trade events → emit FeedEvent::Trade

use crossbeam_channel::Sender;
use futures_util::{SinkExt, StreamExt};
use simd_json::derived::ValueObjectAccessAsScalar;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

use super::{FeedEvent, FeedSource, TradeEvent};

const WS_URL: &str = "wss://pumpportal.fun/api/data";
const MAX_BACKOFF_SECS: u64 = 30;
const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;

/// Run the PumpPortal WebSocket feed loop. Never returns unless shutdown.
/// Reconnects with exponential backoff on failure.
///
/// Protocol:
/// - Subscribes to `subscribeNewToken` for new token creation events
/// - On each new token creation, subscribes to that token's trades via `subscribeTokenTrade`
/// - Trade events are emitted as `FeedEvent::Trade`
pub async fn run(tx: Sender<FeedEvent>, mut shutdown_rx: tokio::sync::watch::Receiver<bool>) {
    let mut backoff_secs: u64 = 1;

    loop {
        if *shutdown_rx.borrow() {
            info!("PumpPortal feed: shutdown requested, exiting");
            return;
        }

        info!("PumpPortal feed: connecting to {}", WS_URL);

        match connect_async(WS_URL).await {
            Ok((ws_stream, _response)) => {
                info!("PumpPortal feed: connected");
                backoff_secs = 1;

                let (mut write, mut read) = ws_stream.split();

                // Step 1: Subscribe to new token creation events
                let sub_msg = r#"{"method":"subscribeNewToken"}"#;
                if let Err(e) = write.send(Message::Text(sub_msg.into())).await {
                    error!("PumpPortal feed: failed to subscribe to new tokens: {}", e);
                    continue;
                }
                info!("PumpPortal feed: subscribed to new token creations");

                // Internal channel for write-side messages (subscribe to trades)
                let (write_tx, mut write_rx) = mpsc::channel::<String>(256);

                // Read loop
                loop {
                    tokio::select! {
                        // Outbound: send subscribe-to-trade messages for new tokens
                        Some(msg) = write_rx.recv() => {
                            if let Err(e) = write.send(Message::Text(msg.into())).await {
                                warn!("PumpPortal feed: write error: {}", e);
                            }
                        }

                        // Inbound: parse messages from PumpPortal
                        msg = read.next() => {
                            match msg {
                                Some(Ok(Message::Text(text))) => {
                                    match parse_message(text.to_string(), &write_tx) {
                                        Ok(Some(event)) => {
                                            if tx.send(FeedEvent::Trade(event)).is_err() {
                                                info!("PumpPortal feed: engine channel closed");
                                                return;
                                            }
                                        }
                                        Ok(None) => {} // ack, creation event, etc.
                                        Err(e) => {
                                            debug!("PumpPortal feed: parse skip: {}", e);
                                        }
                                    }
                                }
                                Some(Ok(Message::Ping(data))) => {
                                    let _ = write.send(Message::Pong(data)).await;
                                }
                                Some(Ok(Message::Close(_))) => {
                                    warn!("PumpPortal feed: server closed connection");
                                    break;
                                }
                                Some(Ok(_)) => {}
                                Some(Err(e)) => {
                                    error!("PumpPortal feed: WS error: {}", e);
                                    break;
                                }
                                None => {
                                    warn!("PumpPortal feed: stream ended");
                                    break;
                                }
                            }
                        }

                        _ = shutdown_rx.changed() => {
                            if *shutdown_rx.borrow() {
                                info!("PumpPortal feed: shutdown during read");
                                let _ = write.send(Message::Close(None)).await;
                                return;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                error!("PumpPortal feed: connection failed: {} (retrying in {}s)", e, backoff_secs);
            }
        }

        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)) => {}
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    info!("PumpPortal feed: shutdown during backoff");
                    return;
                }
            }
        }
        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
    }
}

/// Parse an incoming PumpPortal message.
///
/// Returns:
/// - `Ok(Some(TradeEvent))` for buy/sell trade events
/// - `Ok(None)` for new token creation events, acks, or migrations (handled internally)
/// - `Err(msg)` for parse failures on what looked like a trade
fn parse_message(mut text: String, write_tx: &mpsc::Sender<String>) -> Result<Option<TradeEvent>, String> {
    let bytes = unsafe { text.as_bytes_mut() };
    let val: simd_json::BorrowedValue = simd_json::to_borrowed_value(bytes)
        .map_err(|e| format!("json: {}", e))?;

    // Check what kind of message this is
    let sig_b58 = match val.get_str("signature") {
        Some(s) => s,
        None => return Ok(None), // subscription ack ({"message":"Successfully subscribed..."})
    };

    let mint_b58 = match val.get_str("mint") {
        Some(m) => m,
        None => return Ok(None),
    };

    // Detect new token creation event (has "name"/"symbol" but no txType)
    // → subscribe to this token's trades immediately
    if val.get_str("txType").is_none() {
        // New token creation event — subscribe to its trades
        let sub_msg = format!(
            r#"{{"method":"subscribeTokenTrade","keys":["{}"]}}"#,
            mint_b58
        );
        // Non-blocking — if the channel is full, skip (we'll catch it next token)
        let _ = write_tx.try_send(sub_msg);
        debug!("PumpPortal feed: new token {}, subscribed to trades", &mint_b58[..8.min(mint_b58.len())]);
        return Ok(None);
    }

    // Trade event — parse fully
    let tx_type = val.get_str("txType").unwrap(); // safe — checked above
    let is_buy = tx_type == "buy";

    let sol_amount_f = val.get_f64("solAmount").unwrap_or(0.0);
    let sol_amount = (sol_amount_f * LAMPORTS_PER_SOL) as u64;

    let token_amount = val.get_u64("tokenAmount").unwrap_or(0);

    let vsol_f = val.get_f64("vSolInBondingCurve").unwrap_or(0.0);
    let vsol_reserves = (vsol_f * LAMPORTS_PER_SOL) as u64;

    let vtoken_reserves = val.get_u64("vTokensInBondingCurve").unwrap_or(0);

    let market_cap_f = val.get_f64("marketCapSol").unwrap_or(0.0);
    let market_cap_sol = (market_cap_f * LAMPORTS_PER_SOL) as u64;

    let trader_b58 = val.get_str("traderPublicKey").unwrap_or("");

    // Decode pubkeys
    let sig = decode_sig(sig_b58)?;
    let mint = decode_pubkey(mint_b58)?;
    let trader = if !trader_b58.is_empty() {
        decode_pubkey(trader_b58)?
    } else {
        [0u8; 32]
    };

    let mut sig_prefix = [0u8; 8];
    sig_prefix.copy_from_slice(&sig[..8]);

    let bonding_curve = match val.get_str("bondingCurveKey") {
        Some(k) => decode_pubkey(k).unwrap_or([0u8; 32]),
        None => [0u8; 32],
    };
    let assoc_bonding_curve = match val.get_str("associatedBondingCurve") {
        Some(k) => decode_pubkey(k).unwrap_or([0u8; 32]),
        None => [0u8; 32],
    };

    let timestamp_ms = val.get_u64("timestamp").unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    });

    Ok(Some(TradeEvent {
        mint,
        trader,
        sig,
        sig_prefix,
        sol_amount,
        token_amount,
        vsol_reserves,
        vtoken_reserves,
        market_cap_sol,
        slot: 0, // PumpPortal doesn't provide slot
        timestamp_ms,
        is_buy,
        source: FeedSource::PumpPortal,
        bonding_curve,
        assoc_bonding_curve,
    }))
}

fn decode_sig(b58: &str) -> Result<[u8; 64], String> {
    let bytes = bs58::decode(b58).into_vec()
        .map_err(|e| format!("sig b58: {}", e))?;
    if bytes.len() != 64 {
        return Err(format!("sig len {} != 64", bytes.len()));
    }
    let mut arr = [0u8; 64];
    arr.copy_from_slice(&bytes);
    Ok(arr)
}

fn decode_pubkey(b58: &str) -> Result<[u8; 32], String> {
    let bytes = bs58::decode(b58).into_vec()
        .map_err(|e| format!("pubkey b58: {}", e))?;
    if bytes.len() != 32 {
        return Err(format!("pubkey len {} != 32", bytes.len()));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(arr)
}
