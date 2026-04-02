//! RPC transaction submission with retry, confirmation tracking, and circuit breaker.
//!
//! Submits signed Solana transactions via standard JSON-RPC `sendTransaction`
//! as an alternative to Jito bundles. Includes:
//! - Configurable retry with exponential backoff
//! - Confirmation polling via `getSignatureStatuses`
//! - Circuit breaker: trips to Jito fallback after N consecutive failures
//! - Submission metrics (inclusion rate, cost tracking)

use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

// ── Configuration ────────────────────────────────────────────────────────────

/// Configuration for `RpcSender`.
#[derive(Debug, Clone)]
pub struct RpcSenderConfig {
    /// Priority fee in micro-lamports per compute unit. Default 1000 (~200K lam at 200K CU).
    pub priority_fee_microlamports: u64,
    /// Maximum send retries on retryable errors. Default 3.
    pub max_send_retries: u32,
    /// Delay between retries in milliseconds. Default 500.
    pub retry_delay_ms: u64,
    /// Confirmation polling timeout in milliseconds. Default 30_000.
    pub confirm_timeout_ms: u64,
    /// Skip preflight simulation for speed. Default true.
    pub skip_preflight: bool,
    /// Consecutive failures before tripping circuit breaker. Default 5.
    pub circuit_breaker_threshold: u32,
    /// Cooldown before resetting circuit breaker (ms). Default 120_000.
    pub circuit_breaker_cooldown_ms: u64,
    /// Jito fallback tip in lamports. Default 100_000.
    pub jito_fallback_tip: u64,
}

impl Default for RpcSenderConfig {
    fn default() -> Self {
        Self {
            priority_fee_microlamports: 1_000,
            max_send_retries: 3,
            retry_delay_ms: 500,
            confirm_timeout_ms: 30_000,
            skip_preflight: true,
            circuit_breaker_threshold: 5,
            circuit_breaker_cooldown_ms: 120_000,
            jito_fallback_tip: 100_000,
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
    pub jito_fallback_attempts: u64,
    pub jito_fallback_landed: u64,
    pub avg_confirm_latency_ms: f64,
    pub consecutive_failures: u32,
    pub total_priority_fees_lamports: u64,
    pub total_jito_tips_lamports: u64,
}

impl Default for SubmissionMetrics {
    fn default() -> Self {
        Self {
            rpc_attempts: 0,
            rpc_landed: 0,
            rpc_failed: 0,
            rpc_timed_out: 0,
            jito_fallback_attempts: 0,
            jito_fallback_landed: 0,
            avg_confirm_latency_ms: 0.0,
            consecutive_failures: 0,
            total_priority_fees_lamports: 0,
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

    /// Average cost (priority fees + Jito tips) per successfully landed transaction.
    pub fn cost_per_landed_tx(&self) -> f64 {
        let total_landed = self.rpc_landed + self.jito_fallback_landed;
        if total_landed == 0 {
            return 0.0;
        }
        let total_cost = self.total_priority_fees_lamports + self.total_jito_tips_lamports;
        total_cost as f64 / total_landed as f64
    }
}

// ── Circuit Breaker ──────────────────────────────────────────────────────────

/// Circuit breaker state for RPC → Jito fallback routing.
#[derive(Debug)]
pub enum CircuitState {
    /// RPC primary, working normally.
    Closed,
    /// RPC tripped — consecutive failures exceeded threshold. Jito fallback active.
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
    /// Transaction submitted but confirmation timed out.
    TimedOut { signature: String },
    /// Transaction failed (RPC error, simulation failure, etc.).
    Failed { error: String },
    /// Circuit breaker open — caller should use Jito submission.
    JitoFallback { bundle_id: Option<String> },
}

// ── RPC Sender ───────────────────────────────────────────────────────────────

/// Submits signed Solana transactions via standard RPC with retry and circuit breaker.
pub struct RpcSender {
    client: reqwest::Client,
    rpc_url: String,
    metrics: Arc<RwLock<SubmissionMetrics>>,
    circuit: Arc<RwLock<CircuitState>>,
    config: RpcSenderConfig,
}

impl RpcSender {
    /// Create a new `RpcSender` with the given RPC URL and configuration.
    pub fn new(rpc_url: String, config: RpcSenderConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            rpc_url,
            metrics: Arc::new(RwLock::new(SubmissionMetrics::default())),
            circuit: Arc::new(RwLock::new(CircuitState::default())),
            config,
        }
    }

    /// Submit a signed transaction with retry, confirmation polling, and circuit breaker.
    ///
    /// # Arguments
    /// - `tx_bytes` — serialized signed transaction (wire format)
    /// - `mint_str` — base58 mint address (for logging)
    /// - `label` — caller context label (for logging)
    pub async fn submit_tx(&self, tx_bytes: &[u8], mint_str: &str, label: &str) -> SubmitResult {
        // ── 1. Check circuit breaker state ───────────────────────────────
        {
            let mut circuit = self.circuit.write().await;
            match &*circuit {
                CircuitState::Open { since } => {
                    let elapsed_ms = since.elapsed().as_millis() as u64;
                    if elapsed_ms >= self.config.circuit_breaker_cooldown_ms {
                        // Cooldown expired — reset to Closed, try RPC again
                        tracing::info!(
                            mint = %mint_str,
                            cooldown_ms = elapsed_ms,
                            "[circuit_breaker] cooldown expired — resetting to Closed"
                        );
                        *circuit = CircuitState::Closed;
                        // Fall through to RPC path below
                    } else {
                        // Still in cooldown — return JitoFallback
                        let mut m = self.metrics.write().await;
                        m.jito_fallback_attempts += 1;
                        tracing::info!(
                            mint = %mint_str,
                            remaining_ms = self.config.circuit_breaker_cooldown_ms - elapsed_ms,
                            "[circuit_breaker] circuit OPEN — returning JitoFallback"
                        );
                        return SubmitResult::JitoFallback { bundle_id: None };
                    }
                }
                CircuitState::Closed => {
                    // Normal path — proceed with RPC
                }
            }
        }

        // ── 2. RPC primary path ──────────────────────────────────────────
        use base64::Engine as _;
        let tx_b64 = base64::engine::general_purpose::STANDARD.encode(tx_bytes);

        let mut last_error = String::new();

        for attempt in 0..=self.config.max_send_retries {
            if attempt > 0 {
                tokio::time::sleep(tokio::time::Duration::from_millis(
                    self.config.retry_delay_ms,
                ))
                .await;
                tracing::info!(
                    mint = %mint_str,
                    attempt,
                    "[rpc_send] retrying sendTransaction"
                );
            }

            // Increment attempts
            {
                let mut m = self.metrics.write().await;
                m.rpc_attempts += 1;
                m.total_priority_fees_lamports += self.config.priority_fee_microlamports / 5;
                // ~200K CU * microlamports / 1_000_000 = lamports.
                // Simplified: priority_fee_microlamports * 200_000 / 1_000_000 = fee / 5
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

            // POST to RPC
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

                // Retryable errors: blockhash not found, node behind, rate limit
                let retryable = err_msg.contains("Blockhash not found")
                    || err_msg.contains("Node is behind")
                    || err_msg.contains("Too many requests")
                    || err_msg.contains("Service unavailable");

                if retryable && attempt < self.config.max_send_retries {
                    last_error = err_msg.clone();
                    tracing::warn!(
                        mint = %mint_str,
                        attempt,
                        err = %err_msg,
                        "[rpc_send] retryable RPC error"
                    );
                    continue;
                }

                // Non-retryable or exhausted retries
                tracing::warn!(
                    mint = %mint_str,
                    attempt,
                    err = %err_msg,
                    "[rpc_send] sendTransaction failed"
                );
                let mut m = self.metrics.write().await;
                m.rpc_failed += 1;
                m.consecutive_failures += 1;
                self.maybe_trip_circuit(m.consecutive_failures, mint_str)
                    .await;
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
                "[rpc_send] transaction submitted — polling confirmation"
            );

            // ── 3. Poll getSignatureStatuses for confirmation ────────────
            let confirm_result = self
                .poll_confirmation(&signature, mint_str, send_start)
                .await;

            match confirm_result {
                ConfirmOutcome::Confirmed { latency_ms } => {
                    let mut m = self.metrics.write().await;
                    m.rpc_landed += 1;
                    m.consecutive_failures = 0;
                    // Update rolling average latency
                    let total_landed = m.rpc_landed;
                    m.avg_confirm_latency_ms = m.avg_confirm_latency_ms
                        + (latency_ms as f64 - m.avg_confirm_latency_ms) / total_landed as f64;
                    tracing::info!(
                        mint = %mint_str,
                        signature = %signature,
                        latency_ms,
                        label,
                        "[rpc_confirm] transaction LANDED"
                    );
                    return SubmitResult::Landed {
                        signature,
                        latency_ms,
                    };
                }
                ConfirmOutcome::TimedOut => {
                    let mut m = self.metrics.write().await;
                    m.rpc_timed_out += 1;
                    m.consecutive_failures += 1;
                    tracing::warn!(
                        mint = %mint_str,
                        signature = %signature,
                        timeout_ms = self.config.confirm_timeout_ms,
                        label,
                        "[rpc_confirm] confirmation TIMED OUT"
                    );
                    self.maybe_trip_circuit(m.consecutive_failures, mint_str)
                        .await;
                    return SubmitResult::TimedOut { signature };
                }
                ConfirmOutcome::Error { error } => {
                    // Transaction error (e.g. simulation fail after landing) — retryable
                    last_error = error.clone();
                    tracing::warn!(
                        mint = %mint_str,
                        signature = %signature,
                        err = %error,
                        attempt,
                        "[rpc_confirm] transaction error during confirmation"
                    );
                    // Don't return yet — retry the send if attempts remain
                    continue;
                }
            }
        }

        // Exhausted all retries
        {
            let mut m = self.metrics.write().await;
            m.rpc_failed += 1;
            m.consecutive_failures += 1;
            self.maybe_trip_circuit(m.consecutive_failures, mint_str)
                .await;
        }

        SubmitResult::Failed {
            error: format!(
                "exhausted {} retries: {}",
                self.config.max_send_retries, last_error
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
        let poll_interval = tokio::time::Duration::from_millis(500);
        let deadline =
            send_start + std::time::Duration::from_millis(self.config.confirm_timeout_ms);

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
                        "[rpc_confirm] getSignatureStatuses request failed"
                    );
                    continue; // Retry on next poll
                }
            };

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

            // result.value is an array — first element corresponds to our signature
            let status = &resp_json["result"]["value"][0];

            if status.is_null() {
                // Not yet processed — keep polling
                continue;
            }

            // Check for transaction error
            if let Some(err) = status.get("err") {
                if !err.is_null() {
                    return ConfirmOutcome::Error {
                        error: err.to_string(),
                    };
                }
            }

            // Check confirmation status
            if let Some(confirmation) = status["confirmationStatus"].as_str() {
                match confirmation {
                    "confirmed" | "finalized" => {
                        let latency_ms = send_start.elapsed().as_millis() as u64;
                        return ConfirmOutcome::Confirmed { latency_ms };
                    }
                    "processed" => {
                        // Processed but not yet confirmed — keep polling
                        continue;
                    }
                    _ => {
                        // Unknown status — keep polling
                        continue;
                    }
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
                    "[circuit_breaker] TRIPPED — switching to Jito fallback"
                );
                *circuit = CircuitState::Open {
                    since: Instant::now(),
                };
            }
        }
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

    /// Record a successful Jito fallback landing (called by the Jito submission path).
    pub async fn record_jito_landed(&self, tip_lamports: u64) {
        let mut m = self.metrics.write().await;
        m.jito_fallback_landed += 1;
        m.total_jito_tips_lamports += tip_lamports;
    }
}

// ── Internal confirmation outcome ────────────────────────────────────────────

/// Internal enum for confirmation polling result.
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
        assert_eq!(cfg.circuit_breaker_threshold, 5);
        assert_eq!(cfg.circuit_breaker_cooldown_ms, 120_000);
        assert_eq!(cfg.jito_fallback_tip, 100_000);
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
            rpc_landed: 5,
            jito_fallback_landed: 5,
            total_priority_fees_lamports: 500_000,
            total_jito_tips_lamports: 1_000_000,
            ..Default::default()
        };
        // (500K + 1M) / 10 = 150K per landed tx
        assert!((m.cost_per_landed_tx() - 150_000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_circuit_state_default_is_closed() {
        let cs = CircuitState::default();
        assert!(matches!(cs, CircuitState::Closed));
    }

    #[test]
    fn test_rpc_sender_new() {
        let sender = RpcSender::new(
            "https://example.com".to_string(),
            RpcSenderConfig::default(),
        );
        assert_eq!(sender.rpc_url, "https://example.com");
    }
}
