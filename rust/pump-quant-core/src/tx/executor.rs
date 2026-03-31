//! Transaction executor: builds and submits buy/sell transactions via Jito,
//! or simulates them in paper mode.
//!
//! Includes a `BlockhashCache` that refreshes every 25s to avoid per-trade
//! RPC round-trips (~200ms saved per trade).

use std::sync::Arc;
use tokio::sync::RwLock;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use solana_sdk::hash::Hash;

use crate::engine::positions::ClosedPosition;
use super::builder::{BuyTxRequest, SellTxRequest, TxBuilder};
use super::jito::JitoClient;
use super::wallet::WalletManager;

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

        let mut guard = self.inner.write().await;
        *guard = Some((blockhash_str, now_ms));
        Ok(())
    }

    /// Get the cached blockhash if it's still fresh (within TTL).
    /// Returns `None` if the cache is empty or stale.
    pub async fn get(&self) -> Option<String> {
        let guard = self.inner.read().await;
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

// ── Config ───────────────────────────────────────────────────────────────────

pub struct ExecutorConfig {
    /// If true, skip building/submitting and return zeroed signatures.
    pub paper_mode: bool,
    /// Jito tip per bundle (lamports).
    pub jito_tip_lamports: u64,
    /// Priority fee in microlamports per CU.
    pub priority_fee_lamports: u64,
    /// Slippage tolerance in basis points (e.g. 300 = 3%).
    pub slippage_bps: u32,
    /// Solana RPC URL for blockhash fetching.
    pub rpc_url: String,
}

// ── Executor ─────────────────────────────────────────────────────────────────

pub struct TxExecutor {
    config: ExecutorConfig,
    wallet: Arc<WalletManager>,
    jito: Arc<JitoClient>,
    rpc: reqwest::Client,
    blockhash_cache: Arc<BlockhashCache>,
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

impl TxExecutor {
    pub fn new(
        config: ExecutorConfig,
        wallet: Arc<WalletManager>,
        jito: Arc<JitoClient>,
        blockhash_cache: Arc<BlockhashCache>,
    ) -> Self {
        let rpc = reqwest::Client::new();
        Self {
            config,
            wallet,
            jito,
            rpc,
            blockhash_cache,
        }
    }

    /// Compute the Jito bundle tip for an exit transaction.
    ///
    /// Uses absolute per-tier floors calibrated to Solana MEV market (2026-Q1),
    /// plus a size-proportional component so larger positions pay more.
    /// PnL cap is tiered by gain magnitude to protect small wins.
    pub fn compute_exit_tip(
        size_lamports: u64,
        gain_bps: i64,
        is_hard_sl: bool,
        is_trailing_stop: bool,
        is_time_sl: bool,
        is_max_hold: bool,
    ) -> u64 {
        // ── Absolute floors per urgency tier (min competitive Jito tip) ──
        const FLOOR_HARD_SL: u64      = 500_000;   // 0.0005  SOL — must land NOW
        const FLOOR_TRAIL_LOSS: u64   = 300_000;   // 0.0003  SOL — trailing stop underwater
        const FLOOR_TRAIL_WIN: u64    = 100_000;   // 0.0001  SOL — trailing stop in profit
        const FLOOR_TP: u64           =  80_000;   // 0.00008 SOL — take profit
        const FLOOR_TIME_SL: u64      =  50_000;   // 0.00005 SOL — dead zone kill
        const FLOOR_MAX_HOLD: u64     =  50_000;   // 0.00005 SOL — timeout

        // ── Size-proportional rates (bps of position size) ──
        const RATE_HARD_SL: u64     = 20;  // 0.20% of position
        const RATE_TRAIL_LOSS: u64  = 12;  // 0.12%
        const RATE_TRAIL_WIN: u64   =  6;  // 0.06%
        const RATE_TP: u64          =  5;  // 0.05%
        const RATE_TIME_SL: u64     =  3;  // 0.03%
        const RATE_MAX_HOLD: u64    =  3;  // 0.03%

        // ── Ceilings ──
        const CEILING_NORMAL: u64   =  5_000_000;  // 0.005 SOL
        const CEILING_HARD_SL: u64  = 20_000_000;  // 0.020 SOL

        let (floor, rate_bps, ceiling) = if is_hard_sl {
            (FLOOR_HARD_SL, RATE_HARD_SL, CEILING_HARD_SL)
        } else if is_trailing_stop && gain_bps < 0 {
            (FLOOR_TRAIL_LOSS, RATE_TRAIL_LOSS, CEILING_NORMAL)
        } else if is_trailing_stop {
            (FLOOR_TRAIL_WIN, RATE_TRAIL_WIN, CEILING_NORMAL)
        } else if is_time_sl {
            (FLOOR_TIME_SL, RATE_TIME_SL, CEILING_NORMAL)
        } else if is_max_hold {
            (FLOOR_MAX_HOLD, RATE_MAX_HOLD, CEILING_NORMAL)
        } else {
            (FLOOR_TP, RATE_TP, CEILING_NORMAL)
        };

        // Single division — no precision loss from chained integer ops
        let proportional_tip = size_lamports * rate_bps / 10_000;
        let tip = proportional_tip.max(floor);

        // PnL cap: only on winning, non-hard-SL exits
        // Tiered by gain size to protect small wins from excessive tip cost
        let tip = if gain_bps > 0 && !is_hard_sl {
            let gross_lamports = (size_lamports as u128 * gain_bps as u128 / 10_000) as u64;
            let cap_pct: u64 = if gain_bps < 2_000 { 15 }
                               else if gain_bps < 10_000 { 10 }
                               else { 5 };
            let pnl_cap = gross_lamports * cap_pct / 100;
            tip.min(pnl_cap.max(floor))
        } else {
            tip
        };

        tip.min(ceiling)
    }

    /// Compute the Jito bundle tip for an entry (buy) transaction.
    ///
    /// Higher-scored graduations get more aggressive entry tips to land
    /// before price moves away from the graduation level.
    pub fn compute_buy_tip(grad_score: f64, size_lamports: u64) -> u64 {
        const BUY_FLOOR: u64   = 100_000;    // 0.0001 SOL minimum
        const BUY_CEILING: u64 = 5_000_000;  // 0.005 SOL maximum

        // 0.05% of position size as base
        let base = size_lamports * 5 / 10_000;

        // Score multiplier: score 50→1.0x, 100→3.0x linear
        let score_clamped = (grad_score as u64).clamp(50, 100);
        let multiplier_pct = 100 + (score_clamped - 50) * 4; // 100–300%

        let tip = base * multiplier_pct / 100;
        tip.clamp(BUY_FLOOR, BUY_CEILING)
    }

    /// Execute a buy transaction.
    ///
    /// In paper mode: returns a zeroed `[u8; 64]` signature immediately.
    /// In live mode: gets blockhash (cache-first), builds the buy tx, submits via Jito.
    pub async fn execute_buy(
        &self,
        mint: [u8; 32],
        bonding_curve: [u8; 32],
        assoc_bonding_curve: [u8; 32],
        vsol_lamports: u64,
        vtokens: u64,
        size_lamports: u64,
    ) -> Result<[u8; 64]> {
        if self.config.paper_mode {
            tracing::debug!("paper mode: simulated buy for mint {}", bs58::encode(&mint).into_string());
            return Ok([0u8; 64]);
        }

        // Get blockhash: cache-first, fall back to fresh RPC fetch
        let blockhash = self.get_blockhash().await?;

        // Build the buy transaction
        let keypair = self.wallet.current_keypair();
        let builder = TxBuilder::new(
            solana_sdk::signature::Keypair::from_bytes(&keypair.to_bytes())
                .context("failed to clone keypair for TxBuilder")?,
        );

        let req = BuyTxRequest {
            mint,
            bonding_curve,
            assoc_bonding_curve,
            vsol_lamports,
            vtokens,
            size_lamports,
            slippage_bps: self.config.slippage_bps,
            priority_fee_microlamports: self.config.priority_fee_lamports,
            // TODO: pass grad_score through to use score-adaptive buy tip
            jito_tip_lamports: Self::compute_buy_tip(0.0, size_lamports), // TODO: pass grad_score when available
            recent_blockhash: blockhash.to_bytes(),
        };

        let tx = builder.build_buy_tx(&req).context("failed to build buy tx")?;

        // Submit via Jito
        let bundle_id = self.jito.submit_bundle(&tx).await
            .context("failed to submit buy bundle to Jito")?;

        tracing::info!(
            "buy bundle submitted: {}, mint: {}",
            bundle_id,
            bs58::encode(&mint).into_string()
        );

        // Return the transaction signature
        Ok(tx.signatures[0].into())
    }

    /// Execute a sell transaction for a closed position.
    ///
    /// In paper mode: logs what the dynamic tip WOULD have been and returns a zeroed signature.
    /// In live mode: computes dynamic tip, builds the sell tx, submits via Jito.
    pub async fn execute_sell(
        &self,
        pos: &ClosedPosition,
        vtokens_current: u64,
        gain_bps: i64,
        exit_reason_str: &str,
    ) -> Result<[u8; 64]> {
        let is_hard_sl = exit_reason_str == "hard_sl";
        let is_trailing_stop = exit_reason_str == "trailing_stop";
        let is_time_sl = exit_reason_str == "time_sl";
        let is_max_hold = exit_reason_str == "max_hold";

        if self.config.paper_mode {
            let would_tip = Self::compute_exit_tip(
                pos.size_sol, gain_bps, is_hard_sl, is_trailing_stop, is_time_sl, is_max_hold,
            );
            tracing::debug!(
                would_tip_lamports = would_tip,
                exit_reason = exit_reason_str,
                gain_bps,
                size_lamports = pos.size_sol,
                "[executor] paper mode: simulated sell for mint {}",
                bs58::encode(&pos.mint).into_string()
            );
            return Ok([0u8; 64]);
        }

        let dynamic_tip = Self::compute_exit_tip(
            pos.size_sol, gain_bps, is_hard_sl, is_trailing_stop, is_time_sl, is_max_hold,
        );

        tracing::debug!(
            dynamic_tip_lamports = dynamic_tip,
            exit_reason = exit_reason_str,
            gain_bps,
            "[executor] computed dynamic exit tip"
        );

        // Get blockhash: cache-first, fall back to fresh RPC fetch
        let blockhash = self.get_blockhash().await?;

        // Build the sell transaction
        let keypair = self.wallet.current_keypair();
        let builder = TxBuilder::new(
            solana_sdk::signature::Keypair::from_bytes(&keypair.to_bytes())
                .context("failed to clone keypair for TxBuilder")?,
        );

        // Calculate minimum SOL out with slippage
        let sell_sim = crate::engine::bonding_curve::simulate_sell(
            pos.current_vsol,
            vtokens_current,
            pos.tokens_held,
        );
        let min_sol_out = if self.config.slippage_bps >= 10_000 {
            0
        } else {
            (sell_sim.sol_out as u128 * (10_000 - self.config.slippage_bps) as u128 / 10_000) as u64
        };

        let req = SellTxRequest {
            mint: pos.mint,
            bonding_curve: pos.bonding_curve,
            assoc_bonding_curve: pos.assoc_bonding_curve,
            tokens_to_sell: pos.tokens_held,
            vsol_lamports: pos.current_vsol,
            vtokens: vtokens_current,
            min_sol_out_lamports: min_sol_out,
            priority_fee_microlamports: self.config.priority_fee_lamports,
            jito_tip_lamports: dynamic_tip,
            recent_blockhash: blockhash.to_bytes(),
        };

        let tx = builder.build_sell_tx(&req).context("failed to build sell tx")?;

        // Submit via Jito
        let bundle_id = self.jito.submit_bundle(&tx).await
            .context("failed to submit sell bundle to Jito")?;

        tracing::info!(
            "sell bundle submitted: {}, mint: {}, tip: {}",
            bundle_id,
            bs58::encode(&pos.mint).into_string(),
            dynamic_tip,
        );

        // Return the transaction signature
        Ok(tx.signatures[0].into())
    }

    /// Get a blockhash, trying the cache first. Falls back to a direct RPC
    /// call if the cache is empty or stale.
    async fn get_blockhash(&self) -> Result<Hash> {
        // Try cache first
        if let Some(cached) = self.blockhash_cache.get().await {
            return Self::parse_blockhash_str(&cached);
        }

        // Cache miss/stale — fetch directly
        tracing::debug!("blockhash cache miss, fetching from RPC");
        self.fetch_latest_blockhash().await
    }

    /// Parse a base58 blockhash string into a `Hash`.
    fn parse_blockhash_str(s: &str) -> Result<Hash> {
        let hash_bytes = bs58::decode(s)
            .into_vec()
            .context("blockhash is not valid base58")?;

        if hash_bytes.len() != 32 {
            bail!(
                "blockhash decoded to {} bytes, expected 32",
                hash_bytes.len()
            );
        }

        let mut arr = [0u8; 32];
        arr.copy_from_slice(&hash_bytes);
        Ok(Hash::new_from_array(arr))
    }

    /// Fetch the latest blockhash directly from the Solana RPC.
    async fn fetch_latest_blockhash(&self) -> Result<Hash> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getLatestBlockhash",
            "params": [{"commitment": "finalized"}]
        });

        let resp = self
            .rpc
            .post(&self.config.rpc_url)
            .json(&body)
            .send()
            .await
            .context("failed to send getLatestBlockhash RPC request")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("RPC returned HTTP {status}: {text}");
        }

        let parsed: RpcResponse = resp
            .json()
            .await
            .context("failed to parse getLatestBlockhash response")?;

        if let Some(err) = parsed.error {
            bail!("RPC error in getLatestBlockhash: {err}");
        }

        let blockhash_str = parsed
            .result
            .context("getLatestBlockhash response missing 'result'")?
            .value
            .blockhash;

        Self::parse_blockhash_str(&blockhash_str)
    }
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
            let mut guard = cache.inner.write().await;
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
            let mut guard = cache.inner.write().await;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            // Cached 200ms ago — stale
            *guard = Some(("StaleHash".to_string(), now.saturating_sub(200)));
        }
        assert!(cache.get().await.is_none());
    }

    #[test]
    fn test_parse_blockhash_str_valid() {
        // A valid 32-byte base58-encoded string
        let hash = [42u8; 32];
        let encoded = bs58::encode(&hash).into_string();
        let result = TxExecutor::parse_blockhash_str(&encoded).unwrap();
        assert_eq!(result.to_bytes(), hash);
    }

    #[test]
    fn test_parse_blockhash_str_invalid_length() {
        let short = bs58::encode(&[0u8; 16]).into_string();
        assert!(TxExecutor::parse_blockhash_str(&short).is_err());
    }
}
