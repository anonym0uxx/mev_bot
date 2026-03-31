//! Background blockhash poller — keeps a recent blockhash warm for Jito bundles.
//!
//! Polls `getLatestBlockhash` every 400ms and stores the result in an Arc<AtomicU64>
//! pair (hash bytes + last_slot). The arb engine reads the cached blockhash with
//! zero latency on the hot path — no RPC call at trade time.
//!
//! ## Architecture
//!
//! ```text
//! [background task] ──poll every 400ms──► getLatestBlockhash(confirmed)
//!                                              │
//!                                              ▼
//!                          Arc<RwLock<CachedBlockhash>> ◄── read by arb engine
//! ```

use solana_sdk::hash::Hash;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Cached blockhash with metadata.
#[derive(Debug, Clone)]
pub struct CachedBlockhash {
    /// Recent blockhash for transaction construction.
    pub hash: Hash,
    /// Slot at which this blockhash was valid.
    pub last_valid_block_height: u64,
    /// Epoch ms when we fetched this blockhash.
    pub fetched_at_ms: u64,
}

impl Default for CachedBlockhash {
    fn default() -> Self {
        Self {
            hash: Hash::default(),
            last_valid_block_height: 0,
            fetched_at_ms: 0,
        }
    }
}

/// Shared blockhash cache — lock-free reads via RwLock.
pub type BlockhashCache = Arc<RwLock<CachedBlockhash>>;

/// Create a new empty blockhash cache.
pub fn new_cache() -> BlockhashCache {
    Arc::new(RwLock::new(CachedBlockhash::default()))
}

/// Start the background blockhash poller.
///
/// Polls `getLatestBlockhash` every `poll_interval_ms` and updates the cache.
/// Returns a JoinHandle for the background task.
///
/// # Arguments
/// * `cache` — shared cache to write into
/// * `rpc_url` — Solana RPC endpoint
/// * `client` — shared reqwest client
/// * `poll_interval_ms` — how often to poll (recommend 400ms)
pub fn spawn_poller(
    cache: BlockhashCache,
    rpc_url: String,
    client: reqwest::Client,
    poll_interval_ms: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(
            std::time::Duration::from_millis(poll_interval_ms),
        );
        let mut consecutive_failures: u32 = 0;

        loop {
            interval.tick().await;

            match fetch_blockhash(&client, &rpc_url).await {
                Ok((hash, last_valid_block_height)) => {
                    consecutive_failures = 0;
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;

                    let mut guard = cache.write().await;
                    guard.hash = hash;
                    guard.last_valid_block_height = last_valid_block_height;
                    guard.fetched_at_ms = now_ms;
                }
                Err(e) => {
                    consecutive_failures += 1;
                    if consecutive_failures <= 3 || consecutive_failures % 10 == 0 {
                        tracing::warn!(
                            err = %e,
                            consecutive_failures,
                            "[blockhash] fetch failed"
                        );
                    }
                }
            }
        }
    })
}

/// Fetch latest blockhash from RPC.
async fn fetch_blockhash(
    client: &reqwest::Client,
    rpc_url: &str,
) -> Result<(Hash, u64), String> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getLatestBlockhash",
        "params": [{"commitment": "confirmed"}]
    });

    let resp = client
        .post(rpc_url)
        .json(&body)
        .timeout(std::time::Duration::from_millis(300))
        .send()
        .await
        .map_err(|e| format!("request: {}", e))?;

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("parse: {}", e))?;

    let blockhash_str = json
        .pointer("/result/value/blockhash")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "no blockhash in response".to_string())?;

    let last_valid = json
        .pointer("/result/value/lastValidBlockHeight")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let hash = blockhash_str
        .parse::<Hash>()
        .map_err(|e| format!("invalid hash: {}", e))?;

    Ok((hash, last_valid))
}

/// Read the cached blockhash. Returns None if never fetched or stale (>10s old).
pub async fn get_recent_blockhash(cache: &BlockhashCache) -> Option<CachedBlockhash> {
    let guard = cache.read().await;
    if guard.fetched_at_ms == 0 {
        return None; // never fetched
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    // Blockhashes are valid for ~60s, but we reject >10s stale for safety
    if now_ms.saturating_sub(guard.fetched_at_ms) > 10_000 {
        return None;
    }

    Some(guard.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_cache_is_empty() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let cache = new_cache();
            let result = get_recent_blockhash(&cache).await;
            assert!(result.is_none());
        });
    }

    #[test]
    fn test_cache_write_read() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let cache = new_cache();

            // Write a blockhash
            {
                let mut guard = cache.write().await;
                guard.hash = Hash::new_unique();
                guard.last_valid_block_height = 12345;
                guard.fetched_at_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64;
            }

            // Read it back
            let result = get_recent_blockhash(&cache).await;
            assert!(result.is_some());
            let cached = result.unwrap();
            assert_eq!(cached.last_valid_block_height, 12345);
        });
    }

    #[test]
    fn test_stale_blockhash_rejected() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let cache = new_cache();

            // Write a very old blockhash (>10s ago)
            {
                let mut guard = cache.write().await;
                guard.hash = Hash::new_unique();
                guard.last_valid_block_height = 99999;
                guard.fetched_at_ms = 1000; // epoch ms = 1 second after unix epoch = very old
            }

            // Should be rejected as stale
            let result = get_recent_blockhash(&cache).await;
            assert!(result.is_none());
        });
    }
}
