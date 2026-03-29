# Network & Bare Metal Optimization Spec

**Author:** Network/Systems Architecture Agent  
**Date:** 2026-03-29  
**Status:** Ready for implementation  
**Target:** Reduce end-to-end latency from ~200ms to <50ms (trade detection → bundle submission)

---

## Architecture Overview

Two engineer tasks attacking latency at different layers:

| Engineer | Module | Latency Savings | Layer |
|----------|--------|-----------------|-------|
| **Engineer 4** | `feeds/shredstream.rs` — Jito ShredStream gRPC | ~80-120ms | Network (earlier data) |
| **Engineer 5** | `system/tuning.rs` — Bare metal optimizations | ~20-50ms | OS/kernel (faster processing) |

Combined target: **100-170ms reduction** → achieves <50ms end-to-end.

### Current Latency Breakdown

```
Trade on-chain           ┐
  ~80ms  (block propagation + websocket relay)
PumpPortal/Helius WS     ┤ ← Engineer 4 eliminates this
  ~20ms  (JSON parse + event creation)
Event in engine           ┤
  ~30ms  (gate + score + decision)          ← Engineer 5 reduces
  ~15ms  (TX build + sign)                  ← Engineer 5 reduces
  ~55ms  (bundle submit + network)          ← Engineer 5 reduces
Bundle at Jito            ┘
─────────────────────────
~200ms total
```

### Target Latency Breakdown

```
Trade on-chain           ┐
  ~5ms   (ShredStream shred delivery)       ← Engineer 4
  ~5ms   (shred parse + event)
Event in engine           ┤
  ~15ms  (gate + score — RT scheduling)     ← Engineer 5
  ~10ms  (TX build + sign — mlocked)        ← Engineer 5
  ~15ms  (bundle submit — TCP_NODELAY)      ← Engineer 5
Bundle at Jito            ┘
─────────────────────────
~50ms total
```

---

# Engineer 4: Jito ShredStream gRPC Integration

## File: `feeds/shredstream.rs` — COMPLETE REWRITE

The existing `shredstream.rs` is a UDP stub that scans raw datagrams for Anchor discriminators. This rewrite uses the Jito ShredStream gRPC API for proper pre-confirmation trade detection with full transaction parsing.

### Why ShredStream?

1. **Earliest possible data**: Jito validators forward shreds (raw block fragments) via gRPC **before** blocks are confirmed
2. **~80-120ms faster** than WebSocket feeds (PumpPortal/Helius) which wait for processed/confirmed commitment
3. **Full transaction bytes**: We deserialize actual `VersionedTransaction` — not just discriminator scanning
4. **Program filtering**: gRPC subscription filters for Pump.fun program only, reducing bandwidth by ~95%
5. **Full `TradeEvent` emission**: Unlike the current PreWarm-only stub, this emits proper `TradeEvent` with mint, trader, amounts, bonding curve addresses

### Dependencies to Add (`Cargo.toml`)

```toml
# Jito ShredStream gRPC
tonic = { version = "0.12", features = ["tls", "tls-webpki-roots"] }
prost = "0.13"
```

### Config Section (`canary.json` → add `"shredstream"` key)

```json
{
  "shredstream": {
    "enabled": true,
    "endpoint": "https://mainnet.rpc.jito.wtf",
    "auth_token_env": "JITO_AUTH_TOKEN",
    "reconnect_base_ms": 500,
    "reconnect_max_ms": 15000,
    "emit_full_trade": true,
    "emit_prewarm_fallback": true,
    "stats_interval_s": 60
  }
}
```

### Complete Implementation: `feeds/shredstream.rs`

```rust
//! Jito ShredStream gRPC feed — lowest-latency Solana trade detection.
//!
//! ShredStream delivers raw shred data from Jito validators BEFORE block
//! confirmation (~80-120ms faster than WebSocket feeds). We subscribe via
//! gRPC, filter for Pump.fun program transactions, parse trade data from
//! raw transaction bytes, and emit full `TradeEvent`s into the event joiner.
//!
//! Architecture:
//!   gRPC stream → tx deserialization → pump.fun instruction filter
//!   → field extraction → TradeEvent → event_joiner crossbeam channel
//!
//! Graceful degradation: if ShredStream is unavailable, the bot continues
//! with PumpPortal + Helius feeds (just ~80ms slower).
//!
//! # Reserve Data Limitation
//!
//! ShredStream delivers transaction data, NOT post-execution account state.
//! This means `vsol_reserves` and `vtoken_reserves` are **0** on ShredStream
//! TradeEvents. The engine hot path MUST handle 0-reserves by:
//!   1. Using cached reserves from a prior PumpPortal/Helius event for the same mint
//!   2. Estimating reserves by applying the trade's sol_amount to cached state
//!   3. Skipping scoring if no cached reserves exist (first-ever trade on this mint)
//!
//! This is acceptable because ShredStream's ~80ms advantage means the engine
//! will almost always have cached reserves from an earlier trade before needing
//! to score.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossbeam_channel::Sender;
use solana_sdk::{
    pubkey::Pubkey,
    transaction::VersionedTransaction,
};
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

use crate::feeds::{FeedEvent, FeedSource, PreWarmEvent, TradeEvent};

// ── Constants ───────────────────────────────────────────────────────

/// Pump.fun program ID.
const PUMP_PROGRAM: Pubkey = solana_sdk::pubkey!("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");

/// Anchor discriminator for pump.fun `buy` instruction.
/// Derived from: sha256("global:buy")[0..8]
const BUY_DISC: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];

/// Anchor discriminator for pump.fun `sell` instruction.
/// Derived from: sha256("global:sell")[0..8]
const SELL_DISC: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];

/// Minimum instruction data size: 8 (disc) + 8 (amount) + 8 (sol_cost) = 24.
const MIN_IX_DATA_LEN: usize = 24;

/// Minimum accounts in a pump.fun buy/sell instruction.
const MIN_PUMP_ACCOUNTS: usize = 9;

// Account indices in pump.fun buy/sell instruction account list:
//   0: global          1: fee_recipient    2: mint
//   3: bonding_curve   4: assoc_bonding    5: assoc_user
//   6: user (signer)   7: system_program   8: token_program
const MINT_IX_IDX: usize = 2;
const BONDING_CURVE_IX_IDX: usize = 3;
const ASSOC_BONDING_CURVE_IX_IDX: usize = 4;
const USER_IX_IDX: usize = 6;

/// Maximum sane SOL amount (10,000 SOL in lamports).
const MAX_SOL_LAMPORTS: u64 = 10_000_000_000_000;

// ── Configuration ──────────────────────────────────────────────────

/// Configuration for ShredStream gRPC feed.
/// Loaded from canary.json "shredstream" section.
#[derive(Debug, Clone)]
pub struct ShredStreamConfig {
    pub enabled: bool,
    pub endpoint: String,
    pub auth_token: Option<String>,
    pub reconnect_base: Duration,
    pub reconnect_max: Duration,
    pub emit_full_trade: bool,
    pub emit_prewarm_fallback: bool,
    pub stats_interval: Duration,
}

impl ShredStreamConfig {
    /// Build from canary.json value.
    pub fn from_json(val: &serde_json::Value) -> Self {
        let s = &val["shredstream"];
        let auth_env = s["auth_token_env"].as_str().unwrap_or("JITO_AUTH_TOKEN");
        Self {
            enabled: s["enabled"].as_bool().unwrap_or(false),
            endpoint: s["endpoint"]
                .as_str()
                .unwrap_or("https://mainnet.rpc.jito.wtf")
                .to_string(),
            auth_token: std::env::var(auth_env).ok(),
            reconnect_base: Duration::from_millis(
                s["reconnect_base_ms"].as_u64().unwrap_or(500),
            ),
            reconnect_max: Duration::from_millis(
                s["reconnect_max_ms"].as_u64().unwrap_or(15000),
            ),
            emit_full_trade: s["emit_full_trade"].as_bool().unwrap_or(true),
            emit_prewarm_fallback: s["emit_prewarm_fallback"].as_bool().unwrap_or(true),
            stats_interval: Duration::from_secs(
                s["stats_interval_s"].as_u64().unwrap_or(60),
            ),
        }
    }

    /// Build from environment variables (simple mode).
    pub fn from_env() -> Self {
        let endpoint = std::env::var("SHREDSTREAM_ENDPOINT")
            .unwrap_or_else(|_| "https://mainnet.rpc.jito.wtf".into());
        let auth_token = std::env::var("JITO_AUTH_TOKEN").ok();
        Self {
            enabled: auth_token.is_some(),
            endpoint,
            auth_token,
            reconnect_base: Duration::from_millis(500),
            reconnect_max: Duration::from_secs(15),
            emit_full_trade: true,
            emit_prewarm_fallback: true,
            stats_interval: Duration::from_secs(60),
        }
    }
}

// ── Statistics ──────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct Stats {
    entries_received: u64,
    txs_scanned: u64,
    pump_hits: u64,
    trades_emitted: u64,
    prewarms_emitted: u64,
    parse_failures: u64,
    last_entry_ms: u64,
    window_start_ms: u64,
}

impl Stats {
    fn new() -> Self {
        Self { window_start_ms: now_ms(), ..Default::default() }
    }

    fn log_and_reset(&mut self) {
        let elapsed = (now_ms() - self.window_start_ms) as f64 / 1000.0;
        let age = if self.last_entry_ms > 0 { now_ms() - self.last_entry_ms } else { 0 };
        info!(
            "[shredstream] stats: entries={} txs={} pump={} trades={} prewarm={} \
             fail={} elapsed={:.1}s last_age={}ms",
            self.entries_received, self.txs_scanned, self.pump_hits,
            self.trades_emitted, self.prewarms_emitted, self.parse_failures,
            elapsed, age,
        );
        let last = self.last_entry_ms;
        *self = Self { last_entry_ms: last, window_start_ms: now_ms(), ..Default::default() };
    }
}

// ── Parsed Trade ───────────────────────────────────────────────────

/// Intermediate parsed pump.fun trade from transaction bytes.
#[derive(Debug)]
struct ParsedTrade {
    mint: [u8; 32],
    trader: [u8; 32],
    signature: [u8; 64],
    bonding_curve: [u8; 32],
    assoc_bonding_curve: [u8; 32],
    sol_amount: u64,
    token_amount: u64,
    is_buy: bool,
    slot: u64,
}

// ── Transaction Parsing ────────────────────────────────────────────

/// Parse a serialized Solana transaction for pump.fun buy/sell instructions.
///
/// Returns `Some(ParsedTrade)` if the transaction contains a pump.fun
/// trade. Returns `None` for non-pump.fun transactions or parse failures.
fn parse_pump_tx(tx_bytes: &[u8], slot: u64) -> Option<ParsedTrade> {
    let tx: VersionedTransaction = bincode::deserialize(tx_bytes).ok()?;

    // First signature = transaction signature
    let sig_bytes: [u8; 64] = tx.signatures.first()?.as_ref().try_into().ok()?;

    let message = &tx.message;
    let account_keys = message.static_account_keys();

    for ix in message.instructions() {
        let prog_idx = ix.program_id_index as usize;
        if prog_idx >= account_keys.len() || account_keys[prog_idx] != PUMP_PROGRAM {
            continue;
        }

        if ix.data.len() < MIN_IX_DATA_LEN || ix.accounts.len() < MIN_PUMP_ACCOUNTS {
            continue;
        }

        let is_buy = match ix.data[0..8] {
            ref d if d == BUY_DISC => true,
            ref d if d == SELL_DISC => false,
            _ => continue,
        };

        let token_amount = u64::from_le_bytes(ix.data[8..16].try_into().ok()?);
        let sol_amount = u64::from_le_bytes(ix.data[16..24].try_into().ok()?);

        if sol_amount == 0 || sol_amount > MAX_SOL_LAMPORTS {
            continue;
        }

        // Resolve account indices → pubkeys
        let resolve = |ix_idx: usize| -> Option<[u8; 32]> {
            let key_idx = *ix.accounts.get(ix_idx)? as usize;
            account_keys.get(key_idx).map(|pk| pk.to_bytes())
        };

        let mint = resolve(MINT_IX_IDX)?;
        let bonding_curve = resolve(BONDING_CURVE_IX_IDX)?;
        let assoc_bonding_curve = resolve(ASSOC_BONDING_CURVE_IX_IDX)?;
        let trader = resolve(USER_IX_IDX)?;

        return Some(ParsedTrade {
            mint,
            trader,
            signature: sig_bytes,
            bonding_curve,
            assoc_bonding_curve,
            sol_amount,
            token_amount,
            is_buy,
            slot,
        });
    }

    None
}

/// Convert ParsedTrade → TradeEvent.
///
/// IMPORTANT: vsol_reserves and vtoken_reserves are set to 0 because
/// ShredStream delivers pre-confirmation data (no post-execution state).
/// The engine must use cached reserves or estimate from prior state.
fn to_trade_event(p: ParsedTrade) -> TradeEvent {
    let mut sig_prefix = [0u8; 8];
    sig_prefix.copy_from_slice(&p.signature[0..8]);

    TradeEvent {
        mint: p.mint,
        trader: p.trader,
        sig: p.signature,
        sig_prefix,
        sol_amount: p.sol_amount,
        token_amount: p.token_amount,
        vsol_reserves: 0,       // Not available from shred data
        vtoken_reserves: 0,     // Not available from shred data
        market_cap_sol: 0,      // Requires reserves
        slot: p.slot,
        timestamp_ms: now_ms(),
        is_buy: p.is_buy,
        source: FeedSource::ShredStream,
        bonding_curve: p.bonding_curve,
        assoc_bonding_curve: p.assoc_bonding_curve,
    }
}

/// Convert ParsedTrade → PreWarmEvent (fallback).
fn to_prewarm(p: &ParsedTrade) -> PreWarmEvent {
    PreWarmEvent {
        mint: p.mint,
        trader: p.trader,
        sig: p.signature,
        sol_amount: p.sol_amount,
        is_buy: p.is_buy,
        timestamp_ms: now_ms(),
        source: FeedSource::ShredStream,
    }
}

// ── gRPC Proto Types ───────────────────────────────────────────────
//
// Manually defined to match Jito ShredStream wire format.
// For production, generate from .proto with tonic-build.
// See: https://github.com/jito-foundation/jito-programs
//
// To use proto compilation instead, add to build.rs:
//   tonic_build::compile_protos("proto/shredstream.proto").unwrap();
// And replace this module with: tonic::include_proto!("shredstream");

pub mod jito_proto {
    /// gRPC request to subscribe to shred entries.
    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct SubscribeEntriesRequest {
        /// Filter entries to those containing transactions involving these program IDs.
        /// Empty = all entries (not recommended — very high bandwidth).
        #[prost(bytes = "vec", repeated, tag = "1")]
        pub program_ids: ::prost::alloc::vec::Vec<::prost::alloc::vec::Vec<u8>>,
    }

    /// A shred entry containing serialized transactions.
    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct Entry {
        /// Slot this entry belongs to.
        #[prost(uint64, tag = "1")]
        pub slot: u64,
        /// Entry index within the slot.
        #[prost(uint64, tag = "2")]
        pub index: u64,
        /// Serialized transactions (each is a bincode-encoded VersionedTransaction).
        #[prost(bytes = "vec", repeated, tag = "3")]
        pub transactions: ::prost::alloc::vec::Vec<::prost::alloc::vec::Vec<u8>>,
    }

    /// Generated gRPC client for Jito ShredStream service.
    #[derive(Debug, Clone)]
    pub struct ShredStreamClient<T> {
        inner: tonic::client::Grpc<T>,
    }

    impl ShredStreamClient<tonic::transport::Channel> {
        pub fn new(channel: tonic::transport::Channel) -> Self {
            Self {
                inner: tonic::client::Grpc::new(channel),
            }
        }
    }

    impl<T> ShredStreamClient<T>
    where
        T: tonic::client::GrpcService<tonic::body::BoxBody>,
        T::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        T::ResponseBody: http_body::Body<Data = bytes::Bytes> + std::marker::Send + 'static,
        <T::ResponseBody as http_body::Body>::Error:
            Into<Box<dyn std::error::Error + Send + Sync>> + std::marker::Send,
    {
        pub fn with_interceptor<F>(
            inner: T,
            interceptor: F,
        ) -> ShredStreamClient<tonic::service::interceptor::InterceptedService<T, F>>
        where
            F: tonic::service::Interceptor,
        {
            ShredStreamClient {
                inner: tonic::client::Grpc::new(
                    tonic::service::interceptor::InterceptedService::new(inner, interceptor),
                ),
            }
        }

        /// Subscribe to shred entries filtered by program IDs.
        /// Returns a streaming response of Entry messages.
        pub async fn subscribe_entries(
            &mut self,
            request: impl tonic::IntoRequest<SubscribeEntriesRequest>,
        ) -> Result<tonic::Response<tonic::Streaming<Entry>>, tonic::Status> {
            self.inner.ready().await.map_err(|e| {
                tonic::Status::unknown(format!("Service not ready: {}", e.into()))
            })?;
            let path = http::uri::PathAndQuery::from_static(
                "/shredstream.ShredStream/SubscribeEntries",
            );
            let codec = tonic::codec::ProstCodec::default();
            self.inner.server_streaming(request.into_request(), path, codec).await
        }
    }
}

// ── gRPC Connection ────────────────────────────────────────────────

/// Establish gRPC connection to Jito ShredStream and subscribe to entries.
async fn connect(
    config: &ShredStreamConfig,
) -> Result<tonic::Streaming<jito_proto::Entry>, Box<dyn std::error::Error + Send + Sync>> {
    use tonic::transport::{Channel, ClientTlsConfig};
    use tonic::metadata::MetadataValue;

    let tls = ClientTlsConfig::new().with_webpki_roots();
    let channel = Channel::from_shared(config.endpoint.clone())?
        .tls_config(tls)?
        .connect_timeout(Duration::from_secs(10))
        .connect()
        .await?;

    let auth_token = config.auth_token.clone().unwrap_or_default();
    let mut client = jito_proto::ShredStreamClient::with_interceptor(
        channel,
        move |mut req: tonic::Request<()>| {
            if !auth_token.is_empty() {
                let val: MetadataValue<_> = auth_token
                    .parse()
                    .map_err(|_| tonic::Status::unauthenticated("invalid auth token"))?;
                req.metadata_mut().insert("authorization", val);
            }
            Ok(req)
        },
    );

    let request = jito_proto::SubscribeEntriesRequest {
        program_ids: vec![PUMP_PROGRAM.to_bytes().to_vec()],
    };

    let response = client.subscribe_entries(request).await?;
    Ok(response.into_inner())
}

// ── Main Feed Loop ─────────────────────────────────────────────────

/// Run the ShredStream gRPC feed loop with automatic reconnection.
///
/// Entry point called from `main.rs`. Handles:
/// 1. Config validation (enabled, auth token)
/// 2. gRPC connection + subscription
/// 3. Streaming entry reception + pump.fun tx parsing
/// 4. TradeEvent / PreWarmEvent emission to event joiner
/// 5. Reconnection with exponential backoff on failure
/// 6. Periodic stats logging
/// 7. Graceful shutdown via watch channel
pub async fn run(
    config: ShredStreamConfig,
    tx: Sender<FeedEvent>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    if !config.enabled {
        info!("[shredstream] disabled — skipping");
        return;
    }
    if config.auth_token.is_none() {
        warn!("[shredstream] no auth token (set JITO_AUTH_TOKEN) — skipping");
        return;
    }

    info!(
        "[shredstream] starting — endpoint={} full_trade={} prewarm_fallback={}",
        config.endpoint, config.emit_full_trade, config.emit_prewarm_fallback
    );

    let mut backoff = config.reconnect_base;
    let mut stats = Stats::new();
    let mut stats_deadline = tokio::time::Instant::now() + config.stats_interval;

    loop {
        if *shutdown_rx.borrow() {
            info!("[shredstream] shutdown — exiting");
            return;
        }

        info!("[shredstream] connecting...");
        match connect(&config).await {
            Ok(mut stream) => {
                info!("[shredstream] connected");
                backoff = config.reconnect_base;

                loop {
                    // Log stats periodically
                    if tokio::time::Instant::now() >= stats_deadline {
                        stats.log_and_reset();
                        stats_deadline = tokio::time::Instant::now() + config.stats_interval;
                    }

                    tokio::select! {
                        biased;

                        _ = shutdown_rx.changed() => {
                            if *shutdown_rx.borrow() {
                                info!("[shredstream] shutdown during stream");
                                return;
                            }
                        }

                        msg = stream.message() => {
                            match msg {
                                Ok(Some(entry)) => {
                                    stats.entries_received += 1;
                                    stats.last_entry_ms = now_ms();

                                    for tx_bytes in &entry.transactions {
                                        stats.txs_scanned += 1;

                                        match parse_pump_tx(tx_bytes, entry.slot) {
                                            Some(parsed) => {
                                                stats.pump_hits += 1;

                                                // Emit PreWarmEvent if configured
                                                if config.emit_prewarm_fallback {
                                                    let pw = to_prewarm(&parsed);
                                                    if tx.send(FeedEvent::PreWarm(pw)).is_err() {
                                                        info!("[shredstream] channel closed");
                                                        return;
                                                    }
                                                    stats.prewarms_emitted += 1;
                                                }

                                                // Emit full TradeEvent
                                                if config.emit_full_trade {
                                                    let trade = to_trade_event(parsed);
                                                    if tx.send(FeedEvent::Trade(trade)).is_err() {
                                                        info!("[shredstream] channel closed");
                                                        return;
                                                    }
                                                    stats.trades_emitted += 1;
                                                } else {
                                                    // If not emitting trades, still emit prewarm
                                                    if !config.emit_prewarm_fallback {
                                                        let pw = to_prewarm(&parsed);
                                                        if tx.send(FeedEvent::PreWarm(pw)).is_err() {
                                                            info!("[shredstream] channel closed");
                                                            return;
                                                        }
                                                        stats.prewarms_emitted += 1;
                                                    }
                                                }
                                            }
                                            None => {
                                                // Not a pump.fun tx (expected for most txs
                                                // even with program filter — filter is best-effort)
                                            }
                                        }
                                    }
                                }
                                Ok(None) => {
                                    warn!("[shredstream] stream ended (server closed)");
                                    break; // Reconnect
                                }
                                Err(e) => {
                                    warn!("[shredstream] stream error: {} — reconnecting", e);
                                    stats.parse_failures += 1;
                                    break; // Reconnect
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                error!("[shredstream] connect failed: {} — retry in {:?}", e, backoff);
            }
        }

        // Exponential backoff with jitter
        let jitter = Duration::from_millis(fastrand_u64() % 200);
        tokio::select! {
            _ = tokio::time::sleep(backoff + jitter) => {}
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() { return; }
            }
        }
        backoff = std::cmp::min(backoff * 2, config.reconnect_max);
    }
}

// ── Utility ────────────────────────────────────────────────────────

#[inline(always)]
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Simple fast random u64 using timestamp + thread ID (no external crate).
/// Good enough for jitter — NOT cryptographic.
#[inline]
fn fastrand_u64() -> u64 {
    let t = now_ms();
    let tid = std::thread::current().id();
    let h = format!("{:?}", tid);
    t.wrapping_mul(6364136223846793005).wrapping_add(h.len() as u64)
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::{
        hash::Hash,
        instruction::{AccountMeta, Instruction},
        message::Message,
        signature::Keypair,
        signer::Signer,
        system_program,
        transaction::Transaction,
    };

    /// Helper: build a fake pump.fun buy transaction with known parameters.
    fn make_pump_buy_tx(
        mint: Pubkey,
        user: &Keypair,
        token_amount: u64,
        max_sol_cost: u64,
    ) -> Vec<u8> {
        // Build instruction data: discriminator + token_amount + max_sol_cost
        let mut ix_data = Vec::with_capacity(24);
        ix_data.extend_from_slice(&BUY_DISC);
        ix_data.extend_from_slice(&token_amount.to_le_bytes());
        ix_data.extend_from_slice(&max_sol_cost.to_le_bytes());

        // Fake account pubkeys for the 9 required accounts
        let global = Pubkey::new_unique();
        let fee_recipient = Pubkey::new_unique();
        let bonding_curve = Pubkey::new_unique();
        let assoc_bonding = Pubkey::new_unique();
        let assoc_user = Pubkey::new_unique();
        let token_program = Pubkey::new_unique();

        let accounts = vec![
            AccountMeta::new_readonly(global, false),           // 0: global
            AccountMeta::new(fee_recipient, false),             // 1: fee_recipient
            AccountMeta::new_readonly(mint, false),             // 2: mint
            AccountMeta::new(bonding_curve, false),             // 3: bonding_curve
            AccountMeta::new(assoc_bonding, false),             // 4: assoc_bonding_curve
            AccountMeta::new(assoc_user, false),                // 5: assoc_user
            AccountMeta::new(user.pubkey(), true),              // 6: user (signer)
            AccountMeta::new_readonly(system_program::id(), false), // 7: system_program
            AccountMeta::new_readonly(token_program, false),    // 8: token_program
