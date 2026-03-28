//! ShredStream UDP feed — lowest-latency Solana data via Jito pre-confirmation shreds.
//!
//! Reads `SHREDSTREAM_ENDPOINT` env var. If gRPC/HTTP is specified, falls back to
//! UDP listener on port 10001. Parses raw shred bytes for pump.fun buy/sell
//! discriminators and emits `PreWarmEvent` events.

use crossbeam_channel::Sender;
use tokio::net::UdpSocket;
use tracing::{debug, error, info, warn};

use crate::feeds::{FeedEvent, FeedSource, PreWarmEvent};

/// 8-byte Anchor discriminator for pump.fun buy instruction.
const BUY_DISCRIMINATOR: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];

/// 8-byte Anchor discriminator for pump.fun sell instruction.
const SELL_DISCRIMINATOR: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];

/// Minimum datagram size: 8 (discriminator) + 32 (mint) + 8 (sol_amount) = 48 bytes.
const MIN_PAYLOAD_SIZE: usize = 48;

/// Default UDP listen port for shred relay.
const DEFAULT_UDP_PORT: u16 = 10001;

/// Configuration for ShredStream feed.
pub struct ShredStreamConfig {
    pub endpoint: Option<String>,
    pub enabled: bool,
}

impl ShredStreamConfig {
    /// Build config from environment variables.
    pub fn from_env() -> Self {
        let endpoint = std::env::var("SHREDSTREAM_ENDPOINT").ok();
        let enabled = endpoint.is_some();
        Self { endpoint, enabled }
    }
}

/// Run the ShredStream UDP feed loop.
///
/// - If `SHREDSTREAM_ENDPOINT` is not set: logs "disabled" and returns immediately.
/// - If set with `grpc://` or `http://`: logs fallback warning, listens on UDP 10001.
/// - Parses incoming datagrams for pump.fun trade discriminators.
/// - Emits `FeedEvent::PreWarm` for each matched trade.
pub async fn run(tx: Sender<FeedEvent>, mut shutdown_rx: tokio::sync::watch::Receiver<bool>) {
    let config = ShredStreamConfig::from_env();

    if !config.enabled {
        info!("[shredstream] ShredStream disabled (SHREDSTREAM_ENDPOINT not set)");
        return;
    }

    let endpoint = config.endpoint.as_deref().unwrap_or("");

    if endpoint.starts_with("grpc://") || endpoint.starts_with("http://") {
        warn!(
            "[shredstream] ShredStream gRPC not available in this build, falling back to UDP on port {}",
            DEFAULT_UDP_PORT
        );
    }

    // Bind UDP socket
    let bind_addr = format!("0.0.0.0:{}", DEFAULT_UDP_PORT);
    let socket = match UdpSocket::bind(&bind_addr).await {
        Ok(s) => {
            info!("[shredstream] listening on UDP {}", bind_addr);
            s
        }
        Err(e) => {
            error!("[shredstream] failed to bind UDP {}: {}", bind_addr, e);
            return;
        }
    };

    let mut buf = [0u8; 65536]; // max UDP datagram size
    let mut events_emitted: u64 = 0;
    let mut datagrams_received: u64 = 0;

    loop {
        tokio::select! {
            result = socket.recv_from(&mut buf) => {
                match result {
                    Ok((len, _addr)) => {
                        datagrams_received += 1;
                        if datagrams_received % 10_000 == 0 {
                            debug!(
                                "[shredstream] datagrams={} events_emitted={}",
                                datagrams_received, events_emitted
                            );
                        }

                        if let Some(event) = parse_shred_datagram(&buf[..len]) {
                            events_emitted += 1;
                            if tx.send(FeedEvent::PreWarm(event)).is_err() {
                                info!("[shredstream] engine channel closed — exiting");
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        warn!("[shredstream] recv error: {}", e);
                    }
                }
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    info!("[shredstream] shutdown requested — exiting");
                    return;
                }
            }
        }
    }
}

/// Scan a UDP datagram for pump.fun buy/sell discriminators.
///
/// Layout expected after discriminator match:
///   [0..8]   discriminator (buy or sell)
///   [8..40]  mint pubkey (32 bytes)
///   [40..48] sol_amount (u64, little-endian, lamports)
///
/// The discriminator may appear at any offset in the datagram (shreds contain
/// serialized transaction data at variable offsets), so we scan the full payload.
fn parse_shred_datagram(data: &[u8]) -> Option<PreWarmEvent> {
    if data.len() < MIN_PAYLOAD_SIZE {
        return None;
    }

    // Scan for discriminator at any offset where there's enough room for the full payload
    let max_start = data.len().saturating_sub(MIN_PAYLOAD_SIZE);
    for offset in 0..=max_start {
        let disc = &data[offset..offset + 8];

        let is_buy;
        if disc == BUY_DISCRIMINATOR {
            is_buy = true;
        } else if disc == SELL_DISCRIMINATOR {
            is_buy = false;
        } else {
            continue;
        }

        // Found a discriminator — extract fields
        let mint_start = offset + 8;
        let mint_end = mint_start + 32;
        let sol_start = mint_end;
        let sol_end = sol_start + 8;

        if sol_end > data.len() {
            continue;
        }

        let mut mint = [0u8; 32];
        mint.copy_from_slice(&data[mint_start..mint_end]);

        let sol_amount = u64::from_le_bytes(
            data[sol_start..sol_end].try_into().unwrap(),
        );

        // Basic sanity: skip obviously invalid amounts (0 or > 10k SOL)
        if sol_amount == 0 || sol_amount > 10_000_000_000_000 {
            continue;
        }

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        return Some(PreWarmEvent {
            mint,
            trader: [0u8; 32], // not available from raw shred data
            sig: [0u8; 64],    // not available from raw shred data
            sol_amount,
            is_buy,
            timestamp_ms: now_ms,
            source: FeedSource::ShredStream,
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_datagram(discriminator: &[u8; 8], mint: &[u8; 32], sol_lamports: u64) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(discriminator);
        buf.extend_from_slice(mint);
        buf.extend_from_slice(&sol_lamports.to_le_bytes());
        buf
    }

    #[test]
    fn test_parse_buy_discriminator() {
        let mint = [0xAA; 32];
        let sol = 1_000_000_000u64; // 1 SOL
        let data = make_test_datagram(&BUY_DISCRIMINATOR, &mint, sol);

        let event = parse_shred_datagram(&data).expect("should parse buy");
        assert!(event.is_buy);
        assert_eq!(event.mint, mint);
        assert_eq!(event.sol_amount, sol);
        assert_eq!(event.source, FeedSource::ShredStream);
    }

    #[test]
    fn test_parse_sell_discriminator() {
        let mint = [0xBB; 32];
        let sol = 500_000_000u64; // 0.5 SOL
        let data = make_test_datagram(&SELL_DISCRIMINATOR, &mint, sol);

        let event = parse_shred_datagram(&data).expect("should parse sell");
        assert!(!event.is_buy);
        assert_eq!(event.mint, mint);
        assert_eq!(event.sol_amount, sol);
    }

    #[test]
    fn test_parse_with_prefix_bytes() {
        // Discriminator not at offset 0 — simulate shred framing
        let mint = [0xCC; 32];
        let sol = 2_000_000_000u64;
        let mut data = vec![0xFF; 16]; // 16 bytes of junk prefix
        data.extend_from_slice(&BUY_DISCRIMINATOR);
        data.extend_from_slice(&mint);
        data.extend_from_slice(&sol.to_le_bytes());

        let event = parse_shred_datagram(&data).expect("should parse with prefix");
        assert!(event.is_buy);
        assert_eq!(event.mint, mint);
        assert_eq!(event.sol_amount, sol);
    }

    #[test]
    fn test_reject_too_small() {
        let data = [0u8; 10];
        assert!(parse_shred_datagram(&data).is_none());
    }

    #[test]
    fn test_reject_zero_amount() {
        let mint = [0xDD; 32];
        let data = make_test_datagram(&BUY_DISCRIMINATOR, &mint, 0);
        assert!(parse_shred_datagram(&data).is_none());
    }

    #[test]
    fn test_reject_absurd_amount() {
        let mint = [0xDD; 32];
        let data = make_test_datagram(&BUY_DISCRIMINATOR, &mint, 100_000_000_000_000); // >10k SOL
        assert!(parse_shred_datagram(&data).is_none());
    }

    #[test]
    fn test_no_discriminator_match() {
        let mut data = vec![0u8; 48];
        data[0..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]); // wrong discriminator
        assert!(parse_shred_datagram(&data).is_none());
    }
}
