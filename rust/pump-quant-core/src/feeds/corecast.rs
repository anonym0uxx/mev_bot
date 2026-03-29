//! CoreCast / Bitquery WebSocket feed — multi-stream subscriptions.
//!
//! Connects to the Bitquery streaming GraphQL endpoint and subscribes to
//! multiple streams on a single WebSocket connection using the graphql-ws
//! protocol's multiplexed subscription IDs.
//!
//! Streams (all on 1 WS connection = 1 stream toward Bitquery's 5-stream cap):
//!   ID "1" — pump.fun DEX trades (creator sell detection)
//!   ID "2" — Raydium AMM trades (migration detection → force-exit)
//!   ID "3" — Token supply updates (LP removal / rug detection → force-exit)
//!
//! TASK-10: Uses a shared `Arc<RwLock<HashMap<[u8;32],[u8;32]>>>` (mint → creator)
//! populated by PumpPortal on token creation events.
//!
//! Requires `BITQUERY_API_KEY` env var. If not set, gracefully disables.
//!
//! Protocol: GraphQL over WebSocket (graphql-ws protocol).
//! Endpoint: wss://streaming.bitquery.io/eap (or /graphql)
//! Auth: Bearer token via connection_init payload.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use crossbeam_channel::Sender;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async_with_config, tungstenite};
use tracing::{debug, error, info, warn};

use super::{FeedEvent, MigrationSource};

/// Shared creator map type: mint → creator wallet pubkey.
/// Written by PumpPortal on `create` events, read by CoreCast for signer matching.
///
/// TODO(perf): Bloom filter pre-check for fast-reject on non-creator trades.
/// Benchmark with `criterion` first — at 10-30k active tokens, the HashMap may
/// already be hot in L1/L2 cache, making bloom overhead (branch) counterproductive.
/// Expected savings: ~35-65ns per non-creator trade on the CoreCast path (NOT
/// the critical backrun hot path). See OPTIMIZATION_NOTES.md for details.
pub type CreatorMap = Arc<RwLock<HashMap<[u8; 32], [u8; 32]>>>;

const BITQUERY_WS_URL: &str = "wss://streaming.bitquery.io/eap";
#[allow(dead_code)] // Referenced in GraphQL subscription strings as string literals.
const PUMP_PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
#[allow(dead_code)] // Referenced in GraphQL subscription strings as string literals.
const RAYDIUM_AMM_PROGRAM_ID: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
const MAX_BACKOFF_SECS: u64 = 30;

// ── Subscription ID constants ───────────────────────────────────────
const SUB_ID_BONDING_TRADES: &str = "1";
const SUB_ID_AMM_MIGRATION: &str = "2";
const SUB_ID_LP_REMOVAL: &str = "3";

// ── Graduation noise filters ────────────────────────────────────────
//
// Perf-critical path: every Raydium AMM event hits these filters.
// Optimized for AMD EPYC Zen 4: L1-resident ring buffer, byte-level
// comparisons, cheapest-first fail-fast ordering, cold rejection paths.

/// Wrapped SOL mint as base58 bytes (compile-time).
/// `So11111111111111111111111111111111111111112` — 44 bytes.
const WSOL_MINT_B58: &[u8] = b"So11111111111111111111111111111111111111112";

/// Grace period after startup to ignore Bitquery historical event replay.
const STARTUP_REPLAY_WINDOW_MS: u64 = 10_000;

/// Ring buffer capacity for graduation sig dedup. Must be power of 2.
/// 64 slots × 40 bytes = 2560 bytes — fits comfortably in L1 cache (32 KB).
const GRAD_DEDUP_SLOTS: usize = 64;
const GRAD_DEDUP_MASK: usize = GRAD_DEDUP_SLOTS - 1;

/// Fixed-size ring buffer dedup for graduation tx signatures.
/// Replaces DashMap — zero heap allocation, L1-cache-resident.
/// 64 slots × (8 + 32) bytes = 2560 bytes total.
struct GraduationDedup {
    entries: [(u64, [u8; 32]); GRAD_DEDUP_SLOTS],
    next: usize,
    ttl_ms: u64,
}

impl GraduationDedup {
    const fn new(ttl_ms: u64) -> Self {
        Self {
            entries: [(0, [0u8; 32]); GRAD_DEDUP_SLOTS],
            next: 0,
            ttl_ms,
        }
    }

    /// Returns `true` if `sig` is NEW (not seen within TTL window).
    /// O(64) linear scan — all data in L1 cache, ~192 cycles worst case.
    /// DashMap equivalent: ~500-2000 cycles (hash + lock + heap chase).
    #[inline(always)]
    fn is_new(&mut self, sig: &[u8; 32], now_ms: u64) -> bool {
        let cutoff = now_ms.saturating_sub(self.ttl_ms);
        for (ts, stored) in &self.entries {
            if *ts >= cutoff && stored == sig {
                return false;
            }
        }
        self.entries[self.next] = (now_ms, *sig);
        self.next = (self.next + 1) & GRAD_DEDUP_MASK;
        true
    }

    /// Number of live (non-expired) entries — for stats logging only.
    fn live_count(&self, now_ms: u64) -> usize {
        let cutoff = now_ms.saturating_sub(self.ttl_ms);
        self.entries.iter().filter(|(ts, _)| *ts >= cutoff).count()
    }
}

/// Thread-safe graduation filter wrapping the ring buffer in a Mutex.
/// Single-task access pattern (async read loop) → zero contention.
struct GraduationFilter {
    dedup: Mutex<GraduationDedup>,
    startup_ts_ms: u64,
}

impl GraduationFilter {
    fn new(dedup_ttl_ms: u64) -> Self {
        Self {
            dedup: Mutex::new(GraduationDedup::new(dedup_ttl_ms)),
            startup_ts_ms: now_ms(),
        }
    }

    /// Combined filter: cheapest checks first, ring buffer last.
    /// Returns true for new graduation events that pass WSOL reject, startup
    /// guard, and sig dedup.
    ///
    /// NOTE: pump suffix filter removed — pool type filtering now happens
    /// in the graduation arb engine, not at the feed level. Raydium events
    /// can involve any token, not just pump.fun mints ending in "pump".
    #[inline(always)]
    fn should_emit(&self, mint_b58: &str, sig_prefix: &[u8; 32], ts_ms: u64) -> bool {
        // ── Filter 1: startup replay guard — u64 compare, ~1 cycle ──
        if ts_ms < self.startup_ts_ms.saturating_add(STARTUP_REPLAY_WINDOW_MS) {
            Self::log_startup_reject(ts_ms, self.startup_ts_ms);
            return false;
        }

        // ── Filter 2: WSOL reject — byte slice compare, ~2 cycles ──
        let b = mint_b58.as_bytes();
        if b == WSOL_MINT_B58 {
            return false;
        }

        // ── Filter 3: sig dedup — ring buffer scan, ~192 cycles worst ──
        // Mutex::lock on uncontested single-thread path ≈ 20ns.
        match self.dedup.lock() {
            Ok(mut d) => d.is_new(sig_prefix, ts_ms),
            Err(poisoned) => poisoned.into_inner().is_new(sig_prefix, ts_ms),
        }
    }

    /// Stats: count of live dedup entries (for periodic logging).
    fn live_dedup_count(&self) -> usize {
        match self.dedup.lock() {
            Ok(d) => d.live_count(now_ms()),
            Err(p) => p.into_inner().live_count(now_ms()),
        }
    }

    /// Cold path: log startup replay rejection.
    /// `#[cold]` + `#[inline(never)]` moves this out of the hot path,
    /// improving branch prediction for the common (pass) case.
    #[cold]
    #[inline(never)]
    fn log_startup_reject(ts_ms: u64, startup_ts_ms: u64) {
        debug!(
            "[corecast] ignoring historical replay event ts={} startup={}",
            ts_ms, startup_ts_ms
        );
    }
}

/// Stream 1: pump.fun DEX trades (creator sell detection).
const GQL_BONDING_TRADES: &str = r#"subscription {
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

/// Stream 2: Raydium AMM trades — detects pump.fun token migration to Raydium.
/// When a token migrates, any open position in that mint must be force-exited.
const GQL_AMM_MIGRATION: &str = r#"subscription {
  Solana {
    DEXTrades(
      where: {Trade: {Dex: {ProgramAddress: {in: ["675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8"]}}}}
    ) {
      Trade {
        Buy { Currency { MintAddress } }
        Sell { Currency { MintAddress } }
        Dex { ProgramAddress }
      }
      Transaction { Signature }
      Block { Time }
    }
  }
}"#;

/// Stream 3: Token supply updates — LP removal / rug detection.
/// If PostBalance < PreBalance by >50%, it signals LP removal.
const GQL_LP_REMOVAL: &str = r#"subscription {
  Solana {
    TokenSupplyUpdates(
      where: {
        TokenSupplyUpdate: { Currency: { Native: false } }
        Transaction: { Result: { Success: true } }
      }
    ) {
      TokenSupplyUpdate {
        Currency { MintAddress }
        PostBalance
        PreBalance
      }
      Transaction { Signer Signature }
    }
  }
}"#;



/// Run the CoreCast/Bitquery WebSocket feed loop. Never returns unless shutdown.
/// If `BITQUERY_API_KEY` is not set, logs a warning and returns immediately.
///
/// Sends 3 multiplexed subscriptions on a single WebSocket connection:
///   ID "1" — pump.fun bonding trades (creator sell detection)
///   ID "2" — Raydium AMM trades (migration detection)
///   ID "3" — Token supply updates (LP removal / rug detection)
///
/// All 3 use the same WS connection = 1 stream toward Bitquery's 5-stream cap.
///
/// `creator_map`: shared map of mint → creator wallet, populated by PumpPortal.
pub async fn run(
    tx: Sender<FeedEvent>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
    creator_map: CreatorMap,
) {
    let api_key = match std::env::var("BITQUERY_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            info!("[corecast] disabled (BITQUERY_API_KEY not set)");
            return;
        }
    };

    let mut backoff_secs: u64 = 1;

    // Graduation noise filter: dedup sigs, reject non-pump mints, ignore historical replay.
    // 10s grace period on startup to ignore Bitquery historical event replay.
    let grad_filter = GraduationFilter::new(10_000);

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

                // Step 3: Send ALL subscriptions (multiplexed on same WS connection)
                let subscriptions: &[(&str, &str, &str)] = &[
                    (SUB_ID_BONDING_TRADES, GQL_BONDING_TRADES, "pump.fun DEX trades"),
                    (SUB_ID_AMM_MIGRATION, GQL_AMM_MIGRATION, "Raydium AMM migration"),
                    (SUB_ID_LP_REMOVAL, GQL_LP_REMOVAL, "LP removal / rug detection"),
                ];

                let mut all_subscribed = true;
                for (id, query, label) in subscriptions {
                    let sub_msg = serde_json::json!({
                        "type": "start",
                        "id": id,
                        "payload": {
                            "query": query
                        }
                    });
                    if let Err(e) = write.send(tungstenite::Message::Text(sub_msg.to_string().into())).await {
                        error!("[corecast] failed to send subscription {} ({}): {}", id, label, e);
                        all_subscribed = false;
                        break;
                    }
                    info!("[corecast] subscribed id={} — {}", id, label);
                }

                if !all_subscribed {
                    continue;
                }

                info!(
                    streams = subscriptions.len(),
                    endpoint = BITQUERY_WS_URL,
                    stream_ids = "1=DEXTrades,2=AMMTrades,3=LPRemoval",
                    "CoreCast connected and subscribed"
                );

                // Step 4: Read loop — route messages by subscription ID
                let mut stats = StreamStats::default();
                loop {
                    tokio::select! {
                        msg = read.next() => {
                            match msg {
                                Some(Ok(tungstenite::Message::Text(text))) => {
                                    let ts_ms = now_ms();
                                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&*text) {
                                        let msg_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
                                        if msg_type != "data" {
                                            continue;
                                        }

                                        let sub_id = v.get("id").and_then(|i| i.as_str()).unwrap_or("");
                                        let payload = match v.get("payload") {
                                            Some(p) => p,
                                            None => continue,
                                        };

                                        match sub_id {
                                            SUB_ID_BONDING_TRADES => {
                                                stats.bonding_trades += 1;
                                                if let Some(event) = parse_bonding_trade(payload, ts_ms, &creator_map) {
                                                    if let FeedEvent::CreatorSell { ref mint, .. } = event {
                                                        debug!(
                                                            stream_id = SUB_ID_BONDING_TRADES,
                                                            event_type = "creator_sell",
                                                            mint = %bs58::encode(mint).into_string(),
                                                            "CoreCast stream event"
                                                        );
                                                    }
                                                    if tx.send(event).is_err() {
                                                        info!("[corecast] engine channel closed");
                                                        return;
                                                    }
                                                }
                                            }
                                            SUB_ID_AMM_MIGRATION => {
                                                stats.amm_migrations += 1;
                                                let events = parse_amm_migration(payload, ts_ms, &grad_filter);
                                                for event in &events {
                                                    if let FeedEvent::Migration { ref mint, .. } = event {
                                                        debug!(
                                                            stream_id = SUB_ID_AMM_MIGRATION,
                                                            event_type = "migration",
                                                            mint = %bs58::encode(mint).into_string(),
                                                            "CoreCast stream event"
                                                        );
                                                    }
                                                }
                                                for event in events {
                                                    if tx.send(event).is_err() {
                                                        info!("[corecast] engine channel closed");
                                                        return;
                                                    }
                                                }
                                            }
                                            SUB_ID_LP_REMOVAL => {
                                                stats.lp_removals += 1;
                                                let events = parse_lp_removal(payload, ts_ms);
                                                for event in &events {
                                                    if let FeedEvent::LpRemoval { ref mint, .. } = event {
                                                        debug!(
                                                            stream_id = SUB_ID_LP_REMOVAL,
                                                            event_type = "lp_removal",
                                                            mint = %bs58::encode(mint).into_string(),
                                                            "CoreCast stream event"
                                                        );
                                                    }
                                                }
                                                for event in events {
                                                    if tx.send(event).is_err() {
                                                        info!("[corecast] engine channel closed");
                                                        return;
                                                    }
                                                }
                                            }

                                            _ => {
                                                debug!("[corecast] unknown subscription id: {}", sub_id);
                                            }
                                        }

                                        // Periodic stats logging (ring buffer self-GCs via TTL — no explicit GC needed)
                                        let total = stats.total();
                                        if total % 100 == 0 && total > 0 {
                                            debug!(
                                                "[corecast] bonding={} amm={} lp={} creator_match={} creator_miss={} grad_dedup_live={}",
                                                stats.bonding_trades, stats.amm_migrations,
                                                stats.lp_removals,
                                                stats.creator_matches, stats.creator_mismatches,
                                                grad_filter.live_dedup_count()
                                            );
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

// ── Per-stream statistics ───────────────────────────────────────────

#[derive(Default)]
struct StreamStats {
    bonding_trades: u64,
    amm_migrations: u64,
    lp_removals: u64,
    creator_matches: u64,
    creator_mismatches: u64,
}

impl StreamStats {
    fn total(&self) -> u64 {
        self.bonding_trades + self.amm_migrations + self.lp_removals
    }
}

/// Current epoch ms (used as fallback timestamp for events without Block.Time).
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Decode a base58 string into a 32-byte array on the stack (no heap allocation).
fn decode_bs58_32(s: &str) -> Option<[u8; 32]> {
    let mut arr = [0u8; 32];
    let n = bs58::decode(s).onto(&mut arr[..]).ok()?;
    if n != 32 { return None; }
    Some(arr)
}

// ── Stream 1: Bonding trades (creator sell detection) ───────────────

/// Parse a pump.fun DEX trade from stream 1 payload.
/// Returns `FeedEvent::CreatorSell` if the signer matches the known creator.
fn parse_bonding_trade(
    payload: &serde_json::Value,
    ts_ms: u64,
    creator_map: &CreatorMap,
) -> Option<FeedEvent> {
    let data = payload.get("data")?;
    let solana = data.get("Solana")?;
    let dex_trades = solana.get("DEXTrades")?.as_array()?;

    let trade = dex_trades.first()?;
    let mint_address = trade.pointer("/Trade/Buy/Currency/MintAddress")?.as_str()?;
    let mint = decode_bs58_32(mint_address)?;

    let signer = trade
        .pointer("/Transaction/Signer")
        .and_then(|s| s.as_str())
        .and_then(decode_bs58_32);

    // Verify signer matches known creator for this mint
    let is_creator_sell = if let Some(signer_bytes) = signer {
        match creator_map.read() {
            Ok(map) => {
                if let Some(creator) = map.get(&mint) {
                    *creator == signer_bytes
                } else {
                    // No creator known — emit conservatively
                    true
                }
            }
            Err(_) => {
                warn!("[corecast] creator_map lock poisoned");
                false
            }
        }
    } else {
        // No signer in message — emit conservatively
        true
    };

    if is_creator_sell {
        Some(FeedEvent::CreatorSell { mint, ts_ms })
    } else {
        None
    }
}

// ── Stream 2: AMM migration detection ───────────────────────────────

/// Parse Raydium AMM trades from stream 2 payload.
/// Returns `FeedEvent::Migration` only for validated pump.fun token graduations.
///
/// Filters applied (in order):
///   1. Reject non-pump.fun mints (WSOL, system tokens) — `is_valid_graduation_mint()`
///   2. Reject historical event replays on connect (startup grace period)
///   3. Reject duplicate tx signatures (only first occurrence = graduation)
fn parse_amm_migration(
    payload: &serde_json::Value,
    ts_ms: u64,
    grad_filter: &GraduationFilter,
) -> Vec<FeedEvent> {
    let mut events = Vec::new();

    let trades = match payload
        .pointer("/data/Solana/DEXTrades")
        .and_then(|t| t.as_array())
    {
        Some(t) => t,
        None => return events,
    };

    for trade in trades {
        // Extract full 64-byte transaction signature (for getTransaction RPC + dedup)
        let sig = trade
            .pointer("/Transaction/Signature")
            .and_then(|s| s.as_str())
            .and_then(|s| {
                // Bitquery returns base58-encoded signature (64 bytes decoded).
                let mut buf = [0u8; 64];
                let n = bs58::decode(s).onto(&mut buf[..]).ok()?;
                if n >= 32 {
                    // Zero-pad if base58 decodes to less than 64 bytes (rare edge case)
                    Some(buf)
                } else {
                    None
                }
            })
            .unwrap_or([0u8; 64]);

        // Extract sig prefix (first 32 bytes) for dedup key
        let sig_prefix: [u8; 32] = sig[..32].try_into().unwrap_or([0u8; 32]);

        // Extract both buy and sell mint addresses as base58 strings for filtering
        let buy_mint_b58 = trade
            .pointer("/Trade/Buy/Currency/MintAddress")
            .and_then(|s| s.as_str());
        let sell_mint_b58 = trade
            .pointer("/Trade/Sell/Currency/MintAddress")
            .and_then(|s| s.as_str());

        // Find the pump.fun token mint — it ends with "pump", the other side is WSOL
        // Only emit for validated pump.fun mints that pass all graduation filters
        let mut emitted = false;

        if let Some(mint_b58) = buy_mint_b58 {
            if grad_filter.should_emit(mint_b58, &sig_prefix, ts_ms) {
                if let Some(mint) = decode_bs58_32(mint_b58) {
                    events.push(FeedEvent::Migration {
                        mint,
                        ts_ms,
                        source: MigrationSource::CoreCastStream2,
                        sig,
                    });
                    emitted = true;
                }
            }
        }

        if !emitted {
            if let Some(mint_b58) = sell_mint_b58 {
                if grad_filter.should_emit(mint_b58, &sig_prefix, ts_ms) {
                    if let Some(mint) = decode_bs58_32(mint_b58) {
                        events.push(FeedEvent::Migration {
                            mint,
                            ts_ms,
                            source: MigrationSource::CoreCastStream2,
                            sig,
                        });
                    }
                }
            }
        }
    }

    events
}

// ── Stream 3: LP removal / rug detection ────────────────────────────

/// Parse token supply updates from stream 3 payload.
/// Returns `FeedEvent::LpRemoval` if PostBalance < PreBalance by >50%.
fn parse_lp_removal(payload: &serde_json::Value, ts_ms: u64) -> Vec<FeedEvent> {
    let mut events = Vec::new();

    let updates = match payload
        .pointer("/data/Solana/TokenSupplyUpdates")
        .and_then(|t| t.as_array())
    {
        Some(t) => t,
        None => return events,
    };

    for update in updates {
        let mint_str = match update
            .pointer("/TokenSupplyUpdate/Currency/MintAddress")
            .and_then(|s| s.as_str())
        {
            Some(s) => s,
            None => continue,
        };

        let pre_balance = update
            .pointer("/TokenSupplyUpdate/PreBalance")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);

        let post_balance = update
            .pointer("/TokenSupplyUpdate/PostBalance")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);

        // LP removal: supply dropped by >50%
        if pre_balance > 0.0 && post_balance < pre_balance * 0.5 {
            if let Some(mint) = decode_bs58_32(mint_str) {
                info!(
                    "[corecast] LP removal detected: mint={} pre={:.2} post={:.2} drop={:.1}%",
                    mint_str, pre_balance, post_balance,
                    (1.0 - post_balance / pre_balance) * 100.0
                );
                events.push(FeedEvent::LpRemoval { mint, ts_ms });
            }
        }
    }

    events
}


