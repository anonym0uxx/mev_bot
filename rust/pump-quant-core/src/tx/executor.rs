//! BlockhashCache: cached recent blockhash with TTL-based staleness detection.
//!
//! Solana blockhashes are valid for ~60s (150 slots). We refresh every 25s
//! and consider the cache stale after 30s (TTL). This eliminates the ~200ms
//! per-trade RPC round-trip for `getLatestBlockhash`.

use std::sync::Arc;
use parking_lot::RwLock;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

// ── BlockhashCache ───────────────────────────────────────────────────────────

/// Cached recent blockhash with TTL-based staleness detection.
///
/// Solana blockhashes are valid for ~60s (150 slots). We refresh every 25s
/// and consider the cache stale after 30s (TTL). This eliminates the ~200ms
/// per-trade RPC round-trip for `getLatestBlockhash`.
pub struct BlockhashCache {
    /// (base58_blockhash, cached_at_epoch_ms)
    inner: RwLock<Option<(String, u64)>>,
    /// Maximum age in ms before the cached value is considered stale.
    ttl_ms: u64,
}

impl BlockhashCache {
    /// Create a new empty cache with a 30s TTL.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(None),
            ttl_ms: 30_000,
        })
    }

    /// Create a cache with a custom TTL (for testing).
    #[cfg(test)]
    pub fn with_ttl_ms(ttl_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(None),
            ttl_ms,
        })
    }

    /// Refresh the cached blockhash by calling the Solana RPC.
    pub async fn refresh(&self, rpc_url: &str) -> Result<()> {
        let client = reqwest::Client::new();
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getLatestBlockhash",
            "params": [{"commitment": "finalized"}]
        });

        let resp = client
            .post(rpc_url)
            .json(&body)
            .send()
            .await
            .context("blockhash cache: RPC request failed")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("blockhash cache: RPC HTTP {status}: {text}");
        }

        let parsed: RpcResponse = resp
            .json()
            .await
            .context("blockhash cache: failed to parse response")?;

        if let Some(err) = parsed.error {
            bail!("blockhash cache: RPC error: {err}");
        }

        let blockhash_str = parsed
            .result
            .context("blockhash cache: missing 'result'")?
            .value
            .blockhash;

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let mut guard = self.inner.write();
        *guard = Some((blockhash_str, now_ms));
        Ok(())
    }

    /// Get the cached blockhash if it's still fresh (within TTL).
    /// Returns `None` if the cache is empty or stale.
    pub async fn get(&self) -> Option<String> {
        let guard = self.inner.read();
        if let Some((ref hash, cached_at)) = *guard {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            if now_ms.saturating_sub(cached_at) <= self.ttl_ms {
                return Some(hash.clone());
            }
        }
        None
    }

    /// Get the cached blockhash as raw bytes synchronously (no async).
    /// Used by the momentum engine's sell path to avoid async in the tick loop.
    pub fn get_sync(&self) -> Option<[u8; 32]> {
        let guard = self.inner.read();
        if let Some((ref hash_str, cached_at)) = *guard {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            if now_ms.saturating_sub(cached_at) <= self.ttl_ms {
                if let Ok(bytes) = bs58::decode(hash_str).into_vec() {
                    if bytes.len() == 32 {
                        let mut arr = [0u8; 32];
                        arr.copy_from_slice(&bytes);
                        return Some(arr);
                    }
                }
            }
        }
        None
    }

    /// Spawn a background tokio task that refreshes the blockhash every 25s.
    /// Logs errors but never panics — the hot path falls back to direct RPC
    /// on cache miss.
    pub fn spawn_refresh_task(self: Arc<Self>, rpc_url: String) {
        tokio::spawn(async move {
            tracing::info!("blockhash cache: refresh task started (25s interval, {}ms TTL)", self.ttl_ms);
            loop {
                match self.refresh(&rpc_url).await {
                    Ok(()) => {
                        tracing::debug!("blockhash cache: refreshed");
                    }
                    Err(e) => {
                        tracing::warn!("blockhash cache: refresh failed: {e}");
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(25)).await;
            }
        });
    }
}

/// JSON-RPC response structures for `getLatestBlockhash`.
#[derive(Deserialize)]
struct RpcResponse {
    result: Option<RpcResultValue>,
    error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct RpcResultValue {
    value: BlockhashValue,
}

#[derive(Deserialize)]
struct BlockhashValue {
    blockhash: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_blockhash_cache_empty_returns_none() {
        let cache = BlockhashCache::new();
        assert!(cache.get().await.is_none());
    }

    #[tokio::test]
    async fn test_blockhash_cache_stores_and_returns() {
        let cache = BlockhashCache::new();
        {
            let mut guard = cache.inner.write();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            *guard = Some(("FakeBlockhash123".to_string(), now));
        }
        let result = cache.get().await;
        assert_eq!(result, Some("FakeBlockhash123".to_string()));
    }

    #[tokio::test]
    async fn test_blockhash_cache_stale_returns_none() {
        let cache = BlockhashCache::with_ttl_ms(100); // 100ms TTL
        {
            let mut guard = cache.inner.write();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            // Cached 200ms ago — stale
            *guard = Some(("StaleHash".to_string(), now.saturating_sub(200)));
        }
        assert!(cache.get().await.is_none());
    }
}
