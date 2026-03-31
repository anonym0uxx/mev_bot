//! Post-graduation momentum trading engine.
//!
//! Monitors graduation events (Pump.fun → Raydium/PumpSwap), scores them,
//! and enters positions after a configurable delay. Uses tiered take-profit
//! with trailing stop and hard stop-loss.
//!
//! ## Architecture
//!
//! - `MomentumConfig` — all configuration with serde defaults
//! - `MomentumEngine` — main engine struct with atomic stats
//! - `MomentumPaperLogger` — JSONL writer thread via crossbeam channel
//! - `PriceFeedManager` — Helius WSS price feed with AtomicU64 reads
//! - `PendingEntryRing` — fixed-size ring buffer for delayed entry scheduling
//!
//! ## Lifecycle
//!
//! 1. `on_graduation()` — cold path, called on each migration event
//! 2. `on_tick()` — hot path, called every `check_ms` to manage positions
//! 3. `stats()` — read atomic counters for monitoring

pub mod config;
pub mod logger;
pub mod pool;
pub mod position;
pub mod price_feed;
pub mod scorer;

pub use config::MomentumConfig;
pub use logger::{MomentumClosedPosition, MomentumPaperLogger};
pub use pool::{PoolType, PoolInfo, PoolResolution, BC_TERMINAL_PRICE_LAMPORTS_PER_ATOM};

use crate::momentum::pool::resolve_pool_from_transaction;
use crate::momentum::position::{
    MomentumExitReason, MomentumPosition, PendingEntry, PendingEntryRing, price_to_bps_offset,
};
use crate::momentum::price_feed::{price_from_reserves, PriceFeedManager, VaultSubscription};
use crate::momentum::scorer::score_graduation;
use crate::engine::hot_path::ScoredToken;

use dashmap::DashMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

// ── Blocked mint list: known SPL tokens that should never pass as graduations ──
// Pre-decoded base58 → [u8; 32] for zero-cost runtime comparison.
const BLOCKED_MINTS: [[u8; 32]; 6] = [
    // USDC: EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v
    [0xc6, 0xfa, 0x7a, 0xf3, 0xbe, 0xdb, 0xad, 0x3a, 0x3d, 0x65, 0xf3, 0x6a, 0xab, 0xc9, 0x74, 0x31, 0xb1, 0xbb, 0xe4, 0xc2, 0xd2, 0xf6, 0xe0, 0xe4, 0x7c, 0xa6, 0x02, 0x03, 0x45, 0x2f, 0x5d, 0x61],
    // USDT: Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB
    [0xce, 0x01, 0x0e, 0x60, 0xaf, 0xed, 0xb2, 0x27, 0x17, 0xbd, 0x63, 0x19, 0x2f, 0x54, 0x14, 0x5a, 0x3f, 0x96, 0x5a, 0x33, 0xbb, 0x82, 0xd2, 0xc7, 0x02, 0x9e, 0xb2, 0xce, 0x1e, 0x20, 0x82, 0x64],
    // WSOL: So11111111111111111111111111111111111111112
    [0x06, 0x9b, 0x88, 0x57, 0xfe, 0xab, 0x81, 0x84, 0xfb, 0x68, 0x7f, 0x63, 0x46, 0x18, 0xc0, 0x35, 0xda, 0xc4, 0x39, 0xdc, 0x1a, 0xeb, 0x3b, 0x55, 0x98, 0xa0, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x01],
    // BONK: DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263
    [0xbc, 0x07, 0xc5, 0x6e, 0x60, 0xad, 0x3d, 0x3f, 0x17, 0x73, 0x82, 0xea, 0xc6, 0x54, 0x8f, 0xba, 0x1f, 0xd3, 0x2c, 0xfd, 0x90, 0xca, 0x02, 0xb3, 0xe7, 0xcf, 0xa1, 0x85, 0xfd, 0xce, 0x73, 0x98],
    // JUP: JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN
    [0x04, 0x79, 0xd9, 0xc7, 0xcc, 0x10, 0x35, 0xde, 0x72, 0x11, 0xf9, 0x9e, 0xb4, 0x8c, 0x09, 0xd7, 0x0b, 0x2b, 0xdf, 0x5b, 0xdf, 0x9e, 0x2e, 0x56, 0xb8, 0xa1, 0xfb, 0xb5, 0xa2, 0xea, 0x33, 0x27],
    // RAY: 4k3Dyjzvzp8eMZWUXbBCjEvwSkkk59S5iCNLY3QrkX6R
    [0x37, 0x99, 0x8c, 0xcb, 0xf2, 0xd0, 0x45, 0x8b, 0x61, 0x5c, 0xbc, 0xc6, 0xb1, 0xa3, 0x67, 0xc4, 0x74, 0x9e, 0x9f, 0xef, 0x73, 0x06, 0x62, 0x2e, 0x1b, 0x1b, 0x58, 0x91, 0x01, 0x20, 0xbc, 0x9a],
];

/// Check if a mint is in the blocklist (known SPL tokens or all-zero mint).
#[inline(always)]
fn is_blocked_mint(mint: &[u8; 32]) -> bool {
    // Reject all-zero mint
    if mint == &[0u8; 32] {
        return true;
    }
    // Check against known blocked mints
    for blocked in &BLOCKED_MINTS {
        if mint == blocked {
            return true;
        }
    }
    false
}

/// Post-graduation momentum trading engine.
///
/// Receives graduation events, scores them, delays entry, and manages
/// positions with tiered TP/SL. All stats are lock-free atomics.
pub struct MomentumEngine {
    config: Arc<MomentumConfig>,
    #[allow(dead_code)]
    rpc_url: Arc<String>,
    #[allow(dead_code)]
    http_client: reqwest::Client,

    // ── Active positions: mint → MomentumPosition ───────────────────
    active: DashMap<[u8; 32], MomentumPosition>,

    // ── Pending entries scheduled for T+delay ───────────────────────
    pending: std::sync::Mutex<PendingEntryRing>,

    // ── Price feed ──────────────────────────────────────────────────
    price_feed: PriceFeedManager,

    // ── Logger ──────────────────────────────────────────────────────
    logger: MomentumPaperLogger,

    // ── Kelly-scored tokens from hot_path ────────────────────────────
    // Tokens that passed full Kelly/Bayesian scoring + watchlist promotion.
    // Key = mint. Populated by drain_scored_tokens() each tick.
    // TTL: entries older than 10 minutes are evicted.
    scored_tokens: DashMap<[u8; 32], ScoredToken>,
    /// Receiver for scored tokens from hot_path (crossbeam channel).
    scored_token_rx: crossbeam_channel::Receiver<ScoredToken>,

    // ── Stats (atomic, lock-free) ───────────────────────────────────
    graduations_seen: AtomicU64,
    entries_opened: AtomicU64,
    tp1_exits: AtomicU64,
    tp2_exits: AtomicU64,
    tp3_exits: AtomicU64,
    sl_exits: AtomicU64,
    timeout_exits: AtomicU64,
    daily_pnl_lamports: AtomicI64,
    last_tick_ms: AtomicU64,
}

impl MomentumEngine {
    /// Create a new momentum engine with the given config and RPC URL.
    ///
    /// Spawns a Helius WSS price feed task and a JSONL logger thread.
    /// Returns `(engine, scored_token_sender, ws_handle, logger_handle)`.
    /// The caller passes `scored_token_sender` to `HotPath::set_scored_token_tx()`.
    pub fn new(
        config: Arc<MomentumConfig>,
        rpc_url: Arc<String>,
        helius_wss_url: String,
        log_path: &str,
    ) -> (Self, crossbeam_channel::Sender<ScoredToken>, tokio::task::JoinHandle<()>, std::thread::JoinHandle<()>) {
        let (price_feed, ws_handle) = PriceFeedManager::new(helius_wss_url);
        let (logger, logger_handle) = MomentumPaperLogger::new(log_path);

        // Channel for Kelly-scored tokens from hot_path → momentum engine
        let (scored_tx, scored_rx) = crossbeam_channel::bounded::<ScoredToken>(512);

        let engine = Self {
            config,
            rpc_url,
            http_client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_millis(500))
                .build()
                .expect("reqwest client build should not fail"),
            active: DashMap::new(),
            pending: std::sync::Mutex::new(PendingEntryRing::new()),
            price_feed,
            logger,
            scored_tokens: DashMap::new(),
            scored_token_rx: scored_rx,
            graduations_seen: AtomicU64::new(0),
            entries_opened: AtomicU64::new(0),
            tp1_exits: AtomicU64::new(0),
            tp2_exits: AtomicU64::new(0),
            tp3_exits: AtomicU64::new(0),
            sl_exits: AtomicU64::new(0),
            timeout_exits: AtomicU64::new(0),
            daily_pnl_lamports: AtomicI64::new(0),
            last_tick_ms: AtomicU64::new(0),
        };

        (engine, scored_tx, ws_handle, logger_handle)
    }

    /// Called on every graduation event. Scores and schedules entry.
    ///
    /// This is a cold path — graduation is rare (~10/day Raydium).
    /// Scores the graduation, starts price feed subscription, and
    /// schedules a pending entry at T+entry_delay_ms.
    #[cold]
    #[inline(never)]
    pub async fn on_graduation(
        &self,
        pool_info: &PoolInfo,
        now_ms: u64,
        grad_speed_s: u32,
        grad_volume_sol_x100: u32,
        pre_grad_buys_5s: u32,
    ) {
        if !self.config.enabled {
            return;
        }

        // ── Blocklist: reject known SPL token mints ─────────────────────
        if is_blocked_mint(&pool_info.mint) {
            tracing::debug!(
                mint = %bs58::encode(&pool_info.mint).into_string(),
                "[momentum] blocked mint — skipping fake graduation"
            );
            return;
        }

        self.graduations_seen.fetch_add(1, Ordering::Relaxed);

        // Check daily loss cap
        let daily_pnl = self.daily_pnl_lamports.load(Ordering::Relaxed);
        let cap_lamports = -(self.config.daily_loss_cap_sol * 1e9) as i64;
        if daily_pnl <= cap_lamports {
            return; // daily cap hit
        }

        // Check concurrent position limit
        if self.active.len() >= self.config.max_concurrent as usize {
            return;
        }

        // Skip PumpSwap (no structural arb, and momentum unproven)
        if pool_info.pool_type == PoolType::PumpSwap {
            return;
        }

        // Score the graduation (no recovery score at this point — use 0)
        let score = score_graduation(grad_speed_s, grad_volume_sol_x100, pre_grad_buys_5s, 0);
        let effective_min = if self.config.paper_mode { 20 } else { self.config.min_grad_score };
        if score.total() < effective_min {
            tracing::info!(
                score = score.total(),
                min = effective_min,
                grad_speed_s,
                volume_sol_x100 = grad_volume_sol_x100,
                buys_5s = pre_grad_buys_5s,
                "[momentum] graduation score below threshold — skipping"
            );
            return;
        }
        let mint_b58 = bs58::encode(&pool_info.mint).into_string();
        tracing::info!(
            mint = %mint_b58,
            score = score.total(),
            grad_speed_s,
            volume_sol_x100 = grad_volume_sol_x100,
            buys_5s = pre_grad_buys_5s,
            kelly_scored = self.scored_tokens.contains_key(&pool_info.mint),
            "[momentum] graduation score PASSED — opening position"
        );

        // Start price feed subscription immediately (before entry delay)
        let coin_vault_b58 = bs58::encode(&pool_info.coin_vault).into_string();
        let pc_vault_b58 = bs58::encode(&pool_info.pc_vault).into_string();
        self.price_feed
            .subscribe(VaultSubscription {
                mint: pool_info.mint,
                coin_vault: coin_vault_b58,
                pc_vault: pc_vault_b58,
            })
            .await;

        // Schedule entry at T+entry_delay_ms
        let entry_price_fp = price_from_reserves(pool_info.reserve_sol, pool_info.reserve_token);
        let bc_price_fp = (BC_TERMINAL_PRICE_LAMPORTS_PER_ATOM * 1_000_000.0) as u64;

        let pool_type_u8 = match pool_info.pool_type {
            PoolType::RaydiumAmmV4 => 0u8,
            PoolType::PumpSwap => 1u8,
            PoolType::Unknown => 2u8,
        };

        let entry = PendingEntry {
            mint: pool_info.mint,
            pool_type: pool_type_u8,
            grad_score: score.total(),
            grad_speed_s,
            grad_volume_sol_x100,
            pre_grad_buys_5s,
            scheduled_ts_ms: now_ms + self.config.entry_delay_ms,
            opening_price_fp: entry_price_fp,
            bc_price_fp,
            first_scheduled_ts_ms: now_ms,
            active: true,
        };

        if let Ok(mut ring) = self.pending.lock() {
            ring.push(entry);
        }

        let kelly_scored = self.scored_tokens.contains_key(&pool_info.mint);
        tracing::debug!(
            mint = %bs58::encode(&pool_info.mint).into_string(),
            score = score.total(),
            kelly_scored,
            entry_delay_ms = self.config.entry_delay_ms,
            "[momentum] graduation scored, entry scheduled"
        );
    }

    /// Drain scored tokens from the hot_path channel into the local DashMap.
    /// Also evict entries older than 10 minutes. Called each tick.
    /// PERF: #[inline(never)] — cold path, runs every 150ms but does no alloc.
    #[inline(never)]
    fn drain_scored_tokens(&self, now_ms: u64) {
        // Drain all pending scored tokens (non-blocking)
        while let Ok(st) = self.scored_token_rx.try_recv() {
            self.scored_tokens.insert(st.mint, st);
        }
        // Evict entries older than 10 minutes (600_000ms)
        // Only run eviction every ~10s to avoid scanning DashMap each tick.
        let last_tick = self.last_tick_ms.load(Ordering::Relaxed);
        if now_ms.saturating_sub(last_tick) >= 10_000 || last_tick == 0 {
            self.scored_tokens.retain(|_mint, st| {
                now_ms.saturating_sub(st.timestamp_ms) < 600_000
            });
        }
    }

    /// Compute position size in lamports for a graduation entry.
    ///
    /// Priority:
    /// 1. Kelly-scored (from hot_path Bayesian pipeline) → use Kelly's size_lamports
    /// 2. Fallback tiered sizing based on grad_score:
    ///    - score >= 80: 0.50 SOL
    ///    - score >= 60: 0.30 SOL
    ///    - score >= 40: 0.15 SOL
    ///    - below 40:    rejected at gate (shouldn't reach here)
    #[inline(always)]
    fn compute_size_lamports(&self, mint: &[u8; 32], grad_score: u32) -> u64 {
        // Check if Kelly scored this token
        if let Some(st) = self.scored_tokens.get(mint) {
            if st.kelly_size_lamports > 0 {
                return st.kelly_size_lamports;
            }
        }
        // Fallback: tiered sizing from grad_score
        if grad_score >= 80 {
            500_000_000 // 0.50 SOL
        } else if grad_score >= 60 {
            300_000_000 // 0.30 SOL
        } else {
            150_000_000 // 0.15 SOL
        }
    }

    /// Called every `check_ms`. Manages active positions.
    ///
    /// This is the hot path — runs every 150ms when positions are open.
    /// Processes pending entries and evaluates TP/SL exits.
    #[inline(always)]
    pub async fn on_tick(&self, now_ms: u64) {
        if !self.config.enabled {
            return;
        }

        // Throttle to check_ms interval
        let last = self.last_tick_ms.load(Ordering::Relaxed);
        if now_ms.saturating_sub(last) < self.config.check_ms {
            return;
        }
        self.last_tick_ms.store(now_ms, Ordering::Relaxed);

        // Drain Kelly-scored tokens from hot_path channel
        self.drain_scored_tokens(now_ms);

        // Process pending entries that are ready
        self.process_pending_entries(now_ms).await;

        // Process active positions
        let active_count = self.active.len();
        let pending_count = self.pending.lock().map(|r| r.len()).unwrap_or(0);
        // Log every ~10s (check_ms=150ms, 10000/150≈67)
        let tick_num = self.last_tick_ms.load(Ordering::Relaxed) / self.config.check_ms.max(1);
        if tick_num % 67 == 0 && (active_count > 0 || pending_count > 0) {
            tracing::info!(
                active = active_count,
                pending = pending_count,
                scored = self.scored_tokens.len(),
                "[momentum] tick status"
            );
        }
        self.process_active_positions(now_ms);
    }

    /// Process pending entries whose scheduled time has elapsed.
    ///
    /// If the price feed hasn't delivered live data yet, entries are re-queued
    /// for the next tick rather than entering at the stale graduation-time price.
    /// Entries are abandoned (skipped) once `no_price_timeout_ms` elapses without
    /// a live price, preventing ghost-price entries that trigger false stop-losses.
    #[inline(never)]
    async fn process_pending_entries(&self, now_ms: u64) {
        let ready: Vec<PendingEntry> = if let Ok(mut ring) = self.pending.lock() {
            ring.drain_ready(now_ms).collect()
        } else {
            return;
        };

        // Entries deferred because price feed isn't ready yet
        let mut requeue: Vec<PendingEntry> = Vec::new();

        for entry in ready {
            // Check limits again at entry time — drop excess entries
            if self.active.len() >= self.config.max_concurrent as usize {
                break;
            }

            // Get current live price from price feed
            let current_price_fp = match self.price_feed.current_price(&entry.mint) {
                Some(p) if p > 0 => p,
                _ => {
                    // Price feed not ready yet. Check if we've waited long enough.
                    let waited_ms = now_ms.saturating_sub(entry.first_scheduled_ts_ms);
                    if waited_ms < self.config.no_price_timeout_ms {
                        // Re-queue: try again next tick
                        tracing::debug!(
                            mint = %bs58::encode(&entry.mint).into_string(),
                            waited_ms,
                            "[momentum] price feed not ready, re-queuing entry"
                        );
                        requeue.push(entry);
                    } else {
                        // Timeout: abandon this entry to avoid stale price entry
                        tracing::warn!(
                            mint = %bs58::encode(&entry.mint).into_string(),
                            waited_ms,
                            "[momentum] price feed timeout — abandoning entry (no_price_timeout_ms={})",
                            self.config.no_price_timeout_ms
                        );
                    }
                    continue;
                }
            };

            // Validate entry price — reject zero or impossibly high values
            if current_price_fp == 0 || current_price_fp > 1_000_000_000_000_000 {
                tracing::warn!(
                    mint = %bs58::encode(&entry.mint).into_string(),
                    price = current_price_fp,
                    "[momentum] invalid entry price — skipping"
                );
                continue;
            }

            // Tier-0 sizing: if token is trading BELOW graduation price at entry,
            // enter at reduced size — it's already showing weakness post-migration.
            // If token is flat or up vs graduation price, use full grad_score tiers.
            let bps_from_grad = price_to_bps_offset(entry.opening_price_fp, current_price_fp);
            let size_lamports = if self.config.tier0_size_sol > 0.0 && bps_from_grad < 0 {
                // Token below graduation price → enter small (0.10 SOL default)
                (self.config.tier0_size_sol * 1_000_000_000.0) as u64
            } else {
                // Token holding or running → full confidence, use grad_score tiers
                self.compute_size_lamports(&entry.mint, entry.grad_score as u32)
            };
            let pos = MomentumPosition::new(
                entry.mint,
                now_ms,
                current_price_fp,
                entry.bc_price_fp,
                size_lamports,
                entry.pool_type,
                entry.grad_score,
                entry.grad_speed_s,
                entry.grad_volume_sol_x100,
                entry.pre_grad_buys_5s,
                self.config.entry_delay_ms as u32,
            );

            // Guard: skip if position already active for this mint (late duplicate slipped through ring buffer).
            if self.active.contains_key(&entry.mint) {
                tracing::debug!(
                    mint = %bs58::encode(&entry.mint).into_string(),
                    "[momentum] skipping duplicate entry — mint already active"
                );
                continue;
            }

            self.active.insert(entry.mint, pos);
            self.entries_opened.fetch_add(1, Ordering::Relaxed);

            let kelly_scored = self.scored_tokens.contains_key(&entry.mint);
            tracing::info!(
                mint = %bs58::encode(&entry.mint).into_string(),
                entry_price_fp = current_price_fp,
                size_sol = size_lamports as f64 / 1e9,
                kelly_scored,
                "[momentum] paper position OPENED"
            );
        }

        // Re-push deferred entries back into the pending ring
        if !requeue.is_empty() {
            if let Ok(mut ring) = self.pending.lock() {
                for mut entry in requeue {
                    // Re-schedule for next tick
                    entry.scheduled_ts_ms = now_ms + self.config.check_ms;
                    entry.active = true;
                    ring.push(entry);
                }
            }
        }
    }

    /// Evaluate all active positions for exit conditions.
    ///
    /// Exit priority: max_hold > hard_sl > trailing_stop > time_sl > tp3 > tp2 > tp1
    fn process_active_positions(&self, now_ms: u64) {
        let mut to_close: Vec<([u8; 32], MomentumExitReason, u64)> = Vec::new();

        for mut entry in self.active.iter_mut() {
            let mint = *entry.key();
            let pos = entry.value_mut();

            let elapsed_ms = now_ms.saturating_sub(pos.entry_ts_ms);

            // 0. Max hold handling.
            // After max_hold_trail_activation_ms, switch from blind hold to tight trailing stop.
            // This prevents "held to death" losses on crashing tokens while still capturing
            // late-breaking momentum on meandering winners.
            if elapsed_ms >= self.config.max_hold_ms {
                // Hard ceiling: force exit at max_hold regardless.
                let exit_price = self.price_feed.current_price(&mint).unwrap_or(pos.entry_price_fp);
                to_close.push((mint, MomentumExitReason::MaxHold, exit_price));
                continue;
            }
            // Trailing-stop-at-maturity: once past activation threshold, apply tight trailing stop.
            // Only runs if activation is enabled (> 0) and price feed has data.
            if self.config.max_hold_trail_activation_ms > 0
                && elapsed_ms >= self.config.max_hold_trail_activation_ms
            {
                if let Some(current_fp) = self.price_feed.current_price(&mint).filter(|&p| p > 0) {
                    let trail_bps = (self.config.max_hold_trail_pct * 100.0) as u32;
                    if pos.trailing_stop_hit(current_fp, trail_bps) {
                        to_close.push((mint, MomentumExitReason::MaxHold, current_fp));
                        continue;
                    }
                }
            }

            // Get current price (required for TP/SL evaluation)
            let current_price_fp = match self.price_feed.current_price(&mint) {
                Some(p) if p > 0 => p,
                _ => continue,
            };

            // Fix E: Record first-tick sample when price feed delivers its first reading.
            // Eliminates the structural blind spot where trades exiting in < 10s have zero samples.
            if !pos.first_price_recorded {
                pos.first_price_recorded = true;
                pos.record_sample(current_price_fp);
            }

            // Fix A: Configurable sample interval (default: every ~1s instead of every ~10s).
            let ticks_elapsed = elapsed_ms / self.config.check_ms.max(1);
            let sample_interval = self.config.sample_interval_ticks.max(1);
            if ticks_elapsed > 0 && ticks_elapsed % sample_interval == 0 {
                pos.record_sample(current_price_fp);
            }

            // Update peak for trailing stop
            if current_price_fp > pos.peak_price_fp {
                pos.peak_price_fp = current_price_fp;
            }

            let hold_ms = elapsed_ms;
            let entry_fp = pos.entry_price_fp;

            // Fix D: Micro hard SL — tighter stop for the first ~3 seconds.
            // Catches immediate dump-on-graduation tokens before the first sample window.
            // After micro_sl_ticks, the regular hard_sl_pct takes over.
            if ticks_elapsed <= self.config.micro_sl_ticks {
                let micro_sl_bps = (self.config.micro_sl_pct * 100.0) as u32;
                if pos.hard_sl_hit(current_price_fp, micro_sl_bps) {
                    to_close.push((mint, MomentumExitReason::HardSl, current_price_fp));
                    continue;
                }
            }

            // 2. Hard SL
            let hard_sl_bps = (self.config.hard_sl_pct * 100.0) as u32;
            if pos.hard_sl_hit(current_price_fp, hard_sl_bps) {
                to_close.push((mint, MomentumExitReason::HardSl, current_price_fp));
                continue;
            }

            // 3. Trailing stop (only after hitting TP1)
            if pos.tp_flags & 0x1 != 0 {
                let trailing_bps = (self.config.trailing_stop_pct * 100.0) as u32;
                if pos.trailing_stop_hit(current_price_fp, trailing_bps) {
                    to_close.push((
                        mint,
                        MomentumExitReason::TrailingStop,
                        current_price_fp,
                    ));
                    continue;
                }
            }

            // 4. Time SL (no profit after time_sl_ms)
            if hold_ms >= self.config.time_sl_ms {
                let bps = price_to_bps_offset(entry_fp, current_price_fp);
                if bps <= 0 {
                    to_close.push((mint, MomentumExitReason::TimeSl, current_price_fp));
                    continue;
                }
            }

            // 5. TP tiers
            let gain_bps = price_to_bps_offset(entry_fp, current_price_fp);
            let tp3_bps = (self.config.tp3_pct * 100.0) as i32;
            let tp2_bps = (self.config.tp2_pct * 100.0) as i32;
            let tp1_bps = (self.config.tp1_pct * 100.0) as i32;

            if gain_bps >= tp3_bps && pos.tp_flags & 0x4 == 0 {
                pos.tp_flags |= 0x7; // mark all TP levels hit
                to_close.push((mint, MomentumExitReason::Tp3, current_price_fp));
            } else if gain_bps >= tp2_bps && pos.tp_flags & 0x2 == 0 {
                pos.tp_flags |= 0x3; // mark TP1+TP2 hit
                // Don't close yet — wait for TP3 or trailing stop
            } else if gain_bps >= tp1_bps && pos.tp_flags & 0x1 == 0 {
                pos.tp_flags |= 0x1; // mark TP1 hit — activates trailing stop
                // Don't close yet — wait for TP2+
            }
        }

        // Close positions (must release iter_mut borrow first)
        for (mint, reason, exit_price_fp) in to_close {
            self.close_position(mint, reason, exit_price_fp, now_ms);
        }
    }

    /// Close a position, calculate P&L, update stats, and log.
    #[cold]
    #[inline(never)]
    fn close_position(
        &self,
        mint: [u8; 32],
        reason: MomentumExitReason,
        exit_price_fp: u64,
        now_ms: u64,
    ) {
        let Some((_, pos)) = self.active.remove(&mint) else {
            return;
        };

        // Calculate P&L
        let size_sol = pos.size_lamports as f64 / 1e9;
        let raw_gain_bps = price_to_bps_offset(pos.entry_price_fp, exit_price_fp);
        // Sanity clamp: no real trade gains >1000% or loses >100% — bad price feed data
        let gain_bps = raw_gain_bps.clamp(-10_000, 100_000);
        if raw_gain_bps != gain_bps {
            tracing::warn!(
                mint = %bs58::encode(&mint).into_string(),
                raw_gain_bps,
                clamped_gain_bps = gain_bps,
                entry_price = pos.entry_price_fp,
                exit_price = exit_price_fp,
                "[momentum] PnL sanity clamp — bad price data"
            );
        }
        let gross_pnl_sol = size_sol * gain_bps as f64 / 10_000.0;

        // Fees: use config-specified bps per pool type
        let fee_bps = if pos.pool_type == 0 {
            self.config.raydium_fee_bps
        } else {
            self.config.pumpswap_fee_bps
        };
        // Round-trip fees: entry + exit
        let fee_sol = size_sol * (fee_bps as f64 * 2.0) / 10_000.0;
        let net_pnl_sol = gross_pnl_sol - fee_sol;

        // Update daily P&L
        let net_lamports = (net_pnl_sol * 1e9) as i64;
        self.daily_pnl_lamports
            .fetch_add(net_lamports, Ordering::Relaxed);

        // Update stats
        match reason {
            MomentumExitReason::Tp1 => {
                self.tp1_exits.fetch_add(1, Ordering::Relaxed);
            }
            MomentumExitReason::Tp2 => {
                self.tp2_exits.fetch_add(1, Ordering::Relaxed);
            }
            MomentumExitReason::Tp3 => {
                self.tp3_exits.fetch_add(1, Ordering::Relaxed);
            }
            MomentumExitReason::TrailingStop | MomentumExitReason::HardSl => {
                self.sl_exits.fetch_add(1, Ordering::Relaxed);
            }
            _ => {
                self.timeout_exits.fetch_add(1, Ordering::Relaxed);
            }
        }

        // Unsubscribe from price feed (fire and forget)
        let price_feed = &self.price_feed;
        let mint_copy = mint;
        // Use try_send pattern — we can't .await here since close_position is sync
        // The price feed manager will clean up on next command processing
        tokio::spawn({
            let cmd_tx = price_feed.cmd_sender();
            async move {
                let _ = cmd_tx
                    .send(crate::momentum::price_feed::PriceFeedCommand::Unsubscribe(
                        mint_copy,
                    ))
                    .await;
            }
        });

        // Log to JSONL
        let grad_vol_sol = pos.grad_volume_sol_x100 as f64 / 100.0;
        let bc_price_f64 = pos.bc_terminal_price_fp as f64 / 1_000_000.0;
        let entry_price_f64 = pos.entry_price_fp as f64 / 1_000_000.0;
        let structural_discount = if bc_price_f64 > 0.0 {
            (bc_price_f64 - entry_price_f64) / bc_price_f64 * 100.0
        } else {
            0.0
        };

        let mint_b58 = bs58::encode(&mint).into_string();
        let pool_type_str = if pos.pool_type == 0 {
            "raydium_amm_v4"
        } else {
            "pump_swap"
        };

        tracing::info!(
            mint = %mint_b58,
            exit_reason = reason.as_str(),
            hold_ms = now_ms.saturating_sub(pos.entry_ts_ms),
            net_pnl_sol = format!("{:.6}", net_pnl_sol),
            "[momentum] paper position CLOSED"
        );

        self.logger.log(MomentumClosedPosition {
            strategy_tag: "momentum",
            mint: mint_b58,
            pool_type: pool_type_str,
            grad_score: pos.grad_score,
            grad_speed_s: pos.grad_speed_s as u64,
            grad_volume_sol: grad_vol_sol,
            pre_grad_buys_5s: pos.pre_grad_buys_5s,
            size_sol,
            size_lamports: pos.size_lamports,
            entry_delay_ms: pos.entry_delay_ms as u64,
            entry_price_lamports: pos.entry_price_fp,
            bc_terminal_price_lamports: bc_price_f64,
            structural_discount_pct: structural_discount,
            entry_timestamp_ms: pos.entry_ts_ms,
            exit_timestamp_ms: now_ms,
            hold_ms: now_ms.saturating_sub(pos.entry_ts_ms),
            exit_reason: reason.as_str(),
            gross_pnl_sol,
            fee_sol,
            net_pnl_sol,
            price_samples_bps: pos.price_samples_bps[..pos.sample_count as usize].to_vec(),
            is_paper: self.config.paper_mode,
            config_version: self.config.config_version(),
        });
    }

    /// Called from main.rs on every graduation migration event.
    /// Resolves the pool via getTransaction and calls on_graduation() if successful.
    /// Cold path — graduation is rare (~10-20 Raydium/day).
    #[inline(never)]
    pub async fn on_migration(
        &self,
        _mint: [u8; 32],
        ts_ms: u64,
        sig: [u8; 64],
        enrichment: crate::engine::hot_path::GradEnrichment,
    ) {
        if !self.config.enabled { return; }
        match resolve_pool_from_transaction(&self.http_client, &sig, &self.rpc_url).await {
            Some(resolution) => {
                let mint_b58 = bs58::encode(&resolution.mint).into_string();
                tracing::info!(
                    mint = %mint_b58,
                    pool_type = ?resolution.pool_type,
                    reserve_sol = resolution.reserve_sol_lamports,
                    grad_speed_s = enrichment.grad_speed_s,
                    volume_sol_x100 = enrichment.volume_sol_x100,
                    buys_5s = enrichment.buys_5s,
                    "[momentum] pool resolved — entering on_graduation"
                );
                let pool_info = PoolInfo {
                    coin_vault: resolution.coin_vault,
                    pc_vault: resolution.pc_vault,
                    reserve_token: resolution.reserve_token_atoms,
                    reserve_sol: resolution.reserve_sol_lamports,
                    pool_type: resolution.pool_type,
                    mint: resolution.mint,
                };
                // Use REAL enrichment data from hot_path's mint_map.
                self.on_graduation(
                    &pool_info,
                    ts_ms,
                    enrichment.grad_speed_s,
                    enrichment.volume_sol_x100,
                    enrichment.buys_5s as u32,
                ).await;
            }
            None => {
                let sig_b58 = bs58::encode(&sig).into_string();
                tracing::warn!(sig = %sig_b58, "[momentum] pool resolution FAILED");
            }
        }
    }

    /// Read current stats as a snapshot (lock-free atomic loads).
    pub fn stats(&self) -> MomentumStats {
        MomentumStats {
            enabled: self.config.enabled,
            paper_mode: self.config.paper_mode,
            active_positions: self.active.len() as u64,
            graduations_seen: self.graduations_seen.load(Ordering::Relaxed),
            entries_opened: self.entries_opened.load(Ordering::Relaxed),
            tp1_exits: self.tp1_exits.load(Ordering::Relaxed),
            tp2_exits: self.tp2_exits.load(Ordering::Relaxed),
            tp3_exits: self.tp3_exits.load(Ordering::Relaxed),
            sl_exits: self.sl_exits.load(Ordering::Relaxed),
            timeout_exits: self.timeout_exits.load(Ordering::Relaxed),
            daily_pnl_sol: self.daily_pnl_lamports.load(Ordering::Relaxed) as f64
                / 1_000_000_000.0,
        }
    }
}

/// Snapshot of momentum engine stats for monitoring/API.
#[derive(Debug, serde::Serialize)]
pub struct MomentumStats {
    pub enabled: bool,
    pub paper_mode: bool,
    pub active_positions: u64,
    pub graduations_seen: u64,
    pub entries_opened: u64,
    pub tp1_exits: u64,
    pub tp2_exits: u64,
    pub tp3_exits: u64,
    pub sl_exits: u64,
    pub timeout_exits: u64,
    pub daily_pnl_sol: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::momentum::pool::PoolType;

    /// Helper: create a test engine (price feed connects to invalid URL, that's fine).
    fn make_test_engine(enabled: bool) -> MomentumEngine {
        let mut cfg = MomentumConfig::default();
        cfg.enabled = enabled;
        let config = Arc::new(cfg);
        let rpc_url = Arc::new("https://example.com".to_string());

        let log_path = format!(
            "{}/momentum_test_{}.jsonl",
            std::env::temp_dir().display(),
            std::process::id()
        );

        let (engine, _scored_tx, ws_handle, _logger_handle) = MomentumEngine::new(
            config,
            rpc_url,
            "wss://invalid.example.com".to_string(),
            &log_path,
        );
        // Abort the WS task so it doesn't retry forever
        ws_handle.abort();
        engine
    }

    #[tokio::test]
    async fn test_momentum_engine_new() {
        let engine = make_test_engine(false);
        let stats = engine.stats();
        assert!(!stats.enabled);
        assert_eq!(stats.graduations_seen, 0);
        assert_eq!(stats.entries_opened, 0);
        assert_eq!(stats.tp1_exits, 0);
        assert_eq!(stats.tp2_exits, 0);
        assert_eq!(stats.tp3_exits, 0);
        assert_eq!(stats.sl_exits, 0);
        assert_eq!(stats.timeout_exits, 0);
        assert!((stats.daily_pnl_sol - 0.0).abs() < f64::EPSILON);
        assert_eq!(stats.active_positions, 0);
    }

    #[tokio::test]
    async fn test_momentum_on_graduation_disabled() {
        let engine = make_test_engine(false);

        let pool_info = PoolInfo {
            coin_vault: [1u8; 32],
            pc_vault: [2u8; 32],
            reserve_token: 200_000_000_000_000,
            reserve_sol: 80_000_000_000,
            pool_type: PoolType::RaydiumAmmV4,
            mint: [0xAA; 32],
        };

        engine
            .on_graduation(&pool_info, 1_000_000, 60, 50_000, 15)
            .await;
        assert_eq!(engine.graduations_seen.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_momentum_on_graduation_enabled() {
        let engine = make_test_engine(true);

        let pool_info = PoolInfo {
            coin_vault: [1u8; 32],
            pc_vault: [2u8; 32],
            reserve_token: 200_000_000_000_000,
            reserve_sol: 80_000_000_000,
            pool_type: PoolType::RaydiumAmmV4,
            mint: [0xAA; 32],
        };

        // High-scoring graduation: speed=60 (score 20), volume=50k (score 25), velocity=15
        engine
            .on_graduation(&pool_info, 1_000_000, 60, 50_000, 15)
            .await;
        assert_eq!(engine.graduations_seen.load(Ordering::Relaxed), 1);

        // Should have scheduled a pending entry
        let pending_count = engine.pending.lock().unwrap().active_count();
        assert_eq!(pending_count, 1);
    }

    #[tokio::test]
    async fn test_momentum_on_graduation_pumpswap_skipped() {
        let engine = make_test_engine(true);

        let pool_info = PoolInfo {
            coin_vault: [1u8; 32],
            pc_vault: [2u8; 32],
            reserve_token: 200_000_000_000_000,
            reserve_sol: 80_000_000_000,
            pool_type: PoolType::PumpSwap,
            mint: [0xBB; 32],
        };

        engine
            .on_graduation(&pool_info, 1_000_000, 60, 50_000, 15)
            .await;
        // Counter incremented but no pending entry (PumpSwap skip)
        assert_eq!(engine.graduations_seen.load(Ordering::Relaxed), 1);
        assert_eq!(engine.pending.lock().unwrap().active_count(), 0);
    }

    #[tokio::test]
    async fn test_momentum_on_graduation_low_score_rejected() {
        let engine = make_test_engine(true);

        let pool_info = PoolInfo {
            coin_vault: [1u8; 32],
            pc_vault: [2u8; 32],
            reserve_token: 200_000_000_000_000,
            reserve_sol: 80_000_000_000,
            pool_type: PoolType::RaydiumAmmV4,
            mint: [0xCC; 32],
        };

        // Low-scoring: slow graduation (3600s, score 0), low volume, no velocity
        engine
            .on_graduation(&pool_info, 1_000_000, 3600, 1_000, 0)
            .await;
        assert_eq!(engine.graduations_seen.load(Ordering::Relaxed), 1);
        assert_eq!(engine.pending.lock().unwrap().active_count(), 0);
    }

    #[tokio::test]
    async fn test_momentum_on_tick_disabled() {
        let engine = make_test_engine(false);
        engine.on_tick(1_000_000).await;
        // No crash, no state change
        assert_eq!(engine.entries_opened.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_momentum_pending_entry_opens_position() {
        let engine = make_test_engine(true);

        // Manually push a pending entry that's already ready
        {
            let mut ring = engine.pending.lock().unwrap();
            ring.push(PendingEntry {
                mint: [0xDD; 32],
                pool_type: 0, // Raydium
                grad_score: 72,
                grad_speed_s: 60,
                grad_volume_sol_x100: 50_000,
                pre_grad_buys_5s: 15,
                scheduled_ts_ms: 1_000, // already past
                opening_price_fp: 381,
                bc_price_fp: 411,
                first_scheduled_ts_ms: 1_000,
                active: true,
            });
        }

        // Insert a live price so Fix F doesn't re-queue/abandon the entry
        {
            let state = crate::momentum::price_feed::PriceState::new();
            state.price_fp.store(381, Ordering::Relaxed);
            engine.price_feed.prices.insert([0xDD; 32], state);
        }

        // Tick at T=2000 — entry should happen (delay elapsed)
        engine.on_tick(2_000).await;
        assert_eq!(engine.entries_opened.load(Ordering::Relaxed), 1);
        assert_eq!(engine.active.len(), 1);
    }

    #[tokio::test]
    async fn test_momentum_max_hold_exit() {
        let engine = make_test_engine(true);

        // Insert a position directly (entered at T=1000)
        let pos = MomentumPosition::new(
            [0xEE; 32],
            1_000,
            381,   // entry price
            411,   // bc terminal
            300_000_000,
            0,     // raydium
            72,    // grad score
            60,    // speed
            50_000,
            10,
            15_000,
        );
        engine.active.insert([0xEE; 32], pos);

        // Insert a price so on_tick can read it
        {
            let state = crate::momentum::price_feed::PriceState::new();
            state.price_fp.store(381, Ordering::Relaxed);
            engine.price_feed.prices.insert([0xEE; 32], state);
        }

        // Tick well past max_hold (300_000ms)
        let exit_time = 1_000 + 300_001;
        engine.on_tick(exit_time).await;

        // Position should have been closed
        assert_eq!(engine.active.len(), 0);
        assert_eq!(engine.timeout_exits.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn test_momentum_hard_sl_exit() {
        let engine = make_test_engine(true);

        // Position entered at price 1000
        let pos = MomentumPosition::new(
            [0xFF; 32],
            1_000,
            1000,  // entry price
            411,
            300_000_000,
            0,
            72,
            60,
            50_000,
            10,
            15_000,
        );
        engine.active.insert([0xFF; 32], pos);

        // Set price to -15% (below 12% hard SL)
        {
            let state = crate::momentum::price_feed::PriceState::new();
            state.price_fp.store(850, Ordering::Relaxed); // -15%
            engine.price_feed.prices.insert([0xFF; 32], state);
        }

        engine.on_tick(2_000).await;

        assert_eq!(engine.active.len(), 0);
        assert_eq!(engine.sl_exits.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn test_momentum_tp3_exit() {
        let engine = make_test_engine(true);

        // Position entered at price 1000
        let pos = MomentumPosition::new(
            [0x11; 32],
            1_000,
            1000,
            411,
            300_000_000,
            0,
            72,
            60,
            50_000,
            10,
            15_000,
        );
        engine.active.insert([0x11; 32], pos);

        // Set price to +60% (above 50% TP3)
        {
            let state = crate::momentum::price_feed::PriceState::new();
            state.price_fp.store(1600, Ordering::Relaxed); // +60%
            engine.price_feed.prices.insert([0x11; 32], state);
        }

        engine.on_tick(2_000).await;

        assert_eq!(engine.active.len(), 0);
        assert_eq!(engine.tp3_exits.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn test_momentum_trailing_stop_after_tp1() {
        let engine = make_test_engine(true);

        // Position entered at price 1000, TP1 already hit
        let mut pos = MomentumPosition::new(
            [0x22; 32],
            1_000,
            1000,
            411,
            300_000_000,
            0,
            72,
            60,
            50_000,
            10,
            15_000,
        );
        pos.tp_flags = 0x1; // TP1 hit — trailing stop active
        pos.peak_price_fp = 1200; // peak at +20%
        engine.active.insert([0x22; 32], pos);

        // Price dropped 10% from peak (1200 → 1080), trailing stop is 8%
        // 8% of 1200 = 96 drop → threshold at 1104
        // 1080 is below 1104 → should trigger
        {
            let state = crate::momentum::price_feed::PriceState::new();
            state.price_fp.store(1080, Ordering::Relaxed);
            engine.price_feed.prices.insert([0x22; 32], state);
        }

        engine.on_tick(2_000).await;

        assert_eq!(engine.active.len(), 0);
        assert_eq!(engine.sl_exits.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn test_momentum_time_sl() {
        let engine = make_test_engine(true);

        // Position entered at price 1000, T=1000
        let pos = MomentumPosition::new(
            [0x33; 32],
            1_000,
            1000,
            411,
            300_000_000,
            0,
            72,
            60,
            50_000,
            10,
            15_000,
        );
        engine.active.insert([0x33; 32], pos);

        // Price at entry level (no profit) after time_sl_ms (60_000ms)
        {
            let state = crate::momentum::price_feed::PriceState::new();
            state.price_fp.store(1000, Ordering::Relaxed); // no gain
            engine.price_feed.prices.insert([0x33; 32], state);
        }

        engine.on_tick(1_000 + 60_001).await;

        assert_eq!(engine.active.len(), 0);
        assert_eq!(engine.timeout_exits.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn test_momentum_daily_cap_blocks_entry() {
        let engine = make_test_engine(true);

        // Set daily P&L to -2.0 SOL (at cap)
        engine
            .daily_pnl_lamports
            .store(-2_000_000_000, Ordering::Relaxed);

        let pool_info = PoolInfo {
            coin_vault: [1u8; 32],
            pc_vault: [2u8; 32],
            reserve_token: 200_000_000_000_000,
            reserve_sol: 80_000_000_000,
            pool_type: PoolType::RaydiumAmmV4,
            mint: [0x44; 32],
        };

        engine
            .on_graduation(&pool_info, 1_000_000, 60, 50_000, 15)
            .await;
        assert_eq!(engine.graduations_seen.load(Ordering::Relaxed), 1);
        // But no pending entry because daily cap hit
        assert_eq!(engine.pending.lock().unwrap().active_count(), 0);
    }

    #[tokio::test]
    async fn test_momentum_stats_snapshot() {
        let engine = make_test_engine(true);
        engine.graduations_seen.store(5, Ordering::Relaxed);
        engine.entries_opened.store(3, Ordering::Relaxed);
        engine.tp1_exits.store(1, Ordering::Relaxed);
        engine.sl_exits.store(1, Ordering::Relaxed);
        engine
            .daily_pnl_lamports
            .store(500_000_000, Ordering::Relaxed);

        let stats = engine.stats();
        assert!(stats.enabled);
        assert!(stats.paper_mode);
        assert_eq!(stats.graduations_seen, 5);
        assert_eq!(stats.entries_opened, 3);
        assert_eq!(stats.tp1_exits, 1);
        assert_eq!(stats.sl_exits, 1);
        assert!((stats.daily_pnl_sol - 0.5).abs() < 0.001);
    }
}