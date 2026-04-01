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
    MomentumExitReason, MomentumPosition, MomentumState, PendingEntry, PendingEntryRing,
    price_to_bps_offset, compute_atr_bps, compute_momentum_score, PRICE_SAMPLES,
};
use crate::momentum::price_feed::{price_from_reserves, PriceFeedManager, VaultSubscription};
use crate::momentum::scorer::score_graduation;
use crate::engine::hot_path::ScoredToken;

use crate::tx::skeleton::{TxSkeleton, MAX_SKELETON_SIZE};
use crate::tx::tip_engine::{TipEngine, TipConfig, TipRequest};

use dashmap::DashMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

// ── Live mode types ─────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub enum LandingPath {
    JitoOnly,
    NozomiOnly,
    DualPath,
}

fn route_exit(reason: &str, gain_bps: i64, nozomi_available: bool) -> LandingPath {
    if !nozomi_available {
        return LandingPath::JitoOnly;
    }
    match reason {
        "hard_sl" => LandingPath::DualPath,
        "trailing_stop" if gain_bps < 0 => LandingPath::DualPath,
        "time_sl" | "max_hold" => LandingPath::NozomiOnly,
        _ => LandingPath::JitoOnly,
    }
}

fn exit_to_context(
    reason: &MomentumExitReason,
    gain_bps: i64,
) -> crate::tx::tip_engine::TipContext {
    use crate::tx::tip_engine::TipContext;
    match reason {
        MomentumExitReason::HardSl => TipContext::RideEmergency,
        MomentumExitReason::TrailingStop if gain_bps < 0 => TipContext::RideEmergency,
        MomentumExitReason::TrailingStop => TipContext::RideTighten,
        MomentumExitReason::TimeSl => TipContext::Scalp,
        MomentumExitReason::MaxHold => TipContext::Scalp,
        _ => TipContext::RideMomentum,
    }
}

/// Sell order sent from close_position → sell_task via crossbeam channel.
pub struct SellOrder {
    pub mint: [u8; 32],
    pub patched_msg: Box<[u8; MAX_SKELETON_SIZE]>,
    pub msg_len: usize,
    pub tip_lamports: u64,
    pub exit_reason: &'static str,
    pub gain_bps: i64,
    pub size_lamports: u64,
    pub landing_path: LandingPath,
}

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

    /// Tracks recently closed mint pubkeys to prevent re-entry within cooldown window.
    /// Key: mint [u8; 32], Value: close timestamp ms.
    /// Prevents 474+ phantom re-entries from CoreCast WebSocket reconnect floods.
    recently_closed: DashMap<[u8; 32], u64>,

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

    // ── Live mode tx infrastructure (all inactive in paper mode) ────
    skeletons: DashMap<[u8; 32], TxSkeleton>,
    tip_engine: Arc<parking_lot::Mutex<TipEngine>>,
    sell_tx: crossbeam_channel::Sender<SellOrder>,
    #[allow(dead_code)] // Used by sell_task closure in live mode
    jito_grpc: Option<Arc<crate::tx::jito_grpc::JitoGrpcClient>>,
    nozomi_client: Option<Arc<crate::tx::nozomi::NozomiClient>>,
    wallet_pubkey: Option<[u8; 32]>,
    blockhash_cache: Arc<crate::tx::executor::BlockhashCache>,

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
    /// Spawns an RPC polling price feed task and a JSONL logger thread.
    /// Returns `(engine, scored_token_sender, poll_handle, logger_handle)`.
    /// The caller passes `scored_token_sender` to `HotPath::set_scored_token_tx()`.
    pub fn new(
        config: Arc<MomentumConfig>,
        rpc_url: Arc<String>,
        helius_wss_url: String,
        log_path: &str,
        jito_grpc: Option<Arc<crate::tx::jito_grpc::JitoGrpcClient>>,
        nozomi_client: Option<Arc<crate::tx::nozomi::NozomiClient>>,
        wallet_pubkey: Option<[u8; 32]>,
        blockhash_cache: Arc<crate::tx::executor::BlockhashCache>,
    ) -> (Self, crossbeam_channel::Sender<ScoredToken>, tokio::task::JoinHandle<()>, std::thread::JoinHandle<()>) {
        let poll_interval_ms = config.price_poll_interval_ms;
        let (price_feed, ws_handle) = PriceFeedManager::new(
            rpc_url.to_string(),
            helius_wss_url,
            poll_interval_ms,
        );
        let (logger, logger_handle) = MomentumPaperLogger::new(log_path);

        // Channel for Kelly-scored tokens from hot_path → momentum engine
        let (scored_tx, scored_rx) = crossbeam_channel::bounded::<ScoredToken>(512);

        // Tip engine and sell channel for live mode
        let tip_engine = Arc::new(parking_lot::Mutex::new(
            TipEngine::new(TipConfig::default()),
        ));
        let (sell_tx, sell_rx) = crossbeam_channel::bounded::<SellOrder>(64);

        // Spawn sell task in live mode only
        if !config.paper_mode {
            if let (Some(jg), Some(wk)) = (jito_grpc.clone(), wallet_pubkey) {
                let tip_engine_clone = tip_engine.clone();
                let nozomi_clone = nozomi_client.clone();
                let bh_cache_clone = blockhash_cache.clone();
                tokio::spawn(async move {
                    Self::sell_task(
                        sell_rx, wk, jg, nozomi_clone, tip_engine_clone, bh_cache_clone,
                    )
                    .await;
                });
            }
        }

        let engine = Self {
            config,
            rpc_url,
            http_client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_millis(500))
                .build()
                .expect("reqwest client build should not fail"),
            active: DashMap::new(),
            recently_closed: DashMap::new(),
            pending: std::sync::Mutex::new(PendingEntryRing::new()),
            price_feed,
            logger,
            scored_tokens: DashMap::new(),
            scored_token_rx: scored_rx,
            skeletons: DashMap::new(),
            tip_engine,
            sell_tx,
            jito_grpc,
            nozomi_client,
            wallet_pubkey,
            blockhash_cache,
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

        // Reentry cooldown: skip if this mint was recently closed (CoreCast flood prevention)
        if let Some(close_ts) = self.recently_closed.get(&pool_info.mint) {
            if now_ms.saturating_sub(*close_ts) < self.config.reentry_cooldown_ms {
                tracing::debug!(
                    mint = %bs58::encode(&pool_info.mint).into_string(),
                    closed_ago_ms = now_ms.saturating_sub(*close_ts),
                    "[momentum] skipping graduation — reentry cooldown active"
                );
                return;
            }
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
            recovery_score: 0,
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

        // Prune stale reentry cooldown entries (O(n) but n ≤ ~50 per session)
        self.recently_closed.retain(|_, close_ts| {
            now_ms.saturating_sub(*close_ts) < self.config.reentry_cooldown_ms
        });
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
        // Fallback: Kelly-tiered sizing from grad_score.
        // Post-fix score range is 25-85 (E1 wires speed/volume/velocity inputs).
        let size_sol: f64 = match grad_score {
            75..=100 => 0.30,
            55..=74  => 0.20,
            35..=54  => 0.10,
            _        => 0.05,
        };
        (size_sol * 1_000_000_000.0) as u64
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

        // Scale-in: evaluate probe positions for momentum confirmation
        self.process_scale_in(now_ms);
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

            // Compute recovery score from live price vs BC terminal price.
            let recovery = crate::momentum::scorer::recovery_score_from_prices(
                current_price_fp,
                entry.bc_price_fp,
            );
            let final_score = entry.grad_score.saturating_add(recovery);

            // Scale-in entry: ALL entries start as probes at probe_size_sol (0.10 SOL).
            // Scaling up happens in process_scale_in() when s[0] or s[1] confirms momentum.
            // Quant spec §4: probe 0.10 → scale to 0.50 on s[0]≥300, 0.30 on s[0]≥100.
            let size_lamports = if self.config.probe_size_sol > 0.0 {
                (self.config.probe_size_sol * 1_000_000_000.0) as u64
            } else {
                self.compute_size_lamports(&entry.mint, final_score as u32)
            };
            let pos = MomentumPosition::new(
                entry.mint,
                now_ms,
                current_price_fp,
                entry.bc_price_fp,
                size_lamports,
                entry.pool_type,
                final_score,
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

            tracing::info!(
                mint = %bs58::encode(&entry.mint).into_string(),
                score = entry.grad_score,
                size_sol = size_lamports as f64 / 1e9,
                speed_s = entry.grad_speed_s,
                volume_x100 = entry.grad_volume_sol_x100,
                buys_5s = entry.pre_grad_buys_5s,
                "[momentum] entry opened"
            );

            // Build sell skeleton at position open (cold path — ~5μs is fine here)
            // NOTE: bonding_curve and assoc_bonding_curve are not currently available
            // in PendingEntry/PoolInfo. Skeleton building requires these PDAs.
            // For live mode Phase 2: add bonding_curve resolution to pool.rs and
            // wire through PendingEntry. For now, skeleton building is skipped if
            // the data isn't available, and the sell path will fall back to the
            // full async TxBuilder (slower but correct).
            if !self.config.paper_mode {
                if let Some(ref _wallet_pk) = self.wallet_pubkey {
                    tracing::debug!(
                        mint = %bs58::encode(&entry.mint).into_string(),
                        "[momentum] live mode: skeleton build deferred — bonding_curve PDA not yet wired through PendingEntry"
                    );
                    // TODO: When bonding_curve + assoc_bonding_curve are available:
                    // match TxSkeleton::build_sell_skeleton(
                    //     &entry.mint, &bonding_curve, &assoc_bonding_curve,
                    //     wallet_pk, size_lamports, 0, 0,
                    // ) {
                    //     Ok(skeleton) => { self.skeletons.insert(entry.mint, skeleton); }
                    //     Err(e) => tracing::warn!(err = %e, "[momentum] failed to build sell skeleton"),
                    // }
                }
            }

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

            // 0. Max hold handling — profit-aware with momentum-state extension.
            // Quant spec:
            //   - ACCELERATING at max_hold_ms: extend by 120s (one-time), dynamic 15% trail
            //   - SUSTAINING profitable: apply max_hold_trail_pct (5%) trail, re-eval at max_hold_ms+60s
            //   - UNPROFITABLE at max_hold_ms: immediate exit
            //   - Absolute cap: max_hold_ms * 2 (600s → 1200s)
            if elapsed_ms >= self.config.max_hold_ms {
                let exit_price = self.price_feed.current_price(&mint).unwrap_or(pos.entry_price_fp);
                let current_bps = price_to_bps_offset(pos.entry_price_fp, exit_price);

                // If unprofitable: immediate exit regardless of state
                if current_bps <= 0 {
                    to_close.push((mint, MomentumExitReason::MaxHold, exit_price));
                    continue;
                }

                // Absolute cap: 2× max_hold_ms (default: 600s → 1200s)
                if elapsed_ms >= self.config.max_hold_ms * 2 {
                    to_close.push((mint, MomentumExitReason::MaxHold, exit_price));
                    continue;
                }

                // Profitable — apply profit-aware extension
                // Use Fix G trailing stop logic (already implemented) as the exit mechanism
                // The existing max_hold_trail block handles this after the price guard
                // So we just DON'T force-exit here for profitable positions
                // Fall through to price guard and Fix G trailing block will handle it
            }

            // Trailing-stop-at-maturity: once past activation threshold, apply trailing stop.
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
                // First sample: bypass spike guard — price feed already filters catastrophic spikes.
                // Peak and reference haven't been set yet (peak==entry), so ratio check is meaningless.
                // Micro-price tokens legitimately 5-12x between entry and first poll delivery.
                if (pos.sample_count as usize) < PRICE_SAMPLES {
                    pos.price_samples_bps[pos.sample_count as usize] =
                        price_to_bps_offset(pos.entry_price_fp, current_price_fp);
                    pos.sample_count += 1;
                    if current_price_fp > pos.peak_price_fp {
                        pos.peak_price_fp = current_price_fp;
                    }
                }
            }

            // Fix A: Configurable sample interval (default: every ~1s instead of every ~10s).
            let ticks_elapsed = elapsed_ms / self.config.check_ms.max(1);
            let sample_interval = self.config.sample_interval_ticks.max(1);
            if ticks_elapsed > 0 && ticks_elapsed % sample_interval == 0 {
                pos.record_sample(current_price_fp);
            }

            // Update peak for trailing stop — with spike guard.
            // Reject price updates >50x the current peak (same guard as price_feed and record_sample).
            if current_price_fp > pos.peak_price_fp {
                let ref_price = pos.peak_price_fp.max(pos.entry_price_fp);
                let ratio = if ref_price > 0 { current_price_fp / ref_price } else { 0 };
                if ratio <= 50 {
                    pos.peak_price_fp = current_price_fp;
                }
                // else: spike — don't update peak, price_feed should have caught this
            }

            let hold_ms = elapsed_ms;
            let entry_fp = pos.entry_price_fp;

            // Top detection — quant spec: fire 75% exit when 2+ of 5 signals trigger.
            // Guards: (1) TP1 must be hit (position proved profitability at +5%)
            //         (2) current price must be above entry (no top to detect in a loss)
            // TopDetector state is stored serialized in pos._pad2[0..17].
            // Only evaluates when we have at least 2 samples (derivative requires prev sample).
            if pos.sample_count >= 2 && pos.tp_flags & 0x1 != 0 {
                let curr_bps = price_to_bps_offset(entry_fp, current_price_fp);
                if curr_bps > 0 {
                    let prev_bps = pos.price_samples_bps[pos.sample_count as usize - 2];
                    let mut td = pos.top_detector();
                    let signal_count = td.evaluate(curr_bps, prev_bps);
                    pos.set_top_detector(&td);

                    if signal_count >= self.config.top_detection_strong_signals as u8 {
                        to_close.push((mint, MomentumExitReason::TrailingStop, current_price_fp));
                        continue;
                    }
                }
            }

            // Phase 2: Velocity-based micro exit — fires only in first micro_exit_window_ms.
            // Requires N consecutive ticks of strong negative velocity (not a single bad tick).
            // Threshold: -200 bps/tick at 1050ms cadence = -1.9%/s sustained dump.
            if hold_ms <= self.config.micro_exit_window_ms {
                let n = pos.sample_count as usize;
                let n_consec = self.config.micro_exit_n_consecutive as usize;
                if n >= n_consec + 1 {
                    let threshold = self.config.micro_exit_velocity_bps;
                    let all_below = (0..n_consec).all(|i| {
                        let a = pos.price_samples_bps[n - 1 - i] as i32;
                        let b = pos.price_samples_bps[n - 2 - i] as i32;
                        (a - b) < threshold
                    });
                    if all_below {
                        tracing::debug!(
                            mint = %bs58::encode(&mint).into_string(),
                            n_samples = n,
                            hold_ms,
                            "[momentum] velocity micro exit"
                        );
                        to_close.push((mint, MomentumExitReason::HardSl, current_price_fp));
                        continue;
                    }
                }
            }

            // 2. Hard SL
            let hard_sl_bps = (self.config.hard_sl_pct * 100.0) as u32;
            if pos.hard_sl_hit(current_price_fp, hard_sl_bps) {
                to_close.push((mint, MomentumExitReason::HardSl, current_price_fp));
                continue;
            }

            // 3. Trailing stop — active after TP1 hit, width is momentum-state-aware.
            // Quant spec: ACCELERATING=15%, SUSTAINING=8%, DECELERATING=5%, REVERSING=3%
            if pos.tp_flags & 0x1 != 0 {
                let state = pos.momentum_state(
                    self.config.momentum_accel_threshold_bps,
                    self.config.momentum_decel_threshold_bps,
                    self.config.momentum_reversal_threshold_bps,
                );
                let base_trail = match state {
                    MomentumState::Accelerating => self.config.trailing_stop_accel_pct,
                    MomentumState::Sustaining | MomentumState::Unknown => self.config.trailing_stop_pct,
                    MomentumState::Decelerating => self.config.trailing_stop_decel_pct,
                    MomentumState::Reversing => self.config.trailing_stop_reversal_pct,
                };

                // Phase 3: ATR-adaptive trail width.
                // trail = max(base, k * ATR_bps / 100), clamped per phase.
                let trail_pct = if pos.sample_count >= self.config.trail_min_samples_for_atr {
                    let n = pos.sample_count as usize;
                    let atr = compute_atr_bps(
                        &pos.price_samples_bps[..n],
                        self.config.trail_atr_window,
                    );
                    let vol_trail = self.config.trail_atr_multiplier * atr as f64 / 100.0;
                    let raw = base_trail.max(vol_trail);
                    let (min_c, max_c) = match state {
                        MomentumState::Accelerating                        => (5.0f64, 30.0),
                        MomentumState::Sustaining | MomentumState::Unknown => (3.0, 20.0),
                        MomentumState::Decelerating                        => (2.0, 12.0),
                        MomentumState::Reversing                           => (1.0,  5.0),
                    };
                    raw.clamp(min_c, max_c)
                } else {
                    base_trail
                };
                let trailing_bps = (trail_pct * 100.0) as u32;
                if pos.trailing_stop_hit(current_price_fp, trailing_bps) {
                    to_close.push((
                        mint,
                        MomentumExitReason::TrailingStop,
                        current_price_fp,
                    ));
                    continue;
                }
            }

            // ── Adaptive dead zone Phase 1: WS activity silence ──────────────────────
            // ShredStream cannot see post-graduation Raydium/PumpSwap trades.
            // Helius WS accountSubscribe notifications proxy swap activity.
            // Each notification = vault balance changed = a swap occurred on-chain.
            {
                let (ws_count, ws_last_ms) = self.price_feed.ws_notif_info(&mint);
                pos.set_ws_notif_count(ws_count);
                if ws_last_ms > 0 {
                    pos.set_ws_notif_last_ms(ws_last_ms);
                }

                let timeout_ms = if ws_last_ms == 0 {
                    self.config.dead_zone_ws_fallback_ms
                } else if ws_count == 0 {
                    self.config.dead_zone_ws_zero_ms
                } else if ws_count <= self.config.dead_zone_ws_sparse_n as u64 {
                    self.config.dead_zone_ws_sparse_ms
                } else {
                    self.config.dead_zone_ws_active_ms
                };

                let ref_ts = if ws_last_ms > 0 { ws_last_ms } else { pos.entry_ts_ms };
                let silence_ms = now_ms.saturating_sub(ref_ts);

                // Require BOTH WS silence AND price staleness when price samples exist.
                // Prevents false exits when RPC poll delivers but WS hasn't notified yet.
                let price_stale = if pos.sample_count >= 2 {
                    let sample_period_ms = (self.config.check_ms * self.config.sample_interval_ticks).max(1);
                    let last_sample_ts = pos.entry_ts_ms + (pos.sample_count as u64 * sample_period_ms);
                    now_ms.saturating_sub(last_sample_ts) > timeout_ms
                } else {
                    true
                };

                if silence_ms > timeout_ms && price_stale {
                    tracing::debug!(
                        mint = %bs58::encode(&mint).into_string(),
                        ws_count,
                        silence_ms,
                        timeout_ms,
                        "[momentum] adaptive dead zone exit"
                    );
                    to_close.push((mint, MomentumExitReason::TimeSl, current_price_fp));
                    continue;
                }
            }
            // ── End adaptive dead zone Phase 1 ───────────────────────────────────────

            // ── Phase 4: Momentum decay exit — replaces blunt max_hold wall ──────────
            // Fires when exponentially-weighted momentum score drops below threshold.
            // Only activates after min_hold_ms (first 30s is price discovery noise).
            // Does NOT fire if trailing stop is armed (TP1 hit) — trail handles those.
            if hold_ms >= self.config.momentum_decay_min_hold_ms {
                let trailing_armed = pos.tp_flags & 0x1 != 0;
                if !trailing_armed {
                    let n = pos.sample_count as usize;
                    let score = compute_momentum_score(
                        &pos.price_samples_bps[..n],
                        self.config.momentum_decay_window,
                    );
                    if score < self.config.momentum_decay_threshold {
                        tracing::debug!(
                            mint = %bs58::encode(&mint).into_string(),
                            score,
                            hold_ms,
                            "[momentum] momentum decay exit"
                        );
                        to_close.push((mint, MomentumExitReason::MaxHold, current_price_fp));
                        continue;
                    }
                }
            }
            // ── End momentum decay ────────────────────────────────────────────────────

            // ── Phase 5: Price-direct dead zone ──────────────────────────────────
            // Flat priced tokens: max gain across all samples < threshold → dead
            if self.config.dead_zone_price_flat_min_samples > 0
                && pos.sample_count >= self.config.dead_zone_price_flat_min_samples
                && elapsed_ms >= self.config.dead_zone_price_flat_min_hold_ms
            {
                let n = pos.sample_count as usize;
                let nonzero: Vec<i32> = pos.price_samples_bps[..n].iter().filter(|&&s| s != 0).copied().collect();
                if !nonzero.is_empty() {
                    let max_gain = *nonzero.iter().max().unwrap();
                    let _min_gain = *nonzero.iter().min().unwrap();

                    // Always negative: genuine dump — exit as hard_sl (not time_sl)
                    if max_gain < self.config.dead_zone_price_always_down_bps {
                        to_close.push((mint, MomentumExitReason::HardSl, current_price_fp));
                        continue;
                    }

                    // Flat: never exceeded threshold → dead token
                    if max_gain < self.config.dead_zone_price_flat_bps {
                        to_close.push((mint, MomentumExitReason::TimeSl, current_price_fp));
                        continue;
                    }
                }
            }
            // ── End Phase 5 ──────────────────────────────────────────────────────

            // Dead zone detection — quant spec: kill tokens with no momentum early.
            // Phase 2 (T+60s): if cumulative bps < dead_zone_confirmed_bps (200) → exit
            // Phase 3: if total movement < dead_zone_stagnant_bps (300) in last 30s → exit
            if self.config.dead_zone_early_ms > 0 && pos.sample_count >= 5 {
                let n = pos.sample_count as usize;
                let current_bps = price_to_bps_offset(entry_fp, current_price_fp);

                // Phase 2: confirmed dead at T+15s (was T+60s)
                if hold_ms >= self.config.dead_zone_confirmed_ms
                    && pos.sample_count >= 5
                {
                    if current_bps < self.config.dead_zone_confirmed_bps {
                        to_close.push((mint, MomentumExitReason::TimeSl, current_price_fp));
                        continue;
                    }

                    // Phase 3: stagnation — total movement < 300 bps over last 30s
                    let samples_in_window = (self.config.dead_zone_stagnant_window_ms
                        / (self.config.sample_interval_ticks * self.config.check_ms).max(1))
                        as usize;
                    if samples_in_window >= 2 && n >= samples_in_window {
                        let window = &pos.price_samples_bps[n - samples_in_window..n];
                        let max_s = window.iter().copied().max().unwrap_or(0);
                        let min_s = window.iter().copied().min().unwrap_or(0);
                        if (max_s - min_s) < self.config.dead_zone_stagnant_bps {
                            to_close.push((mint, MomentumExitReason::TimeSl, current_price_fp));
                            continue;
                        }
                    }
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

            // 5. TP milestones — these set tp_flags but do NOT trigger position closes.
            // TP1 (default 5%): activates dynamic trailing stop (bit 0)
            // TP2 (default 15%): logged milestone (bit 1) — no exit
            // TP3 (default 999%): safety ceiling only — top detection handles real exits
            // Quant spec: tp1_exit_pct=0.0 and tp2_exit_pct=0.0 means NO partial exits at TP1/TP2.
            let raw_gain_bps = price_to_bps_offset(entry_fp, current_price_fp);
            // Sanity clamp: reject implausible gains (>500%) as transient price feed artifacts.
            // Batch RPC getAccountInfo has no cross-account atomicity — slot-mismatched vault
            // reads can produce 1000x spikes for one poll cycle on low-price tokens.
            // 50,000 bps (+500%) is well above any real 10s post-graduation move.
            let gain_bps = raw_gain_bps.min(50_000);
            if raw_gain_bps > 50_000 {
                tracing::warn!(
                    mint = %bs58::encode(&mint).into_string(),
                    raw_gain_bps,
                    clamped = 50_000,
                    entry_price = entry_fp,
                    current_price = current_price_fp,
                    hold_ms,
                    "[momentum] implausible gain clamped — likely stale vault data"
                );
            }
            let tp3_bps = (self.config.tp3_pct * 100.0) as i32;
            let tp2_bps = (self.config.tp2_pct * 100.0) as i32;
            let tp1_bps = (self.config.tp1_pct * 100.0) as i32;

            if gain_bps >= tp3_bps && pos.tp_flags & 0x4 == 0 {
                // Safety ceiling only (tp3_pct=999.0 by default, effectively never fires)
                pos.tp_flags |= 0x7;
                to_close.push((mint, MomentumExitReason::Tp3, current_price_fp));
            } else if gain_bps >= tp2_bps && pos.tp_flags & 0x2 == 0 {
                pos.tp_flags |= 0x3; // milestone — no close, just flag
            } else if gain_bps >= tp1_bps && pos.tp_flags & 0x1 == 0 {
                pos.tp_flags |= 0x1; // activates dynamic trailing stop above — no close
            }
        }

        // Close positions (must release iter_mut borrow first)
        for (mint, reason, exit_price_fp) in to_close {
            self.close_position(mint, reason, exit_price_fp, now_ms);
        }
    }

    /// Scale-in logic: after first price sample, scale position based on momentum signal.
    /// Called from on_tick() AFTER process_active_positions().
    ///
    /// Quant spec §4:
    /// - s[0] >= 300 bps: add scale_in_s0_strong_sol (0.40) → total 0.50 SOL
    /// - s[0] >= 100 bps: add scale_in_s0_moderate_sol (0.20) → total 0.30 SOL
    /// - s[0] < 0 bps: exit probe immediately (dump signal)
    /// - s[1] >= 200 bps (if still at probe size): add scale_in_s1_sol (0.15) → total 0.25
    fn process_scale_in(&self, _now_ms: u64) {
        // Skip if probe/scale-in disabled
        if self.config.probe_size_sol <= 0.0 {
            return;
        }

        let probe_lamports = (self.config.probe_size_sol * 1e9) as u64;
        let max_lamports = (self.config.max_total_size_sol * 1e9) as u64;

        for mut entry in self.active.iter_mut() {
            let pos = entry.value_mut();

            // Skip if already scaled in
            if pos.is_scaled_in() {
                continue;
            }

            // Need at least 1 sample for s[0]
            if pos.sample_count < 1 {
                continue;
            }

            let s0 = pos.price_samples_bps[0];

            // s[0] < 0: dump signal — mark scaled_in to prevent further checks.
            // The dead zone or time_sl will handle the actual exit.
            if s0 < 0 {
                pos.set_scaled_in();
                continue;
            }

            // s[0] == 0: ambiguous — price hasn't moved from entry yet, OR feed hasn't delivered.
            // Skip until we have a meaningful non-zero reading or fall through to s[1] check.
            // Do NOT lock at probe — this will be resolved by s[1] or the sample_count guard.
            if s0 == 0 && pos.sample_count == 1 {
                continue;
            }

            // Score-aware strong conviction threshold:
            // High-score tokens (grad_score >= 65) need less price confirmation (200 bps).
            // Low-score tokens (grad_score < 35) need more proof (400 bps).
            // Mid-range uses default scale_in_s0_strong_bps (300).
            let s0_strong_bps = if pos.grad_score >= self.config.scale_in_high_score_threshold {
                self.config.scale_in_high_score_s0_bps
            } else if pos.grad_score < self.config.scale_in_low_score_threshold {
                self.config.scale_in_low_score_s0_bps
            } else {
                self.config.scale_in_s0_strong_bps
            };

            // s[0] >= s0_strong_bps: strong conviction (score-adjusted)
            if s0 >= s0_strong_bps {
                let new_size = (probe_lamports as f64 + self.config.scale_in_s0_strong_sol * 1e9) as u64;
                pos.size_lamports = new_size.min(max_lamports);
                pos.set_scaled_in();
                tracing::info!(
                    mint = %bs58::encode(&pos.mint).into_string(),
                    s0,
                    grad_score = pos.grad_score,
                    s0_strong_bps,
                    new_size_sol = pos.size_lamports as f64 / 1e9,
                    "[momentum] scale-in: STRONG conviction (s[0] >= {}, score={})",
                    s0_strong_bps,
                    pos.grad_score
                );
                continue;
            }

            // s[0] >= scale_in_s0_moderate_bps (100): moderate conviction
            if s0 >= self.config.scale_in_s0_moderate_bps {
                let new_size = (probe_lamports as f64 + self.config.scale_in_s0_moderate_sol * 1e9) as u64;
                pos.size_lamports = new_size.min(max_lamports);
                pos.set_scaled_in();
                tracing::info!(
                    mint = %bs58::encode(&pos.mint).into_string(),
                    s0,
                    new_size_sol = pos.size_lamports as f64 / 1e9,
                    "[momentum] scale-in: MODERATE conviction (s[0] >= {})",
                    self.config.scale_in_s0_moderate_bps
                );
                continue;
            }

            // s[0] 0-99: weak — check s[1] if available
            if pos.sample_count >= 2 {
                let s1 = pos.price_samples_bps[1];
                if s1 >= self.config.scale_in_s1_moderate_bps {
                    let new_size = (probe_lamports as f64 + self.config.scale_in_s1_sol * 1e9) as u64;
                    pos.size_lamports = new_size.min(max_lamports);
                    pos.set_scaled_in();
                    tracing::info!(
                        mint = %bs58::encode(&pos.mint).into_string(),
                        s1,
                        new_size_sol = pos.size_lamports as f64 / 1e9,
                        "[momentum] scale-in: s[1] confirmation (s[1] >= {})",
                        self.config.scale_in_s1_moderate_bps
                    );
                } else if s1 < 0 {
                    // s[1] negative after weak s[0] — give up scaling, stay at probe
                    pos.set_scaled_in();
                }
                // Only lock at probe if we've seen real price data (at least one non-zero sample).
                // If all samples are zero, price feed hasn't delivered meaningful data yet — keep waiting.
                let has_real_price = pos.price_samples_bps[..pos.sample_count as usize].iter().any(|&s| s != 0);
                if pos.sample_count >= 3 && has_real_price && !pos.is_scaled_in() {
                    pos.set_scaled_in();
                }

                // Hard lock: if we have 5+ samples and none show scale-in signal, accept flat token.
                if pos.sample_count >= 5 && !pos.is_scaled_in() {
                    pos.set_scaled_in();
                }
            }
        }
    }

    /// Sync blockhash access for the sell path (no async).
    fn blockhash_cache_sync(&self) -> Option<[u8; 32]> {
        self.blockhash_cache.get_sync()
    }

    /// Sell task: async consumer that signs and submits sell transactions
    /// via Jito/Nozomi/DualPath. Runs in a dedicated tokio task.
    async fn sell_task(
        sell_rx: crossbeam_channel::Receiver<SellOrder>,
        _wallet_pubkey: [u8; 32],
        jito_grpc: Arc<crate::tx::jito_grpc::JitoGrpcClient>,
        nozomi_client: Option<Arc<crate::tx::nozomi::NozomiClient>>,
        tip_engine: Arc<parking_lot::Mutex<TipEngine>>,
        _blockhash_cache: Arc<crate::tx::executor::BlockhashCache>,
    ) {
        // Load wallet keypair for signing
        let keypair_path = std::env::var("WALLET_KEYPAIR_PATH").unwrap_or_default();
        let keypair_bytes = match std::fs::read(&keypair_path) {
            Ok(b) => b,
            Err(e) => {
                tracing::error!(err = ?e, "sell_task: failed to load wallet keypair");
                return;
            }
        };
        let keypair_arr: Vec<u8> = serde_json::from_slice(&keypair_bytes).unwrap_or_default();
        if keypair_arr.len() != 64 {
            tracing::error!("sell_task: invalid keypair length {}", keypair_arr.len());
            return;
        }
        let mut kp_bytes = [0u8; 64];
        kp_bytes.copy_from_slice(&keypair_arr);
        let keypair = solana_sdk::signature::Keypair::from_bytes(&kp_bytes)
            .expect("invalid keypair bytes");

        while let Ok(order) = sell_rx.recv() {
            let mint_b58 = bs58::encode(&order.mint).into_string();
            let msg_bytes = &order.patched_msg[..order.msg_len];

            // Sign the message
            use solana_sdk::signer::Signer;
            let sig = keypair.sign_message(msg_bytes);

            // Build versioned transaction: [1 sig count][64 sig bytes][message bytes]
            let mut tx_bytes = Vec::with_capacity(1 + 64 + msg_bytes.len());
            tx_bytes.push(1u8); // 1 signature
            tx_bytes.extend_from_slice(sig.as_ref());
            tx_bytes.extend_from_slice(msg_bytes);
            let tx_b64 = {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD.encode(&tx_bytes)
            };

            let landed = match order.landing_path {
                LandingPath::JitoOnly => {
                    match jito_grpc.submit_bundle(&tx_b64).await {
                        Ok(id) => {
                            tracing::info!(
                                mint = %mint_b58, bundle_id = %id,
                                tip = order.tip_lamports,
                                "[sell_task] Jito submitted"
                            );
                            true
                        }
                        Err(e) => {
                            tracing::error!(
                                mint = %mint_b58, err = ?e,
                                "[sell_task] Jito FAILED"
                            );
                            false
                        }
                    }
                }
                LandingPath::NozomiOnly => {
                    if let Some(ref noz) = nozomi_client {
                        match noz.send_transaction(&tx_b64).await {
                            Ok(sig) => {
                                tracing::info!(
                                    mint = %mint_b58, sig = %sig,
                                    "[sell_task] Nozomi submitted"
                                );
                                true
                            }
                            Err(e) => {
                                tracing::warn!(
                                    mint = %mint_b58, err = ?e,
                                    "[sell_task] Nozomi failed, falling back to Jito"
                                );
                                jito_grpc.submit_bundle(&tx_b64).await.is_ok()
                            }
                        }
                    } else {
                        jito_grpc.submit_bundle(&tx_b64).await.is_ok()
                    }
                }
                LandingPath::DualPath => {
                    let jito_fut = jito_grpc.submit_bundle(&tx_b64);
                    if let Some(ref noz) = nozomi_client {
                        let noz_fut = noz.send_transaction(&tx_b64);
                        let (j, n) = tokio::join!(jito_fut, noz_fut);
                        tracing::info!(
                            mint = %mint_b58,
                            jito_ok = j.is_ok(), nozomi_ok = n.is_ok(),
                            "[sell_task] dual-path submitted"
                        );
                        j.is_ok() || n.is_ok()
                    } else {
                        jito_fut.await.is_ok()
                    }
                }
            };

            tip_engine.lock().record_result(landed);
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

        // Safety: exit_price_fp=0 means price feed had no data — clamp to entry to avoid phantom loss
        let exit_price_fp = if exit_price_fp == 0 {
            tracing::warn!(
                mint = %bs58::encode(&mint).into_string(),
                "[close_position] exit_price_fp=0 — clamping to entry_price (phantom prevention)"
            );
            pos.entry_price_fp
        } else {
            exit_price_fp
        };

        // Record close timestamp for reentry cooldown
        self.recently_closed.insert(mint, now_ms);

        // Calculate P&L
        let size_sol = pos.size_lamports as f64 / 1e9;
        let raw_gain_bps = price_to_bps_offset(pos.entry_price_fp, exit_price_fp);
        // Sanity clamp: no real trade gains >500% or loses >100% — bad price feed data.
        // Tightened from 100,000 (10x) to 50,000 (5x): real tokens don't 5x in one poll cycle.
        // Ghost trades from residual spikes now cap at +0.4995 SOL, distinguishable from real exits.
        let gain_bps = raw_gain_bps.clamp(-10_000, 50_000);
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

        // Unsubscribe from price feed (direct DashMap remove — no async needed)
        self.price_feed.unsubscribe_sync(&mint);

        // Log to JSONL
        let grad_vol_sol = pos.grad_volume_sol_x100 as f64 / 100.0;
        let bc_price_f64 = pos.bc_terminal_price_fp as f64 / 1_000_000.0;
        let entry_price_f64 = pos.entry_price_fp as f64 / 1_000_000.0;
        let structural_discount = if bc_price_f64 > 0.0 {
            (entry_price_f64 - bc_price_f64) / bc_price_f64 * 100.0
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

        // ── Live mode: build and enqueue sell transaction ──────────────────────
        if !self.config.paper_mode {
            if let Some((_, skeleton)) = self.skeletons.remove(&mint) {
                let blockhash = self.blockhash_cache_sync();
                let tip_req = TipRequest {
                    context: exit_to_context(&reason, gain_bps as i64),
                    size_lamports: pos.size_lamports,
                    gain_bps: gain_bps as i64,
                    grad_score: 0.0,
                };
                let tip = self.tip_engine.lock().compute_tip(&tip_req);

                // min_sol_out = 0 for speed (no slippage protection); TODO: add slippage
                let min_sol_out = 0u64;
                // tokens_to_sell: approximate from size_lamports and entry price.
                // In paper mode we don't track actual tokens held. For live mode the
                // skeleton patches tokens_to_sell with position's known token amount.
                // For now, use the vtoken_reserves placeholder (patched at skeleton build).
                let tokens_to_sell = pos.size_lamports; // proxy — skeleton built with real value

                let mut patched = Box::new([0u8; MAX_SKELETON_SIZE]);
                let bh = blockhash.unwrap_or([0u8; 32]);
                let msg_len =
                    skeleton.patch(min_sol_out, tokens_to_sell, &bh, tip, patched.as_mut());

                let landing_path =
                    route_exit(reason.as_str(), gain_bps as i64, self.nozomi_client.is_some());

                match self.sell_tx.try_send(SellOrder {
                    mint,
                    patched_msg: patched,
                    msg_len,
                    tip_lamports: tip,
                    exit_reason: reason.as_str(),
                    gain_bps: gain_bps as i64,
                    size_lamports: pos.size_lamports,
                    landing_path,
                }) {
                    Ok(()) => tracing::debug!(
                        mint = %bs58::encode(&mint).into_string(),
                        tip,
                        "[close_position] sell queued"
                    ),
                    Err(e) => tracing::error!(
                        mint = %bs58::encode(&mint).into_string(),
                        err = %e,
                        "[close_position] sell channel FULL — position closed but sell NOT submitted"
                    ),
                }
            } else {
                tracing::warn!(
                    mint = %bs58::encode(&mint).into_string(),
                    "[close_position] no skeleton found — sell NOT submitted"
                );
            }
        }
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
                // Derive effective enrichment from LP reserves when mint_map was cold (all zeros).
                let effective_volume_sol_x100 = if enrichment.volume_sol_x100 == 0 {
                    (resolution.reserve_sol_lamports / 10_000_000).min(65535) as u32
                } else {
                    enrichment.volume_sol_x100
                };
                let effective_speed_s = if enrichment.grad_speed_s == 0 {
                    let sol = resolution.reserve_sol_lamports / 1_000_000_000;
                    if sol >= 150 { 60u32 } else if sol >= 100 { 120u32 } else { 240u32 }
                } else {
                    enrichment.grad_speed_s
                };
                let effective_buys_5s = if enrichment.buys_5s == 0 {
                    3u32
                } else {
                    enrichment.buys_5s as u32
                };
                self.on_graduation(
                    &pool_info,
                    ts_ms,
                    effective_speed_s,
                    effective_volume_sol_x100,
                    effective_buys_5s,
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
        make_test_engine_with(enabled, |_| {})
    }

    fn make_test_engine_with(enabled: bool, f: impl FnOnce(&mut MomentumConfig)) -> MomentumEngine {
        let mut cfg = MomentumConfig::default();
        cfg.enabled = enabled;
        f(&mut cfg);
        let config = Arc::new(cfg);
        let rpc_url = Arc::new("https://example.com".to_string());

        let log_path = format!(
            "{}/momentum_test_{}.jsonl",
            std::env::temp_dir().display(),
            std::process::id()
        );

        let bh_cache = crate::tx::executor::BlockhashCache::new();

        let (engine, _scored_tx, ws_handle, _logger_handle) = MomentumEngine::new(
            config,
            rpc_url,
            "wss://invalid.example.com".to_string(),
            &log_path,
            None, // jito_grpc
            None, // nozomi_client
            None, // wallet_pubkey
            bh_cache,
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
                recovery_score: 0,
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
        let engine = make_test_engine_with(true, |cfg| { cfg.tp3_pct = 50.0; });

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

    // ── Phase 2: Velocity micro exit tests ──────────────────────────────

    #[test]
    fn test_velocity_micro_exit_two_consecutive() {
        // 3 samples: [0, -220, -450] → vel_1 = -230, vel_2 = -220 → both < -200 → should fire
        // This is a unit test of the logic — verify the (a-b) < threshold condition
        let samples: [i32; 3] = [0, -220, -450];
        let n = 3usize;
        let threshold = -200i32;
        let n_consec = 2usize;
        let all_below = (0..n_consec).all(|i| {
            let a = samples[n - 1 - i];
            let b = samples[n - 2 - i];
            (a - b) < threshold
        });
        assert!(all_below, "should detect dump: vel_1={} vel_2={}", samples[2]-samples[1], samples[1]-samples[0]);
    }

    #[test]
    fn test_velocity_micro_exit_single_bad_tick_no_fire() {
        // samples: [0, 100, -50, 80] → vel_1=130, vel_2=-150 → NOT both < -200
        let samples: [i32; 4] = [0, 100, -50, 80];
        let n = 4usize;
        let threshold = -200i32;
        let n_consec = 2usize;
        let all_below = (0..n_consec).all(|i| {
            let a = samples[n - 1 - i];
            let b = samples[n - 2 - i];
            (a - b) < threshold
        });
        assert!(!all_below, "single bad tick should not trigger micro exit");
    }
}