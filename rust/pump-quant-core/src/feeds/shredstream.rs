//! ShredStream feed — lowest-latency trade detection.
//!
//! Connects to Jito ShredStream (or compatible endpoint) for raw shred data,
//! ~80-200ms faster than PumpPortal/Helius websocket feeds.
//!
//! Architecture:
//! - `ShredStreamFeed` is the primary struct, holding config + channel sender + stats.
//! - `start()` spawns a tokio task that runs the connection loop with reconnect.
//! - **gRPC mode (PRIMARY):** Subscribes to local shredstream-proxy gRPC, deserializes
//!   `Entry` objects into full `VersionedTransaction`s, parses Pump.fun buy/sell
//!   instructions, and emits `FeedEvent::Trade` with complete fields.
//! - **WebSocket mode:** Processes raw binary shred data via `parse_trade()`.
//! - **UDP mode:** Listens for raw shred datagrams, fallback for legacy setups.
//!
//! Integration:
//! - gRPC mode sends `FeedEvent::Trade` (full TradeEvent with sig, mint, trader, etc.)
//! - WS/UDP modes send `FeedEvent::PreWarm` (partial, discriminator-scanned)
//! - EventJoiner has `shredstream_rx: Option<Receiver<FeedEvent>>` wired up.
//!
//! Compatibility:
//! - `ShredStreamConfig::from_env()` and `run()` are kept for backward compat with main.rs.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crossbeam_channel::Sender;
use solana_sdk::pubkey::Pubkey;
use tokio::net::UdpSocket;
use tracing::{debug, error, info, warn};

use crate::feeds::{FeedEvent, FeedSource, MigrationSource, PreWarmEvent, TradeEvent};

// ── Pump.fun Anchor discriminators ──────────────────────────────────

/// 8-byte Anchor discriminator for pump.fun `buy` instruction.
const BUY_DISCRIMINATOR: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];

/// 8-byte Anchor discriminator for pump.fun `sell` instruction.
const SELL_DISCRIMINATOR: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];

/// 8-byte Anchor discriminator for pump.fun `migrate` instruction.
/// This fires when a token reaches ~85 SOL on the bonding curve and
/// graduates to Raydium AMM. Detecting this in ShredStream gives us
/// 80-200ms advantage over websocket-based graduation detection.
const MIGRATE_DISCRIMINATOR: [u8; 8] = [155, 234, 231, 146, 236, 158, 162, 30];

// ── PumpSwap graduation detection ───────────────────────────────────
// Post-March 2025, most pump.fun tokens graduate to PumpSwap (pAMM) instead
// of Raydium. The PumpSwap program's `MigrateFunds` instruction is the
// graduation signal — detectable alongside pump.fun's own `migrate`.

/// PumpSwap AMM program ID bytes (pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA).
const PUMPSWAP_PROGRAM_ID: [u8; 32] = [
    0x0c, 0x14, 0xde, 0xfc, 0x82, 0x5e, 0xc6, 0x76,
    0x94, 0x25, 0x08, 0x18, 0xbb, 0x65, 0x40, 0x65,
    0xf4, 0x29, 0x8d, 0x31, 0x56, 0xd5, 0x71, 0xb4,
    0xd4, 0xf8, 0x09, 0x0c, 0x18, 0xe9, 0xa8, 0x63,
];

/// PumpSwap program ID as a `Pubkey` for comparison in parsed transactions.
const PUMPSWAP_PROGRAM_PUBKEY: Pubkey = Pubkey::new_from_array(PUMPSWAP_PROGRAM_ID);

/// 8-byte Anchor discriminator for PumpSwap `migrate_funds` instruction.
/// SHA256("global:migrate_funds")[..8].
const PUMPSWAP_MIGRATE_DISCRIMINATOR: [u8; 8] = [42, 229, 10, 231, 189, 62, 193, 174];

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

/// Pump.fun program ID as a `Pubkey` for comparison in parsed transactions.
const PUMP_PROGRAM_PUBKEY: Pubkey = Pubkey::new_from_array(PUMP_PROGRAM_ID);

/// Minimum instruction data length for a Pump.fun buy/sell:
/// 8 (discriminator) + 8 (token_amount) + 8 (max_sol_cost / min_sol_output) = 24.
const MIN_PUMP_IX_DATA_LEN: usize = 24;

/// Minimum number of accounts in a Pump.fun buy/sell instruction.
/// accounts[0..6] required: global, feeRecipient, mint, bondingCurve,
/// associatedBondingCurve, associatedUser, user.
const MIN_PUMP_IX_ACCOUNTS: usize = 7;

// ── Minimal gRPC proto types (hand-coded) ───────────────────────────
//
// We hand-code the minimal proto types for the ShredstreamProxy service
// rather than depending on the jito-protos crate (which lives in a separate
// workspace with incompatible tonic/solana version pins).
//
// Proto source: shredstream-proxy/jito_protos/protos/shredstream.proto

mod grpc_proto {
    /// `message SubscribeEntriesRequest {}` — empty request.
    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct SubscribeEntriesRequest {}

    /// `message Entry { uint64 slot = 1; bytes entries = 2; }`
    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct Entry {
        #[prost(uint64, tag = "1")]
        pub slot: u64,
        #[prost(bytes = "vec", tag = "2")]
        pub entries: ::prost::alloc::vec::Vec<u8>,
    }

    /// Generated tonic client for `service ShredstreamProxy`.
    pub mod shredstream_proxy_client {
        use super::{Entry, SubscribeEntriesRequest};

        #[derive(Debug, Clone)]
        pub struct ShredstreamProxyClient<T> {
            inner: tonic::client::Grpc<T>,
        }

        impl ShredstreamProxyClient<tonic::transport::Channel> {
            pub async fn connect<D>(dst: D) -> Result<Self, tonic::transport::Error>
            where
                D: TryInto<tonic::transport::Endpoint>,
                D::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
            {
                let conn = tonic::transport::Endpoint::new(dst)?.connect().await?;
                Ok(Self { inner: tonic::client::Grpc::new(conn) })
            }

            pub async fn subscribe_entries(
                &mut self,
                request: impl tonic::IntoRequest<SubscribeEntriesRequest>,
            ) -> Result<tonic::Response<tonic::Streaming<Entry>>, tonic::Status> {
                self.inner.ready().await.map_err(|e| {
                    tonic::Status::new(
                        tonic::Code::Unknown,
                        format!("Service was not ready: {}", e),
                    )
                })?;
                let codec = tonic::codec::ProstCodec::default();
                let path = http::uri::PathAndQuery::from_static(
                    "/shredstream.ShredstreamProxy/SubscribeEntries",
                );
                let mut req = request.into_request();
                req.extensions_mut().insert(tonic::GrpcMethod::new(
                    "shredstream.ShredstreamProxy",
                    "SubscribeEntries",
                ));
                self.inner.server_streaming(req, path, codec).await
            }
        }
    }
}

// ── ShredStreamConfig ───────────────────────────────────────────────

/// Configuration for the ShredStream feed.
pub struct ShredStreamConfig {
    /// Endpoint URL. Scheme determines mode:
    /// - `grpc://host:port` or `http://host:port` -> gRPC mode (PRIMARY)
    /// - `wss://...` or `ws://...` -> WebSocket mode
    /// - anything else -> UDP on DEFAULT_UDP_PORT
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
pub struct ShredStreamFeed {
    config: ShredStreamConfig,
    tx: Sender<FeedEvent>,
    /// Total events successfully parsed and sent.
    pub events_received: AtomicU64,
    /// Number of reconnection attempts.
    pub reconnections: AtomicU64,
    /// Total raw datagrams/messages received (including non-trade).
    pub messages_received: AtomicU64,
    /// Total gRPC entries received (slot-level).
    pub grpc_entries_received: AtomicU64,
    /// Total transactions scanned in gRPC mode.
    pub grpc_txns_scanned: AtomicU64,
}

impl ShredStreamFeed {
    pub fn new(config: ShredStreamConfig, tx: Sender<FeedEvent>) -> Self {
        Self {
            config,
            tx,
            events_received: AtomicU64::new(0),
            reconnections: AtomicU64::new(0),
            messages_received: AtomicU64::new(0),
            grpc_entries_received: AtomicU64::new(0),
            grpc_txns_scanned: AtomicU64::new(0),
        }
    }

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

        if endpoint.starts_with("grpc://") || endpoint.starts_with("http://") {
            self.run_grpc_loop(&endpoint).await;
        } else if endpoint.starts_with("wss://") || endpoint.starts_with("ws://") {
            self.run_websocket_loop(&endpoint).await;
        } else {
            self.run_udp_loop().await;
        }
    }

    // ── gRPC mode (PRIMARY) ─────────────────────────────────────────

    async fn run_grpc_loop(&self, endpoint: &str) {
        use grpc_proto::shredstream_proxy_client::ShredstreamProxyClient;
        use grpc_proto::SubscribeEntriesRequest;

        // Normalize: grpc:// -> http:// for tonic transport
        let grpc_url = if let Some(rest) = endpoint.strip_prefix("grpc://") {
            format!("http://{}", rest)
        } else {
            endpoint.to_string()
        };

        let mut backoff_ms = self.config.reconnect_delay_ms;

        info!(
            "[shredstream] gRPC mode — connecting to {} (decoded entries, full tx parsing)",
            grpc_url
        );

        loop {
            match ShredstreamProxyClient::connect(grpc_url.clone()).await {
                Ok(mut client) => {
                    info!("[shredstream] gRPC connected to {}", grpc_url);
                    backoff_ms = self.config.reconnect_delay_ms;

                    match client
                        .subscribe_entries(SubscribeEntriesRequest {})
                        .await
                    {
                        Ok(response) => {
                            let mut stream = response.into_inner();

                            loop {
                                match stream.message().await {
                                    Ok(Some(slot_entry)) => {
                                        self.grpc_entries_received
                                            .fetch_add(1, Ordering::Relaxed);

                                        let now_ms = std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_millis()
                                            as u64;

                                        if self.process_grpc_entry(
                                            slot_entry.slot,
                                            &slot_entry.entries,
                                            now_ms,
                                        ) {
                                            return; // channel closed
                                        }
                                    }
                                    Ok(None) => {
                                        warn!(
                                            "[shredstream] gRPC stream ended (server closed)"
                                        );
                                        break;
                                    }
                                    Err(e) => {
                                        error!("[shredstream] gRPC stream error: {}", e);
                                        break;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            error!(
                                "[shredstream] gRPC subscribe_entries failed: {}",
                                e
                            );
                        }
                    }
                }
                Err(e) => {
                    let reconnects = self.reconnections.fetch_add(1, Ordering::Relaxed);
                    if reconnects == 0 {
                        warn!(
                            "[shredstream] gRPC connection failed: {} — will retry \
                             (initial {}ms, max {}ms backoff)",
                            e, backoff_ms, self.config.max_reconnect_delay_ms
                        );
                    } else {
                        debug!(
                            "[shredstream] gRPC reconnect attempt {} failed: {}",
                            reconnects + 1,
                            e
                        );
                    }
                }
            }

            tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
            backoff_ms = (backoff_ms * 2).min(self.config.max_reconnect_delay_ms);
        }
    }

    /// Process a single gRPC Entry. Returns `true` if the channel is closed
    /// (caller should exit).
    #[inline]
    fn process_grpc_entry(&self, slot: u64, entries_bytes: &[u8], now_ms: u64) -> bool {
        let entries: Vec<solana_entry::entry::Entry> = match bincode::deserialize(entries_bytes) {
            Ok(e) => e,
            Err(e) => {
                debug!(
                    "[shredstream] bincode deserialize failed slot {}: {}",
                    slot, e
                );
                return false;
            }
        };

        let entry_count = self.grpc_entries_received.load(Ordering::Relaxed);
        if entry_count % 5000 == 0 && entry_count > 0 {
            info!(
                "[shredstream] gRPC stats: entries={} txns={} trades={}",
                entry_count,
                self.grpc_txns_scanned.load(Ordering::Relaxed),
                self.events_received.load(Ordering::Relaxed),
            );
        }

        for entry in &entries {
            for tx in &entry.transactions {
                self.grpc_txns_scanned.fetch_add(1, Ordering::Relaxed);

                if let Some(trade) = parse_pump_transaction(tx, slot, now_ms) {
                    self.events_received.fetch_add(1, Ordering::Relaxed);
                    if self.tx.send(FeedEvent::Trade(trade)).is_err() {
                        info!("[shredstream] engine channel closed — exiting gRPC loop");
                        return true;
                    }
                } else if let Some(migration) = parse_pump_migration(tx, slot, now_ms) {
                    self.events_received.fetch_add(1, Ordering::Relaxed);
                    if self.tx.send(migration).is_err() {
                        info!("[shredstream] engine channel closed — exiting gRPC loop");
                        return true;
                    }
                } else if let Some(migration) = parse_pumpswap_migration(tx, now_ms) {
                    self.events_received.fetch_add(1, Ordering::Relaxed);
                    if self.tx.send(migration).is_err() {
                        info!("[shredstream] engine channel closed — exiting gRPC loop");
                        return true;
                    }
                }
            }
        }

        false
    }

    // ── WebSocket mode ──────────────────────────────────────────────

    async fn run_websocket_loop(&self, endpoint: &str) {
        use futures_util::StreamExt;
        use tokio_tungstenite::{connect_async, tungstenite::Message};

        let mut backoff_ms = self.config.reconnect_delay_ms;

        info!(
            "[shredstream] WebSocket mode — connecting to {}",
            endpoint
        );

        loop {
            match connect_async(endpoint).await {
                Ok((ws_stream, _response)) => {
                    info!("[shredstream] WebSocket connected to {}", endpoint);
                    backoff_ms = self.config.reconnect_delay_ms;

                    let (_write, mut read) = ws_stream.split();

                    loop {
                        match read.next().await {
                            Some(Ok(Message::Binary(data))) => {
                                self.messages_received.fetch_add(1, Ordering::Relaxed);
                                if let Some(event) = Self::parse_trade(&data) {
                                    self.events_received.fetch_add(1, Ordering::Relaxed);
                                    if self.tx.send(FeedEvent::PreWarm(event)).is_err() {
                                        info!("[shredstream] channel closed — exiting");
                                        return;
                                    }
                                }
                            }
                            Some(Ok(Message::Text(text))) => {
                                self.messages_received.fetch_add(1, Ordering::Relaxed);
                                if let Some(event) = Self::parse_trade(text.as_bytes()) {
                                    self.events_received.fetch_add(1, Ordering::Relaxed);
                                    if self.tx.send(FeedEvent::PreWarm(event)).is_err() {
                                        info!("[shredstream] channel closed — exiting");
                                        return;
                                    }
                                }
                            }
                            Some(Ok(Message::Ping(data))) => {
                                debug!("[shredstream] ping ({} bytes)", data.len());
                            }
                            Some(Ok(Message::Close(frame))) => {
                                warn!("[shredstream] server closed WS: {:?}", frame);
                                break;
                            }
                            Some(Ok(_)) => {}
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
                    let reconnects = self.reconnections.fetch_add(1, Ordering::Relaxed);
                    if reconnects == 0 {
                        warn!(
                            "[shredstream] WS connection failed: {} — retrying \
                             ({:.1}s init, {:.1}s max)",
                            e,
                            backoff_ms as f64 / 1000.0,
                            self.config.max_reconnect_delay_ms as f64 / 1000.0,
                        );
                    } else {
                        debug!(
                            "[shredstream] WS reconnect {} failed: {}",
                            reconnects + 1,
                            e
                        );
                    }
                }
            }

            tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
            backoff_ms = (backoff_ms * 2).min(self.config.max_reconnect_delay_ms);
        }
    }

    // ── UDP mode ────────────────────────────────────────────────────

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

        let mut buf = [0u8; 65536];

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
                            info!("[shredstream] channel closed — exiting UDP");
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

    // ── Legacy raw shred parser ─────────────────────────────────────

    /// Parse raw shred/transaction bytes for a Pump.fun buy/sell trade.
    /// Scans for Anchor discriminators at any offset.
    pub fn parse_trade(raw: &[u8]) -> Option<PreWarmEvent> {
        if raw.len() < MIN_PAYLOAD_SIZE {
            return None;
        }

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

            let mint_start = offset + 8;
            let mint_end = mint_start + 32;
            let sol_start = mint_end;
            let sol_end = sol_start + 8;

            if sol_end > raw.len() {
                continue;
            }

            let mut mint = [0u8; 32];
            mint.copy_from_slice(&raw[mint_start..mint_end]);

            let sol_amount =
                u64::from_le_bytes(raw[sol_start..sol_end].try_into().unwrap());

            // Sanity: skip 0 or > 10k SOL
            if sol_amount == 0 || sol_amount > 10_000_000_000_000 {
                continue;
            }

            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            return Some(PreWarmEvent {
                mint,
                trader: [0u8; 32],
                sig: [0u8; 64],
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

pub async fn run(tx: Sender<FeedEvent>, _shutdown_rx: tokio::sync::watch::Receiver<bool>) {
    let config = ShredStreamConfig::from_env();
    let feed = Arc::new(ShredStreamFeed::new(config, tx));
    feed.run_loop().await;
}

// ── Pump.fun transaction parser (gRPC mode) ─────────────────────────

/// Parse a Pump.fun buy/sell from a decoded Solana `VersionedTransaction`.
/// Returns `None` if not a Pump.fun trade.
///
/// # Performance
/// - `#[inline(always)]` — per-transaction in the gRPC hot loop
/// - Zero heap allocation — stack-only byte arrays
/// - No f64 anywhere
///
/// # Pump.fun account layout (buy instruction):
/// ```text
/// ix.accounts[0]  = global config
/// ix.accounts[1]  = feeRecipient
/// ix.accounts[2]  = mint
/// ix.accounts[3]  = bondingCurve
/// ix.accounts[4]  = associatedBondingCurve
/// ix.accounts[5]  = associatedUser (trader ATA)
/// ix.accounts[6]  = user (signer/trader)
/// ix.accounts[7+] = system, token, rent, eventAuth, program
/// ```
///
/// Instruction data: `[0..8]` discriminator, `[8..16]` token_amount (u64 LE),
/// `[16..24]` max_sol_cost (buy) or min_sol_output (sell) (u64 LE).
#[inline(always)]
fn parse_pump_transaction(
    tx: &solana_sdk::transaction::VersionedTransaction,
    slot: u64,
    now_ms: u64,
) -> Option<TradeEvent> {
    // Get static account keys from the message
    let account_keys = tx.message.static_account_keys();

    // Find the Pump.fun program instruction
    let instructions = tx.message.instructions();

    for ix in instructions {
        let program_id_index = ix.program_id_index as usize;
        if program_id_index >= account_keys.len() {
            continue;
        }

        // Fast-path: compare program ID
        if account_keys[program_id_index] != PUMP_PROGRAM_PUBKEY {
            continue;
        }

        // Check minimum data length
        if ix.data.len() < MIN_PUMP_IX_DATA_LEN {
            continue;
        }

        // Check discriminator
        let disc: &[u8] = &ix.data[..8];
        let is_buy = if disc == BUY_DISCRIMINATOR {
            true
        } else if disc == SELL_DISCRIMINATOR {
            false
        } else {
            continue;
        };

        // Check minimum accounts
        if ix.accounts.len() < MIN_PUMP_IX_ACCOUNTS {
            continue;
        }

        // Extract account indices -> resolve to pubkeys
        let mint_idx = ix.accounts[2] as usize;
        let bonding_curve_idx = ix.accounts[3] as usize;
        let assoc_bonding_curve_idx = ix.accounts[4] as usize;
        let trader_idx = ix.accounts[6] as usize;

        // Bounds check all indices
        let max_idx = account_keys.len();
        if mint_idx >= max_idx
            || bonding_curve_idx >= max_idx
            || assoc_bonding_curve_idx >= max_idx
            || trader_idx >= max_idx
        {
            continue;
        }

        let mint_key = &account_keys[mint_idx];
        let bonding_curve_key = &account_keys[bonding_curve_idx];
        let assoc_bonding_curve_key = &account_keys[assoc_bonding_curve_idx];
        let trader_key = &account_keys[trader_idx];

        // Extract amounts from instruction data: [8..16] = token_amount, [16..24] = sol param
        let token_amount = u64::from_le_bytes(
            ix.data[8..16].try_into().ok()?,
        );
        let sol_amount = u64::from_le_bytes(
            ix.data[16..24].try_into().ok()?,
        );

        // Sanity: skip zero amounts or absurdly large (> 10k SOL)
        if sol_amount == 0 || sol_amount > 10_000_000_000_000 {
            continue;
        }
        if token_amount == 0 {
            continue;
        }

        // Extract signature (first signature is always the fee payer's)
        let sig_bytes: [u8; 64] = if !tx.signatures.is_empty() {
            tx.signatures[0].into()
        } else {
            continue; // no signatures = invalid tx
        };

        // Build sig_prefix (first 8 bytes for dedup)
        let mut sig_prefix = [0u8; 8];
        sig_prefix.copy_from_slice(&sig_bytes[..8]);

        return Some(TradeEvent {
            mint: mint_key.to_bytes(),
            trader: trader_key.to_bytes(),
            sig: sig_bytes,
            sig_prefix,
            sol_amount,
            token_amount,
            vsol_reserves: 0,   // not available from instruction data
            vtoken_reserves: 0,  // not available from instruction data
            market_cap_sol: 0,   // not available from instruction data
            slot,
            timestamp_ms: now_ms,
            is_buy,
            source: FeedSource::ShredStream,
            bonding_curve: bonding_curve_key.to_bytes(),
            assoc_bonding_curve: assoc_bonding_curve_key.to_bytes(),
        });
    }

    None
}

/// Parse a pump.fun MIGRATE instruction from a decoded Solana transaction.
///
/// The migrate instruction fires when a token reaches ~85 SOL on the bonding
/// curve and graduates to Raydium AMM. Detecting this in ShredStream gives us
/// 80-200ms advantage over websocket-based graduation detection (Helius, CoreCast).
///
/// Migrate instruction account layout:
/// ```text
/// accounts[0] = mint
/// accounts[1] = bonding curve
/// accounts[2..] = migration-specific accounts (Raydium pool creation, etc.)
/// ```
///
/// Returns `FeedEvent::Migration` with mint + full signature for pool resolution.
#[inline(always)]
fn parse_pump_migration(
    tx: &solana_sdk::transaction::VersionedTransaction,
    _slot: u64,
    now_ms: u64,
) -> Option<FeedEvent> {
    let account_keys = tx.message.static_account_keys();
    let instructions = tx.message.instructions();

    for ix in instructions {
        let program_id_index = ix.program_id_index as usize;
        if program_id_index >= account_keys.len() {
            continue;
        }

        // Fast-path: must be pump.fun program
        if account_keys[program_id_index] != PUMP_PROGRAM_PUBKEY {
            continue;
        }

        // Check discriminator — must be exactly MIGRATE
        if ix.data.len() < 8 {
            continue;
        }
        if ix.data[..8] != MIGRATE_DISCRIMINATOR {
            continue;
        }

        // Extract mint from accounts[0] (first account in migrate instruction)
        if ix.accounts.is_empty() {
            continue;
        }
        let mint_idx = ix.accounts[0] as usize;
        if mint_idx >= account_keys.len() {
            continue;
        }
        let mint = account_keys[mint_idx].to_bytes();

        // Extract full signature for pool resolution RPC calls
        let sig: [u8; 64] = if !tx.signatures.is_empty() {
            tx.signatures[0].into()
        } else {
            continue;
        };

        return Some(FeedEvent::Migration {
            mint,
            ts_ms: now_ms,
            source: MigrationSource::ShredStream,
            sig,
        });
    }

    None
}

/// Parse a PumpSwap `MigrateFunds` instruction from a decoded Solana transaction.
///
/// Post-March 2025, pump.fun tokens increasingly graduate to PumpSwap (pAMM)
/// instead of Raydium. This function detects PumpSwap's `migrate_funds` instruction
/// as a backup graduation signal alongside pump.fun's own `migrate` discriminator.
///
/// PumpSwap MigrateFunds account layout (Anchor IDL):
/// ```text
/// accounts[0] = pool (new PumpSwap pool being created)
/// accounts[1] = bondingCurve
/// accounts[2] = mint
/// accounts[3..] = various vaults, authority, token programs
/// ```
///
/// The mint is extracted from accounts[2]. Returns `FeedEvent::Migration` with
/// the full tx signature for downstream pool resolution via `getTransaction`.
///
/// Note: `mint` may be [0u8; 32] if extraction fails — pool resolution will
/// resolve the real mint from `postTokenBalances`, same as Helius feed does.
#[inline(always)]
fn parse_pumpswap_migration(
    tx: &solana_sdk::transaction::VersionedTransaction,
    now_ms: u64,
) -> Option<FeedEvent> {
    let account_keys = tx.message.static_account_keys();
    let instructions = tx.message.instructions();

    for ix in instructions {
        let program_id_index = ix.program_id_index as usize;
        if program_id_index >= account_keys.len() {
            continue;
        }

        // Fast-path: must be PumpSwap program
        if account_keys[program_id_index] != PUMPSWAP_PROGRAM_PUBKEY {
            continue;
        }

        // Check discriminator — must be MigrateFunds
        if ix.data.len() < 8 {
            continue;
        }
        if ix.data[..8] != PUMPSWAP_MIGRATE_DISCRIMINATOR {
            continue;
        }

        // Extract mint from accounts[2] (third account in MigrateFunds instruction)
        let mint = if ix.accounts.len() > 2 {
            let mint_idx = ix.accounts[2] as usize;
            if mint_idx < account_keys.len() {
                account_keys[mint_idx].to_bytes()
            } else {
                [0u8; 32] // fallback: pool resolution will find the real mint
            }
        } else {
            [0u8; 32] // fallback: pool resolution will find the real mint
        };

        // Extract full signature for pool resolution RPC calls
        let sig: [u8; 64] = if !tx.signatures.is_empty() {
            tx.signatures[0].into()
        } else {
            continue;
        };

        return Some(FeedEvent::Migration {
            mint,
            ts_ms: now_ms,
            source: MigrationSource::ShredStream,
            sig,
        });
    }

    None
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::hash::Hash;
    use solana_sdk::instruction::CompiledInstruction;
    use solana_sdk::message::{self, Message, MessageHeader};
    use solana_sdk::pubkey::Pubkey;
    use solana_sdk::signature::Signature;
    use solana_sdk::transaction::VersionedTransaction;

    // ── Helpers ─────────────────────────────────────────────────────

    fn make_test_datagram(discriminator: &[u8; 8], mint: &[u8; 32], sol_lamports: u64) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(discriminator);
        buf.extend_from_slice(mint);
        buf.extend_from_slice(&sol_lamports.to_le_bytes());
        buf
    }

    /// Build a mock VersionedTransaction with a single Pump.fun instruction.
    ///
    /// Account layout follows the Pump.fun buy/sell pattern:
    /// [0]=global, [1]=feeRecipient, [2]=mint, [3]=bondingCurve,
    /// [4]=assocBondingCurve, [5]=assocUser(traderATA), [6]=user(trader),
    /// [7]=systemProgram, [8]=tokenProgram, [9]=rent, [10]=eventAuth,
    /// [11]=pumpProgram
    fn make_pump_tx(
        discriminator: &[u8; 8],
        mint: Pubkey,
        bonding_curve: Pubkey,
        assoc_bonding_curve: Pubkey,
        trader: Pubkey,
        token_amount: u64,
        sol_amount: u64,
        signature: Signature,
    ) -> VersionedTransaction {
        let global = Pubkey::new_unique();
        let fee_recipient = Pubkey::new_unique();
        let assoc_user = Pubkey::new_unique(); // trader ATA
        let system_program = solana_sdk::system_program::id();
        let token_program = Pubkey::new_unique();
        let rent = solana_sdk::sysvar::rent::id();
        let event_authority = Pubkey::new_unique();
        let pump_program = PUMP_PROGRAM_PUBKEY;

        // account_keys: indices 0..11
        let account_keys = vec![
            global,               // 0
            fee_recipient,        // 1
            mint,                 // 2
            bonding_curve,        // 3
            assoc_bonding_curve,  // 4
            assoc_user,           // 5
            trader,               // 6
            system_program,       // 7
            token_program,        // 8
            rent,                 // 9
            event_authority,      // 10
            pump_program,         // 11 (program_id_index)
        ];

        // Build instruction data: discriminator + token_amount + sol_amount
        let mut ix_data = Vec::with_capacity(24);
        ix_data.extend_from_slice(discriminator);
        ix_data.extend_from_slice(&token_amount.to_le_bytes());
        ix_data.extend_from_slice(&sol_amount.to_le_bytes());

        let ix = CompiledInstruction {
            program_id_index: 11, // pump_program
            accounts: vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
            data: ix_data,
        };

        let header = MessageHeader {
            num_required_signatures: 1,
            num_readonly_signed_accounts: 0,
            num_readonly_unsigned_accounts: 5,
        };

        let message = Message {
            header,
            account_keys,
            recent_blockhash: Hash::default(),
            instructions: vec![ix],
        };

        VersionedTransaction {
            signatures: vec![signature],
            message: message::VersionedMessage::Legacy(message),
        }
    }

    // ── gRPC transaction parser tests ───────────────────────────────

    #[test]
    fn test_parse_pump_buy_transaction() {
        let mint = Pubkey::new_unique();
        let bonding_curve = Pubkey::new_unique();
        let assoc_bonding_curve = Pubkey::new_unique();
        let trader = Pubkey::new_unique();
        let token_amount: u64 = 1_000_000_000; // 1B tokens
        let sol_amount: u64 = 500_000_000; // 0.5 SOL
        let sig = Signature::new_unique();

        let tx = make_pump_tx(
            &BUY_DISCRIMINATOR,
            mint,
            bonding_curve,
            assoc_bonding_curve,
            trader,
            token_amount,
            sol_amount,
            sig,
        );

        let result = parse_pump_transaction(&tx, 42, 1234567890);
        let trade = result.expect("should parse pump buy");

        assert!(trade.is_buy);
        assert_eq!(trade.mint, mint.to_bytes());
        assert_eq!(trade.trader, trader.to_bytes());
        assert_eq!(trade.bonding_curve, bonding_curve.to_bytes());
        assert_eq!(trade.assoc_bonding_curve, assoc_bonding_curve.to_bytes());
        assert_eq!(trade.token_amount, token_amount);
        assert_eq!(trade.sol_amount, sol_amount);
        assert_eq!(trade.slot, 42);
        assert_eq!(trade.timestamp_ms, 1234567890);
        assert_eq!(trade.source, FeedSource::ShredStream);
        assert_eq!(trade.vsol_reserves, 0);
        assert_eq!(trade.vtoken_reserves, 0);
        assert_eq!(trade.market_cap_sol, 0);
        // Verify signature
        let sig_bytes: [u8; 64] = sig.into();
        assert_eq!(trade.sig, sig_bytes);
        assert_eq!(trade.sig_prefix, sig_bytes[..8]);
    }

    #[test]
    fn test_parse_pump_sell_transaction() {
        let mint = Pubkey::new_unique();
        let bonding_curve = Pubkey::new_unique();
        let assoc_bonding_curve = Pubkey::new_unique();
        let trader = Pubkey::new_unique();
        let token_amount: u64 = 2_000_000_000;
        let sol_amount: u64 = 300_000_000; // min_sol_output
        let sig = Signature::new_unique();

        let tx = make_pump_tx(
            &SELL_DISCRIMINATOR,
            mint,
            bonding_curve,
            assoc_bonding_curve,
            trader,
            token_amount,
            sol_amount,
            sig,
        );

        let result = parse_pump_transaction(&tx, 100, 9999999);
        let trade = result.expect("should parse pump sell");

        assert!(!trade.is_buy);
        assert_eq!(trade.mint, mint.to_bytes());
        assert_eq!(trade.trader, trader.to_bytes());
        assert_eq!(trade.token_amount, token_amount);
        assert_eq!(trade.sol_amount, sol_amount);
        assert_eq!(trade.slot, 100);
    }

    #[test]
    fn test_parse_non_pump_transaction_returns_none() {
        // Transaction with a non-Pump.fun program
        let random_program = Pubkey::new_unique();

        let account_keys = vec![
            Pubkey::new_unique(), // 0
            random_program,       // 1 (program)
        ];

        let ix = CompiledInstruction {
            program_id_index: 1,
            accounts: vec![0],
            data: vec![0u8; 32],
        };

        let header = MessageHeader {
            num_required_signatures: 1,
            num_readonly_signed_accounts: 0,
            num_readonly_unsigned_accounts: 1,
        };

        let message = Message {
            header,
            account_keys,
            recent_blockhash: Hash::default(),
            instructions: vec![ix],
        };

        let tx = VersionedTransaction {
            signatures: vec![Signature::new_unique()],
            message: message::VersionedMessage::Legacy(message),
        };

        assert!(parse_pump_transaction(&tx, 1, 1000).is_none());
    }

    #[test]
    fn test_parse_empty_transaction_returns_none() {
        // Transaction with no instructions
        let account_keys = vec![Pubkey::new_unique()];

        let header = MessageHeader {
            num_required_signatures: 1,
            num_readonly_signed_accounts: 0,
            num_readonly_unsigned_accounts: 0,
        };

        let message = Message {
            header,
            account_keys,
            recent_blockhash: Hash::default(),
            instructions: vec![],
        };

        let tx = VersionedTransaction {
            signatures: vec![Signature::new_unique()],
            message: message::VersionedMessage::Legacy(message),
        };

        assert!(parse_pump_transaction(&tx, 1, 1000).is_none());
    }

    #[test]
    fn test_parse_pump_tx_too_few_accounts() {
        // Pump program instruction but only 3 accounts (need >= 7)
        let account_keys = vec![
            Pubkey::new_unique(),  // 0
            Pubkey::new_unique(),  // 1
            Pubkey::new_unique(),  // 2
            PUMP_PROGRAM_PUBKEY,   // 3 (program)
        ];

        let mut ix_data = Vec::with_capacity(24);
        ix_data.extend_from_slice(&BUY_DISCRIMINATOR);
        ix_data.extend_from_slice(&1000u64.to_le_bytes());
        ix_data.extend_from_slice(&2000u64.to_le_bytes());

        let ix = CompiledInstruction {
            program_id_index: 3,
            accounts: vec![0, 1, 2], // only 3 accounts
            data: ix_data,
        };

        let header = MessageHeader {
            num_required_signatures: 1,
            num_readonly_signed_accounts: 0,
            num_readonly_unsigned_accounts: 3,
        };

        let message = Message {
            header,
            account_keys,
            recent_blockhash: Hash::default(),
            instructions: vec![ix],
        };

        let tx = VersionedTransaction {
            signatures: vec![Signature::new_unique()],
            message: message::VersionedMessage::Legacy(message),
        };

        assert!(parse_pump_transaction(&tx, 1, 1000).is_none());
    }

    #[test]
    fn test_parse_pump_tx_zero_sol_rejected() {
        let tx = make_pump_tx(
            &BUY_DISCRIMINATOR,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            1_000_000, // token_amount
            0,         // sol_amount = 0 → rejected
            Signature::new_unique(),
        );

        assert!(parse_pump_transaction(&tx, 1, 1000).is_none());
    }

    #[test]
    fn test_parse_pump_tx_absurd_sol_rejected() {
        let tx = make_pump_tx(
            &BUY_DISCRIMINATOR,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            1_000_000,
            100_000_000_000_000, // > 10k SOL → rejected
            Signature::new_unique(),
        );

        assert!(parse_pump_transaction(&tx, 1, 1000).is_none());
    }

    #[test]
    fn test_parse_pump_tx_wrong_discriminator() {
        // Pump program instruction but with unknown discriminator
        let account_keys = vec![
            Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique(),
            Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique(),
            Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique(),
            Pubkey::new_unique(), Pubkey::new_unique(), PUMP_PROGRAM_PUBKEY,
        ];

        let mut ix_data = Vec::with_capacity(24);
        ix_data.extend_from_slice(&[0xFF; 8]); // bogus discriminator
        ix_data.extend_from_slice(&1000u64.to_le_bytes());
        ix_data.extend_from_slice(&2000u64.to_le_bytes());

        let ix = CompiledInstruction {
            program_id_index: 11,
            accounts: vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
            data: ix_data,
        };

        let header = MessageHeader {
            num_required_signatures: 1,
            num_readonly_signed_accounts: 0,
            num_readonly_unsigned_accounts: 5,
        };

        let message = Message {
            header,
            account_keys,
            recent_blockhash: Hash::default(),
            instructions: vec![ix],
        };

        let tx = VersionedTransaction {
            signatures: vec![Signature::new_unique()],
            message: message::VersionedMessage::Legacy(message),
        };

        assert!(parse_pump_transaction(&tx, 1, 1000).is_none());
    }

    // ── Legacy raw shred parser tests (preserved) ───────────────────

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
        let random_bytes: Vec<u8> = (0..128).map(|i| (i * 37 + 13) as u8).collect();
        assert!(ShredStreamFeed::parse_trade(&random_bytes).is_none());
        assert!(ShredStreamFeed::parse_trade(&[]).is_none());
        assert!(ShredStreamFeed::parse_trade(&[0u8; 10]).is_none());

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

    #[test]
    fn test_parse_buy_discriminator() {
        let mint = [0xAA; 32];
        let sol = 1_000_000_000u64;
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
        let sol = 500_000_000u64;
        let data = make_test_datagram(&SELL_DISCRIMINATOR, &mint, sol);

        let event = ShredStreamFeed::parse_trade(&data).expect("should parse sell");
        assert!(!event.is_buy);
        assert_eq!(event.mint, mint);
        assert_eq!(event.sol_amount, sol);
    }

    #[test]
    fn test_parse_with_prefix_bytes() {
        let mint = [0xCC; 32];
        let sol = 2_000_000_000u64;
        let mut data = vec![0xFF; 16];
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
        let decoded = bs58::decode("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P")
            .into_vec()
            .expect("valid b58");
        assert_eq!(decoded.len(), 32);
        assert_eq!(&decoded[..], &PUMP_PROGRAM_ID[..]);
    }
}