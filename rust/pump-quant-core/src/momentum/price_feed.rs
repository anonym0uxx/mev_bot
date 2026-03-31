//! RPC polling price feed for Raydium vault accounts.
//!
//! Polls vault SPL token accounts via HTTP RPC `getAccountInfo` batch requests
//! and streams real-time reserves via AtomicU64. The tick loop reads prices with
//! zero allocation via `PriceFeedManager::current_price()`.
//!
//! ## Architecture
//!
//! ```text
//! HTTP RPC ──getAccountInfo batch──▶ price_feed_poll_loop ──AtomicU64──▶ on_tick()
//!   (500ms poll interval)            (dedicated tokio task)              (main loop)
//! ```
//!
//! ## Performance
//!
//! - Zero-allocation hot path: `current_price()` does DashMap::get + AtomicU64::load
//! - SPL token amount parsed as `u64::from_le_bytes(data[64..72])` — no serde
//! - Batch RPC: one HTTP request per poll cycle for all active subscriptions

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::mpsc;
use tracing;

// ── Fixed-point price type ───────────────────────────────────────────────────

/// Fixed-point price: lamports per 1,000,000 token atoms.
///
/// Example: if 1 token atom costs 0.000381 lamports,
/// `price_fp = 381` (i.e., 381 lamports per 1M atoms).
///
/// This fits u64 for any realistic Pump.fun token price.
pub type PriceFp = u64;

/// Compute fixed-point price from raw reserves.
///
/// `price_fp = (reserve_sol * 1_000_000) / reserve_token`
///
/// Uses u128 intermediate to prevent overflow when reserve_sol > 18.4B lamports
/// (which is ~18.4 SOL — very common).
#[inline(always)]
pub fn price_from_reserves(reserve_sol: u64, reserve_token: u64) -> PriceFp {
    if reserve_token == 0 {
        return 0;
    }
    // u128 intermediate prevents overflow for reserve_sol up to u64::MAX
    ((reserve_sol as u128).saturating_mul(1_000_000) / reserve_token as u128) as u64
}

// ── Subscription types ───────────────────────────────────────────────────────

/// Request to subscribe to a token's vault accounts.
#[derive(Debug)]
pub struct VaultSubscription {
    /// Token mint address (32 bytes).
    pub mint: [u8; 32],
    /// Coin vault (base token) SPL account address, base58-encoded.
    pub coin_vault: String,
    /// PC vault (WSOL) SPL account address, base58-encoded.
    pub pc_vault: String,
}

/// Commands sent via mpsc channel (kept for API compatibility).
pub enum PriceFeedCommand {
    /// Subscribe to a token's vault accounts.
    Subscribe(VaultSubscription),
    /// Unsubscribe from a token's vault accounts by mint.
    Unsubscribe([u8; 32]),
    /// Graceful shutdown.
    Shutdown,
}

// ── Shared price state ───────────────────────────────────────────────────────

/// Shared price state for a single token. Written atomically by poll task,
/// read lock-free by tick loop. All fields are AtomicU64 for zero-contention
/// reads on the hot path.
pub struct PriceState {
    /// Fixed-point price: lamports per 1M token atoms.
    pub price_fp: AtomicU64,
    /// Last update timestamp (epoch ms).
    pub last_update_ms: AtomicU64,
    /// Raw SOL reserve (lamports) for debugging/logging.
    pub reserve_sol: AtomicU64,
    /// Raw token reserve (atoms) for debugging/logging.
    pub reserve_token: AtomicU64,
}

impl PriceState {
    /// Create a new zeroed PriceState wrapped in Arc.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            price_fp: AtomicU64::new(0),
            last_update_ms: AtomicU64::new(0),
            reserve_sol: AtomicU64::new(0),
            reserve_token: AtomicU64::new(0),
        })
    }
}

// ── PriceFeedManager ─────────────────────────────────────────────────────────

/// Manages RPC polling price feed subscriptions.
///
/// Owns a DashMap of active subscriptions polled by the background task,
/// and a shared DashMap of per-mint price states readable by the tick loop.
pub struct PriceFeedManager {
    /// mint → PriceState. Read lock-free by tick thread via AtomicU64.
    pub prices: Arc<DashMap<[u8; 32], Arc<PriceState>>>,
    /// mint → VaultSubscription (coin_vault + pc_vault pubkeys for polling).
    active_subs: Arc<DashMap<[u8; 32], VaultSubscription>>,
    /// Command sender — kept for API compatibility (used by close_position fire-and-forget).
    cmd_tx: mpsc::Sender<PriceFeedCommand>,
}

impl PriceFeedManager {
    /// Create a new PriceFeedManager and spawn the RPC polling loop task.
    ///
    /// Returns `(manager, join_handle)`. The join handle can be used to
    /// await graceful shutdown of the polling task.
    pub fn new(rpc_url: String, poll_interval_ms: u64) -> (Self, tokio::task::JoinHandle<()>) {
        let prices: Arc<DashMap<[u8; 32], Arc<PriceState>>> = Arc::new(DashMap::new());
        let active_subs: Arc<DashMap<[u8; 32], VaultSubscription>> = Arc::new(DashMap::new());
        let (cmd_tx, _cmd_rx) = mpsc::channel(64); // keep for API compat

        let prices_clone = prices.clone();
        let subs_clone = active_subs.clone();

        let handle = tokio::spawn(async move {
            price_feed_poll_loop(rpc_url, subs_clone, prices_clone, poll_interval_ms).await;
        });

        (Self { prices, active_subs, cmd_tx }, handle)
    }

    /// Subscribe to a token's vault accounts for price tracking.
    pub async fn subscribe(&self, sub: VaultSubscription) {
        tracing::info!(
            mint = %bs58::encode(&sub.mint).into_string(),
            coin_vault = %sub.coin_vault,
            pc_vault = %sub.pc_vault,
            "[price_feed] subscribing to vaults for polling"
        );
        self.prices.entry(sub.mint).or_insert_with(PriceState::new);
        self.active_subs.insert(sub.mint, sub);
    }

    /// Unsubscribe from a token's vault accounts (async, for API compat).
    pub async fn unsubscribe(&self, mint: [u8; 32]) {
        self.unsubscribe_sync(&mint);
    }

    /// Unsubscribe from a token's vault accounts (sync).
    pub fn unsubscribe_sync(&self, mint: &[u8; 32]) {
        self.active_subs.remove(mint);
        self.prices.remove(mint);
    }

    /// Request graceful shutdown (no-op for polling loop, kept for API compat).
    pub async fn shutdown(&self) {
        let _ = self.cmd_tx.send(PriceFeedCommand::Shutdown).await;
    }

    /// Get a clone of the command sender for fire-and-forget operations.
    ///
    /// Used by `MomentumEngine::close_position()` to unsubscribe without awaiting.
    /// NOTE: With the polling architecture, unsubscribe is handled via active_subs
    /// DashMap directly. This method is kept for API compatibility.
    pub fn cmd_sender(&self) -> mpsc::Sender<PriceFeedCommand> {
        self.cmd_tx.clone()
    }

    /// Get a reference to active_subs for direct unsubscribe from sync context.
    pub fn active_subs(&self) -> &Arc<DashMap<[u8; 32], VaultSubscription>> {
        &self.active_subs
    }

    /// Hot path: read current fixed-point price for a mint. Zero allocation.
    ///
    /// Returns `None` if mint is not subscribed or no price update received yet.
    #[inline(always)]
    pub fn current_price(&self, mint: &[u8; 32]) -> Option<u64> {
        self.prices
            .get(mint)
            .map(|s| s.price_fp.load(Ordering::Relaxed))
    }

    /// Read full price state for a mint (for logging/debugging).
    #[inline(always)]
    pub fn price_state(&self, mint: &[u8; 32]) -> Option<Arc<PriceState>> {
        self.prices.get(mint).map(|s| Arc::clone(s.value()))
    }
}

// ── RPC Polling Loop ─────────────────────────────────────────────────────────

/// RPC polling loop — replaces broken accountSubscribe WS loop.
/// Polls all active vault subscriptions via getAccountInfo batch RPC every poll_interval_ms.
async fn price_feed_poll_loop(
    rpc_url: String,
    active_subs: Arc<DashMap<[u8; 32], VaultSubscription>>,
    prices: Arc<DashMap<[u8; 32], Arc<PriceState>>>,
    poll_interval_ms: u64,
) {
    if rpc_url.is_empty() {
        tracing::warn!("[price_feed] RPC URL not configured — price polling disabled");
        return;
    }

    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let mut interval = tokio::time::interval(std::time::Duration::from_millis(poll_interval_ms));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    tracing::info!(poll_interval_ms, url = %rpc_url, "[price_feed] RPC polling loop started");

    loop {
        interval.tick().await;

        // Snapshot current subscriptions
        let subs: Vec<([u8; 32], String, String)> = active_subs
            .iter()
            .map(|e| (*e.key(), e.value().coin_vault.clone(), e.value().pc_vault.clone()))
            .collect();

        if subs.is_empty() {
            continue;
        }

        let sub_count = subs.len();
        if sub_count > 50 {
            tracing::warn!(
                sub_count,
                "[price_feed] large active_subs — possible unsubscribe leak"
            );
        }

        // Process in chunks of 10 mints (20 getAccountInfo calls) to stay within
        // Helius batch-size limits. Poll each chunk sequentially within the interval.
        const CHUNK_SIZE: usize = 10;
        for chunk in subs.chunks(CHUNK_SIZE) {
            let mut batch = Vec::with_capacity(chunk.len() * 2);
            for (i, (_mint, coin_vault, pc_vault)) in chunk.iter().enumerate() {
                batch.push(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": i * 2,
                    "method": "getAccountInfo",
                    "params": [coin_vault, {"encoding": "base64", "commitment": "confirmed"}]
                }));
                batch.push(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": i * 2 + 1,
                    "method": "getAccountInfo",
                    "params": [pc_vault, {"encoding": "base64", "commitment": "confirmed"}]
                }));
            }

            // Single-retry on HTTP 429: sleep 100ms, retry once, then skip chunk
            let results: Vec<serde_json::Value> = {
                let maybe = async {
                    let resp = http_client.post(&rpc_url).json(&batch).send().await
                        .map_err(|e| { tracing::warn!(error = %e, "[price_feed] RPC batch request failed"); })?;

                    if resp.status().as_u16() == 429 {
                        tracing::warn!("[price_feed] HTTP 429 rate-limited — retry in 100ms");
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

                        let resp2 = http_client.post(&rpc_url).json(&batch).send().await
                            .map_err(|e| { tracing::warn!(error = %e, "[price_feed] RPC retry request failed"); })?;

                        if resp2.status().as_u16() == 429 {
                            tracing::warn!("[price_feed] HTTP 429 on retry — skipping chunk");
                            return Err(());
                        }

                        return resp2.json::<Vec<serde_json::Value>>().await
                            .map_err(|e| { tracing::warn!(error = %e, "[price_feed] RPC retry response parse failed"); });
                    }

                    resp.json::<Vec<serde_json::Value>>().await
                        .map_err(|e| { tracing::warn!(error = %e, "[price_feed] RPC batch response parse failed"); })
                }.await;

                match maybe {
                    Ok(r) => r,
                    Err(()) => continue,
                }
            };

            // Process pairwise: results[i*2] = coin_vault, results[i*2+1] = pc_vault
            for (i, (mint, _, _)) in chunk.iter().enumerate() {
                let coin_data = extract_account_data(&results, i * 2);
                let pc_data = extract_account_data(&results, i * 2 + 1);

                let (sol_reserve, token_reserve) = match (coin_data, pc_data) {
                    (Some(coin), Some(pc)) => {
                        // Raydium vault layout:
                        // coin_vault = base token (pump.fun token) — token reserve
                        // pc_vault = quote token (WSOL) — SOL reserve
                        // SPL Token account: bytes 64..72 = amount (u64 LE)
                        let token_reserve = parse_spl_amount(&coin);
                        let sol_reserve = parse_spl_amount(&pc);
                        match (sol_reserve, token_reserve) {
                            (Some(s), Some(t)) => (s, t),
                            _ => continue,
                        }
                    }
                    _ => continue,
                };

                if sol_reserve == 0 || token_reserve == 0 {
                    continue;
                }

                let price_fp = price_from_reserves(sol_reserve, token_reserve);
                if price_fp == 0 {
                    continue;
                }

                if let Some(state) = prices.get(mint) {
                    let prev = state.price_fp.swap(price_fp, Ordering::Release);
                    state.reserve_sol.store(sol_reserve, Ordering::Relaxed);
                    state.reserve_token.store(token_reserve, Ordering::Relaxed);
                    state.last_update_ms.store(
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64,
                        Ordering::Relaxed,
                    );
                    if prev == 0 {
                        tracing::info!(
                            mint = %bs58::encode(mint).into_string(),
                            price_fp,
                            sol_reserve,
                            token_reserve,
                            "[price_feed] first price received for mint"
                        );
                    }
                }
            }
        }
    }
}

/// Extract and base64-decode account data from a JSON-RPC batch response by request id.
fn extract_account_data(results: &[serde_json::Value], id: usize) -> Option<Vec<u8>> {
    let entry = results.iter().find(|r| r.get("id").and_then(|i| i.as_u64()) == Some(id as u64))?;
    let data_arr = entry.pointer("/result/value/data")?.as_array()?;
    let b64 = data_arr.first()?.as_str()?;
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(b64).ok()
}

/// Parse SPL token account amount from raw account data.
/// SPL Token account layout: [mint(32), owner(32), amount(8), ...]
/// amount is at bytes 64..72, little-endian u64.
fn parse_spl_amount(data: &[u8]) -> Option<u64> {
    if data.len() < 72 {
        return None;
    }
    Some(u64::from_le_bytes(data[64..72].try_into().ok()?))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_price_from_reserves_basic() {
        // 79 SOL (79e9 lamports) / 206.9T token atoms → ~381 lamports per 1M atoms
        let reserve_sol: u64 = 79_000_000_000; // 79 SOL in lamports
        let reserve_token: u64 = 206_900_000_000_000; // 206.9T atoms

        let price = price_from_reserves(reserve_sol, reserve_token);

        // Expected: 79e9 * 1e6 / 206.9e12 = 79e15 / 206.9e12 = ~381.8
        // Integer division: 79_000_000_000_000_000 / 206_900_000_000_000 = 381
        assert!(
            price >= 380 && price <= 383,
            "expected ~381, got {price}"
        );
    }

    #[test]
    fn test_price_from_reserves_zero_token() {
        // Zero token reserve should return 0, not panic
        let price = price_from_reserves(79_000_000_000, 0);
        assert_eq!(price, 0);
    }

    #[test]
    fn test_price_from_reserves_zero_sol() {
        // Zero SOL reserve → price is 0
        let price = price_from_reserves(0, 206_900_000_000_000);
        assert_eq!(price, 0);
    }

    #[test]
    fn test_price_from_reserves_overflow_safety() {
        // Large reserves that would overflow u64 multiplication
        // u128 intermediate should handle this
        let reserve_sol: u64 = 10_000_000_000_000; // 10,000 SOL
        let reserve_token: u64 = 1_000_000_000_000_000; // 1 quadrillion atoms
        let price = price_from_reserves(reserve_sol, reserve_token);
        // 10e12 * 1e6 / 1e15 = 10e18 / 1e15 = 10_000
        assert_eq!(price, 10_000);
    }

    #[test]
    fn test_parse_spl_amount_valid() {
        // Create a minimal 72-byte SPL token account
        let mut data = vec![0u8; 165]; // standard SPL token account size
        // Set amount at bytes 64..72
        let amount: u64 = 1_234_567_890;
        data[64..72].copy_from_slice(&amount.to_le_bytes());
        assert_eq!(parse_spl_amount(&data), Some(amount));
    }

    #[test]
    fn test_parse_spl_amount_too_short() {
        let data = vec![0u8; 71]; // too short
        assert_eq!(parse_spl_amount(&data), None);
    }

    #[tokio::test]
    async fn test_price_feed_manager_creates() {
        // PriceFeedManager::new() should return without panic.
        // Poll loop will fail to reach RPC (invalid URL) but that's fine.
        let (manager, handle) = PriceFeedManager::new("https://invalid.example.com".to_string(), 500);

        // Verify prices map is empty initially
        assert_eq!(manager.prices.len(), 0);

        // Verify current_price returns None for unknown mint
        let unknown_mint = [0u8; 32];
        assert!(manager.current_price(&unknown_mint).is_none());

        // Shutdown the poll loop
        handle.abort();
    }
}
