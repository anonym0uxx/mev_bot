use crossbeam_channel::Sender;
use futures_util::{SinkExt, StreamExt};
use simd_json::derived::ValueObjectAccessAsScalar;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{error, info, warn};

use super::{FeedEvent, FeedSource, TradeEvent};

const WS_URL: &str = "wss://pumpportal.fun/api/data";
const SUBSCRIBE_MSG: &str = r#"{"method":"subscribeTokenTrade"}"#;
const MAX_BACKOFF_SECS: u64 = 30;
const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;

/// Run the PumpPortal WebSocket feed loop. Never returns unless shutdown.
/// Reconnects with exponential backoff on failure.
pub async fn run(tx: Sender<FeedEvent>, mut shutdown_rx: tokio::sync::watch::Receiver<bool>) {
    let mut backoff_secs: u64 = 1;

    loop {
        // Check shutdown before connecting
        if *shutdown_rx.borrow() {
            info!("PumpPortal feed: shutdown requested, exiting");
            return;
        }

        info!("PumpPortal feed: connecting to {}", WS_URL);

        match connect_async(WS_URL).await {
            Ok((ws_stream, _response)) => {
                info!("PumpPortal feed: connected");
                backoff_secs = 1; // Reset backoff on successful connect

                let (mut write, mut read) = ws_stream.split();

                // Send subscription message
                if let Err(e) = write.send(Message::Text(SUBSCRIBE_MSG.into())).await {
                    error!("PumpPortal feed: failed to send subscription: {}", e);
                    continue;
                }
                info!("PumpPortal feed: subscribed to token trades");

                // Read loop
                loop {
                    tokio::select! {
                        msg = read.next() => {
                            match msg {
                                Some(Ok(Message::Text(text))) => {
                                    match parse_trade_message(text.to_string()) {
                                        Ok(Some(event)) => {
                                            if tx.send(FeedEvent::Trade(event)).is_err() {
                                                info!("PumpPortal feed: engine channel closed, exiting");
                                                return;
                                            }
                                        }
                                        Ok(None) => {
                                            // Non-trade message (subscription ack, etc.) — skip
                                        }
                                        Err(e) => {
                                            warn!("PumpPortal feed: parse error: {}", e);
                                        }
                                    }
                                }
                                Some(Ok(Message::Ping(data))) => {
                                    let _ = write.send(Message::Pong(data)).await;
                                }
                                Some(Ok(Message::Close(_))) => {
                                    warn!("PumpPortal feed: server sent close frame");
                                    break;
                                }
                                Some(Ok(_)) => {} // Binary, Pong, etc.
                                Some(Err(e)) => {
                                    error!("PumpPortal feed: WebSocket error: {}", e);
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
                                info!("PumpPortal feed: shutdown requested");
                                let _ = write.send(Message::Close(None)).await;
                                return;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                error!(
                    "PumpPortal feed: connection failed: {} (retrying in {}s)",
                    e, backoff_secs
                );
            }
        }

        // Exponential backoff before reconnect
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

/// Parse a PumpPortal JSON trade message into a TradeEvent.
/// Returns Ok(None) for non-trade messages (e.g. subscription acks).
fn parse_trade_message(mut text: String) -> Result<Option<TradeEvent>, String> {
    // SAFETY: simd_json requires mutable access to the buffer for in-situ parsing
    let bytes = unsafe { text.as_bytes_mut() };
    let val: simd_json::BorrowedValue = simd_json::to_borrowed_value(bytes)
        .map_err(|e| format!("simd_json parse: {}", e))?;

    // PumpPortal trade messages have "signature" field. Subscription acks don't.
    let sig_b58 = match val.get_str("signature") {
        Some(s) => s,
        None => return Ok(None), // Not a trade message
    };

    let mint_b58 = val
        .get_str("mint")
        .ok_or("missing mint")?;
    let trader_b58 = val
        .get_str("traderPublicKey")
        .ok_or("missing traderPublicKey")?;
    let tx_type = val
        .get_str("txType")
        .ok_or("missing txType")?;

    let is_buy = tx_type == "buy";

    // SOL amounts come as floats — convert to lamports
    let sol_amount_f = val
        .get_f64("solAmount")
        .ok_or("missing solAmount")?;
    let sol_amount = (sol_amount_f * LAMPORTS_PER_SOL) as u64;

    let token_amount = val
        .get_u64("tokenAmount")
        .ok_or("missing tokenAmount")?;

    let vsol_f = val
        .get_f64("vSolInBondingCurve")
        .unwrap_or(0.0);
    let vsol_reserves = (vsol_f * LAMPORTS_PER_SOL) as u64;

    let vtoken_reserves = val
        .get_u64("vTokensInBondingCurve")
        .unwrap_or(0);

    let market_cap_f = val
        .get_f64("marketCapSol")
        .unwrap_or(0.0);
    let market_cap_sol = (market_cap_f * LAMPORTS_PER_SOL) as u64;

    // Decode base58 fields
    let sig = decode_sig(sig_b58)?;
    let mint = decode_pubkey(mint_b58)?;
    let trader = decode_pubkey(trader_b58)?;

    let mut sig_prefix = [0u8; 8];
    sig_prefix.copy_from_slice(&sig[..8]);

    // Bonding curve key (optional — not all messages may have it)
    let bonding_curve = match val.get_str("bondingCurveKey") {
        Some(s) => decode_pubkey(s).unwrap_or([0u8; 32]),
        None => [0u8; 32],
    };

    // Timestamp: use current time since PumpPortal doesn't provide epoch ms
    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

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
        assoc_bonding_curve: [0u8; 32], // Not provided by PumpPortal
    }))
}

/// Decode a base58 pubkey string to [u8; 32].
fn decode_pubkey(b58: &str) -> Result<[u8; 32], String> {
    let mut out = [0u8; 32];
    let decoded = bs58::decode(b58)
        .into_vec()
        .map_err(|e| format!("bs58 decode pubkey '{}': {}", b58, e))?;
    if decoded.len() != 32 {
        return Err(format!(
            "pubkey wrong length: expected 32, got {}",
            decoded.len()
        ));
    }
    out.copy_from_slice(&decoded);
    Ok(out)
}

/// Decode a base58 tx signature to [u8; 64].
fn decode_sig(b58: &str) -> Result<[u8; 64], String> {
    let mut out = [0u8; 64];
    let decoded = bs58::decode(b58)
        .into_vec()
        .map_err(|e| format!("bs58 decode sig: {}", e))?;
    if decoded.len() != 64 {
        return Err(format!(
            "sig wrong length: expected 64, got {}",
            decoded.len()
        ));
    }
    out.copy_from_slice(&decoded);
    Ok(out)
}
