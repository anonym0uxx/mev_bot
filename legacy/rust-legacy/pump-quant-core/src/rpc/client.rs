//! Multi-endpoint RPC client with priority-based rate limiting.
//!
//! Routes RPC calls to the correct endpoint based on method, and applies
//! the appropriate rate limiter with the correct priority tier.
//!
//! See `RPC-RATE-LIMIT-SPEC.md` §2.2 (Endpoint Assignment) and §4 (Endpoint Routing Layer).
//!
//! # Usage
//!
//! ```rust,ignore
//! let config = RpcClientConfig::from_env()?;
//! let client = Arc::new(RpcClient::new(config));
//!
//! // Before every RPC call:
//! client.acquire(RpcMethod::SendTransaction).await?;
//! let url = client.url_for(RpcMethod::SendTransaction);
//! let resp = http.post(url).json(&body).send().await?;
//! ```

use std::sync::Arc;
use super::rate_limiter::{AcquireResult, Priority, RateLimiter, RateLimiterConfig, RateLimiterStats};

// Re-export for consumer convenience — callers need AcquireError to match on acquire() results.
pub use super::rate_limiter::AcquireError;

// ─── RPC Method Enum ────────────────────────────────────────────────────────

/// Every RPC method used in the codebase.
///
/// Determines both endpoint routing and priority tier.
/// See spec §1.1 for the full call inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RpcMethod {
    /// `sendTransaction` → HELIUS_SEND_URL, Critical priority.
    /// Source: `rpc_sender.rs` — buy/sell transactions.
    SendTransaction,

    /// `getSignatureStatuses` → HELIUS_SEND_URL, Critical priority.
    /// Source: `rpc_sender.rs` — confirmation polling (every 500ms per pending TX).
    GetSignatureStatuses,

    /// `getAccountInfo` (batch) → SOLANA_RPC_URL (Helius read), Normal priority.
    /// Source: `price_feed.rs` — vault polling every 500ms × 2 vaults per sub.
    GetAccountInfo,

    /// `accountSubscribe` (WebSocket) → SOLANA_WS_URL, Normal priority.
    /// Source: `price_feed.rs` — live vault subscriptions.
    /// Note: WS subscriptions don't go through HTTP rate limiter, but the
    /// method is here for completeness and priority classification.
    AccountSubscribe,

    /// `getLatestBlockhash` → PUBLIC_RPC_URL, Normal priority.
    /// Source: `executor.rs` — blockhash cache refresh every 25s.
    GetLatestBlockhash,

    /// `getBalance` → PUBLIC_RPC_URL, Background priority.
    /// Source: `mod.rs` — wallet balance poller every 30s.
    GetBalance,

    /// `getTransaction` → PUBLIC_RPC_URL, Background priority.
    /// Source: `pool.rs` — resolve pool from graduation sig (1-5 retries).
    GetTransaction,

    /// `getProgramAccounts` → PUBLIC_RPC_URL, Background priority.
    /// Source: `pool.rs` — PumpSwap/Raydium mint lookup.
    /// Fallback: helius_api_url (public RPC may reject heavy calls).
    GetProgramAccounts,

    /// `getMultipleAccounts` → PUBLIC_RPC_URL, Background priority.
    /// Source: `pool.rs` — vault reserve fetching after pool resolution.
    GetMultipleAccounts,

    /// `getSignaturesForAddress` → PUBLIC_RPC_URL, Background priority.
    /// Source: `pool.rs` — Raydium activity check.
    GetSignaturesForAddress,
}

impl RpcMethod {
    /// Map method to its priority tier.
    ///
    /// - **Critical**: `sendTransaction`, `getSignatureStatuses` — never throttled.
    /// - **Normal**: `getAccountInfo`, `accountSubscribe`, `getLatestBlockhash` — waits for tokens.
    /// - **Background**: all pool resolution reads — shed immediately if empty.
    pub fn priority(&self) -> Priority {
        match self {
            Self::SendTransaction | Self::GetSignatureStatuses => Priority::Critical,
            Self::GetAccountInfo | Self::AccountSubscribe | Self::GetLatestBlockhash => {
                Priority::Normal
            }
            Self::GetBalance
            | Self::GetTransaction
            | Self::GetProgramAccounts
            | Self::GetMultipleAccounts
            | Self::GetSignaturesForAddress => Priority::Background,
        }
    }

    /// The JSON-RPC method string sent on the wire.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SendTransaction => "sendTransaction",
            Self::GetSignatureStatuses => "getSignatureStatuses",
            Self::GetAccountInfo => "getAccountInfo",
            Self::AccountSubscribe => "accountSubscribe",
            Self::GetLatestBlockhash => "getLatestBlockhash",
            Self::GetBalance => "getBalance",
            Self::GetTransaction => "getTransaction",
            Self::GetProgramAccounts => "getProgramAccounts",
            Self::GetMultipleAccounts => "getMultipleAccounts",
            Self::GetSignaturesForAddress => "getSignaturesForAddress",
        }
    }
}

impl std::fmt::Display for RpcMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ─── Endpoint Enum (internal) ───────────────────────────────────────────────

/// Which logical endpoint a method routes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Endpoint {
    /// Helius fast/staked — isolated for TX submission + confirmation.
    HeliusSend,
    /// Helius fast — price feed reads (getAccountInfo batches).
    HeliusRead,
    /// Public Solana RPC — pool resolution, blockhash, balance.
    Public,
}

impl RpcMethod {
    /// Determine which endpoint this method routes to (spec §2.2).
    fn endpoint(&self) -> Endpoint {
        match self {
            Self::SendTransaction | Self::GetSignatureStatuses => Endpoint::HeliusSend,
            Self::GetAccountInfo | Self::AccountSubscribe => Endpoint::HeliusRead,
            Self::GetLatestBlockhash
            | Self::GetBalance
            | Self::GetTransaction
            | Self::GetProgramAccounts
            | Self::GetMultipleAccounts
            | Self::GetSignaturesForAddress => Endpoint::Public,
        }
    }
}

// ─── Configuration ──────────────────────────────────────────────────────────

/// Multi-endpoint RPC client configuration.
///
/// Loadable from environment variables:
///
/// | Env var | Maps to | Default |
/// |---------|---------|---------|
/// | `SOLANA_RPC_URL` | `helius_read_url` (and `helius_send_url` if override absent) | *required* |
/// | `HELIUS_SEND_URL` | `helius_send_url` override | falls back to `SOLANA_RPC_URL` |
/// | `PUBLIC_RPC_URL` | `public_rpc_url` | `https://api.mainnet-beta.solana.com` |
/// | `HELIUS_API_URL` | `helius_api_url` (getProgramAccounts fallback) | *none* |
#[derive(Debug, Clone)]
pub struct RpcClientConfig {
    /// Helius fast endpoint for sendTransaction + getSignatureStatuses.
    /// Isolated from read traffic to protect TX submission.
    pub helius_send_url: String,

    /// Helius endpoint for price feed reads (getAccountInfo batches).
    /// Usually the same base Helius URL as send, but separate rate limiter.
    pub helius_read_url: String,

    /// Public Solana RPC for pool resolution reads.
    /// Generous rate limits, no API key needed.
    pub public_rpc_url: String,

    /// Helius API key endpoint for getProgramAccounts fallback.
    /// Public RPC may reject heavy getProgramAccounts calls.
    pub helius_api_url: Option<String>,

    /// Rate limiter config for the Helius send endpoint.
    /// Default: 25 burst, 20 tokens/sec sustained.
    pub helius_send_limiter: RateLimiterConfig,

    /// Rate limiter config for the Helius read endpoint.
    /// Default: 25 burst, 15 tokens/sec sustained.
    pub helius_read_limiter: RateLimiterConfig,

    /// Rate limiter config for the public RPC endpoint.
    /// Default: 30 burst, 15 tokens/sec sustained.
    pub public_limiter: RateLimiterConfig,
}

/// Default public Solana RPC URL.
const DEFAULT_PUBLIC_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

impl RpcClientConfig {
    /// Load configuration from environment variables.
    ///
    /// **Required:** `SOLANA_RPC_URL`
    ///
    /// **Optional:** `HELIUS_SEND_URL`, `PUBLIC_RPC_URL`, `HELIUS_API_URL`
    pub fn from_env() -> Result<Self, RpcClientConfigError> {
        let solana_rpc_url = std::env::var("SOLANA_RPC_URL")
            .map_err(|_| RpcClientConfigError::MissingEnv("SOLANA_RPC_URL"))?;

        let helius_send_url =
            std::env::var("HELIUS_SEND_URL").unwrap_or_else(|_| solana_rpc_url.clone());

        let helius_read_url = solana_rpc_url;

        let public_rpc_url = std::env::var("PUBLIC_RPC_URL")
            .unwrap_or_else(|_| DEFAULT_PUBLIC_RPC_URL.to_string());

        let helius_api_url = std::env::var("HELIUS_API_URL").ok();

        Ok(Self {
            helius_send_url,
            helius_read_url,
            public_rpc_url,
            helius_api_url,
            // Helius send: 25 burst, 20/sec — reserved for TX submission.
            // Only Critical callers use this endpoint.
            helius_send_limiter: RateLimiterConfig {
                tokens_per_sec: 20.0,
                burst_capacity: 25,
                normal_wait_timeout_ms: 2000,
            },
            // Helius read: 25 burst, 15/sec — price feed getAccountInfo batches.
            helius_read_limiter: RateLimiterConfig {
                tokens_per_sec: 15.0,
                burst_capacity: 25,
                normal_wait_timeout_ms: 2000,
            },
            // Public RPC: 30 burst, 15/sec — pool resolution, blockhash, balance.
            public_limiter: RateLimiterConfig {
                tokens_per_sec: 15.0,
                burst_capacity: 30,
                normal_wait_timeout_ms: 2000,
            },
        })
    }
}

/// Errors during RPC client configuration loading.
#[derive(Debug)]
pub enum RpcClientConfigError {
    /// A required environment variable is missing.
    MissingEnv(&'static str),
}

impl std::fmt::Display for RpcClientConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingEnv(var) => write!(f, "required environment variable not set: {}", var),
        }
    }
}

impl std::error::Error for RpcClientConfigError {}

// ─── RPC Client ─────────────────────────────────────────────────────────────

/// Shared multi-endpoint RPC client.
///
/// Passed as `Arc<RpcClient>` to all components. Each component calls:
/// 1. `client.acquire(method).await?` — rate-limit + priority gate
/// 2. `client.url_for(method)` — get correct endpoint URL
///
/// The client owns one [`RateLimiter`] per endpoint, each with its own
/// token bucket.
pub struct RpcClient {
    config: RpcClientConfig,

    /// Rate limiter for Helius send endpoint.
    /// Only Critical-priority calls (sendTransaction, getSignatureStatuses).
    helius_send_limiter: Arc<RateLimiter>,

    /// Rate limiter for Helius read endpoint.
    /// Normal-priority calls (getAccountInfo price feed batches).
    helius_read_limiter: Arc<RateLimiter>,

    /// Rate limiter for public RPC endpoint.
    /// Background-priority calls (pool resolution, blockhash, balance).
    public_limiter: Arc<RateLimiter>,
}

impl RpcClient {
    /// Create a new multi-endpoint RPC client from config.
    pub fn new(config: RpcClientConfig) -> Self {
        let helius_send_limiter = Arc::new(RateLimiter::new(&config.helius_send_limiter));
        let helius_read_limiter = Arc::new(RateLimiter::new(&config.helius_read_limiter));
        let public_limiter = Arc::new(RateLimiter::new(&config.public_limiter));

        Self {
            config,
            helius_send_limiter,
            helius_read_limiter,
            public_limiter,
        }
    }

    /// Get the URL for a given RPC method.
    ///
    /// Routing table (spec §2.2):
    ///
    /// | Method | Endpoint |
    /// |--------|----------|
    /// | `sendTransaction`, `getSignatureStatuses` | `helius_send_url` |
    /// | `getAccountInfo`, `accountSubscribe` | `helius_read_url` |
    /// | everything else | `public_rpc_url` |
    pub fn url_for(&self, method: RpcMethod) -> &str {
        match method.endpoint() {
            Endpoint::HeliusSend => &self.config.helius_send_url,
            Endpoint::HeliusRead => &self.config.helius_read_url,
            Endpoint::Public => &self.config.public_rpc_url,
        }
    }

    /// Get the fallback URL for methods that have one.
    ///
    /// Currently only `getProgramAccounts` falls back to Helius API key endpoint
    /// when public RPC rejects the call (some public nodes disable `getProgramAccounts`).
    pub fn fallback_url_for(&self, method: RpcMethod) -> Option<&str> {
        match method {
            RpcMethod::GetProgramAccounts => self.config.helius_api_url.as_deref(),
            _ => None,
        }
    }

    /// Acquire a rate limit token for the given method.
    ///
    /// This is the main gate — call before every RPC request.
    ///
    /// Behavior by priority:
    /// - **Critical** (`sendTransaction`, `getSignatureStatuses`): never blocked,
    ///   overdrafts the bucket if empty.
    /// - **Normal** (`getAccountInfo`, `getLatestBlockhash`): waits up to
    ///   `normal_wait_timeout_ms` for a token.
    /// - **Background** (pool resolution reads): shed immediately if no
    ///   tokens available — returns `Err(AcquireError::Shed)`.
    pub async fn acquire(&self, method: RpcMethod) -> AcquireResult {
        let priority = method.priority();
        let limiter = self.limiter_for(method);
        limiter.acquire(priority).await
    }

    /// Get a reference to the rate limiter for a given method's endpoint.
    fn limiter_for(&self, method: RpcMethod) -> &RateLimiter {
        match method.endpoint() {
            Endpoint::HeliusSend => &self.helius_send_limiter,
            Endpoint::HeliusRead => &self.helius_read_limiter,
            Endpoint::Public => &self.public_limiter,
        }
    }

    /// Get stats for all three rate limiters.
    ///
    /// Returns `(helius_send, helius_read, public)` stats.
    pub fn all_stats(&self) -> (RateLimiterStats, RateLimiterStats, RateLimiterStats) {
        (
            self.helius_send_limiter.stats(),
            self.helius_read_limiter.stats(),
            self.public_limiter.stats(),
        )
    }

    /// Log a summary of all rate limiter stats at info level.
    pub fn log_stats(&self) {
        let (send, read, public) = self.all_stats();
        tracing::info!(
            helius_send = %send,
            helius_read = %read,
            public_rpc = %public,
            "[rpc_client] rate limiter stats"
        );
    }

    /// Access the underlying config.
    pub fn config(&self) -> &RpcClientConfig {
        &self.config
    }

    /// Get a clone of the send limiter Arc (useful for components that need
    /// to hold a reference independently of the RpcClient lifetime).
    pub fn helius_send_limiter(&self) -> Arc<RateLimiter> {
        Arc::clone(&self.helius_send_limiter)
    }

    /// Get a clone of the read limiter Arc.
    pub fn helius_read_limiter(&self) -> Arc<RateLimiter> {
        Arc::clone(&self.helius_read_limiter)
    }

    /// Get a clone of the public limiter Arc.
    pub fn public_limiter(&self) -> Arc<RateLimiter> {
        Arc::clone(&self.public_limiter)
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a test config with distinct URLs so routing is easy to verify.
    fn test_config() -> RpcClientConfig {
        RpcClientConfig {
            helius_send_url: "https://helius-send.example.com".to_string(),
            helius_read_url: "https://helius-read.example.com".to_string(),
            public_rpc_url: "https://public.example.com".to_string(),
            helius_api_url: Some("https://helius-api.example.com".to_string()),
            helius_send_limiter: RateLimiterConfig {
                tokens_per_sec: 20.0,
                burst_capacity: 25,
                normal_wait_timeout_ms: 2000,
            },
            helius_read_limiter: RateLimiterConfig {
                tokens_per_sec: 15.0,
                burst_capacity: 25,
                normal_wait_timeout_ms: 2000,
            },
            public_limiter: RateLimiterConfig {
                tokens_per_sec: 15.0,
                burst_capacity: 30,
                normal_wait_timeout_ms: 2000,
            },
        }
    }

    // ── URL Routing Tests ───────────────────────────────────────────────

    #[test]
    fn send_transaction_routes_to_helius_send() {
        let client = RpcClient::new(test_config());
        assert_eq!(
            client.url_for(RpcMethod::SendTransaction),
            "https://helius-send.example.com"
        );
    }

    #[test]
    fn get_signature_statuses_routes_to_helius_send() {
        let client = RpcClient::new(test_config());
        assert_eq!(
            client.url_for(RpcMethod::GetSignatureStatuses),
            "https://helius-send.example.com"
        );
    }

    #[test]
    fn get_account_info_routes_to_helius_read() {
        let client = RpcClient::new(test_config());
        assert_eq!(
            client.url_for(RpcMethod::GetAccountInfo),
            "https://helius-read.example.com"
        );
    }

    #[test]
    fn account_subscribe_routes_to_helius_read() {
        let client = RpcClient::new(test_config());
        assert_eq!(
            client.url_for(RpcMethod::AccountSubscribe),
            "https://helius-read.example.com"
        );
    }

    #[test]
    fn pool_resolution_methods_route_to_public() {
        let client = RpcClient::new(test_config());
        let public_methods = [
            RpcMethod::GetTransaction,
            RpcMethod::GetProgramAccounts,
            RpcMethod::GetMultipleAccounts,
            RpcMethod::GetSignaturesForAddress,
        ];
        for method in &public_methods {
            assert_eq!(
                client.url_for(*method),
                "https://public.example.com",
                "method {:?} should route to public RPC",
                method
            );
        }
    }

    #[test]
    fn utility_methods_route_to_public() {
        let client = RpcClient::new(test_config());
        assert_eq!(
            client.url_for(RpcMethod::GetLatestBlockhash),
            "https://public.example.com"
        );
        assert_eq!(
            client.url_for(RpcMethod::GetBalance),
            "https://public.example.com"
        );
    }

    // ── Exhaustive routing: every method maps to exactly one endpoint ───

    #[test]
    fn every_method_has_a_url() {
        let client = RpcClient::new(test_config());
        let all_methods = [
            RpcMethod::SendTransaction,
            RpcMethod::GetSignatureStatuses,
            RpcMethod::GetAccountInfo,
            RpcMethod::AccountSubscribe,
            RpcMethod::GetLatestBlockhash,
            RpcMethod::GetBalance,
            RpcMethod::GetTransaction,
            RpcMethod::GetProgramAccounts,
            RpcMethod::GetMultipleAccounts,
            RpcMethod::GetSignaturesForAddress,
        ];
        for method in &all_methods {
            let url = client.url_for(*method);
            assert!(!url.is_empty(), "method {:?} should have a URL", method);
        }
    }

    // ── Fallback URL Tests ──────────────────────────────────────────────

    #[test]
    fn get_program_accounts_has_fallback() {
        let client = RpcClient::new(test_config());
        assert_eq!(
            client.fallback_url_for(RpcMethod::GetProgramAccounts),
            Some("https://helius-api.example.com")
        );
    }

    #[test]
    fn other_methods_have_no_fallback() {
        let client = RpcClient::new(test_config());
        let methods = [
            RpcMethod::SendTransaction,
            RpcMethod::GetSignatureStatuses,
            RpcMethod::GetAccountInfo,
            RpcMethod::GetTransaction,
            RpcMethod::GetMultipleAccounts,
            RpcMethod::GetBalance,
            RpcMethod::GetLatestBlockhash,
            RpcMethod::GetSignaturesForAddress,
            RpcMethod::AccountSubscribe,
        ];
        for method in &methods {
            assert!(
                client.fallback_url_for(*method).is_none(),
                "method {:?} should have no fallback",
                method
            );
        }
    }

    #[test]
    fn no_helius_api_url_means_no_fallback() {
        let mut config = test_config();
        config.helius_api_url = None;
        let client = RpcClient::new(config);
        assert!(client
            .fallback_url_for(RpcMethod::GetProgramAccounts)
            .is_none());
    }

    // ── Priority Mapping Tests ──────────────────────────────────────────

    #[test]
    fn critical_priority_methods() {
        assert_eq!(RpcMethod::SendTransaction.priority(), Priority::Critical);
        assert_eq!(
            RpcMethod::GetSignatureStatuses.priority(),
            Priority::Critical
        );
    }

    #[test]
    fn normal_priority_methods() {
        assert_eq!(RpcMethod::GetAccountInfo.priority(), Priority::Normal);
        assert_eq!(RpcMethod::AccountSubscribe.priority(), Priority::Normal);
        assert_eq!(RpcMethod::GetLatestBlockhash.priority(), Priority::Normal);
    }

    #[test]
    fn background_priority_methods() {
        let bg_methods = [
            RpcMethod::GetBalance,
            RpcMethod::GetTransaction,
            RpcMethod::GetProgramAccounts,
            RpcMethod::GetMultipleAccounts,
            RpcMethod::GetSignaturesForAddress,
        ];
        for method in &bg_methods {
            assert_eq!(
                method.priority(),
                Priority::Background,
                "method {:?} should be Background priority",
                method
            );
        }
    }

    // ── Method String Tests ─────────────────────────────────────────────

    #[test]
    fn method_as_str_matches_json_rpc() {
        assert_eq!(RpcMethod::SendTransaction.as_str(), "sendTransaction");
        assert_eq!(
            RpcMethod::GetSignatureStatuses.as_str(),
            "getSignatureStatuses"
        );
        assert_eq!(RpcMethod::GetAccountInfo.as_str(), "getAccountInfo");
        assert_eq!(RpcMethod::AccountSubscribe.as_str(), "accountSubscribe");
        assert_eq!(
            RpcMethod::GetLatestBlockhash.as_str(),
            "getLatestBlockhash"
        );
        assert_eq!(RpcMethod::GetBalance.as_str(), "getBalance");
        assert_eq!(RpcMethod::GetTransaction.as_str(), "getTransaction");
        assert_eq!(
            RpcMethod::GetProgramAccounts.as_str(),
            "getProgramAccounts"
        );
        assert_eq!(
            RpcMethod::GetMultipleAccounts.as_str(),
            "getMultipleAccounts"
        );
        assert_eq!(
            RpcMethod::GetSignaturesForAddress.as_str(),
            "getSignaturesForAddress"
        );
    }

    #[test]
    fn rpc_method_display() {
        assert_eq!(format!("{}", RpcMethod::SendTransaction), "sendTransaction");
        assert_eq!(
            format!("{}", RpcMethod::GetProgramAccounts),
            "getProgramAccounts"
        );
    }

    // ── Config from_env Tests ───────────────────────────────────────────

    #[test]
    fn config_from_env_minimal() {
        // Set only the required var
        unsafe {
            std::env::set_var("SOLANA_RPC_URL", "https://helius-test.example.com");
            std::env::remove_var("HELIUS_SEND_URL");
            std::env::remove_var("PUBLIC_RPC_URL");
            std::env::remove_var("HELIUS_API_URL");
        }

        let config = RpcClientConfig::from_env().expect("should load from env");

        // helius_send_url defaults to SOLANA_RPC_URL when HELIUS_SEND_URL not set
        assert_eq!(config.helius_send_url, "https://helius-test.example.com");
        // helius_read_url is always SOLANA_RPC_URL
        assert_eq!(config.helius_read_url, "https://helius-test.example.com");
        // public_rpc_url defaults to mainnet-beta
        assert_eq!(
            config.public_rpc_url,
            "https://api.mainnet-beta.solana.com"
        );
        // No fallback API URL
        assert!(config.helius_api_url.is_none());
    }

    #[test]
    fn config_from_env_with_overrides() {
        unsafe {
            std::env::set_var("SOLANA_RPC_URL", "https://helius-read.example.com");
            std::env::set_var(
                "HELIUS_SEND_URL",
                "https://helius-send-override.example.com",
            );
            std::env::set_var("PUBLIC_RPC_URL", "https://custom-public.example.com");
            std::env::set_var("HELIUS_API_URL", "https://helius-api.example.com");
        }

        let config = RpcClientConfig::from_env().expect("should load from env");

        // HELIUS_SEND_URL overrides the default (which would be SOLANA_RPC_URL)
        assert_eq!(
            config.helius_send_url,
            "https://helius-send-override.example.com"
        );
        assert_eq!(config.helius_read_url, "https://helius-read.example.com");
        assert_eq!(config.public_rpc_url, "https://custom-public.example.com");
        assert_eq!(
            config.helius_api_url.as_deref(),
            Some("https://helius-api.example.com")
        );
    }

    #[test]
    fn config_from_env_missing_solana_rpc_url() {
        unsafe {
            std::env::remove_var("SOLANA_RPC_URL");
        }
        let result = RpcClientConfig::from_env();
        assert!(result.is_err());
    }

    #[test]
    fn config_error_display() {
        let err = RpcClientConfigError::MissingEnv("SOLANA_RPC_URL");
        let msg = format!("{}", err);
        assert!(
            msg.contains("SOLANA_RPC_URL"),
            "error message should mention the missing var"
        );
    }

    // ── Endpoint Routing Consistency Tests ───────────────────────────────

    #[test]
    fn endpoint_and_limiter_use_same_routing() {
        let client = RpcClient::new(test_config());

        let all_methods = [
            RpcMethod::SendTransaction,
            RpcMethod::GetSignatureStatuses,
            RpcMethod::GetAccountInfo,
            RpcMethod::AccountSubscribe,
            RpcMethod::GetLatestBlockhash,
            RpcMethod::GetBalance,
            RpcMethod::GetTransaction,
            RpcMethod::GetProgramAccounts,
            RpcMethod::GetMultipleAccounts,
            RpcMethod::GetSignaturesForAddress,
        ];

        for method in &all_methods {
            let url = client.url_for(*method);
            let limiter = client.limiter_for(*method) as *const RateLimiter;

            match method.endpoint() {
                Endpoint::HeliusSend => {
                    assert_eq!(url, "https://helius-send.example.com");
                    assert_eq!(
                        limiter,
                        &*client.helius_send_limiter as *const RateLimiter,
                        "method {:?} should use helius_send_limiter",
                        method
                    );
                }
                Endpoint::HeliusRead => {
                    assert_eq!(url, "https://helius-read.example.com");
                    assert_eq!(
                        limiter,
                        &*client.helius_read_limiter as *const RateLimiter,
                        "method {:?} should use helius_read_limiter",
                        method
                    );
                }
                Endpoint::Public => {
                    assert_eq!(url, "https://public.example.com");
                    assert_eq!(
                        limiter,
                        &*client.public_limiter as *const RateLimiter,
                        "method {:?} should use public_limiter",
                        method
                    );
                }
            }
        }
    }

    // ── Acquire Integration Tests ───────────────────────────────────────

    #[tokio::test]
    async fn acquire_critical_always_succeeds() {
        let client = RpcClient::new(test_config());
        // Critical should never fail, even after many calls (overdrafts allowed)
        for _ in 0..50 {
            let result = client.acquire(RpcMethod::SendTransaction).await;
            assert!(result.is_ok(), "Critical acquire should never fail");
        }
    }

    #[tokio::test]
    async fn acquire_normal_succeeds_with_full_bucket() {
        let client = RpcClient::new(test_config());
        // Normal should succeed when bucket is full
        let result = client.acquire(RpcMethod::GetAccountInfo).await;
        assert!(
            result.is_ok(),
            "Normal acquire should succeed with full bucket"
        );
    }

    #[tokio::test]
    async fn acquire_background_succeeds_with_full_bucket() {
        let client = RpcClient::new(test_config());
        let result = client.acquire(RpcMethod::GetTransaction).await;
        assert!(
            result.is_ok(),
            "Background acquire should succeed with full bucket"
        );
    }

    // ── Stats Tests ─────────────────────────────────────────────────────

    #[test]
    fn all_stats_returns_three_snapshots() {
        let client = RpcClient::new(test_config());
        let (send, read, public) = client.all_stats();
        // Fresh limiters should have full buckets
        assert!(send.tokens_available > 0);
        assert!(read.tokens_available > 0);
        assert!(public.tokens_available > 0);
    }

    // ── Arc accessor tests ──────────────────────────────────────────────

    #[test]
    fn limiter_arc_accessors_return_valid_refs() {
        let client = RpcClient::new(test_config());
        let send = client.helius_send_limiter();
        let read = client.helius_read_limiter();
        let public = client.public_limiter();
        // Just verify they're the same Arcs (not new instances)
        assert!(Arc::ptr_eq(&send, &client.helius_send_limiter));
        assert!(Arc::ptr_eq(&read, &client.helius_read_limiter));
        assert!(Arc::ptr_eq(&public, &client.public_limiter));
    }
}
