//! Jito Block Engine persistent HTTP/2 client for low-latency bundle submission.
//!
//! Maintains persistent connections to 2 block engines for failover.
//! Uses reqwest with HTTP/2 prior-knowledge and connection pooling to keep
//! a warm TCP+TLS session open. This eliminates per-bundle TCP handshake +
//! TLS negotiation overhead.
//!
//! Target: <10ms from bundle-ready to wire (vs ~60-80ms cold HTTP/1.1).
//!
//! Design:
//! - Primary + secondary block engine connections (Frankfurt / Amsterdam)
//! - Failover: try primary first, if it fails fall back to secondary
//! - Background reconnection on failure (non-blocking)
//! - Atomic stats for monitoring

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use reqwest::Client as HttpClient;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

// ── Constants ────────────────────────────────────────────────────────────────

/// Jito block engine gRPC/HTTP endpoints (also serve JSON-RPC on /api/v1/bundles)
// VPS is US East (Boston/NY) — use NY as primary, Frankfurt as secondary
const JITO_NY: &str = "https://ny.mainnet.block-engine.jito.wtf";
const JITO_FRANKFURT: &str = "https://frankfurt.mainnet.block-engine.jito.wtf";

/// Maximum idle connections per host in the pool
const POOL_MAX_IDLE_PER_HOST: usize = 4;

/// Pool idle timeout — keep connections warm for this long
const POOL_IDLE_TIMEOUT_SECS: u64 = 90;

/// TCP connect timeout for establishing new connections
const CONNECT_TIMEOUT_MS: u64 = 3000;

// ── Config ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct JitoGrpcConfig {
    /// Primary block engine URL
    pub primary_url: String,
    /// Secondary block engine URL (failover)
    pub secondary_url: String,
    /// Per-request timeout in milliseconds
    pub timeout_ms: u64,
    /// HTTP/2 keep-alive interval in milliseconds
    pub keepalive_ms: u64,
    /// Max transactions per bundle (Jito hard limit = 5)
    pub max_bundle_size: usize,
}

impl Default for JitoGrpcConfig {
    fn default() -> Self {
        Self {
            primary_url: JITO_NY.to_string(),
            secondary_url: JITO_FRANKFURT.to_string(),
            timeout_ms: 5000,
            keepalive_ms: 10_000,
            max_bundle_size: 5,
        }
    }
}

// ── JSON-RPC types ───────────────────────────────────────────────────────────

#[derive(Serialize)]
struct BundleRequest {
    jsonrpc: &'static str,
    id: u64,
    method: &'static str,
    params: Vec<Vec<String>>,
}

#[derive(Deserialize, Debug)]
struct BundleResponse {
    result: Option<String>,
    error: Option<serde_json::Value>,
}

// ── Endpoint state ───────────────────────────────────────────────────────────

/// Wraps an HTTP client pinned to a specific block engine endpoint.
/// The reqwest Client maintains an internal connection pool with HTTP/2
/// multiplexing, so repeated requests to the same host reuse the existing
/// TCP+TLS connection.
struct Endpoint {
    client: HttpClient,
    url: String,
    /// Last successful submission time
    last_success: Option<Instant>,
    /// Consecutive failure count (for circuit-breaking)
    consecutive_failures: u32,
}

impl Endpoint {
    fn bundle_url(&self) -> String {
        format!("{}/api/v1/bundles", self.url.trim_end_matches('/'))
    }
}

// ── Client ───────────────────────────────────────────────────────────────────

pub struct JitoGrpcClient {
    primary: Arc<RwLock<Option<Endpoint>>>,
    secondary: Arc<RwLock<Option<Endpoint>>>,
    config: JitoGrpcConfig,

    // Atomic stats for monitoring
    pub bundles_sent: AtomicU64,
    pub bundles_failed: AtomicU64,
    pub primary_failures: AtomicU64,
    pub secondary_failures: AtomicU64,
    pub failovers: AtomicU64,
}

impl JitoGrpcClient {
    /// Create a new persistent client with connections to both block engines.
    pub async fn new(config: JitoGrpcConfig) -> Result<Self> {
        let primary_ep = Self::build_endpoint(
            &config.primary_url,
            config.timeout_ms,
            config.keepalive_ms,
        )
        .context("failed to build primary endpoint")?;

        let secondary_ep = Self::build_endpoint(
            &config.secondary_url,
            config.timeout_ms,
            config.keepalive_ms,
        )
        .context("failed to build secondary endpoint")?;

        info!(
            primary = %config.primary_url,
            secondary = %config.secondary_url,
            "Jito persistent HTTP/2 client initialized (dual endpoint)"
        );

        Ok(Self {
            primary: Arc::new(RwLock::new(Some(primary_ep))),
            secondary: Arc::new(RwLock::new(Some(secondary_ep))),
            config,
            bundles_sent: AtomicU64::new(0),
            bundles_failed: AtomicU64::new(0),
            primary_failures: AtomicU64::new(0),
            secondary_failures: AtomicU64::new(0),
            failovers: AtomicU64::new(0),
        })
    }

    /// Build a reqwest HTTP client configured for persistent HTTP/2 connections.
    ///
    /// Key optimizations:
    /// - `http2_prior_knowledge` not used (TLS-ALPN negotiates h2 automatically)
    /// - `http2_keep_alive_interval` sends PING frames to keep the connection warm
    /// - `pool_max_idle_per_host` keeps multiple connections ready
    /// - `tcp_nodelay(true)` disables Nagle's for minimum latency
    /// - `pool_idle_timeout` prevents stale connections
    fn build_endpoint(url: &str, timeout_ms: u64, keepalive_ms: u64) -> Result<Endpoint> {
        let client = HttpClient::builder()
            // HTTP/2 via TLS-ALPN (reqwest handles this with rustls/native-tls)
            .http2_prior_knowledge()
            // Request-level timeout
            .timeout(Duration::from_millis(timeout_ms))
            // Connection-level timeout
            .connect_timeout(Duration::from_millis(CONNECT_TIMEOUT_MS))
            // Keep-alive pings to prevent idle connection closure
            .http2_keep_alive_interval(Duration::from_millis(keepalive_ms))
            .http2_keep_alive_timeout(Duration::from_secs(20))
            .http2_keep_alive_while_idle(true)
            // Connection pool settings
            .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
            .pool_idle_timeout(Duration::from_secs(POOL_IDLE_TIMEOUT_SECS))
            // TCP optimizations
            .tcp_nodelay(true)
            // Disable redirects for API calls
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("failed to build reqwest HTTP/2 client")?;

        Ok(Endpoint {
            client,
            url: url.to_string(),
            last_success: None,
            consecutive_failures: 0,
        })
    }

    /// Warm up both connections by sending a lightweight request.
    /// Call this at startup so the first real bundle doesn't pay TLS cost.
    pub async fn warmup(&self) -> Result<()> {
        let start = Instant::now();

        // Fire warmup requests to both endpoints in parallel
        let (r1, r2) = tokio::join!(
            self.warmup_endpoint(&self.primary, "primary"),
            self.warmup_endpoint(&self.secondary, "secondary"),
        );

        if let Err(e) = r1 {
            warn!("primary warmup failed: {e:#}");
        }
        if let Err(e) = r2 {
            warn!("secondary warmup failed: {e:#}");
        }

        info!(elapsed_ms = start.elapsed().as_millis(), "connection warmup complete");
        Ok(())
    }

    async fn warmup_endpoint(
        &self,
        lock: &Arc<RwLock<Option<Endpoint>>>,
        label: &str,
    ) -> Result<()> {
        let guard = lock.read().await;
        if let Some(ep) = guard.as_ref() {
            // Send a getTipAccounts request — lightweight and always valid
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "getTipAccounts",
                "params": []
            });

            let resp = ep
                .client
                .post(ep.bundle_url())
                .json(&body)
                .send()
                .await
                .with_context(|| format!("{label} warmup request failed"))?;

            debug!(
                label,
                status = %resp.status(),
                "warmup response received"
            );
        }
        Ok(())
    }

    /// Submit a single base64-encoded transaction as a Jito bundle.
    ///
    /// Strategy: try primary first, on failure fall back to secondary.
    /// Non-blocking reconnection is triggered on persistent failures.
    pub async fn submit_bundle(&self, tx_base64: &str) -> Result<String> {
        self.submit_multi_bundle(&[tx_base64.to_string()]).await
    }

    /// Submit a multi-transaction bundle (up to `max_bundle_size` txs).
    ///
    /// Failover logic:
    /// 1. Try primary endpoint
    /// 2. If primary fails, try secondary
    /// 3. If both fail, return the primary error (more useful for debugging)
    pub async fn submit_multi_bundle(&self, txs_base64: &[String]) -> Result<String> {
        if txs_base64.is_empty() {
            bail!("cannot submit empty bundle");
        }
        if txs_base64.len() > self.config.max_bundle_size {
            bail!(
                "bundle size {} exceeds Jito limit {}",
                txs_base64.len(),
                self.config.max_bundle_size
            );
        }

        let body = BundleRequest {
            jsonrpc: "2.0",
            id: 1,
            method: "sendBundle",
            params: vec![txs_base64.to_vec()],
        };
        let body_bytes = serde_json::to_vec(&body).context("failed to serialize bundle request")?;

        // Try primary
        let primary_result = self
            .send_to_endpoint(&self.primary, &body_bytes, "primary")
            .await;

        match primary_result {
            Ok(bundle_id) => {
                self.bundles_sent.fetch_add(1, Ordering::Relaxed);
                return Ok(bundle_id);
            }
            Err(primary_err) => {
                self.primary_failures.fetch_add(1, Ordering::Relaxed);
                self.failovers.fetch_add(1, Ordering::Relaxed);
                warn!(
                    err = %primary_err,
                    "primary endpoint failed, falling back to secondary"
                );

                // Try secondary
                match self
                    .send_to_endpoint(&self.secondary, &body_bytes, "secondary")
                    .await
                {
                    Ok(bundle_id) => {
                        self.bundles_sent.fetch_add(1, Ordering::Relaxed);
                        // Trigger background reconnect of primary
                        self.schedule_reconnect_primary();
                        return Ok(bundle_id);
                    }
                    Err(secondary_err) => {
                        self.secondary_failures.fetch_add(1, Ordering::Relaxed);
                        self.bundles_failed.fetch_add(1, Ordering::Relaxed);
                        error!(
                            primary_err = %primary_err,
                            secondary_err = %secondary_err,
                            "both endpoints failed"
                        );
                        // Return primary error as it's typically more informative
                        bail!(
                            "both Jito endpoints failed. primary: {primary_err}, secondary: {secondary_err}"
                        );
                    }
                }
            }
        }
    }

    /// Send a pre-serialized JSON body to a specific endpoint.
    async fn send_to_endpoint(
        &self,
        lock: &Arc<RwLock<Option<Endpoint>>>,
        body: &[u8],
        label: &str,
    ) -> Result<String> {
        // Read lock — fast path, no contention
        let guard = lock.read().await;
        let ep = guard
            .as_ref()
            .with_context(|| format!("{label} endpoint not connected"))?;

        let start = Instant::now();

        let resp = ep
            .client
            .post(ep.bundle_url())
            .header("Content-Type", "application/json")
            .body(body.to_vec())
            .send()
            .await
            .with_context(|| format!("{label} bundle request failed"))?;

        let status = resp.status();
        let elapsed = start.elapsed();

        debug!(
            label,
            status = %status,
            elapsed_ms = elapsed.as_millis(),
            "bundle response"
        );

        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("{label} returned HTTP {status}: {text}");
        }

        let parsed: BundleResponse = resp
            .json::<BundleResponse>()
            .await
            .with_context(|| format!("failed to parse {label} bundle response"))?;

        if let Some(err) = parsed.error {
            bail!("{label} RPC error: {err}");
        }

        // Drop read lock before acquiring write
        drop(guard);

        // Update success state
        if let Ok(mut guard) = lock.try_write() {
            if let Some(ep) = guard.as_mut() {
                ep.last_success = Some(Instant::now());
                ep.consecutive_failures = 0;
            }
        }

        parsed
            .result
            .with_context(|| format!("{label} bundle response missing 'result' field"))
    }

    /// Schedule a background reconnection of the primary endpoint.
    /// Non-blocking — spawns a tokio task.
    fn schedule_reconnect_primary(&self) {
        let primary = Arc::clone(&self.primary);
        let url = self.config.primary_url.clone();
        let timeout_ms = self.config.timeout_ms;
        let keepalive_ms = self.config.keepalive_ms;

        tokio::spawn(async move {
            Self::reconnect_endpoint(primary, &url, timeout_ms, keepalive_ms, "primary").await;
        });
    }

    /// Reconnect the primary endpoint. Exposed for manual recovery.
    pub async fn reconnect_primary(&self) {
        Self::reconnect_endpoint(
            Arc::clone(&self.primary),
            &self.config.primary_url,
            self.config.timeout_ms,
            self.config.keepalive_ms,
            "primary",
        )
        .await;
    }

    /// Reconnect the secondary endpoint. Exposed for manual recovery.
    pub async fn reconnect_secondary(&self) {
        Self::reconnect_endpoint(
            Arc::clone(&self.secondary),
            &self.config.secondary_url,
            self.config.timeout_ms,
            self.config.keepalive_ms,
            "secondary",
        )
        .await;
    }

    /// Replace an endpoint's client with a fresh connection.
    async fn reconnect_endpoint(
        lock: Arc<RwLock<Option<Endpoint>>>,
        url: &str,
        timeout_ms: u64,
        keepalive_ms: u64,
        label: &str,
    ) {
        info!(label, url, "reconnecting endpoint");

        match Self::build_endpoint(url, timeout_ms, keepalive_ms) {
            Ok(new_ep) => {
                let mut guard = lock.write().await;
                *guard = Some(new_ep);
                info!(label, "endpoint reconnected");
            }
            Err(e) => {
                error!(label, err = %e, "failed to reconnect endpoint");
            }
        }
    }

    /// Get a snapshot of current stats.
    pub fn stats(&self) -> JitoGrpcStats {
        JitoGrpcStats {
            bundles_sent: self.bundles_sent.load(Ordering::Relaxed),
            bundles_failed: self.bundles_failed.load(Ordering::Relaxed),
            primary_failures: self.primary_failures.load(Ordering::Relaxed),
            secondary_failures: self.secondary_failures.load(Ordering::Relaxed),
            failovers: self.failovers.load(Ordering::Relaxed),
        }
    }
}

// ── Stats ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct JitoGrpcStats {
    pub bundles_sent: u64,
    pub bundles_failed: u64,
    pub primary_failures: u64,
    pub secondary_failures: u64,
    pub failovers: u64,
}

impl std::fmt::Display for JitoGrpcStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "sent={} failed={} primary_fail={} secondary_fail={} failovers={}",
            self.bundles_sent,
            self.bundles_failed,
            self.primary_failures,
            self.secondary_failures,
            self.failovers,
        )
    }
}
