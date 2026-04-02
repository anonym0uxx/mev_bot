//! RPC transaction submission with retry, confirmation tracking, rate limiting, and circuit breaker.
//!
//! Submits signed Solana transactions via standard JSON-RPC `sendTransaction`
//! as the PRIMARY path (not Jito bundles). Includes:
//! - Token bucket rate limiter (prevents burst-induced 429s)
//! - Configurable retry with backoff on 429/rate-limit responses
//! - Retry-After header support for server-guided backoff
//! - Confirmation polling via `getSignatureStatuses`
//! - Circuit breaker: backs off with increasing delay, never falls back to Jito
//! - Pre-flight circuit breaker check to skip sends during cooldown
//! - Dedicated send endpoint (`send_rpc_url`) isolated from read traffic
//! - Submission metrics (inclusion rate, cost tracking)

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

// ── Configuration ────────────────────────────────────────────────────────────

/// Configuration for `RpcSender`.
#[derive(Debug, Clone)]
pub struct RpcSenderConfig {
    /// Priority fee in micro-lamports per compute unit. Default 1000 (~200K lam at 200K CU).
    pub priority_fee_microlamports: u64,
    /// Maximum send retries on retryable errors. Default 3.
    pub max_send_retries: u32,
    /// Base delay between retries in milliseconds. Doubles on 429. Default 500.
    pub retry_delay_ms: u64,
    /// Confirmation polling timeout in milliseconds. Default 30_000.
    pub confirm_timeout_ms: u64,
    /// Skip preflight simulation for speed. Default true.
    pub skip_preflight: bool,
    /// Consecutive failures before circuit breaker trips. Default 3 (was 5).
    pub circuit_breaker_threshold: u32,
    /// Cooldown before resetting circuit breaker (ms). Default 15_000 (was 120_000).
    pub circuit_breaker_cooldown_ms: u64,
    /// (Legacy, unused) Jito fallback tip in lamports. Default 100_000.
    pub jito_fallback_tip: u64,
    /// Minimum interval between sendTransaction calls (ms). Default 200.
    /// Prevents burst-induced 429s on startup or rapid entry/exit.
    pub min_send_interval_ms: u64,
}

impl Default for RpcSenderConfig {
    fn default() -> Self {
        Self {
            priority_fee_microlamports: 1_000,
            max_send_retries: 3,
            retry_delay_ms: 500,
            confirm_timeout_ms: 30_000,
            skip_preflight: true,
            circuit_breaker_threshold: 3,
            circuit_breaker_cooldown_ms: 15_000,
            jito_fallback_tip: 100_000,
            min_send_interval_ms: 200,
        }
    }
}

impl RpcSenderConfig {
    /// Create from the deserialized momentum config section.
    pub fn from_momentum_config(cfg: &super::config::RpcSenderConfig) -> Self {
        Self {
            priority_fee_microlamports: cfg.priority_fee_microlamports,
            max_send_retries: cfg.max_send_retries,
            retry_delay_ms: cfg.retry_delay_ms,
            confirm_timeout_ms: cfg.confirm_timeout_ms,
            skip_preflight: cfg.skip_preflight,
            circuit_breaker_threshold: cfg.circuit_breaker_threshold,
            circuit_breaker_cooldown_ms: cfg.circuit_breaker_cooldown_ms,
            jito_fallback_tip: cfg.jito_fallback_tip,
            min_send_interval_ms: 200,
        }
    }
}

// ── Metrics ──────────────────────────────────────────────────────────────────

/// Tracks submission statistics for monitoring and adaptive routing.
#[derive(Debug, Clone)]
pub struct SubmissionMetrics {
    pub rpc_attempts: u64,
    pub rpc_landed: u64,
    pub rpc_failed: u64,
    pub rpc_timed_out: u64,
    pub rpc_rate_limited: u64,
    pub avg_confirm_latency_ms: f64,
    pub consecutive_failures: u32,
    pub total_priority_fees_lamports: u64,
    // Legacy fields kept for API compat
    pub jito_fallback_attempts: u64,
    pub jito_fallback_landed: u64,
    pub total_jito_tips_lamports: u64,
}

impl Default for SubmissionMetrics {
    fn default() -> Self {
        Self {
            rpc_attempts: 0,
            rpc_landed: 0,
            rpc_failed: 0,
            rpc_timed_out: 0,
            rpc_rate_limited: 0,
            avg_confirm_latency_ms: 0.0,
            consecutive_failures: 0,
            total_priority_fees_lamports: 0,
            jito_fallback_attempts: 0,
            jito_fallback_landed: 0,
            total_jito_tips_lamports: 0,
        }
    }
}

impl SubmissionMetrics {
    /// Fraction of RPC attempts that landed on-chain.
    pub fn inclusion_rate(&self) -> f64 {
        if self.rpc_attempts == 0 {
            return 0.0;
        }
        self.rpc_landed as f64 / self.rpc_attempts as f64
    }

    /// Average cost (priority fees) per successfully landed transaction.
    pub fn cost_per_landed_tx(&self) -> f64 {
        if self.rpc_landed == 0 {
            return 0.0;
        }
        self.total_priority_fees_lamports as f64 / self.rpc_landed as f64
    }
}

// ── Circuit Breaker ──────────────────────────────────────────────────────────

/// Circuit breaker state — controls backoff delay, NOT routing to Jito.
/// When open, submit_tx skips immediately instead of attempting and failing.
#[derive(Debug)]
pub enum CircuitState {
    /// RPC primary, working normally.
    Closed,
    /// RPC rate limited — back off and wait before retrying.
    Open { since: Instant },
}

impl Default for CircuitState {
    fn default() -> Self {
        Self::Closed
    }
}

// ── Submit Result ────────────────────────────────────────────────────────────

/// Outcome of a transaction submission attempt.
#[derive(Debug)]
pub enum SubmitResult {
    /// Transaction confirmed on-chain.
    Landed { signature: String, latency_ms: u64 },
    /// Transaction submitted but confirmation timed out (may still land).
    TimedOut { signature: String },
    /// Transaction failed after all retries.
    Failed { error: String },
    /// Transaction skipped — circuit breaker is open (cooldown in progress).
    CircuitOpen { remaining_ms: u64 },
}

// ── Rate Limiter ─────────────────────────────────────────────────────────────

/// Simple token bucket rate limiter. Ensures minimum interval between sends.
struct RateLimiter {
    last_send: RwLock<Instant>,
    min_interval: Duration,
}

impl RateLimiter {
    fn new(min_interval_ms: u64) -> Self {
        Self {
            // Start in the past so first send goes immediately
            last_send: RwLock::new(Instant::now() - Duration::from_secs(10)),
            min_interval: Duration::from_millis(min_interval_ms),
        }
    }

    /// Wait until the minimum interval has passed since last send.
    async fn acquire(&self) {
        let now = Instant::now();
        let last = *self.last_send.read().await;
        let elapsed = now.duration_since(last);
        if elapsed < self.min_interval {
            let wait = self.min_interval - elapsed;
            tracing::debug!(
                wait_ms = wait.as_millis() as u64,
                "[rate_limiter] throttling — waiting"
            );
            tokio::time::sleep(wait).await;
        }
        *self.last_send.write().await = Instant::now();
    }
}

// ── RPC Sender ───────────────────────────────────────────────────────────────

/// Submits signed Solana transactions via standard RPC with rate limiting,
/// retry, confirmation polling, and circuit breaker.
///
/// Uses a dedicated `send_rpc_url` for `sendTransaction` calls, isolated from
/// read traffic. The `rpc_url` is used for `getSignatureStatuses` (confirmation
/// polling) and can optionally point to the same endpoint.
///
/// NO Jito fallback. RPC is the only path.
pub struct RpcSender {
    client: reqwest::Client,
    /// URL used for getSignatureStatuses (confirmation polling).
    rpc_url: String,
    /// Dedicated URL for sendTransaction — isolated from read traffic.
    /// If not explicitly provided, defaults to the same as `rpc_url`.
    send_rpc_url: String,
    metrics: Arc<RwLock<SubmissionMetrics>>,
    circuit: Arc<RwLock<CircuitState>>,
    rate_limiter: RateLimiter,
    config: RpcSenderConfig,
}

impl RpcSender {
    /// Create a new `RpcSender` with a single URL for both send and confirm.
    pub fn new(rpc_url: String, config: RpcSenderConfig) -> Self {
        let send_rpc_url = rpc_url.clone();
        Self::with_send_url(rpc_url, send_rpc_url, config)
    }

    /// Create a new `RpcSender` with a dedicated send endpoint.
    ///
    /// - `rpc_url`: Used for `getSignatureStatuses` (confirmation polling).
    /// - `send_rpc_url`: Dedicated URL for `sendTransaction` calls, isolated
    ///   from read traffic to avoid 429 contention.
    pub fn with_send_url(rpc_url: String, send_rpc_url: String, config: RpcSenderConfig) -> Self {
        let rate_limiter = RateLimiter::new(config.min_send_interval_ms);
        if rpc_url != send_rpc_url {
            tracing::info!(
                send_url = %send_rpc_url,
                confirm_url = %rpc_url,
                "[rpc_sender] using dedicated send endpoint"
            );
        }
        Self {
            client: reqwest::Client::new(),
            rpc_url,
            send_rpc_url,
            metrics: Arc::new(RwLock::new(SubmissionMetrics::default())),
            circuit: Arc::new(RwLock::new(CircuitState::default())),
            rate_limiter,
            config,
        }
    }

    /// Submit a signed transaction. Rate-limited, retried, confirmed.
    ///
    /// When the circuit breaker is open (rate limited), returns `CircuitOpen`
    /// immediately instead of waiting — avoids wasting retry budget on a
    /// known-bad endpoint.
    pub async fn submit_tx(&self, tx_bytes: &[u8], mint_str: &str, label: &str) -> SubmitResult {
        let submit_start = Instant::now();

        // ── 0. Pre-flight: skip immediately if circuit breaker is open ───
        // With the rate gate protecting us, 429s should be rare. If the
        // circuit is open, don't waste time — return immediately so the
        // caller can decide (drop, queue, or wait).
        {
            let circuit = self.circuit.read().await;
            if let CircuitState::Open { since } = &*circuit {
                let elapsed_ms = since.elapsed().as_millis() as u64;
                if elapsed_ms < self.config.circuit_breaker_cooldown_ms {
                    let remaining_ms = self.config.circuit_breaker_cooldown_ms - elapsed_ms;
                    tracing::warn!(
                        mint = %mint_str,
                        remaining_ms,
                        "[rpc_send] ⏸️  circuit breaker OPEN — skipping send"
                    );
                    return SubmitResult::CircuitOpen { remaining_ms };
                }
                // Cooldown expired — fall through to reset below
            }
        }

        // ── 1. Circuit breaker: reset if cooldown has expired ────────────
        {
            let circuit = self.circuit.read().await;
            if let CircuitState::Open { since } = &*circuit {
                let elapsed_ms = since.elapsed().as_millis() as u64;
                if elapsed_ms >= self.config.circuit_breaker_cooldown_ms {
                    drop(circuit);
                    let mut circuit = self.circuit.write().await;
                    if matches!(&*circuit, CircuitState::Open { .. }) {
                        tracing::info!(
                            mint = %mint_str,
                            cooldown_ms = self.config.circuit_breaker_cooldown_ms,
                            "[circuit_breaker] cooldown complete — resetting to Closed"
                        );
                        *circuit = CircuitState::Closed;
                        let mut m = self.metrics.write().await;
                        m.consecutive_failures = 0;
                    }
                }
            }
        }

        // ── 2. Rate limit: wait for minimum interval ─────────────────────
        self.rate_limiter.acquire().await;

        // ── 3. RPC send + retry loop ─────────────────────────────────────
        use base64::Engine as _;
        let tx_b64 = base64::engine::general_purpose::STANDARD.encode(tx_bytes);

        let mut last_error = String::new();
        let mut retry_delay = self.config.retry_delay_ms;
        let mut had_429 = false;

        for attempt in 0..=self.config.max_send_retries {
            if attempt > 0 {
                // Exponential backoff for retries (especially on 429)
                tracing::info!(
                    mint = %mint_str,
                    attempt,
                    delay_ms = retry_delay,
                    "[rpc_send] retrying after backoff"
                );
                tokio::time::sleep(Duration::from_millis(retry_delay)).await;
                // Only apply exponential backoff if retry_delay wasn't already
                // set by Retry-After header (in which case it's server-guided)
                retry_delay = (retry_delay * 2).min(5_000); // Cap at 5s
            }

            // Track attempt
            {
                let mut m = self.metrics.write().await;
                m.rpc_attempts += 1;
                // Estimate priority fee cost (priority_fee_microlamports * 200K CU / 1M)
                m.total_priority_fees_lamports += self.config.priority_fee_microlamports / 5;
            }

            // Build JSON-RPC request
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "sendTransaction",
                "params": [
                    tx_b64,
                    {
                        "encoding": "base64",
                        "skipPreflight": self.config.skip_preflight,
                        "maxRetries": 0
                    }
                ]
            });

            let send_start = Instant::now();

            // POST to dedicated send endpoint
            let resp = match self
                .client
                .post(&self.send_rpc_url)
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    last_error = format!("reqwest error: {e}");
                    tracing::warn!(
                        mint = %mint_str,
                        attempt,
                        err = %e,
                        "[rpc_send] HTTP request failed"
                    );
                    continue;
                }
            };

            // Check HTTP status for 429 before parsing JSON
            let http_status = resp.status();
            if http_status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                had_429 = true;
                let mut m = self.metrics.write().await;
                m.rpc_rate_limited += 1;
                drop(m);

                // Parse Retry-After header if present (value in seconds)
                let retry_after_ms = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<f64>().ok())
                    .map(|secs| (secs * 1000.0) as u64);

                if let Some(ra_ms) = retry_after_ms {
                    // Server told us exactly how long to wait — use it
                    tracing::warn!(
                        mint = %mint_str,
                        attempt,
                        retry_after_ms = ra_ms,
                        "[rpc_send] HTTP 429 — honoring Retry-After header"
                    );
                    retry_delay = ra_ms;
                } else {
                    // No Retry-After header — fall back to exponential backoff
                    tracing::warn!(
                        mint = %mint_str,
                        attempt,
                        backoff_ms = retry_delay,
                        "[rpc_send] HTTP 429 — no Retry-After, using exponential backoff"
                    );
                }

                last_error = "HTTP 429 Too Many Requests".to_string();
                continue;
            }

            let resp_json: serde_json::Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    last_error = format!("response parse error: {e}");
                    tracing::warn!(
                        mint = %mint_str,
                        attempt,
                        err = %e,
                        "[rpc_send] failed to parse response"
                    );
                    continue;
                }
            };

            // Check for RPC-level error
            if let Some(err_obj) = resp_json.get("error") {
                let err_msg = err_obj.to_string();

                let is_rate_limit = err_msg.contains("Too many requests")
                    || err_msg.contains("too many requests")
                    || err_msg.contains("Service unavailable");

                if is_rate_limit {
                    had_429 = true;
                    let mut m = self.metrics.write().await;
                    m.rpc_rate_limited += 1;
                    drop(m);
                }

                // Retryable errors
                let retryable = is_rate_limit
                    || err_msg.contains("Blockhash not found")
                    || err_msg.contains("Node is behind");

                if retryable && attempt < self.config.max_send_retries {
                    last_error = err_msg;
                    tracing::warn!(
                        mint = %mint_str,
                        attempt,
                        "[rpc_send] retryable error — will backoff"
                    );
                    continue;
                }

                // Non-retryable or exhausted retries
                let total_ms = submit_start.elapsed().as_millis() as u64;
                tracing::warn!(
                    mint = %mint_str,
                    attempt,
                    total_ms,
                    err = %err_msg,
                    "[rpc_send] ❌ TX FAILED after {total_ms}ms mint={mint} err={err}",
                    total_ms = total_ms,
                    mint = mint_str,
                    err = err_msg,
                );
                let mut m = self.metrics.write().await;
                m.rpc_failed += 1;
                m.consecutive_failures += 1;
                let cf = m.consecutive_failures;
                drop(m);
                self.maybe_trip_circuit(cf, mint_str).await;
                return SubmitResult::Failed { error: err_msg };
            }

            // Extract signature from result
            let signature = match resp_json["result"].as_str() {
                Some(sig) => sig.to_string(),
                None => {
                    last_error = format!("unexpected result format: {}", resp_json);
                    tracing::warn!(
                        mint = %mint_str,
                        resp = %resp_json,
                        "[rpc_send] no signature in result"
                    );
                    continue;
                }
            };

            tracing::info!(
                mint = %mint_str,
                signature = %signature,
                attempt,
                label,
                "[rpc_send] TX submitted — polling confirmation"
            );

            // ── 4. Poll getSignatureStatuses for confirmation ────────────
            let confirm_result = self
                .poll_confirmation(&signature, mint_str, send_start)
                .await;

            match confirm_result {
                ConfirmOutcome::Confirmed { latency_ms } => {
                    let mut m = self.metrics.write().await;
                    m.rpc_landed += 1;
                    m.consecutive_failures = 0;
                    let total_landed = m.rpc_landed;
                    m.avg_confirm_latency_ms = m.avg_confirm_latency_ms
                        + (latency_ms as f64 - m.avg_confirm_latency_ms) / total_landed as f64;
                    drop(m);
                    // Reset circuit breaker on success
                    let mut circuit = self.circuit.write().await;
                    *circuit = CircuitState::Closed;

                    if had_429 {
                        tracing::info!(
                            mint = %mint_str,
                            signature = %signature,
                            latency_ms,
                            attempt,
                            label,
                            "[rpc_send] ✅ TX landed after {attempts} retries mint={mint}",
                            attempts = attempt,
                            mint = mint_str,
                        );
                    } else {
                        tracing::info!(
                            mint = %mint_str,
                            signature = %signature,
                            latency_ms,
                            label,
                            "[rpc_confirm] TX LANDED ✅"
                        );
                    }
                    return SubmitResult::Landed {
                        signature,
                        latency_ms,
                    };
                }
                ConfirmOutcome::TimedOut => {
                    let mut m = self.metrics.write().await;
                    m.rpc_timed_out += 1;
                    m.consecutive_failures += 1;
                    let cf = m.consecutive_failures;
                    drop(m);
                    tracing::warn!(
                        mint = %mint_str,
                        signature = %signature,
                        timeout_ms = self.config.confirm_timeout_ms,
                        label,
                        "[rpc_confirm] confirmation TIMED OUT"
                    );
                    self.maybe_trip_circuit(cf, mint_str).await;
                    return SubmitResult::TimedOut { signature };
                }
                ConfirmOutcome::Error { error } => {
                    last_error = error.clone();
                    tracing::warn!(
                        mint = %mint_str,
                        signature = %signature,
                        err = %error,
                        attempt,
                        "[rpc_confirm] TX error — will retry"
                    );
                    continue;
                }
            }
        }

        // Exhausted all retries
        let total_ms = submit_start.elapsed().as_millis() as u64;
        {
            let mut m = self.metrics.write().await;
            m.rpc_failed += 1;
            m.consecutive_failures += 1;
            let cf = m.consecutive_failures;
            drop(m);
            self.maybe_trip_circuit(cf, mint_str).await;
        }

        tracing::warn!(
            mint = %mint_str,
            total_ms,
            retries = self.config.max_send_retries,
            err = %last_error,
            "[rpc_send] ❌ TX FAILED after {total_ms}ms mint={mint} err={err}",
            total_ms = total_ms,
            mint = mint_str,
            err = last_error,
        );

        SubmitResult::Failed {
            error: format!(
                "exhausted {} retries in {}ms: {}",
                self.config.max_send_retries, total_ms, last_error
            ),
        }
    }

    /// Poll `getSignatureStatuses` until confirmed, timed out, or error.
    async fn poll_confirmation(
        &self,
        signature: &str,
        mint_str: &str,
        send_start: Instant,
    ) -> ConfirmOutcome {
        let poll_interval = Duration::from_millis(500);
        let deadline =
            send_start + Duration::from_millis(self.config.confirm_timeout_ms);

        loop {
            if Instant::now() >= deadline {
                return ConfirmOutcome::TimedOut;
            }

            tokio::time::sleep(poll_interval).await;

            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "getSignatureStatuses",
                "params": [
                    [signature],
                    { "searchTransactionHistory": false }
                ]
            });

            // Use the main rpc_url for confirmation polling (same endpoint as send
            // in the current setup; can be split to a different endpoint if needed)
            let resp = match self
                .client
                .post(&self.rpc_url)
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        mint = %mint_str,
                        err = %e,
                        "[rpc_confirm] getSignatureStatuses failed — retrying"
                    );
                    continue;
                }
            };

            // Handle 429 on confirmation polling gracefully
            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let retry_after_ms = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<f64>().ok())
                    .map(|secs| (secs * 1000.0) as u64)
                    .unwrap_or(1000);
                tracing::warn!(
                    mint = %mint_str,
                    retry_after_ms,
                    "[rpc_confirm] 429 on getSignatureStatuses — backing off"
                );
                tokio::time::sleep(Duration::from_millis(retry_after_ms)).await;
                continue;
            }

            let resp_json: serde_json::Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        mint = %mint_str,
                        err = %e,
                        "[rpc_confirm] failed to parse status response"
                    );
                    continue;
                }
            };

            let status = &resp_json["result"]["value"][0];

            if status.is_null() {
                continue; // Not yet processed
            }

            if let Some(err) = status.get("err") {
                if !err.is_null() {
                    return ConfirmOutcome::Error {
                        error: err.to_string(),
                    };
                }
            }

            if let Some(confirmation) = status["confirmationStatus"].as_str() {
                match confirmation {
                    "confirmed" | "finalized" => {
                        let latency_ms = send_start.elapsed().as_millis() as u64;
                        return ConfirmOutcome::Confirmed { latency_ms };
                    }
                    "processed" => continue,
                    _ => continue,
                }
            }
        }
    }

    /// Trip the circuit breaker if consecutive failures exceed threshold.
    async fn maybe_trip_circuit(&self, consecutive_failures: u32, mint_str: &str) {
        if consecutive_failures >= self.config.circuit_breaker_threshold {
            let mut circuit = self.circuit.write().await;
            if matches!(*circuit, CircuitState::Closed) {
                tracing::warn!(
                    mint = %mint_str,
                    consecutive_failures,
                    threshold = self.config.circuit_breaker_threshold,
                    cooldown_ms = self.config.circuit_breaker_cooldown_ms,
                    "[circuit_breaker] TRIPPED — backing off for {}ms (threshold={})",
                    self.config.circuit_breaker_cooldown_ms,
                    self.config.circuit_breaker_threshold,
                );
                *circuit = CircuitState::Open {
                    since: Instant::now(),
                };
            }
        }
    }

    /// Check if the circuit breaker is currently open.
    /// Returns `Some(remaining_ms)` if open, `None` if closed.
    pub async fn circuit_remaining_ms(&self) -> Option<u64> {
        let circuit = self.circuit.read().await;
        if let CircuitState::Open { since } = &*circuit {
            let elapsed_ms = since.elapsed().as_millis() as u64;
            if elapsed_ms < self.config.circuit_breaker_cooldown_ms {
                return Some(self.config.circuit_breaker_cooldown_ms - elapsed_ms);
            }
        }
        None
    }

    /// Get a snapshot of current submission metrics.
    pub async fn metrics(&self) -> SubmissionMetrics {
        self.metrics.read().await.clone()
    }

    /// Get current circuit breaker state as a string for monitoring.
    pub async fn circuit_state_str(&self) -> &'static str {
        let circuit = self.circuit.read().await;
        match &*circuit {
            CircuitState::Closed => "closed",
            CircuitState::Open { .. } => "open",
        }
    }
}

// ── Internal confirmation outcome ────────────────────────────────────────────

enum ConfirmOutcome {
    Confirmed { latency_ms: u64 },
    TimedOut,
    Error { error: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let cfg = RpcSenderConfig::default();
        assert_eq!(cfg.priority_fee_microlamports, 1_000);
        assert_eq!(cfg.max_send_retries, 3);
        assert_eq!(cfg.retry_delay_ms, 500);
        assert_eq!(cfg.confirm_timeout_ms, 30_000);
        assert!(cfg.skip_preflight);
        // Updated defaults: faster trip, shorter cooldown
        assert_eq!(cfg.circuit_breaker_threshold, 3);
        assert_eq!(cfg.circuit_breaker_cooldown_ms, 15_000);
        assert_eq!(cfg.min_send_interval_ms, 200);
    }

    #[test]
    fn test_metrics_inclusion_rate_zero() {
        let m = SubmissionMetrics::default();
        assert!((m.inclusion_rate() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_metrics_inclusion_rate() {
        let m = SubmissionMetrics {
            rpc_attempts: 10,
            rpc_landed: 7,
            ..Default::default()
        };
        assert!((m.inclusion_rate() - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn test_metrics_cost_per_landed_tx_zero() {
        let m = SubmissionMetrics::default();
        assert!((m.cost_per_landed_tx() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_metrics_cost_per_landed_tx() {
        let m = SubmissionMetrics {
            rpc_landed: 10,
            total_priority_fees_lamports: 500_000,
            ..Default::default()
        };
        assert!((m.cost_per_landed_tx() - 50_000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_circuit_state_default_is_closed() {
        let cs = CircuitState::default();
        assert!(matches!(cs, CircuitState::Closed));
    }

    #[test]
    fn test_rpc_sender_new_single_url() {
        let sender = RpcSender::new(
            "https://example.com".to_string(),
            RpcSenderConfig::default(),
        );
        assert_eq!(sender.rpc_url, "https://example.com");
        assert_eq!(sender.send_rpc_url, "https://example.com");
    }

    #[test]
    fn test_rpc_sender_with_send_url() {
        let sender = RpcSender::with_send_url(
            "https://confirm.example.com".to_string(),
            "https://send.example.com".to_string(),
            RpcSenderConfig::default(),
        );
        assert_eq!(sender.rpc_url, "https://confirm.example.com");
        assert_eq!(sender.send_rpc_url, "https://send.example.com");
    }

    #[tokio::test]
    async fn test_rate_limiter_no_block_first_call() {
        let rl = RateLimiter::new(200);
        let start = Instant::now();
        rl.acquire().await;
        assert!(start.elapsed().as_millis() < 50); // Should not block
    }

    #[tokio::test]
    async fn test_circuit_remaining_ms_closed() {
        let sender = RpcSender::new(
            "https://example.com".to_string(),
            RpcSenderConfig::default(),
        );
        assert!(sender.circuit_remaining_ms().await.is_none());
    }
}