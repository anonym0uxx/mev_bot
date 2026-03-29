//! ShredStream feed — lowest-latency trade detection.
//!
//! Connects to Jito ShredStream (or compatible endpoint) for raw shred data,
//! ~80ms faster than PumpPortal/Helius websocket feeds.
//!
//! Architecture:
//! - `ShredStreamFeed` is the primary struct, holding config + channel sender + stats.
//! - `start()` spawns a tokio task that runs the connection loop with reconnect.
//! - `parse_trade()` decodes raw shred/transaction bytes for Pump.fun buy/sell.
//! - Falls back to UDP listener when WebSocket endpoint is not available.
//!
//! Integration:
//! - Sends `FeedEvent::PreWarm` into the event joiner channel.
//! - EventJoiner already has `shredstream_rx: Option<Receiver<FeedEvent>>` wired up.
//!
//! Compatibility:
//! - `ShredStreamConfig::from_env()` and `run()` are kept for backward compat with main.rs.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crossbeam_channel::Sender;
use tokio::net::UdpSocket;
use tracing::{debug, error, info, warn};

use crate::feeds::{FeedEvent, FeedSource, PreWarmEvent};

// ── Pump.fun Anchor discriminators ──────────────────────────────────

/// 8-byte Anchor discriminator for pump.fun `buy` instruction.
const BUY_DISCRIMINATOR: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];

/// 8-byte Anchor discriminator for pump.fun `sell` instruction.
const SELL_DISCRIMINATOR: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];

/// Minimum datagram size: 8 (discriminator) + 32 (mint) + 8 (sol_amount) = 48 bytes.
const MIN_PAYLOAD_SIZE: usize = 48;

/// Default UDP listen port for shred relay.
const DEFAULT_UDP_PORT: u16 = 10001;

/// Pump.fun program ID bytes (6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P).
/// Pre-computed from bs58 decode to avoid runtime parsing.
const PUMP_PROGRAM_ID: [u8; 32] = [
    0x01, 0x56, 0xe0, 0xf6, 0x93, 0x66, 0x5a, 0xcf,
    0x44, 0xdb, 0x15, 0x68, 0xbf, 0x17, 0x5b, 0xaa,
    0x51, 0x89, 0xcb, 0x97, 0xf5, 0xd2, 0xff, 0x3b,
    0x65, 0x5d, 0x2b, 0xb6, 0xfd, 0x6d, 0x18, 0xb0,
];

// ── ShredStreamConfig ───────────────────────────────────────────────

/// Configuration for the ShredStream feed.
pub struct ShredStreamConfig {
    /// WebSocket or UDP endpoint URL.
    /// - `wss://...` or `ws://...` → WebSocket mode (Jito ShredStream / compatible relay)
    /// - `udp://host:port` → UDP listener mode
    /// - Any other value → falls back to UDP on DEFAULT_UDP_PORT
    pub endpoint: Option<String>,
    /// Whether the feed is enabled at all.
    pub enabled: bool,
    /// Pump.fun program ID bytes for instruction filtering.
    pub program_filter: [u8; 32],
    /// Initial reconnect delay in milliseconds.
    pub reconnect_delay_ms: u64,
    /// Maximum reconnect delay (exponential backoff cap).
    pub max_reconnect_delay_ms: u64,
}

impl Default for ShredStreamConfig {
    fn default() -> Self {
        Self {
            endpoint: None,
            enabled: false,
            program_filter: PUMP_PROGRAM_ID,
            reconnect_delay_ms: 100,
            max_reconnect_delay_ms: 5000,
        }
    }
}

impl ShredStreamConfig {
    /// Build config from environment variables.
    ///
    /// - `SHREDSTREAM_ENDPOINT` → endpoint URL (presence enables the feed)
    /// - `SHREDSTREAM_RECONNECT_MS` → initial reconnect delay (default 100)
    /// - `SHREDSTREAM_MAX_RECONNECT_MS` → max reconnect delay (default 5000)
    pub fn from_env() -> Self {
        let endpoint = std::env::var("SHREDSTREAM_ENDPOINT").ok();
        let enabled = endpoint.is_some();
        let reconnect_delay_ms = std::env::var("SHREDSTREAM_RECONNECT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);
        let max_reconnect_delay_ms = std::env::var("SHREDSTREAM_MAX_RECONNECT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5000);

        Self {
            endpoint,
            enabled,
            program_filter: PUMP_PROGRAM_ID,
            reconnect_delay_ms,
            max_reconnect_delay_ms,
        }
    }
}

// ── ShredStreamFeed ─────────────────────────────────────────────────

/// ShredStream feed client.
///
/// Connects to a Jito ShredStream-compatible endpoint for lowest-latency
/// Pump.fun trade detection. Emits `FeedEvent::PreWarm` events into the
/// event joiner channel.
///
/// # Usage
/// ```ignore
/// let (tx, rx) = crossbeam_channel::bounded(256);
/// let config = ShredStreamConfig::from_env();
/// let feed = Arc::new(ShredStreamFeed::new(config, tx));
/// let handle = feed.start();
/// // rx is wired into EventJoiner as shredstream_rx
/// ```
pub struct ShredStreamFeed {
    config: ShredStreamConfig,
    tx: Sender<FeedEvent>,
    /// Total events successfully parsed and sent.
    pub events_received: AtomicU64,
    /// Number of reconnection attempts.
    pub reconnections: AtomicU64,
    /// Total raw datagrams/messages received (including non-trade).
    pub messages_received: AtomicU64,
}

impl ShredStreamFeed {
    /// Create a new ShredStream feed with the given config and output channel.
    pub fn new(config: ShredStreamConfig, tx: Sender<FeedEvent>) -> Self {
        Self {
            config,
            tx,
            events_received: AtomicU64::new(0),
            reconnections: AtomicU64::new(0),
            messages_received: AtomicU64::new(0),
        }
    }

    /// Start the feed in a background tokio task.
    ///
    /// Returns a `JoinHandle` for lifecycle management. The task runs until:
    /// - The engine channel (`tx`) is closed (receiver dropped)
    /// - The feed is not enabled (returns immediately after logging)
    ///
    /// Reconnects automatically with exponential backoff on connection failure.
    pub fn start(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            self.run_loop().await;
        })
    }

    /// Internal connection loop. Determines mode from endpoint URL scheme.
    async fn run_loop(&self) {
        if !self.config.enabled {
            info!("[shredstream] disabled (SHREDSTREAM_ENDPOINT not set)");
            return;
        }

        let endpoint = match &self.config.endpoint {
            Some(ep) => ep.clone(),
            None => {
                info!("[shredstream] no endpoint configured — disabled");
                return;
            }
        };

        if endpoint.starts_with("wss://") || endpoint.starts_with("ws://") {
            self.run_websocket_loop(&endpoint).await;
        } else {
            // gRPC, HTTP, or any other scheme → fall back to UDP listener
            if endpoint.starts_with("grpc://") || endpoint.starts_with("http://") {
                warn!(
                    "[shredstream] gRPC/HTTP not available in this build, falling back to UDP on port {}",
                    DEFAULT_UDP_PORT
                );
            }
            self.run_udp_loop().await;
        }
    }

    // ── WebSocket mode ──────────────────────────────────────────────

    /// Connect to a WebSocket ShredStream endpoint and process messages.
    ///
    /// NOTE: Jito ShredStream requires their SDK/gRPC proto and a whitelist.
    /// This implementation is structured to work with any WebSocket-based
    /// shred relay that forwards raw transaction/shred bytes as binary messages.
    ///
    /// When Jito WL is not available, this logs a warning and waits for
    /// connection, retrying with exponential backoff. Once the actual
    /// ShredStream relay is available, the parsing logic is ready to go.
    async fn run_websocket_loop(&self, endpoint: &str) {
        use futures_util::StreamExt;
        use tokio_tungstenite::{connect_async, tungstenite::Message};

        let mut backoff_ms = self.config.reconnect_delay_ms;

        info!(
            "[shredstream] WebSocket mode — connecting to {} (Jito ShredStream / compatible relay)",
            endpoint
        );

        loop {
            match connect_async(endpoint).await {
                Ok((ws_stream, _response)) => {
                    info!("[shredstream] WebSocket connected to {}", endpoint);
                    backoff_ms = self.config.reconnect_delay_ms; // reset on success

                    let (_write, mut read) = ws_stream.split();

                    loop {
                        match read.next().await {
                            Some(Ok(Message::Binary(data))) => {
                                self.messages_received.fetch_add(1, Ordering::Relaxed);
                                if let Some(event) = Self::parse_trade(&data) {
                                    self.events_received.fetch_add(1, Ordering::Relaxed);
                                    if self.tx.send(FeedEvent::PreWarm(event)).is_err() {
                                        info!("[shredstream] engine channel closed — exiting");
                                        return;
                                    }
                                }
                            }
                            Some(Ok(Message::Text(text))) => {
                                // Some relays send JSON-wrapped shred data
                                self.messages_received.fetch_add(1, Ordering::Relaxed);
                                if let Some(event) = Self::parse_trade(text.as_bytes()) {
                                    self.events_received.fetch_add(1, Ordering::Relaxed);
                                    if self.tx.send(FeedEvent::PreWarm(event)).is_err() {
                                        info!("[shredstream] engine channel closed — exiting");
                                        return;
                                    }
                                }
                            }
                            Some(Ok(Message::Ping(data))) => {
                                // Auto-respond with pong (tungstenite handles this by default
                                // but we log for diagnostics)
                                debug!("[shredstream] ping received ({} bytes)", data.len());
                            }
                            Some(Ok(Message::Close(frame))) => {
                                warn!("[shredstream] server closed WebSocket: {:?}", frame);
                                break;
                            }
                            Some(Ok(_)) => {} // Pong, Frame — ignore
                            Some(Err(e)) => {
                                error!("[shredstream] WebSocket error: {}", e);
                                break;
                            }
                            None => {
                                warn!("[shredstream] WebSocket stream ended");
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    // Connection failed — this is expected if Jito WL isn't granted yet.
                    // Log at warn (not error) since this is a known optional feed.
                    let reconnects = self.reconnections.fetch_add(1, Ordering::Relaxed);
                    if reconnects == 0 {
                        warn!(
                            "[shredstream] WebSocket connection failed: {} — \
                             ShredStream not yet connected (waiting for Jito WL). \
                             Will retry every {:.1}s (max {:.1}s backoff)",
                            e,
                            backoff_ms as f64 / 1000.0,
                            self.config.max_reconnect_delay_ms as f64 / 1000.0,
                        );
                    } else {
                        debug!(
                            "[shredstream] WebSocket reconnect attempt {} failed: {}",
                            reconnects + 1,
                            e
                        );
                    }
                }
            }

            // Exponential backoff before reconnect
            tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
            backoff_ms = (backoff_ms * 2).min(self.config.max_reconnect_delay_ms);
        }
    }

    // ── UDP mode (original behavior) ────────────────────────────────

    /// Listen for raw shred datagrams on UDP.
    ///
    /// This is the original ShredStream integration mode — a local shred relay
    /// forwards Jito shreds as UDP datagrams to our listen port.
    async fn run_udp_loop(&self) {
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

        loop {
            match socket.recv_from(&mut buf).await {
                Ok((len, _addr)) => {
                    self.messages_received.fetch_add(1, Ordering::Relaxed);
                    let msgs = self.messages_received.load(Ordering::Relaxed);
                    if msgs % 10_000 == 0 {
                        debug!(
                            "[shredstream] datagrams={} events={}",
                            msgs,
                            self.events_received.load(Ordering::Relaxed),
                        );
                    }

                    if let Some(event) = Self::parse_trade(&buf[..len]) {
                        self.events_received.fetch_add(1, Ordering::Relaxed);
                        if self.tx.send(FeedEvent::PreWarm(event)).is_err() {
                            info!("[shredstream] engine channel closed — exiting");
                            return;
                        }
                    }
                }
                Err(e) => {
                    warn!("[shredstream] UDP recv error: {}", e);
                }
            }
        }
    }

    // ── Trade parsing ───────────────────────────────────────────────

    /// Parse raw shred/transaction bytes for a Pump.fun buy/sell trade.
    ///
    /// Scans the payload for Anchor instruction discriminators (buy/sell) and
    /// extracts trade fields from the expected layout:
    ///
    /// ```text
    /// [offset+0..8]   discriminator (buy: 660x3d1201daebea, sell: 33e685a4017f83ad)
    /// [offset+8..40]  mint pubkey (32 bytes)
    /// [offset+40..48] sol_amount (u64 LE, lamports)
    /// ```
    ///
    /// Returns `None` if no Pump.fun trade discriminator is found, or if the
    /// extracted values fail sanity checks.
    pub fn parse_trade(raw: &[u8]) -> Option<PreWarmEvent> {
        if raw.len() < MIN_PAYLOAD_SIZE {
            return None;
        }

        // Scan for discriminator at any offset — shreds contain serialized
        // transaction data at variable offsets within the datagram.
        let max_start = raw.len().saturating_sub(MIN_PAYLOAD_SIZE);
        for offset in 0..=max_start {
            let disc = &raw[offset..offset + 8];

            let is_buy = if disc == BUY_DISCRIMINATOR {
                true
            } else if disc == SELL_DISCRIMINATOR {
                false
            } else {
                continue;
            };

            // Found a discriminator — extract fields
            let mint_start = offset + 8;
            let mint_end = mint_start + 32;
            let sol_start = mint_end;
            let sol_end = sol_start + 8;

            if sol_end > raw.len() {
                continue;
            }

            let mut mint = [0u8; 32];
            mint.copy_from_slice(&raw[mint_start..mint_end]);

            let sol_amount = u64::from_le_bytes(
                raw[sol_start..sol_end].try_into().unwrap(),
            );

            // Sanity: skip obviously invalid amounts (0 or > 10k SOL)
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
}

// ── Backward-compatible free function (used by main.rs) ─────────────

/// Run the ShredStream feed loop. Backward-compatible wrapper around `ShredStreamFeed`.
///
/// Used by main.rs:
/// ```ignore
/// let (shred_tx, shred_rx) = bounded::<FeedEvent>(256);
/// let shred_shutdown_rx = shutdown_rx.clone();
/// tokio::spawn(async move {
///     pump_quant_core::feeds::shredstream::run(shred_tx, shred_shutdown_rx).await;
/// });
/// ```
pub async fn run(tx: Sender<FeedEvent>, _shutdown_rx: tokio::sync::watch::Receiver<bool>) {
    let config = ShredStreamConfig::from_env();
    let feed = Arc::new(ShredStreamFeed::new(config, tx));
    // Run directly in this task (not spawning another) since main.rs already
    // wraps the call in tokio::spawn.
    feed.run_loop().await;
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helper: build a test datagram with discriminator + mint + sol_amount ─

    fn make_test_datagram(discriminator: &[u8; 8], mint: &[u8; 32], sol_lamports: u64) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(discriminator);
        buf.extend_from_slice(mint);
        buf.extend_from_slice(&sol_lamports.to_le_bytes());
        buf
    }

    // ── Requested tests ─────────────────────────────────────────────

    #[test]
    fn test_config_defaults() {
        let config = ShredStreamConfig::default();
        assert!(config.endpoint.is_none());
        assert!(!config.enabled);
        assert_eq!(config.program_filter, PUMP_PROGRAM_ID);
        assert_eq!(config.reconnect_delay_ms, 100);
        assert_eq!(config.max_reconnect_delay_ms, 5000);
    }

    #[test]
    fn test_parse_trade_non_pump_returns_none() {
        // Random bytes with no valid discriminator → None
        let random_bytes: Vec<u8> = (0..128).map(|i| (i * 37 + 13) as u8).collect();
        assert!(ShredStreamFeed::parse_trade(&random_bytes).is_none());

        // Empty bytes
        assert!(ShredStreamFeed::parse_trade(&[]).is_none());

        // Too small
        assert!(ShredStreamFeed::parse_trade(&[0u8; 10]).is_none());

        // Wrong discriminator but correct size
        let mut wrong_disc = vec![0u8; 48];
        wrong_disc[0..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(ShredStreamFeed::parse_trade(&wrong_disc).is_none());
    }

    #[test]
    fn test_shredstream_feed_construction() {
        let (tx, _rx) = crossbeam_channel::bounded::<FeedEvent>(16);
        let config = ShredStreamConfig {
            endpoint: Some("wss://shredstream.jito.wtf".to_string()),
            enabled: true,
            program_filter: PUMP_PROGRAM_ID,
            reconnect_delay_ms: 200,
            max_reconnect_delay_ms: 10_000,
        };

        let feed = ShredStreamFeed::new(config, tx);

        assert_eq!(feed.events_received.load(Ordering::Relaxed), 0);
        assert_eq!(feed.reconnections.load(Ordering::Relaxed), 0);
        assert_eq!(feed.messages_received.load(Ordering::Relaxed), 0);
        assert!(feed.config.enabled);
        assert_eq!(
            feed.config.endpoint.as_deref(),
            Some("wss://shredstream.jito.wtf")
        );
    }

    // ── Original tests (preserved) ──────────────────────────────────

    #[test]
    fn test_parse_buy_discriminator() {
        let mint = [0xAA; 32];
        let sol = 1_000_000_000u64; // 1 SOL
        let data = make_test_datagram(&BUY_DISCRIMINATOR, &mint, sol);

        let event = ShredStreamFeed::parse_trade(&data).expect("should parse buy");
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

        let event = ShredStreamFeed::parse_trade(&data).expect("should parse sell");
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

        let event = ShredStreamFeed::parse_trade(&data).expect("should parse with prefix");
        assert!(event.is_buy);
        assert_eq!(event.mint, mint);
        assert_eq!(event.sol_amount, sol);
    }

    #[test]
    fn test_reject_too_small() {
        let data = [0u8; 10];
        assert!(ShredStreamFeed::parse_trade(&data).is_none());
    }

    #[test]
    fn test_reject_zero_amount() {
        let mint = [0xDD; 32];
        let data = make_test_datagram(&BUY_DISCRIMINATOR, &mint, 0);
        assert!(ShredStreamFeed::parse_trade(&data).is_none());
    }

    #[test]
    fn test_reject_absurd_amount() {
        let mint = [0xDD; 32];
        let data = make_test_datagram(&BUY_DISCRIMINATOR, &mint, 100_000_000_000_000);
        assert!(ShredStreamFeed::parse_trade(&data).is_none());
    }

    #[test]
    fn test_pump_program_id_bytes() {
        // Verify PUMP_PROGRAM_ID matches the base58 string
        let decoded = bs58::decode("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P")
            .into_vec()
            .expect("valid b58");
        assert_eq!(decoded.len(), 32);
        assert_eq!(&decoded[..], &PUMP_PROGRAM_ID[..]);
    }
}
