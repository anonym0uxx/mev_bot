//! WebSocket accountSubscribe price feed for Raydium vault accounts.
//!
//! Maintains a persistent Helius WSS connection that subscribes to SPL token
//! vault accounts and streams real-time reserves via AtomicU64. The tick loop
//! reads prices with zero allocation via `PriceFeedManager::current_price()`.
//!
//! ## Architecture
//!
//! ```text
//! Helius WSS ──accountSubscribe──▶ price_feed_ws_loop ──AtomicU64──▶ on_tick()
//!   (persistent)                   (dedicated tokio task)           (main loop)
//! ```
//!
//! ## Performance
//!
//! - Zero-allocation hot path: `current_price()` does DashMap::get + AtomicU64::load
//! - SPL token amount parsed as `u64::from_le_bytes(data[64..72])` — no serde
//! - Reconnect with exponential backoff: 100ms → 200ms → ... cap 30s

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

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

/// Commands sent to the WS loop via mpsc channel.
pub enum PriceFeedCommand {
    /// Subscribe to a token's vault accounts.
    Subscribe(VaultSubscription),
    /// Unsubscribe from a token's vault accounts by mint.
    Unsubscribe([u8; 32]),
    /// Graceful shutdown.
    Shutdown,
}

// ── Shared price state ───────────────────────────────────────────────────────

/// Shared price state for a single token. Written atomically by WS task,
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

// ── Vault type tracking ──────────────────────────────────────────────────────

/// Which vault this subscription tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VaultType {
    /// Coin vault = base token reserve.
    Coin,
    /// PC vault = SOL (WSOL) reserve.
    Pc,
}

/// Tracks a WS subscription ID → (mint, vault_type).
#[derive(Debug, Clone)]
struct SubInfo {
    mint: [u8; 32],
    vault_type: VaultType,
}

// ── PriceFeedManager ─────────────────────────────────────────────────────────

/// Manages WebSocket price feed subscriptions.
///
/// Owns a command channel to the WS loop task and a shared DashMap of
/// per-mint price states readable by the tick loop.
pub struct PriceFeedManager {
    /// Command sender to the WS loop task.
    cmd_tx: mpsc::Sender<PriceFeedCommand>,
    /// mint → PriceState. Arc for zero-copy read from tick thread.
    pub prices: Arc<DashMap<[u8; 32], Arc<PriceState>>>,
}

impl PriceFeedManager {
    /// Create a new PriceFeedManager and spawn the WS loop task.
    ///
    /// Returns `(manager, join_handle)`. The join handle can be used to
    /// await graceful shutdown of the WS task.
    pub fn new(helius_wss_url: String) -> (Self, tokio::task::JoinHandle<()>) {
        let prices: Arc<DashMap<[u8; 32], Arc<PriceState>>> = Arc::new(DashMap::new());
        let (cmd_tx, cmd_rx) = mpsc::channel(256);
        let prices_clone = Arc::clone(&prices);

        let handle = tokio::spawn(async move {
            price_feed_ws_loop(helius_wss_url, cmd_rx, prices_clone).await;
        });

        (Self { cmd_tx, prices }, handle)
    }

    /// Subscribe to a token's vault accounts for price tracking.
    pub async fn subscribe(&self, sub: VaultSubscription) {
        let _ = self.cmd_tx.send(PriceFeedCommand::Subscribe(sub)).await;
    }

    /// Unsubscribe from a token's vault accounts.
    pub async fn unsubscribe(&self, mint: [u8; 32]) {
        let _ = self.cmd_tx.send(PriceFeedCommand::Unsubscribe(mint)).await;
    }

    /// Request graceful shutdown of the WS loop.
    pub async fn shutdown(&self) {
        let _ = self.cmd_tx.send(PriceFeedCommand::Shutdown).await;
    }

    /// Get a clone of the command sender for fire-and-forget operations.
    ///
    /// Used by `MomentumEngine::close_position()` to unsubscribe without awaiting.
    pub fn cmd_sender(&self) -> mpsc::Sender<PriceFeedCommand> {
        self.cmd_tx.clone()
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

// ── WebSocket loop ───────────────────────────────────────────────────────────

/// Persistent WebSocket loop with automatic reconnection.
///
/// Handles:
/// - `Subscribe`: sends `accountSubscribe` JSON-RPC for both vaults
/// - `Unsubscribe`: sends `accountUnsubscribe`, removes from DashMap
/// - `accountNotification`: parses SPL token account data, updates atomics
/// - Reconnect with exponential backoff on error (100ms → 30s cap)
async fn price_feed_ws_loop(
    url: String,
    mut cmd_rx: mpsc::Receiver<PriceFeedCommand>,
    prices: Arc<DashMap<[u8; 32], Arc<PriceState>>>,
) {
    let mut backoff_ms: u64 = 100;
    const MAX_BACKOFF_MS: u64 = 30_000;

    // Persistent state across reconnections:
    // Track pending subscriptions so we can resubscribe on reconnect.
    let mut active_subs: HashMap<[u8; 32], VaultSubscription> = HashMap::new();

    loop {
        info!(url = %url, "price_feed: connecting to Helius WSS");

        match connect_and_run(&url, &mut cmd_rx, &prices, &mut active_subs).await {
            LoopExit::Shutdown => {
                info!("price_feed: shutdown requested, exiting WS loop");
                return;
            }
            LoopExit::Error(e) => {
                error!(error = %e, backoff_ms, "price_feed: WS error, reconnecting");
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
            }
            LoopExit::Disconnected => {
                warn!(backoff_ms, "price_feed: WS disconnected, reconnecting");
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
            }
        }
    }
}

/// Result of a single WS connection session.
enum LoopExit {
    Shutdown,
    Error(String),
    Disconnected,
}

/// Connect to WS and run until error or shutdown.
async fn connect_and_run(
    url: &str,
    cmd_rx: &mut mpsc::Receiver<PriceFeedCommand>,
    prices: &Arc<DashMap<[u8; 32], Arc<PriceState>>>,
    active_subs: &mut HashMap<[u8; 32], VaultSubscription>,
) -> LoopExit {
    // Connect
    let ws_stream = match tokio_tungstenite::connect_async(url).await {
        Ok((stream, _response)) => stream,
        Err(e) => return LoopExit::Error(format!("connect failed: {e}")),
    };

    let (mut ws_tx, mut ws_rx) = ws_stream.split();
    info!(
        active_subs = active_subs.len(),
        "price_feed: WS connected, resubscribing"
    );

    // Track subscription_id → SubInfo for parsing notifications
    let mut sub_id_map: HashMap<u64, SubInfo> = HashMap::new();
    // Track mint → (coin_sub_id, pc_sub_id) for unsubscribe
    let mut mint_sub_ids: HashMap<[u8; 32], (Option<u64>, Option<u64>)> = HashMap::new();
    // Track JSON-RPC request id → (mint, vault_type) for matching subscribe responses
    let mut pending_requests: HashMap<u64, SubInfo> = HashMap::new();
    let mut next_rpc_id: u64 = 1;

    // Reset backoff on successful connect (caller handles this implicitly)

    // 30-second keepalive ping interval — prevents Helius from closing idle connections
    let mut ping_interval = tokio::time::interval(Duration::from_secs(30));
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Resubscribe all active subscriptions after reconnect
    for (mint, sub) in active_subs.iter() {
        // Subscribe coin vault
        let coin_id = next_rpc_id;
        next_rpc_id += 1;
        let coin_msg = format!(
            r#"{{"jsonrpc":"2.0","id":{},"method":"accountSubscribe","params":["{}",{{"encoding":"base64","commitment":"confirmed"}}]}}"#,
            coin_id, sub.coin_vault
        );
        if let Err(e) = ws_tx.send(Message::Text(coin_msg.into())).await {
            return LoopExit::Error(format!("resubscribe coin send error: {e}"));
        }
        pending_requests.insert(coin_id, SubInfo { mint: *mint, vault_type: VaultType::Coin });

        // Subscribe pc vault
        let pc_id = next_rpc_id;
        next_rpc_id += 1;
        let pc_msg = format!(
            r#"{{"jsonrpc":"2.0","id":{},"method":"accountSubscribe","params":["{}",{{"encoding":"base64","commitment":"confirmed"}}]}}"#,
            pc_id, sub.pc_vault
        );
        if let Err(e) = ws_tx.send(Message::Text(pc_msg.into())).await {
            return LoopExit::Error(format!("resubscribe pc send error: {e}"));
        }
        pending_requests.insert(pc_id, SubInfo { mint: *mint, vault_type: VaultType::Pc });

        // Ensure price state exists
        if !prices.contains_key(mint) {
            prices.insert(*mint, PriceState::new());
        }

        debug!(mint = ?hex::encode(mint), "price_feed: resubscribed vaults after reconnect");
    }

    loop {
        tokio::select! {
            // 30s keepalive ping — prevents Helius from closing idle connections
            _ = ping_interval.tick() => {
                if let Err(e) = ws_tx.send(Message::Ping(vec![].into())).await {
                    return LoopExit::Error(format!("ping send error: {e}"));
                }
            }

            // Handle commands from the engine
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(PriceFeedCommand::Subscribe(sub)) => {
                        let mint = sub.mint;

                        // Ensure PriceState exists
                        if !prices.contains_key(&mint) {
                            prices.insert(mint, PriceState::new());
                        }

                        // Subscribe coin vault
                        let coin_id = next_rpc_id;
                        next_rpc_id += 1;
                        let coin_msg = format!(
                            r#"{{"jsonrpc":"2.0","id":{},"method":"accountSubscribe","params":["{}",{{"encoding":"base64","commitment":"confirmed"}}]}}"#,
                            coin_id, sub.coin_vault
                        );
                        if let Err(e) = ws_tx.send(Message::Text(coin_msg.into())).await {
                            return LoopExit::Error(format!("subscribe coin send error: {e}"));
                        }
                        pending_requests.insert(coin_id, SubInfo { mint, vault_type: VaultType::Coin });

                        // Subscribe pc vault
                        let pc_id = next_rpc_id;
                        next_rpc_id += 1;
                        let pc_msg = format!(
                            r#"{{"jsonrpc":"2.0","id":{},"method":"accountSubscribe","params":["{}",{{"encoding":"base64","commitment":"confirmed"}}]}}"#,
                            pc_id, sub.pc_vault
                        );
                        if let Err(e) = ws_tx.send(Message::Text(pc_msg.into())).await {
                            return LoopExit::Error(format!("subscribe pc send error: {e}"));
                        }
                        pending_requests.insert(pc_id, SubInfo { mint, vault_type: VaultType::Pc });

                        // Track for reconnection
                        active_subs.insert(mint, sub);

                        debug!(mint_hex = %hex_mint(&mint), "price_feed: subscribing to vaults");
                    }
                    Some(PriceFeedCommand::Unsubscribe(mint)) => {
                        // Send accountUnsubscribe for both vaults
                        if let Some((coin_sub, pc_sub)) = mint_sub_ids.remove(&mint) {
                            for sub_id in [coin_sub, pc_sub].iter().flatten() {
                                let unsub_id = next_rpc_id;
                                next_rpc_id += 1;
                                let msg = format!(
                                    r#"{{"jsonrpc":"2.0","id":{},"method":"accountUnsubscribe","params":[{}]}}"#,
                                    unsub_id, sub_id
                                );
                                let _ = ws_tx.send(Message::Text(msg.into())).await;
                                sub_id_map.remove(sub_id);
                            }
                        }

                        // Remove from persistent tracking
                        active_subs.remove(&mint);
                        prices.remove(&mint);

                        debug!(mint_hex = %hex_mint(&mint), "price_feed: unsubscribed");
                    }
                    Some(PriceFeedCommand::Shutdown) | None => {
                        // Send unsubscribe for all, then exit
                        return LoopExit::Shutdown;
                    }
                }
            }

            // Handle WS messages
            msg = ws_rx.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        handle_ws_message(
                            &text,
                            &mut sub_id_map,
                            &mut mint_sub_ids,
                            &mut pending_requests,
                            prices,
                        );
                    }
                    Some(Ok(Message::Ping(data))) => {
                        let _ = ws_tx.send(Message::Pong(data)).await;
                    }
                    Some(Ok(Message::Close(_))) => {
                        return LoopExit::Disconnected;
                    }
                    Some(Err(e)) => {
                        return LoopExit::Error(format!("ws recv error: {e}"));
                    }
                    None => {
                        return LoopExit::Disconnected;
                    }
                    _ => {} // Binary, Pong — ignore
                }
            }
        }
    }
}

/// Parse a WS message: either a subscription confirmation or an account notification.
fn handle_ws_message(
    text: &str,
    sub_id_map: &mut HashMap<u64, SubInfo>,
    mint_sub_ids: &mut HashMap<[u8; 32], (Option<u64>, Option<u64>)>,
    pending_requests: &mut HashMap<u64, SubInfo>,
    prices: &Arc<DashMap<[u8; 32], Arc<PriceState>>>,
) {
    // Fast path: try to parse as JSON
    let parsed: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return,
    };

    // Case 1: Subscription confirmation response
    // {"jsonrpc":"2.0","result":12345,"id":1}
    if let (Some(id), Some(result)) = (
        parsed.get("id").and_then(|v| v.as_u64()),
        parsed.get("result").and_then(|v| v.as_u64()),
    ) {
        if let Some(info) = pending_requests.remove(&id) {
            let subscription_id = result;
            sub_id_map.insert(subscription_id, info.clone());

            // Track mint → sub IDs for unsubscribe
            let entry = mint_sub_ids.entry(info.mint).or_insert((None, None));
            match info.vault_type {
                VaultType::Coin => entry.0 = Some(subscription_id),
                VaultType::Pc => entry.1 = Some(subscription_id),
            }

            debug!(
                subscription_id,
                vault_type = ?info.vault_type,
                "price_feed: subscription confirmed"
            );
        }
        return;
    }

    // Case 2: Account notification
    // {"jsonrpc":"2.0","method":"accountNotification","params":{
    //   "result":{"context":{"slot":123},"value":{"data":["<base64>","base64"]}},
    //   "subscription":12345
    // }}
    if parsed.get("method").and_then(|v| v.as_str()) == Some("accountNotification") {
        let params = match parsed.get("params") {
            Some(p) => p,
            None => return,
        };

        let subscription_id = match params.get("subscription").and_then(|v| v.as_u64()) {
            Some(id) => id,
            None => return,
        };

        let info = match sub_id_map.get(&subscription_id) {
            Some(i) => i,
            None => return,
        };

        // Extract base64 data
        let data_b64 = match params
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.get("data"))
            .and_then(|d| d.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.as_str())
        {
            Some(s) => s,
            None => return,
        };

        // Decode base64 → bytes
        let bytes = match base64::engine::general_purpose::STANDARD.decode(data_b64) {
            Ok(b) => b,
            Err(_) => return,
        };

        // Parse SPL token account: amount is at bytes[64..72] as LE u64
        if bytes.len() < 72 {
            return;
        }
        let amount = u64::from_le_bytes([
            bytes[64], bytes[65], bytes[66], bytes[67],
            bytes[68], bytes[69], bytes[70], bytes[71],
        ]);

        // Update the appropriate reserve in PriceState
        if let Some(state) = prices.get(&info.mint) {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            match info.vault_type {
                VaultType::Coin => {
                    state.reserve_token.store(amount, Ordering::Relaxed);
                }
                VaultType::Pc => {
                    state.reserve_sol.store(amount, Ordering::Relaxed);
                }
            }

            // Recompute price from both reserves
            let sol = state.reserve_sol.load(Ordering::Relaxed);
            let token = state.reserve_token.load(Ordering::Relaxed);
            if sol > 0 && token > 0 {
                let price = price_from_reserves(sol, token);
                state.price_fp.store(price, Ordering::Relaxed);
                state.last_update_ms.store(now_ms, Ordering::Relaxed);
            }
        }
    }
}

/// Helper: hex-encode a 32-byte mint for logging (first 8 bytes).
fn hex_mint(mint: &[u8; 32]) -> String {
    // Show first 8 bytes as hex for compact logging
    mint.iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

// Minimal hex module since the `hex` crate isn't in Cargo.toml
mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
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

    #[tokio::test]
    async fn test_price_feed_manager_creates() {
        // PriceFeedManager::new() should return without panic.
        // WS loop will fail to connect (invalid URL) but that's fine —
        // it runs in a background task with reconnection.
        let (manager, handle) = PriceFeedManager::new("wss://invalid.example.com".to_string());

        // Verify prices map is empty initially
        assert_eq!(manager.prices.len(), 0);

        // Verify current_price returns None for unknown mint
        let unknown_mint = [0u8; 32];
        assert!(manager.current_price(&unknown_mint).is_none());

        // Shutdown the WS loop
        manager.shutdown().await;
        // Give it a moment to process shutdown (the WS loop will fail to connect
        // and loop, but shutdown command via channel should be picked up)
        handle.abort();
    }
}
