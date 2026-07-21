//! Sell pipeline reliability engine with escalation ladder and retry logic.
//!
//! ## Problem
//! The original sell path fires a single `rpc_sender.submit_tx()` from a `tokio::spawn`.
//! If that TX fails (slippage, RPC error, circuit breaker open), the token is stuck
//! in the wallet forever. 13 tokens were lost this way (0.219 SOL, 55% of total losses).
//!
//! ## Solution
//! `SellEngine` runs as a background tokio task that:
//! 1. Accepts `PendingSell` requests from `close_position()`
//! 2. Processes them through a 5-level escalation ladder (increasing slippage + priority fees)
//! 3. Rebuilds TXs at each level with modified parameters
//! 4. Logs exhausted sells for Telegram alerting
//!
//! `inventory_watchdog` runs every 30s to detect orphaned tokens on-chain that
//! aren't in any active position or sell queue, and submits emergency sells.

use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;

use crate::momentum::rpc_sender::{self, RpcSender};
use crate::tx::executor::BlockhashCache;
use crate::tx::pumpswap::PumpSwapPoolAccounts;
use crate::tx::raydium::RaydiumPoolAccounts;

// ── Sell Strategy & Escalation ───────────────────────────────────────────────

/// Strategy for each escalation level. Controls TX parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SellStrategy {
    /// Current rpc_sender.submit_tx path with default parameters.
    RpcDefault,
    /// Same path but with higher compute unit price (3x base priority fee).
    RpcHighPriority,
    /// Rebuild TX with much higher slippage tolerance.
    RpcMaxSlippage,
    /// Nuclear option: min_sol_out = 0, max priority fee. Get out at any cost.
    ForceMarketSell,
}

/// Configuration for a single escalation attempt.
#[derive(Debug, Clone, Copy)]
pub struct SellEscalation {
    /// 0-indexed attempt number.
    pub attempt: u8,
    /// Which strategy to use for this attempt.
    pub strategy: SellStrategy,
    /// Maximum acceptable slippage in basis points.
    pub max_slippage_bps: u16,
    /// Additional priority fee in lamports (on top of base Jito tip).
    pub extra_priority_lamports: u64,
    /// Compute unit price override (microlamports per CU).
    /// Base is 5000; escalation increases this.
    pub compute_unit_price: u64,
    /// How long to wait for confirmation before escalating.
    pub timeout: Duration,
}

/// 5-level escalation ladder. Each level is progressively more aggressive.
///
/// Level 0: Default params (300 bps slippage, base priority)
/// Level 1: Wider slippage (800 bps), small priority bump
/// Level 2: High priority (3x CU price), 1500 bps slippage
/// Level 3: Max slippage (5000 bps), high priority fee
/// Level 4: Force market sell (9900 bps ≈ 99%, max priority)
pub const ESCALATION_LADDER: [SellEscalation; 5] = [
    SellEscalation {
        attempt: 0,
        strategy: SellStrategy::RpcDefault,
        max_slippage_bps: 300,
        extra_priority_lamports: 0,
        compute_unit_price: 5_000,
        timeout: Duration::from_millis(3_000),
    },
    SellEscalation {
        attempt: 1,
        strategy: SellStrategy::RpcDefault,
        max_slippage_bps: 800,
        extra_priority_lamports: 10_000,
        compute_unit_price: 8_000,
        timeout: Duration::from_millis(3_000),
    },
    SellEscalation {
        attempt: 2,
        strategy: SellStrategy::RpcHighPriority,
        max_slippage_bps: 1_500,
        extra_priority_lamports: 50_000,
        compute_unit_price: 15_000,
        timeout: Duration::from_millis(5_000),
    },
    SellEscalation {
        attempt: 3,
        strategy: SellStrategy::RpcMaxSlippage,
        max_slippage_bps: 5_000,
        extra_priority_lamports: 100_000,
        compute_unit_price: 30_000,
        timeout: Duration::from_millis(5_000),
    },
    SellEscalation {
        attempt: 4,
        strategy: SellStrategy::ForceMarketSell,
        max_slippage_bps: 9_900,
        extra_priority_lamports: 200_000,
        compute_unit_price: 100_000,
        timeout: Duration::from_millis(10_000),
    },
];

// ── Pool info union ──────────────────────────────────────────────────────────

/// Pool accounts needed to rebuild sell TXs. Stored once at submission time.
#[derive(Clone)]
pub enum PoolAccounts {
    Raydium(RaydiumPoolAccounts),
    PumpSwap(PumpSwapPoolAccounts),
}

// ── Sell result ──────────────────────────────────────────────────────────────

/// Outcome of a single sell attempt.
enum SellAttemptResult {
    /// TX confirmed on-chain.
    Confirmed { signature: String },
    /// TX submitted but not yet confirmed (timed out waiting).
    MaybeConfirmed { signature: String },
    /// TX definitively failed (RPC error, build error, etc.).
    Failed { error: String },
    /// Circuit breaker is open — sell was not attempted.
    CircuitOpen { remaining_ms: u64 },
}

// ── Pending sell request ─────────────────────────────────────────────────────

/// A sell request in the queue. Created by `close_position()` or `inventory_watchdog`.
pub struct PendingSell {
    /// Token mint address (32 bytes).
    pub mint: [u8; 32],
    /// Token amount to sell (from on-chain balance query, or estimated from position).
    pub tokens: u64,
    /// Pool accounts for TX building. `None` = needs resolution (orphan watchdog path).
    pub pool: Option<PoolAccounts>,
    /// Current escalation level index (0..=4).
    pub current_attempt: u8,
    /// Epoch ms when the first attempt was made.
    pub first_attempt_ms: u64,
    /// Epoch ms when the last attempt was submitted.
    pub last_attempt_ms: u64,
    /// Epoch ms when the sell was first queued.
    pub queued_at_ms: u64,
    /// Exit reason that triggered the sell (for logging).
    pub reason: String,
    /// Jito tip lamports (computed once at close time).
    pub jito_tip_lamports: u64,
    /// Fee recipient index for PumpSwap (rotated per-TX).
    pub fee_recipient_idx: usize,
}

// ── SellEngine ───────────────────────────────────────────────────────────────

/// Background engine that processes pending sells with escalation retry.
///
/// Thread-safe: all public methods are `&self`. The engine is shared via `Arc<SellEngine>`.
pub struct SellEngine {
    /// Pending sell queue: mint → PendingSell.
    pending: DashMap<[u8; 32], PendingSell>,
    /// RPC sender with rate limiter + circuit breaker.
    rpc_sender: Arc<RpcSender>,
    /// Cached recent blockhash (refreshed externally by the blockhash updater task).
    blockhash_cache: Arc<BlockhashCache>,
    /// Path to the wallet keypair JSON file.
    keypair_path: String,
    /// Public RPC URL for balance queries.
    public_rpc_url: Arc<String>,
    /// Helius RPC URL for pool resolution (getProgramAccounts).
    helius_rpc_url: Arc<String>,
    /// HTTP client for balance queries (reused, no per-request allocation).
    http_client: reqwest::Client,
    /// HTTP client for pool resolution (shared with momentum engine).
    pool_resolve_client: reqwest::Client,
    /// Counter: total sells completed successfully.
    sells_completed: std::sync::atomic::AtomicU64,
    /// Counter: total sells where all escalation attempts were exhausted.
    sells_exhausted: std::sync::atomic::AtomicU64,
}

impl SellEngine {
    /// Create a new SellEngine.
    pub fn new(
        rpc_sender: Arc<RpcSender>,
        blockhash_cache: Arc<BlockhashCache>,
        keypair_path: String,
        public_rpc_url: Arc<String>,
        helius_rpc_url: Arc<String>,
        http_client: reqwest::Client,
        pool_resolve_client: reqwest::Client,
    ) -> Arc<Self> {
        Arc::new(Self {
            pending: DashMap::with_capacity(32),
            rpc_sender,
            blockhash_cache,
            keypair_path,
            public_rpc_url,
            helius_rpc_url,
            http_client,
            pool_resolve_client,
            sells_completed: std::sync::atomic::AtomicU64::new(0),
            sells_exhausted: std::sync::atomic::AtomicU64::new(0),
        })
    }

    // ── Public API ───────────────────────────────────────────────────────────

    /// Submit a new sell request. Called from `close_position()`.
    ///
    /// If a sell for this mint is already pending, the new request is ignored
    /// (the existing one will continue escalating).
    pub fn submit_sell(&self, sell: PendingSell) {
        let mint = sell.mint;
        let mint_str = bs58::encode(&mint).into_string();

        if self.pending.contains_key(&mint) {
            tracing::debug!(
                mint = %mint_str,
                "[sell_engine] sell already pending — ignoring duplicate"
            );
            return;
        }

        tracing::info!(
            mint = %mint_str,
            tokens = sell.tokens,
            reason = %sell.reason,
            "[sell_engine] new sell queued"
        );
        self.pending.insert(mint, sell);
    }

    /// Check if a mint has a pending sell.
    #[inline]
    pub fn has_pending(&self, mint: &[u8; 32]) -> bool {
        self.pending.contains_key(mint)
    }

    /// Number of sells currently in the queue.
    #[inline]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Total sells completed successfully.
    #[inline]
    pub fn total_completed(&self) -> u64 {
        self.sells_completed.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Total sells where all escalation attempts were exhausted.
    #[inline]
    pub fn total_exhausted(&self) -> u64 {
        self.sells_exhausted.load(std::sync::atomic::Ordering::Relaxed)
    }

    // ── Background Loop ──────────────────────────────────────────────────────

    /// Background loop: process pending sells with escalation.
    ///
    /// Runs forever. Checks all pending sells every 500ms.
    /// For each pending sell, if the current attempt's timeout has elapsed,
    /// escalates to the next level and retries.
    ///
    /// Processes sells sequentially within each tick to avoid overwhelming RPC.
    pub async fn run(self: &Arc<Self>) {
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        tracing::info!("[sell_engine] background task started");

        loop {
            interval.tick().await;

            // Collect mints to process (avoid holding DashMap refs across await).
            let mints_to_process: Vec<[u8; 32]> = self
                .pending
                .iter()
                .filter_map(|entry| {
                    let sell = entry.value();
                    let now = current_ms();

                    // First attempt: process immediately (last_attempt_ms == 0).
                    if sell.last_attempt_ms == 0 {
                        return Some(*entry.key());
                    }

                    // Check if current attempt's timeout has elapsed.
                    let idx = (sell.current_attempt as usize).min(ESCALATION_LADDER.len() - 1);
                    let escalation = &ESCALATION_LADDER[idx];
                    if now.saturating_sub(sell.last_attempt_ms)
                        >= escalation.timeout.as_millis() as u64
                    {
                        Some(*entry.key())
                    } else {
                        None
                    }
                })
                .collect();

            for mint in mints_to_process {
                self.process_sell(&mint).await;
            }
        }
    }

    // ── Internal: process a single sell ──────────────────────────────────────

    async fn process_sell(&self, mint: &[u8; 32]) {
        let mint_str = bs58::encode(mint).into_string();
        let now = current_ms();

        // Snapshot current state (release DashMap borrow before async work).
        let (current_attempt, tokens, pool, jito_tip, fee_idx, reason) = {
            let Some(entry) = self.pending.get(mint) else {
                return;
            };
            let sell = entry.value();
            (
                sell.current_attempt,
                sell.tokens,
                sell.pool.clone(),
                sell.jito_tip_lamports,
                sell.fee_recipient_idx,
                sell.reason.clone(),
            )
        };

        let idx = (current_attempt as usize).min(ESCALATION_LADDER.len() - 1);
        let escalation = &ESCALATION_LADDER[idx];

        tracing::info!(
            mint = %mint_str,
            attempt = current_attempt,
            strategy = ?escalation.strategy,
            slippage_bps = escalation.max_slippage_bps,
            extra_priority = escalation.extra_priority_lamports,
            "[sell_engine] attempting sell"
        );

        // Resolve pool if missing (orphan watchdog path).
        let pool = match pool {
            Some(p) => p,
            None => match self.resolve_pool(mint).await {
                Some(p) => {
                    // Store resolved pool back.
                    if let Some(mut entry) = self.pending.get_mut(mint) {
                        entry.value_mut().pool = Some(p.clone());
                    }
                    p
                }
                None => {
                    tracing::error!(
                        mint = %mint_str,
                        attempt = current_attempt,
                        "[sell_engine] pool resolution failed — will retry next tick"
                    );
                    if let Some(mut entry) = self.pending.get_mut(mint) {
                        entry.value_mut().last_attempt_ms = now;
                    }
                    return;
                }
            },
        };

        // Verify on-chain balance before building TX.
        let actual_tokens = match self.query_token_balance(mint).await {
            Some(bal) => bal,
            None => {
                tracing::warn!(
                    mint = %mint_str,
                    "[sell_engine] balance query failed — will retry next tick"
                );
                if let Some(mut entry) = self.pending.get_mut(mint) {
                    entry.value_mut().last_attempt_ms = now;
                }
                return;
            }
        };

        if actual_tokens == 0 {
            tracing::info!(
                mint = %mint_str,
                "[sell_engine] on-chain balance is 0 — token already sold, removing"
            );
            self.pending.remove(mint);
            self.sells_completed
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return;
        }

        // Use actual balance (may differ from original if partial sells landed).
        let tokens_to_sell = actual_tokens;

        // Build and submit TX.
        let result = self
            .attempt_sell(mint, tokens_to_sell, &pool, escalation, jito_tip, fee_idx)
            .await;

        match result {
            SellAttemptResult::Confirmed { signature } => {
                tracing::info!(
                    mint = %mint_str,
                    sig = %signature,
                    attempt = current_attempt,
                    reason = %reason,
                    "[sell_engine] SOLD ✅"
                );
                self.pending.remove(mint);
                self.sells_completed
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            SellAttemptResult::MaybeConfirmed { signature } => {
                // TX submitted but unconfirmed. May still land.
                // Next tick will re-check balance to confirm or escalate.
                tracing::warn!(
                    mint = %mint_str,
                    sig = %signature,
                    attempt = current_attempt,
                    "[sell_engine] TX submitted but unconfirmed — re-checking next tick"
                );
                if let Some(mut entry) = self.pending.get_mut(mint) {
                    entry.value_mut().last_attempt_ms = now;
                    // Don't increment attempt — TX might still land.
                }
            }
            SellAttemptResult::Failed { error } => {
                tracing::warn!(
                    mint = %mint_str,
                    attempt = current_attempt,
                    err = %error,
                    "[sell_engine] attempt failed — escalating"
                );
                if let Some(mut entry) = self.pending.get_mut(mint) {
                    let sell = entry.value_mut();
                    sell.current_attempt = current_attempt.saturating_add(1);
                    sell.last_attempt_ms = now;
                    if sell.first_attempt_ms == 0 {
                        sell.first_attempt_ms = now;
                    }

                    if sell.current_attempt as usize >= ESCALATION_LADDER.len() {
                        let elapsed_ms = now.saturating_sub(sell.queued_at_ms);
                        tracing::error!(
                            mint = %mint_str,
                            total_attempts = ESCALATION_LADDER.len(),
                            elapsed_ms,
                            tokens = tokens_to_sell,
                            reason = %reason,
                            "[sell_engine] 🚨 ALL ATTEMPTS EXHAUSTED — token stuck in wallet"
                        );
                        self.sells_exhausted
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        // Keep retrying at max escalation level with 30s cooldown.
                        // Human intervention via Telegram alert is the final backstop.
                        sell.current_attempt = (ESCALATION_LADDER.len() - 1) as u8;
                        sell.last_attempt_ms = now + 25_000; // 30s effective cooldown
                    }
                }
            }
            SellAttemptResult::CircuitOpen { remaining_ms } => {
                tracing::warn!(
                    mint = %mint_str,
                    remaining_ms,
                    attempt = current_attempt,
                    "[sell_engine] circuit breaker open — will retry after cooldown"
                );
                // Don't escalate — RPC is overloaded. Retry same level after circuit recovery.
                if let Some(mut entry) = self.pending.get_mut(mint) {
                    entry.value_mut().last_attempt_ms = now + remaining_ms;
                }
            }
        }
    }

    // ── Internal: build and submit TX ────────────────────────────────────────

    async fn attempt_sell(
        &self,
        mint: &[u8; 32],
        tokens: u64,
        pool: &PoolAccounts,
        escalation: &SellEscalation,
        base_jito_tip: u64,
        fee_recipient_idx: usize,
    ) -> SellAttemptResult {
        let mint_str = bs58::encode(mint).into_string();

        // Load keypair.
        let keypair = match self.load_keypair() {
            Ok(kp) => kp,
            Err(e) => {
                return SellAttemptResult::Failed {
                    error: format!("keypair load: {e}"),
                };
            }
        };

        // Fresh blockhash.
        let blockhash = match self.blockhash_cache.get_sync() {
            Some(bh) => bh,
            None => {
                return SellAttemptResult::Failed {
                    error: "blockhash cache empty/stale".into(),
                };
            }
        };

        // min_sol_out: Use 0 for all levels.
        // The existing codebase already uses min_sol_out=0 for PumpSwap sells
        // because non-zero floors cause SlippageExceeded (Custom:6004) when the
        // pool price moves between close decision and TX landing.
        // The AMM guarantees fair value by construction; we don't need a floor.
        let min_sol_out = 0u64;

        // Tip: base + escalation extra.
        let tip = base_jito_tip.saturating_add(escalation.extra_priority_lamports);

        // Rotate tip account.
        let tip_account = {
            use std::str::FromStr;
            let idx = (fee_recipient_idx.wrapping_add(escalation.attempt as usize))
                % crate::tx::raydium::JITO_TIP_ACCOUNTS.len();
            Pubkey::from_str(crate::tx::raydium::JITO_TIP_ACCOUNTS[idx]).unwrap()
        };

        // Build TX.
        let tx_bytes = match pool {
            PoolAccounts::Raydium(ray_pool) => {
                match crate::tx::raydium::build_raydium_sell_tx(
                    ray_pool, mint, &keypair, tokens, min_sol_out, tip, tip_account, blockhash,
                ) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        return SellAttemptResult::Failed {
                            error: format!("raydium TX build: {e}"),
                        };
                    }
                }
            }
            PoolAccounts::PumpSwap(ps_pool) => {
                match crate::tx::pumpswap::build_pumpswap_sell_tx(
                    ps_pool,
                    &keypair,
                    tokens,
                    min_sol_out,
                    tip,
                    tip_account,
                    blockhash,
                    fee_recipient_idx,
                ) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        return SellAttemptResult::Failed {
                            error: format!("pumpswap TX build: {e}"),
                        };
                    }
                }
            }
        };

        // Submit via RPC sender.
        let label = format!("sell_engine_L{}", escalation.attempt);
        match self
            .rpc_sender
            .submit_tx(&tx_bytes, &mint_str, &label)
            .await
        {
            rpc_sender::SubmitResult::Landed {
                signature,
                latency_ms,
            } => {
                tracing::info!(
                    mint = %mint_str,
                    sig = %signature,
                    latency_ms,
                    level = escalation.attempt,
                    "[sell_engine] TX landed"
                );
                SellAttemptResult::Confirmed { signature }
            }
            rpc_sender::SubmitResult::TimedOut { signature } => {
                tracing::debug!(
                    mint = %mint_str,
                    sig = %signature,
                    level = escalation.attempt,
                    "[sell_engine] TX timed out (may still land)"
                );
                SellAttemptResult::MaybeConfirmed { signature }
            }
            rpc_sender::SubmitResult::Failed { error } => SellAttemptResult::Failed { error },
            rpc_sender::SubmitResult::CircuitOpen { remaining_ms } => {
                SellAttemptResult::CircuitOpen { remaining_ms }
            }
        }
    }

    // ── Internal: keypair loading ────────────────────────────────────────────

    fn load_keypair(&self) -> Result<Keypair, String> {
        let kp_bytes =
            std::fs::read(&self.keypair_path).map_err(|e| format!("read keypair: {e}"))?;
        let kp_arr: Vec<u8> =
            serde_json::from_slice(&kp_bytes).map_err(|e| format!("parse keypair: {e}"))?;
        if kp_arr.len() != 64 {
            return Err(format!(
                "invalid keypair len: {} (expected 64)",
                kp_arr.len()
            ));
        }
        let mut kb = [0u8; 64];
        kb.copy_from_slice(&kp_arr);
        Keypair::from_bytes(&kb).map_err(|e| format!("keypair from_bytes: {e}"))
    }

    // ── Internal: balance query ──────────────────────────────────────────────

    /// Query on-chain token balance for a mint.
    /// Returns `Some(0)` if ATA doesn't exist (token sold or never existed).
    /// Returns `None` only on total RPC failure (all retries exhausted).
    async fn query_token_balance(&self, mint: &[u8; 32]) -> Option<u64> {
        let keypair = match self.load_keypair() {
            Ok(kp) => kp,
            Err(e) => {
                tracing::error!(err = %e, "[sell_engine] keypair load failed in balance query");
                return None;
            }
        };
        let wallet_pubkey = keypair.pubkey();
        let token_mint = Pubkey::new_from_array(*mint);

        // Derive ATA using standard SPL Token program.
        let token_program = {
            use std::str::FromStr;
            Pubkey::from_str(crate::tx::pumpswap::SPL_TOKEN_PROGRAM_STR).unwrap()
        };
        let ata_program = {
            use std::str::FromStr;
            Pubkey::from_str(crate::tx::pumpswap::SPL_ATA_PROGRAM_STR).unwrap()
        };
        let (token_ata, _) = Pubkey::find_program_address(
            &[
                wallet_pubkey.as_ref(),
                token_program.as_ref(),
                token_mint.as_ref(),
            ],
            &ata_program,
        );

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getTokenAccountBalance",
            "params": [token_ata.to_string()]
        });

        // Retry up to 3x with 500ms backoff.
        for attempt in 0..3u32 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }

            let resp = match self
                .http_client
                .post(self.public_rpc_url.as_str())
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::debug!(
                        mint = %bs58::encode(mint).into_string(),
                        attempt,
                        err = ?e,
                        "[sell_engine] balance RPC request failed"
                    );
                    continue;
                }
            };

            match resp.json::<serde_json::Value>().await {
                Ok(json) => {
                    if json.get("error").is_some() {
                        return Some(0); // ATA not found → already sold.
                    }
                    match json["result"]["value"]["amount"]
                        .as_str()
                        .and_then(|s| s.parse::<u64>().ok())
                    {
                        Some(bal) => return Some(bal),
                        None => return Some(0), // Null → empty account.
                    }
                }
                Err(e) => {
                    tracing::debug!(
                        mint = %bs58::encode(mint).into_string(),
                        attempt,
                        err = ?e,
                        "[sell_engine] balance response parse failed"
                    );
                    continue;
                }
            }
        }

        None // All retries exhausted.
    }

    // ── Internal: pool resolution for orphaned tokens ────────────────────────

    /// Resolve PumpSwap pool accounts for an orphaned token.
    /// Used by inventory watchdog when pool accounts aren't available.
    async fn resolve_pool(&self, mint: &[u8; 32]) -> Option<PoolAccounts> {
        let mint_str = bs58::encode(mint).into_string();

        // Try PumpSwap pool resolution (most common for pump.fun tokens).
        let resolution = crate::momentum::pool::resolve_pumpswap_pool_from_mint(
            &self.pool_resolve_client,
            mint,
            self.public_rpc_url.as_str(),
            self.helius_rpc_url.as_str(),
        )
        .await?;

        let pool_raw = crate::momentum::pool::extract_pumpswap_pool_accounts(&resolution)?;

        let mut ps_pool: PumpSwapPoolAccounts = pool_raw.into();

        // Resolve token_mint_program if missing.
        if ps_pool.token_mint_program == [0u8; 32] {
            ps_pool.token_mint_program =
                match crate::momentum::pool::resolve_mint_program_with_fallback(
                    &self.pool_resolve_client,
                    mint,
                    self.helius_rpc_url.as_str(),
                    Some(self.public_rpc_url.as_str()),
                )
                .await
                {
                    Some(prog) => prog,
                    None => crate::tx::pumpswap::SPL_TOKEN_PROGRAM_BYTES,
                };
        }

        tracing::info!(
            mint = %mint_str,
            pool = %bs58::encode(&ps_pool.pool).into_string(),
            token_is_base = ps_pool.token_is_base,
            "[sell_engine] pool resolved for orphan"
        );

        Some(PoolAccounts::PumpSwap(ps_pool))
    }
}

// ── Utility: current epoch ms ────────────────────────────────────────────────

/// Current epoch time in milliseconds.
#[inline]
fn current_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ── Inventory Watchdog ───────────────────────────────────────────────────────

/// Background task that detects orphaned tokens on-chain.
///
/// Runs every 30s. Queries all SPL token accounts owned by the wallet,
/// then checks each non-zero balance against:
/// 1. Known active positions (momentum engine)
/// 2. Pending sell queue (sell engine)
///
/// Any token with a balance that isn't in either set is considered orphaned
/// and submitted as an emergency sell.
///
/// # Arguments
/// - `sell_engine` — shared sell engine to submit emergency sells
/// - `active_positions` — live positions from the momentum engine
/// - `keypair_path` — wallet keypair for deriving pubkey
/// - `public_rpc_url` — RPC endpoint for token account queries
/// - `http_client` — reused HTTP client
///
/// # Known Tokens to Ignore
/// The watchdog ignores WSOL (wrapped SOL) since the wallet may hold it
/// for operational reasons.
pub async fn inventory_watchdog(
    sell_engine: Arc<SellEngine>,
    active_positions: Arc<DashMap<[u8; 32], crate::momentum::position::MomentumPosition>>,
    keypair_path: String,
    public_rpc_url: Arc<String>,
    http_client: reqwest::Client,
) {
    // WSOL mint — ignore (wallet may hold wrapped SOL for operations).
    const WSOL_MINT_STR: &str = "So11111111111111111111111111111111111111112";

    let mut interval = tokio::time::interval(Duration::from_secs(30));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    tracing::info!("[watchdog] inventory watchdog started (30s interval)");

    loop {
        interval.tick().await;

        // Load wallet pubkey.
        let wallet_pubkey = match load_wallet_pubkey(&keypair_path) {
            Some(pk) => pk,
            None => {
                tracing::error!("[watchdog] failed to load wallet keypair — skipping cycle");
                continue;
            }
        };

        // Query all SPL token accounts owned by the wallet.
        let token_accounts = match query_token_accounts(&wallet_pubkey, &public_rpc_url, &http_client).await {
            Some(accounts) => accounts,
            None => {
                tracing::debug!("[watchdog] token account query failed — skipping cycle");
                continue;
            }
        };

        let now = current_ms();
        let mut orphans_found = 0u32;

        for (mint_b58, balance) in &token_accounts {
            if balance == &0u64 {
                continue;
            }

            // Ignore WSOL.
            if mint_b58 == WSOL_MINT_STR {
                continue;
            }

            // Decode mint to [u8; 32].
            let mint_bytes: [u8; 32] = match bs58::decode(mint_b58).into_vec() {
                Ok(v) if v.len() == 32 => {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&v);
                    arr
                }
                _ => continue,
            };

            // Skip if tracked by momentum engine or already in sell queue.
            if active_positions.contains_key(&mint_bytes) {
                continue;
            }
            if sell_engine.has_pending(&mint_bytes) {
                continue;
            }

            tracing::error!(
                mint = %mint_b58,
                balance,
                "[watchdog] ORPHANED TOKEN — submitting emergency sell"
            );

            sell_engine.submit_sell(PendingSell {
                mint: mint_bytes,
                tokens: *balance,
                pool: None, // SellEngine will resolve pool accounts.
                current_attempt: 0,
                first_attempt_ms: 0,
                last_attempt_ms: 0,
                queued_at_ms: now,
                reason: "orphan_watchdog".to_string(),
                jito_tip_lamports: 10_000, // Minimal tip for emergency sells.
                fee_recipient_idx: (now % 8) as usize,
            });
            orphans_found += 1;
        }

        if orphans_found > 0 {
            tracing::warn!(
                count = orphans_found,
                "[watchdog] submitted {count} orphaned tokens for emergency sell",
                count = orphans_found,
            );
        }
    }
}

// ── Watchdog helpers ─────────────────────────────────────────────────────────

/// Load wallet public key from keypair file.
fn load_wallet_pubkey(keypair_path: &str) -> Option<Pubkey> {
    let kp_bytes = std::fs::read(keypair_path).ok()?;
    let kp_arr: Vec<u8> = serde_json::from_slice(&kp_bytes).ok()?;
    if kp_arr.len() != 64 {
        return None;
    }
    let mut kb = [0u8; 64];
    kb.copy_from_slice(&kp_arr);
    let keypair = Keypair::from_bytes(&kb).ok()?;
    Some(keypair.pubkey())
}

/// Query all SPL token accounts owned by a wallet.
/// Returns Vec<(mint_base58, balance_u64)>.
async fn query_token_accounts(
    wallet: &Pubkey,
    rpc_url: &str,
    http_client: &reqwest::Client,
) -> Option<Vec<(String, u64)>> {
    use std::str::FromStr;

    let spl_token_program = Pubkey::from_str(crate::tx::pumpswap::SPL_TOKEN_PROGRAM_STR).unwrap();

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getTokenAccountsByOwner",
        "params": [
            wallet.to_string(),
            { "programId": spl_token_program.to_string() },
            { "encoding": "jsonParsed" }
        ]
    });

    let resp = http_client
        .post(rpc_url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .ok()?;

    let json: serde_json::Value = resp.json().await.ok()?;

    let accounts = json["result"]["value"].as_array()?;
    let mut result = Vec::with_capacity(accounts.len());

    for account in accounts {
        let info = &account["account"]["data"]["parsed"]["info"];
        let mint = info["mint"].as_str().unwrap_or_default().to_string();
        let balance = info["tokenAmount"]["amount"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        result.push((mint, balance));
    }

    Some(result)
}

// ── Convenience: create PendingSell from close_position context ──────────────

impl PendingSell {
    /// Create a PendingSell for a Raydium position.
    pub fn from_raydium(
        mint: [u8; 32],
        tokens: u64,
        pool: RaydiumPoolAccounts,
        reason: &str,
        jito_tip_lamports: u64,
    ) -> Self {
        let now = current_ms();
        Self {
            mint,
            tokens,
            pool: Some(PoolAccounts::Raydium(pool)),
            current_attempt: 0,
            first_attempt_ms: 0,
            last_attempt_ms: 0,
            queued_at_ms: now,
            reason: reason.to_string(),
            jito_tip_lamports,
            fee_recipient_idx: (now % 8) as usize,
        }
    }

    /// Create a PendingSell for a PumpSwap position.
    pub fn from_pumpswap(
        mint: [u8; 32],
        tokens: u64,
        pool: PumpSwapPoolAccounts,
        reason: &str,
        jito_tip_lamports: u64,
    ) -> Self {
        let now = current_ms();
        Self {
            mint,
            tokens,
            pool: Some(PoolAccounts::PumpSwap(pool)),
            current_attempt: 0,
            first_attempt_ms: 0,
            last_attempt_ms: 0,
            queued_at_ms: now,
            reason: reason.to_string(),
            jito_tip_lamports,
            fee_recipient_idx: (now % 8) as usize,
        }
    }

    /// Create a PendingSell without pool info (for orphan watchdog).
    pub fn orphan(mint: [u8; 32], tokens: u64) -> Self {
        let now = current_ms();
        Self {
            mint,
            tokens,
            pool: None,
            current_attempt: 0,
            first_attempt_ms: 0,
            last_attempt_ms: 0,
            queued_at_ms: now,
            reason: "orphan_watchdog".to_string(),
            jito_tip_lamports: 10_000,
            fee_recipient_idx: (now % 8) as usize,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escalation_ladder_is_monotonically_aggressive() {
        for i in 1..ESCALATION_LADDER.len() {
            let prev = &ESCALATION_LADDER[i - 1];
            let curr = &ESCALATION_LADDER[i];
            assert!(
                curr.max_slippage_bps >= prev.max_slippage_bps,
                "slippage must increase: level {} ({}) < level {} ({})",
                i - 1,
                prev.max_slippage_bps,
                i,
                curr.max_slippage_bps
            );
            assert!(
                curr.extra_priority_lamports >= prev.extra_priority_lamports,
                "priority must increase: level {} ({}) < level {} ({})",
                i - 1,
                prev.extra_priority_lamports,
                i,
                curr.extra_priority_lamports
            );
        }
    }

    #[test]
    fn escalation_ladder_final_level_is_force_market() {
        let last = &ESCALATION_LADDER[ESCALATION_LADDER.len() - 1];
        assert_eq!(last.strategy, SellStrategy::ForceMarketSell);
        assert!(last.max_slippage_bps >= 9000);
    }

    #[test]
    fn escalation_ladder_has_five_levels() {
        assert_eq!(ESCALATION_LADDER.len(), 5);
    }

    #[test]
    fn pending_sell_orphan_defaults() {
        let mint = [42u8; 32];
        let sell = PendingSell::orphan(mint, 1_000_000);
        assert_eq!(sell.mint, mint);
        assert_eq!(sell.tokens, 1_000_000);
        assert!(sell.pool.is_none());
        assert_eq!(sell.current_attempt, 0);
        assert_eq!(sell.reason, "orphan_watchdog");
    }
}
