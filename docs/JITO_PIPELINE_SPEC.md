# JITO_PIPELINE_SPEC.md — Bare-Metal Optimized Jito Bundle Pipeline

> **Status:** DESIGN COMPLETE — Ready for implementation  
> **Author:** Apollo (architect session)  
> **Date:** 2026-03-29  
> **Replaces:** HTTP REST path (preserved as fallback)  
> **Files to create:** `tx/jito_grpc.rs`, `tx/tip_engine.rs`, `tx/skeleton.rs`

---

## Table of Contents

1. [Architecture Overview](#architecture-overview)
2. [Engineer 1: Jito gRPC Persistent Stream](#engineer-1-jito-grpc-persistent-stream)
3. [Engineer 2: Dynamic Tip Engine](#engineer-2-dynamic-tip-engine)
4. [Engineer 3: Pre-serialized Transaction Skeletons](#engineer-3-pre-serialized-transaction-skeletons)
5. [Integration: Executor Changes](#integration-executor-changes)
6. [Config Schema](#config-schema)
7. [Migration Checklist](#migration-checklist)

---

## Architecture Overview

### Current State (HTTP REST)

```
exit_signal → build_sell_tx() → serialize → HTTP POST /api/v1/bundles → response
              ~2ms build         ~1ms ser    ~60-80ms round-trip
              + heap allocs       + base64   + TCP handshake per request
              + fixed 1 mSOL tip             + no failover
```

**Total latency: ~65-85ms from exit signal to wire**

### Target State (gRPC Pipeline)

```
position_open → pre-build skeleton (store 280 bytes on stack in OpenPosition)
                     │
exit_signal → patch_and_sign()  → submit via persistent gRPC stream
              ~200ns patch        ~1-3ms to wire (pre-established connection)
              + 1 ed25519 sign    + automatic failover to secondary BE
              + zero heap alloc   + dynamic tip from TipEngine
              + stack buffer      + SubscribeBundleResults for confirmation
```

**Total latency: <5ms from exit signal to wire (15-20x improvement)**

### Data Flow

```
                    ┌─────────────────────┐
                    │   TxSkeleton        │  Created at position open
                    │   [u8; 280] stack   │  Stored in OpenPosition
                    └────────┬────────────┘
                             │ patch_and_sign()
                             │ (~200ns, zero-alloc)
                             ▼
┌──────────────┐   ┌─────────────────────┐   ┌──────────────────────┐
│  TipEngine   │──▶│   VersionedTx       │──▶│   JitoGrpcClient    │
│  (lamports)  │   │   (signed, ready)   │   │   (persistent conn)  │
└──────────────┘   └─────────────────────┘   └──────────┬───────────┘
                                                        │
                                              ┌─────────┴─────────┐
                                              ▼                   ▼
                                        Frankfurt BE       Amsterdam BE
                                        (primary)          (failover)
```

---

## Engineer 1: Jito gRPC Persistent Stream

**File:** `rust/pump-quant-core/src/tx/jito_grpc.rs` (CREATE)

### 1.1 Dependencies

Add to `Cargo.toml`:

```toml
tonic = { version = "0.12", features = ["tls", "tls-native-roots"] }
tonic-build = "0.12"  # build-dependency
prost = "0.13"
```

### 1.2 Proto Build Setup

Create `rust/pump-quant-core/build.rs`:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/");
    tonic_build::configure()
        .build_server(false)
        .compile_protos(
            &[
                "proto/block_engine.proto",
                "proto/bundle.proto",
                "proto/shared.proto",
                "proto/auth.proto",
            ],
            &["proto/"],
        )?;
    Ok(())
}
```

Download protos from `https://github.com/jito-labs/mev-protos` into `rust/pump-quant-core/proto/`.

### 1.3 Complete Implementation

```rust
//! Jito Block Engine gRPC client with persistent connections, automatic
//! failover, and bundle result subscriptions.
//!
//! Replaces the HTTP REST path (`tx/jito.rs`) with persistent gRPC streams
//! for ~15-20x latency improvement. Old HTTP path preserved as fallback.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;
use solana_sdk::transaction::VersionedTransaction;
use tokio::sync::{mpsc, Notify, RwLock};
use tonic::metadata::MetadataValue;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use tonic::{Request, Status};

// ── Generated proto modules (from build.rs) ──────────────────────────────────

pub mod proto {
    pub mod auth {
        tonic::include_proto!("auth");
    }
    pub mod bundle {
        tonic::include_proto!("bundle");
    }
    pub mod block_engine {
        tonic::include_proto!("block_engine");
    }
    pub mod shared {
        tonic::include_proto!("shared");
    }
}

use proto::auth::auth_service_client::AuthServiceClient;
use proto::auth::{GenerateAuthChallengeRequest, GenerateAuthTokensRequest, Token};
use proto::block_engine::block_engine_validator_client::BlockEngineValidatorClient;

// ── Constants ────────────────────────────────────────────────────────────────

/// Default Jito Block Engine gRPC endpoints.
pub const BLOCK_ENGINE_FRANKFURT: &str = "https://frankfurt.mainnet.block-engine.jito.wtf";
pub const BLOCK_ENGINE_AMSTERDAM: &str = "https://amsterdam.mainnet.block-engine.jito.wtf";
pub const BLOCK_ENGINE_NY: &str = "https://ny.mainnet.block-engine.jito.wtf";
pub const BLOCK_ENGINE_TOKYO: &str = "https://tokyo.mainnet.block-engine.jito.wtf";

const MAX_BACKOFF_MS: u64 = 5_000;
const INITIAL_BACKOFF_MS: u64 = 100;
const AUTH_REFRESH_SECS: u64 = 240; // Tokens expire ~300s, refresh at 240s

// ── Config ───────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct JitoGrpcConfig {
    /// Primary block engine gRPC URL.
    pub primary_url: String,
    /// Secondary block engine gRPC URL (failover).
    pub secondary_url: String,
    /// Auth keypair for Jito gRPC authentication.
    pub auth_keypair: Arc<Keypair>,
    /// TCP_NODELAY on gRPC connections (default: true).
    pub tcp_nodelay: bool,
    /// Connection timeout in ms (default: 5000).
    pub connect_timeout_ms: u64,
    /// Per-request timeout in ms (default: 3000).
    pub request_timeout_ms: u64,
    /// Paper mode: log but don't submit.
    pub paper_mode: bool,
}

impl Default for JitoGrpcConfig {
    fn default() -> Self {
        Self {
            primary_url: BLOCK_ENGINE_FRANKFURT.to_string(),
            secondary_url: BLOCK_ENGINE_AMSTERDAM.to_string(),
            auth_keypair: Arc::new(Keypair::new()),
            tcp_nodelay: true,
            connect_timeout_ms: 5_000,
            request_timeout_ms: 3_000,
            paper_mode: false,
        }
    }
}

// ── Bundle Status ────────────────────────────────────────────────────────────

/// Result of a bundle submission attempt.
#[derive(Debug, Clone)]
pub enum BundleStatus {
    /// Submitted, awaiting confirmation. Contains Jito-assigned UUID.
    Submitted { bundle_id: String },
    /// Landed on-chain.
    Landed { bundle_id: String, slot: u64 },
    /// Rejected or dropped.
    Failed { bundle_id: String, reason: String },
    /// Paper mode — not actually submitted.
    PaperMode { would_be_bundle_id: String },
}

// ── Landing Rate Tracker ─────────────────────────────────────────────────────

/// Tracks bundle landing rates for congestion-aware tipping.
/// The TipEngine reads `landing_rate_bps()` on the hot path.
pub struct BundleLandingTracker {
    outcomes: RwLock<Vec<bool>>,
    max_entries: usize,
    /// Cached landing rate in basis points (0–10000). Atomic for hot-path reads.
    landing_rate_bps: AtomicU64,
}

impl BundleLandingTracker {
    pub fn new(max_entries: usize) -> Self {
        Self {
            outcomes: RwLock::new(Vec::with_capacity(max_entries)),
            max_entries,
            landing_rate_bps: AtomicU64::new(10_000), // 100% until data arrives
        }
    }

    pub async fn record_outcome(&self, landed: bool) {
        let mut v = self.outcomes.write().await;
        if v.len() >= self.max_entries {
            v.remove(0);
        }
        v.push(landed);
        let count = v.iter().filter(|&&x| x).count();
        let bps = (count as u64 * 10_000) / v.len().max(1) as u64;
        self.landing_rate_bps.store(bps, Ordering::Relaxed);
    }

    /// Hot-path read: current landing rate in basis points.
    #[inline(always)]
    pub fn landing_rate_bps(&self) -> u64 {
        self.landing_rate_bps.load(Ordering::Relaxed)
    }
}

// ── Block Engine Connection ──────────────────────────────────────────────────

/// A single authenticated gRPC connection to a Jito block engine.
struct BlockEngineConn {
    label: String,
    url: String,
    channel: Channel,
    auth_token: RwLock<Option<Token>>,
    healthy: AtomicBool,
    consecutive_failures: AtomicU64,
    last_success_ms: AtomicU64,
}

impl BlockEngineConn {
    async fn connect(label: &str, url: &str, cfg: &JitoGrpcConfig) -> Result<Self> {
        let tls = ClientTlsConfig::new().with_native_roots();
        let endpoint = Endpoint::from_shared(url.to_string())
            .context("invalid gRPC endpoint")?
            .tls_config(tls)?
            .connect_timeout(Duration::from_millis(cfg.connect_timeout_ms))
            .timeout(Duration::from_millis(cfg.request_timeout_ms))
            .tcp_nodelay(cfg.tcp_nodelay)
            .tcp_keepalive(Some(Duration::from_secs(30)))
            .http2_keep_alive_interval(Duration::from_secs(20))
            .keep_alive_timeout(Duration::from_secs(10))
            .keep_alive_while_idle(true)
            .initial_stream_window_size(Some(2 * 1024 * 1024))
            .initial_connection_window_size(Some(4 * 1024 * 1024));

        let channel = endpoint.connect().await
            .with_context(|| format!("failed to connect to {url}"))?;

        tracing::info!("gRPC connected: {label} ({url})");
        Ok(Self {
            label: label.to_string(),
            url: url.to_string(),
            channel,
            auth_token: RwLock::new(None),
            healthy: AtomicBool::new(true),
            consecutive_failures: AtomicU64::new(0),
            last_success_ms: AtomicU64::new(0),
        })
    }

    /// Try to rebuild the channel (reconnect).
    async fn reconnect(&mut self, cfg: &JitoGrpcConfig) -> Result<()> {
        let tls = ClientTlsConfig::new().with_native_roots();
        let endpoint = Endpoint::from_shared(self.url.clone())
            .context("invalid gRPC endpoint")?
            .tls_config(tls)?
            .connect_timeout(Duration::from_millis(cfg.connect_timeout_ms))
            .timeout(Duration::from_millis(cfg.request_timeout_ms))
            .tcp_nodelay(cfg.tcp_nodelay)
            .tcp_keepalive(Some(Duration::from_secs(30)))
            .http2_keep_alive_interval(Duration::from_secs(20))
            .keep_alive_timeout(Duration::from_secs(10))
            .keep_alive_while_idle(true);

        self.channel = endpoint.connect().await
            .with_context(|| format!("reconnect failed: {}", self.url))?;
        self.consecutive_failures.store(0, Ordering::Relaxed);
        self.healthy.store(true, Ordering::Release);
        tracing::info!("gRPC reconnected: {} ({})", self.label, self.url);
        Ok(())
    }

    fn mark_failure(&self) {
        let n = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        if n >= 3 {
            self.healthy.store(false, Ordering::Release);
            tracing::warn!("{} marked unhealthy ({n} failures)", self.label);
        }
    }

    fn mark_success(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
        self.healthy.store(true, Ordering::Release);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.last_success_ms.store(now, Ordering::Relaxed);
    }

    fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    fn backoff(&self) -> Duration {
        let n = self.consecutive_failures.load(Ordering::Relaxed);
        if n == 0 { return Duration::ZERO; }
        let ms = INITIAL_BACKOFF_MS * (1u64 << n.min(6));
        Duration::from_millis(ms.min(MAX_BACKOFF_MS))
    }
}

// ── Main Client ──────────────────────────────────────────────────────────────

pub struct JitoGrpcClient {
    config: JitoGrpcConfig,
    primary: Arc<RwLock<BlockEngineConn>>,
    secondary: Arc<RwLock<BlockEngineConn>>,
    /// 0 = primary active, 1 = secondary active.
    active_idx: AtomicUsize,
    /// Bundle landing rate tracker — shared with TipEngine.
    pub landing_tracker: Arc<BundleLandingTracker>,
    /// Monotonic counter for local bundle IDs (paper mode / logging).
    bundle_counter: AtomicU64,
    reconnect_notify: Arc<Notify>,
}

impl JitoGrpcClient {
    /// Connect to both block engines and authenticate.
    /// Spawns background tasks for auth refresh and health monitoring.
    pub async fn new(config: JitoGrpcConfig) -> Result<Arc<Self>> {
        tracing::info!(
            "Jito gRPC init: primary={}, secondary={}",
            config.primary_url, config.secondary_url
        );

        let primary = BlockEngineConn::connect("primary", &config.primary_url, &config).await
            .context("primary block engine connect failed")?;
        let secondary = BlockEngineConn::connect("secondary", &config.secondary_url, &config).await
            .context("secondary block engine connect failed")?;

        let primary = Arc::new(RwLock::new(primary));
        let secondary = Arc::new(RwLock::new(secondary));

        let client = Arc::new(Self {
            config: config.clone(),
            primary: primary.clone(),
            secondary: secondary.clone(),
            active_idx: AtomicUsize::new(0),
            landing_tracker: Arc::new(BundleLandingTracker::new(50)),
            bundle_counter: AtomicU64::new(0),
            reconnect_notify: Arc::new(Notify::new()),
        });

        // Authenticate both
        Self::authenticate_conn(&config.auth_keypair, &primary).await
            .context("primary auth failed")?;
        Self::authenticate_conn(&config.auth_keypair, &secondary).await
            .context("secondary auth failed")?;

        // Spawn background tasks
        client.clone().spawn_auth_refresh_task();
        client.clone().spawn_health_monitor();

        tracing::info!("Jito gRPC client ready");
        Ok(client)
    }

    /// Authenticate with a block engine.
    /// 1. Request auth challenge (nonce)
    /// 2. Sign with auth keypair (Ed25519)
    /// 3. Exchange for access + refresh tokens
    async fn authenticate_conn(
        keypair: &Keypair,
        conn: &Arc<RwLock<BlockEngineConn>>,
    ) -> Result<()> {
        let channel = { conn.read().await.channel.clone() };
        let mut auth = AuthServiceClient::new(channel);
        let pubkey = keypair.pubkey();

        // Step 1: challenge
        let challenge = auth
            .generate_auth_challenge(GenerateAuthChallengeRequest {
                role: 1, // Searcher
                pubkey: pubkey.to_bytes().to_vec(),
            })
            .await
            .context("auth challenge failed")?
            .into_inner()
            .challenge;

        // Step 2: sign
        let sig = keypair.sign_message(challenge.as_bytes());

        // Step 3: exchange
        let tokens = auth
            .generate_auth_tokens(GenerateAuthTokensRequest {
                challenge,
                client_pubkey: pubkey.to_bytes().to_vec(),
                signed_challenge: sig.as_ref().to_vec(),
            })
            .await
            .context("auth token exchange failed")?
            .into_inner();

        let access = tokens.access_token.context("no access token")?;
        {
            let c = conn.read().await;
            let mut guard = c.auth_token.write().await;
            *guard = Some(access);
            tracing::info!("authenticated: {}", c.label);
        }
        Ok(())
    }

    /// Spawn a task that refreshes auth tokens every AUTH_REFRESH_SECS.
    fn spawn_auth_refresh_task(self: Arc<Self>) {
        let primary = self.primary.clone();
        let secondary = self.secondary.clone();
        let keypair = self.config.auth_keypair.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(AUTH_REFRESH_SECS)).await;
                if let Err(e) = Self::authenticate_conn(&keypair, &primary).await {
                    tracing::warn!("primary auth refresh failed: {e}");
                }
                if let Err(e) = Self::authenticate_conn(&keypair, &secondary).await {
                    tracing::warn!("secondary auth refresh failed: {e}");
                }
            }
        });
    }

    /// Spawn a health monitor that reconnects unhealthy connections.
    fn spawn_health_monitor(self: Arc<Self>) {
        let primary = self.primary.clone();
        let secondary = self.secondary.clone();
        let config = self.config.clone();
        let keypair = self.config.auth_keypair.clone();
        let notify = self.reconnect_notify.clone();
        tokio::spawn(async move {
            loop {
                // Wait for a reconnect signal or check every 10s
                tokio::select! {
                    _ = notify.notified() => {},
                    _ = tokio::time::sleep(Duration::from_secs(10)) => {},
                }

                // Check primary
                {
                    let healthy = { primary.read().await.is_healthy() };
                    if !healthy {
                        let backoff = { primary.read().await.backoff() };
                        tracing::info!("primary unhealthy, reconnecting after {:?}", backoff);
                        tokio::time::sleep(backoff).await;
                        let mut conn = primary.write().await;
                        if let Err(e) = conn.reconnect(&config).await {
                            tracing::error!("primary reconnect failed: {e}");
                        } else {
                            drop(conn);
                            if let Err(e) = Self::authenticate_conn(&keypair, &primary).await {
                                tracing::error!("primary re-auth failed: {e}");
                            }
                        }
                    }
                }

                // Check secondary
                {
                    let healthy = { secondary.read().await.is_healthy() };
                    if !healthy {
                        let backoff = { secondary.read().await.backoff() };
                        tracing::info!("secondary unhealthy, reconnecting after {:?}", backoff);
                        tokio::time::sleep(backoff).await;
                        let mut conn = secondary.write().await;
                        if let Err(e) = conn.reconnect(&config).await {
                            tracing::error!("secondary reconnect failed: {e}");
                        } else {
                            drop(conn);
                            if let Err(e) = Self::authenticate_conn(&keypair, &secondary).await {
                                tracing::error!("secondary re-auth failed: {e}");
                            }
                        }
                    }
                }
            }
        });
    }

    // ── Bundle Submission (Hot Path) ─────────────────────────────────────

    /// Submit a single-transaction bundle via gRPC.
    ///
    /// Hot path: serialize → select active conn → gRPC SendBundle → failover on error.
    pub async fn submit_bundle(&self, tx: &VersionedTransaction) -> Result<BundleStatus> {
        if self.config.paper_mode {
            let id = self.next_bundle_id();
            let sig = tx.signatures.first()
                .map(|s| bs58::encode(s).into_string())
                .unwrap_or_else(|| "no-sig".into());
            tracing::info!("paper: would submit bundle {id}, tx={sig}");
            return Ok(BundleStatus::PaperMode { would_be_bundle_id: id });
        }

        let tx_bytes = bincode::serialize(tx)
            .context("failed to serialize tx")?;
        self.submit_bundle_bytes(&tx_bytes).await
    }

    /// Submit pre-serialized transaction bytes as a single-element bundle.
    /// Used by skeleton path to avoid double-serialization.
    pub async fn submit_bundle_bytes(&self, tx_bytes: &[u8]) -> Result<BundleStatus> {
        if self.config.paper_mode {
            let id = self.next_bundle_id();
            tracing::info!("paper: would submit bundle {id} ({} bytes)", tx_bytes.len());
            return Ok(BundleStatus::PaperMode { would_be_bundle_id: id });
        }

        // Try active connection
        let active_conn = self.get_active_conn();
        match self.send_to_conn(&active_conn, tx_bytes).await {
            Ok(status) => {
                active_conn.read().await.mark_success();
                return Ok(status);
            }
            Err(e) => {
                tracing::warn!("active conn failed, failover: {e}");
                active_conn.read().await.mark_failure();
            }
        }

        // Failover
        let failover_conn = self.get_failover_conn();
        match self.send_to_conn(&failover_conn, tx_bytes).await {
            Ok(status) => {
                failover_conn.read().await.mark_success();
                self.switch_active();
                Ok(status)
            }
            Err(e2) => {
                failover_conn.read().await.mark_failure();
                self.reconnect_notify.notify_one();
                bail!("both block engines failed: {e2}")
            }
        }
    }

    /// Low-level: send a bundle to a specific connection.
    async fn send_to_conn(
        &self,
        conn: &Arc<RwLock<BlockEngineConn>>,
        tx_bytes: &[u8],
    ) -> Result<BundleStatus> {
        let (channel, auth_value, label) = {
            let c = conn.read().await;
            let token_guard = c.auth_token.read().await;
            let token = token_guard.as_ref()
                .context("not authenticated")?;
            (c.channel.clone(), token.value.clone(), c.label.clone())
        };

        let packet = proto::shared::Packet {
            data: tx_bytes.to_vec(),
            meta: None,
        };
        let bundle = proto::bundle::Bundle {
            header: None,
            packets: vec![packet],
        };

        let mut request = Request::new(proto::bundle::SendBundleRequest {
            bundle: Some(bundle),
        });
        request.metadata_mut().insert(
            "authorization",
            MetadataValue::try_from(&format!("Bearer {auth_value}"))
                .context("invalid auth token for header")?,
        );

        let mut client = BlockEngineValidatorClient::new(channel);
        let resp = client.send_bundle(request).await
            .map_err(|s: Status| anyhow::anyhow!(
                "gRPC SendBundle failed ({}): code={}, msg={}",
                label, s.code(), s.message()
            ))?
            .into_inner();

        tracing::debug!("bundle submitted via {} → id={}", label, resp.uuid);
        Ok(BundleStatus::Submitted { bundle_id: resp.uuid })
    }

    // ── Bundle Result Subscription ───────────────────────────────────────

    /// Spawn a background task that subscribes to bundle results.
    /// Updates `landing_tracker` and forwards results to `result_tx`.
    pub fn spawn_result_subscription(
        self: Arc<Self>,
        result_tx: mpsc::UnboundedSender<BundleStatus>,
    ) {
        tokio::spawn(async move {
            loop {
                let conn = self.get_active_conn();
                let label = { conn.read().await.label.clone() };
                tracing::info!("subscribing to bundle results on {label}");

                if let Err(e) = self.run_result_stream(&conn, &result_tx).await {
                    tracing::warn!("result subscription error ({label}): {e}");
                }

                // Brief pause before retry
                let backoff = { conn.read().await.backoff() };
                tokio::time::sleep(backoff.max(Duration::from_millis(500))).await;
            }
        });
    }

    async fn run_result_stream(
        &self,
        conn: &Arc<RwLock<BlockEngineConn>>,
        result_tx: &mpsc::UnboundedSender<BundleStatus>,
    ) -> Result<()> {
        let (channel, auth_value) = {
            let c = conn.read().await;
            let guard = c.auth_token.read().await;
            let token = guard.as_ref().context("no auth token")?;
            (c.channel.clone(), token.value.clone())
        };

        let mut req = Request::new(proto::bundle::SubscribeBundleResultsRequest {});
        req.metadata_mut().insert(
            "authorization",
            MetadataValue::try_from(&format!("Bearer {auth_value}"))?,
        );

        let mut client = BlockEngineValidatorClient::new(channel);
        let mut stream = client
            .subscribe_bundle_results(req)
            .await
            .context("failed to open results stream")?
            .into_inner();

        while let Some(msg) = stream.message().await? {
            let id = msg.bundle_id.clone();
            match msg.result {
                Some(proto::bundle::bundle_result::Result::Accepted(a)) => {
                    tracing::info!("bundle {id} landed slot {}", a.slot);
                    self.landing_tracker.record_outcome(true).await;
                    let _ = result_tx.send(BundleStatus::Landed { bundle_id: id, slot: a.slot });
                }
                Some(proto::bundle::bundle_result::Result::Rejected(r)) => {
                    let reason = format!("{r:?}");
                    tracing::warn!("bundle {id} rejected: {reason}");
                    self.landing_tracker.record_outcome(false).await;
                    let _ = result_tx.send(BundleStatus::Failed { bundle_id: id, reason });
                }
                None => {
                    tracing::debug!("bundle {id}: pending");
                }
            }
        }
        Ok(())
    }

    // ── Helpers ──────────────────────────────────────────────────────────

    fn get_active_conn(&self) -> Arc<RwLock<BlockEngineConn>> {
        match self.active_idx.load(Ordering::Acquire) {
            0 => self.primary.clone(),
            _ => self.secondary.clone(),
        }
    }

    fn get_failover_conn(&self) -> Arc<RwLock<BlockEngineConn>> {
        match self.active_idx.load(Ordering::Acquire) {
            0 => self.secondary.clone(),
            _ => self.primary.clone(),
        }
    }

    fn switch_active(&self) {
        let old = self.active_idx.load(Ordering::Acquire);
        let new = 1 - old;
        self.active_idx.store(new, Ordering::Release);
        tracing::info!(
            "switched active block engine: {} → {}",
            if old == 0 { "primary" } else { "secondary" },
            if new == 0 { "primary" } else { "secondary" }
        );
    }

    fn next_bundle_id(&self) -> String {
        let n = self.bundle_counter.fetch_add(1, Ordering::Relaxed);
        format!("local-{n}")
    }
}
```

### 1.4 `mod.rs` Changes

Add