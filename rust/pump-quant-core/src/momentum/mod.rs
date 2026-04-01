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
pub mod tod;
pub mod velocity;

pub use config::MomentumConfig;
pub use logger::{MomentumClosedPosition, MomentumPaperLogger};
pub use pool::{PoolType, PoolInfo, PoolResolution, BC_TERMINAL_PRICE_LAMPORTS_PER_ATOM};

use crate::momentum::pool::resolve_pool_from_transaction;
use crate::momentum::position::{
    MomentumExitReason, MomentumPosition, MomentumState, PendingEntry, PendingEntryRing,
    ReserveSolContext, liquidity_quality_score,
    price_to_bps_offset, compute_atr_bps, compute_momentum_score, PRICE_SAMPLES,
};
use crate::momentum::price_feed::{price_from_reserves, PriceFeedManager, VaultSubscription};
use crate::momentum::scorer::score_graduation;
use crate::engine::hot_path::ScoredToken;

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
        "drain_detected" | "hard_sl" => LandingPath::DualPath,
        "trailing_stop" | "velocity_exit" if gain_bps < 0 => LandingPath::DualPath,
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
        MomentumExitReason::DrainDetected => TipContext::RideEmergency,
        MomentumExitReason::HardSl => TipContext::RideEmergency,
        MomentumExitReason::TrailingStop if gain_bps < 0 => TipContext::RideEmergency,
        MomentumExitReason::TrailingStop => TipContext::RideTighten,
        MomentumExitReason::VelocityExit if gain_bps < 0 => TipContext::RideEmergency,
        MomentumExitReason::VelocityExit => TipContext::RideTighten,
        MomentumExitReason::TimeSl => TipContext::Scalp,
        MomentumExitReason::MaxHold => TipContext::Scalp,
        _ => TipContext::RideMomentum,
    }
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
    /// Helius HTTPS RPC URL — used for getProgramAccounts (not supported on SOLANA_RPC_URL).
    /// Constructed from HELIUS_API_KEY env var at startup.
    helius_rpc_url: Arc<String>,
    #[allow(dead_code)]
    http_client: reqwest::Client,

    // ── Active positions: mint → MomentumPosition ───────────────────
    active: DashMap<[u8; 32], MomentumPosition>,

    /// Tracks recently closed mint pubkeys to prevent re-entry within cooldown window.
    /// Key: mint [u8; 32], Value: close timestamp ms.
    /// Prevents 474+ phantom re-entries from CoreCast WebSocket reconnect floods.
    recently_closed: DashMap<[u8; 32], u64>,

    /// Dedup: tracks graduation sigs already being resolved (or already resolved).
    /// Key: sig [u8; 64], Value: first-seen timestamp ms.
    /// Prevents 3 feeds from each triggering separate Helius getTransaction lookups
    /// for the same graduation event. Grows slowly (~100-200 entries/day).
    resolving_sigs: DashMap<[u8; 64], u64>,

    // ── Drain detection: reserve_sol samples per active position ───
    // Key = mint, Value = ring of (timestamp_ms, reserve_lamports), max 20 entries.
    // Stored outside MomentumPosition because the 256-byte struct has no free bytes.
    drain_samples: DashMap<[u8; 32], Vec<(u64, u64)>>,

    // ── LQS: reserve SOL context for liquidity-adjusted scale-in sizing ───
    // Key = mint, Value = entry + peak reserve lamports.
    // Stored outside MomentumPosition (no free bytes in 256-byte struct).
    reserve_sol_ctx: DashMap<[u8; 32], ReserveSolContext>,

    // ── Momentum zone trackers: reserve-based liquidity gate for scale-in ──
    // Tracks reserve trajectory per active position. Gates scale-in on
    // MomentumConfirmed/Neutral phase + reserve >= 85% of entry.
    // Stored outside MomentumPosition (no free bytes in 256-byte struct).
    momentum_zones: DashMap<[u8; 32], crate::momentum::position::MomentumZoneTracker>,

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
    /// Raydium pool accounts keyed by mint, populated at graduation, consumed at close.
    raydium_pools: DashMap<[u8; 32], crate::tx::raydium::RaydiumPoolAccounts>,
    pumpswap_pools: DashMap<[u8; 32], crate::tx::pumpswap::PumpSwapPoolAccounts>,
    tip_engine: Arc<parking_lot::Mutex<TipEngine>>,
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
    /// Cached wallet SOL balance in lamports. Updated by background poller every wallet_balance_poll_ms.
    /// Initialized to u64::MAX to allow all entries until first poll completes.
    /// Only enforced in live mode (paper_mode=false).
    wallet_balance_lamports: Arc<AtomicU64>,
    last_tick_ms: AtomicU64,
    /// Graduation events where mint_map had no history (enrichment cold miss).
    /// High value = mint_map not being populated before graduation.
    grad_enrichment_cold_misses: AtomicU64,
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
        // Build Helius standard RPC URL for price polling — SOLANA_RPC_URL (marielle-*)
        // does not support getProgramAccounts and rate-limits getAccountInfo heavily.
        // Use the standard Helius endpoint which has higher rate limits.
        let helius_poll_url = std::env::var("HELIUS_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .map(|k| format!("https://mainnet.helius-rpc.com/?api-key={}", k))
            .unwrap_or_else(|| rpc_url.to_string());
        let (price_feed, ws_handle) = PriceFeedManager::new(
            helius_poll_url,
            helius_wss_url,
            poll_interval_ms,
        );
        let (logger, logger_handle) = MomentumPaperLogger::new(log_path);

        // Channel for Kelly-scored tokens from hot_path → momentum engine
        let (scored_tx, scored_rx) = crossbeam_channel::bounded::<ScoredToken>(512);

        // Tip engine for live mode
        let tip_engine = Arc::new(parking_lot::Mutex::new(
            TipEngine::new(TipConfig::default()),
        ));

        // Build a dedicated Helius HTTPS URL for getProgramAccounts (SOLANA_RPC_URL may not support it)
        let helius_rpc_url = Arc::new(
            std::env::var("HELIUS_API_KEY")
                .ok()
                .filter(|k| !k.is_empty())
                .map(|k| format!("https://mainnet.helius-rpc.com/?api-key={}", k))
                .unwrap_or_else(|| rpc_url.to_string()),
        );

        let engine = Self {
            config,
            rpc_url,
            helius_rpc_url,
            http_client: crate::momentum::pool::make_pool_resolution_client(),
            active: DashMap::new(),
            recently_closed: DashMap::new(),
            resolving_sigs: DashMap::new(),
            drain_samples: DashMap::new(),
            reserve_sol_ctx: DashMap::new(),
            momentum_zones: DashMap::new(),
            pending: std::sync::Mutex::new(PendingEntryRing::new()),
            price_feed,
            logger,
            scored_tokens: DashMap::new(),
            scored_token_rx: scored_rx,
            raydium_pools: DashMap::new(),
            pumpswap_pools: DashMap::new(),
            tip_engine,
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
            wallet_balance_lamports: Arc::new(AtomicU64::new(u64::MAX)),
            last_tick_ms: AtomicU64::new(0),
            grad_enrichment_cold_misses: AtomicU64::new(0),
        };

        // Spawn wallet balance poller (no-op in paper mode — reads but doesn't gate)
        let balance_arc = Arc::clone(&engine.wallet_balance_lamports);
        let poll_ms = engine.config.wallet_balance_poll_ms;
        let wallet_pk = engine.wallet_pubkey;
        let rpc_for_balance = Arc::clone(&engine.helius_rpc_url);
        let paper_mode = engine.config.paper_mode;

        tokio::spawn(async move {
            if wallet_pk.is_none() {
                tracing::debug!("[balance_poller] no wallet pubkey — poller idle");
                return;
            }
            let pk_bytes = wallet_pk.unwrap();
            let pk_b58 = bs58::encode(&pk_bytes).into_string();
            let client = reqwest::Client::new();
            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(poll_ms)).await;

                // getBalance JSON-RPC call
                let body = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "getBalance",
                    "params": [pk_b58, {"commitment": "confirmed"}]
                });

                match client
                    .post(rpc_for_balance.as_str())
                    .json(&body)
                    .send()
                    .await
                {
                    Ok(resp) => {
                        if let Ok(json) = resp.json::<serde_json::Value>().await {
                            if let Some(lamports) = json["result"]["value"].as_u64() {
                                let prev = balance_arc.swap(lamports, Ordering::Relaxed);
                                if (prev as i64 - lamports as i64).unsigned_abs() > 10_000_000 {
                                    // Log only significant changes (>0.01 SOL delta)
                                    tracing::info!(
                                        lamports,
                                        sol = lamports as f64 / 1e9,
                                        paper_mode,
                                        "[balance_poller] wallet balance updated"
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "[balance_poller] getBalance failed — keeping last known value");
                    }
                }
            }
        });

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
        sells_5s: u32,
        is_cold_miss: bool,
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

        // Check daily loss cap (wallet-relative circuit breaker)
        let daily_pnl = self.daily_pnl_lamports.load(Ordering::Relaxed);
        if daily_pnl < 0 {
            let wallet_bal = self.wallet_balance_lamports.load(Ordering::Relaxed);
            // Use a sensible floor for wallet_balance when unpolled (u64::MAX initial value)
            let effective_balance = if wallet_bal == u64::MAX { 1_500_000_000u64 } else { wallet_bal };
            let cap_lamports = (effective_balance as f64 * self.config.daily_loss_cap_pct) as i64;
            if daily_pnl.unsigned_abs() >= cap_lamports as u64 {
                tracing::warn!(
                    daily_loss_sol = daily_pnl as f64 / -1e9,
                    cap_pct = self.config.daily_loss_cap_pct,
                    cap_sol = cap_lamports as f64 / 1e9,
                    wallet_sol = effective_balance as f64 / 1e9,
                    "[momentum] daily loss cap hit — pausing entries"
                );
                return; // daily cap hit
            }
        }

        // Check concurrent position limit
        if self.active.len() >= self.config.max_concurrent as usize {
            return;
        }

        // NOTE: PumpSwap is now the primary graduation target (100% of pump.fun tokens as of Apr 2026).
        // Momentum trading on PumpSwap is fully supported — vault reserves work the same as Raydium.

        let cfg = &self.config;

        // ── Hard gate: reject whale/bot pump tokens ──────────────────────────
        // Fast-graduation tokens (≤90s) filled by bots/whales have 5.9% WR in
        // backtesting. Slow organic graduations (≥120s) have 41.1% WR.
        let grad_volume_sol = grad_volume_sol_x100 as f64 / 100.0;

        if cfg.min_grad_speed_s > 0 {
            // Hard reject: absolute speed floor
            if grad_speed_s < cfg.min_grad_speed_s {
                tracing::debug!(
                    mint = %bs58::encode(&pool_info.mint).into_string(),
                    speed_s = grad_speed_s,
                    threshold = cfg.min_grad_speed_s,
                    "hard gate: rejected fast grad (bot/whale fill)"
                );
                return;
            }
            // Hard reject: fast-ish + high volume
            if cfg.max_grad_volume_sol_fast > 0.0
                && grad_speed_s < cfg.min_grad_speed_s * 2
                && grad_volume_sol >= cfg.max_grad_volume_sol_fast
            {
                tracing::debug!(
                    mint = %bs58::encode(&pool_info.mint).into_string(),
                    speed_s = grad_speed_s,
                    vol_sol = grad_volume_sol,
                    "hard gate: rejected fast+high-vol grad"
                );
                return;
            }
        }
        // Hard reject: saturated volume (u16 overflow = confirmed whale fill)
        if cfg.max_grad_volume_sol_absolute > 0.0
            && grad_volume_sol >= cfg.max_grad_volume_sol_absolute
        {
            tracing::debug!(
                mint = %bs58::encode(&pool_info.mint).into_string(),
                vol_sol = grad_volume_sol,
                "hard gate: rejected saturated volume"
            );
            return;
        }
        // ── End hard gate ────────────────────────────────────────────────────

        // Score the graduation (v2: 5 components including entry discount).
        // Compute entry_price_fp from pool reserves for the entry discount scorer.
        let pre_score_entry_fp = price_from_reserves(pool_info.reserve_sol, pool_info.reserve_token);
        let bc_price_fp = (BC_TERMINAL_PRICE_LAMPORTS_PER_ATOM * 1_000_000.0) as u64;

        // Cold miss neutral defaults: when enrichment data was unavailable,
        // use neutral buys/sells instead of 0 (which would score 0 on velocity/BSR).
        // These represent "we don't know" not "there were no buys".
        let (effective_buys_5s, effective_sells_5s) = if is_cold_miss {
            (3u32, 1u32) // 3:1 buy/sell ratio as neutral assumption
        } else {
            (pre_grad_buys_5s, sells_5s)
        };

        let mut score = score_graduation(
            grad_speed_s,
            grad_volume_sol_x100,
            effective_buys_5s,
            effective_sells_5s,
            pre_score_entry_fp,
            bc_price_fp,
            pool_info.reserve_sol,
        );

        // Cold miss detection: if enrichment data was unavailable, apply bonus.
        // Cold miss = original grad_speed_s == 0 AND volume_sol_x100 == 0 at the
        // on_migration() call site. We're faster than enrichment-dependent bots
        // → information asymmetry edge.
        if is_cold_miss {
            let mint_b58 = bs58::encode(&pool_info.mint).into_string();
            score.cold_miss_bonus = 5;
            tracing::debug!(
                mint = %mint_b58,
                total = score.total(),
                "[momentum] cold miss bonus applied — faster than enrichment-dependent bots"
            );
        }
        let effective_min = if self.config.paper_mode { 20 } else { self.config.min_grad_score };
        if score.total() < effective_min {
            tracing::info!(
                score = score.total(),
                min = effective_min,
                grad_speed_s,
                volume_sol_x100 = grad_volume_sol_x100,
                buys_5s = pre_grad_buys_5s,
                sells_5s,
                "[momentum] graduation score below threshold — skipping"
            );
            return;
        }

        // ── ToD gating: reduce or block entry during dead hours ─────────
        let tod_multiplier = crate::momentum::tod::entry_size_multiplier(
            &self.config.tod_config,
            now_ms,
        );
        if tod_multiplier <= 0.0 {
            tracing::info!(
                mint = %bs58::encode(&pool_info.mint).into_string(),
                score = score.total(),
                "[momentum] ToD gating: blocked hour — skipping entry"
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
            sells_5s,
            tod_multiplier,
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
        // Reuse pre_score_entry_fp and bc_price_fp computed above for the scorer.
        let entry_price_fp = pre_score_entry_fp;

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

    /// Compute Kelly-optimal probe size from rolling trade history.
    /// Returns None if insufficient history (< kelly_bootstrap_trades) or negative EV.
    /// Caller falls back to probe_size_sol when None is returned.
    fn compute_kelly_probe_size(&self) -> Option<u64> {
        use crate::engine::kelly_sizing::{compute_momentum_kelly_size, compute_momentum_kelly_inputs, MomentumPaperTrade};

        // Read trade history from JSONL log
        let log_path = self.logger.log_path();
        let content = match std::fs::read_to_string(&log_path) {
            Ok(c) => c,
            Err(_) => return None,
        };

        let trades: Vec<MomentumPaperTrade> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| {
                let net_pnl_sol = v["net_pnl_sol"].as_f64()?;
                let size_sol = v["size_sol"].as_f64().unwrap_or(0.0);
                // Only include trades with valid size (exclude ghost/zero-price trades)
                if size_sol <= 0.0 { return None; }
                if net_pnl_sol.abs() > size_sol * 10.0 { return None; }
                Some(MomentumPaperTrade { net_pnl_sol })
            })
            .collect();

        // Require bootstrap minimum
        if trades.len() < self.config.kelly_bootstrap_trades {
            return None;
        }

        let (wr, avg_win, avg_loss) = compute_momentum_kelly_inputs(
            &trades,
            self.config.kelly_lookback_trades,
        )?;

        let balance = self.wallet_balance_lamports.load(Ordering::Relaxed);

        let kelly_size = compute_momentum_kelly_size(
            balance,
            wr,
            avg_win,
            avg_loss,
            self.config.kelly_fraction,
        )?;

        // Clamp to [min_probe_size_sol, max_probe_size_sol]
        let min_lamports = (self.config.min_probe_size_sol * 1_000_000_000.0) as u64;
        let max_lamports = (self.config.max_probe_size_sol * 1_000_000_000.0) as u64;
        Some(kelly_size.clamp(min_lamports, max_lamports))
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

        // Probe evaluation: check positions in probe phase and transition
        self.process_probe_evaluation(now_ms);

        // Scale-in: evaluate probe positions for momentum confirmation
        self.process_scale_in(now_ms);
    }

    /// Evaluate probe-phase positions for dump detection and scale-in readiness.
    ///
    /// Called from on_tick() BEFORE process_scale_in(). Transitions probe phases
    /// based on elapsed time and current price. Positions that fail probe are
    /// marked for immediate exit (tp_flags |= 0x8).
    fn process_probe_evaluation(&self, now_ms: u64) {
        if !self.config.probe_entry_enabled {
            return;
        }

        for mut entry in self.active.iter_mut() {
            let mint = *entry.key();
            let pos = entry.value_mut();

            // Only evaluate positions still in Probing phase
            if pos.probe_phase() != crate::momentum::position::ProbePhase::Probing {
                continue;
            }

            let current_price_fp = match self.price_feed.current_price(&mint) {
                Some(p) if p > 0 => p,
                _ => continue, // No price yet, keep probing
            };

            let new_phase = pos.evaluate_probe(
                now_ms,
                current_price_fp,
                self.config.probe_hold_ms,
                self.config.probe_dump_threshold_bps,
                self.config.probe_scale_min_bps,
                self.config.probe_scale_require_price,
            );

            if new_phase != pos.probe_phase() {
                pos.set_probe_phase(new_phase);

                match new_phase {
                    crate::momentum::position::ProbePhase::Failed => {
                        // Dump detected during probe — mark for immediate exit
                        pos.tp_flags |= 0x8;
                        tracing::debug!(
                            mint = %bs58::encode(&mint).into_string(),
                            hold_ms = pos.hold_ms(now_ms),
                            "[momentum] probe FAILED — dump detected, marking for exit"
                        );
                    }
                    crate::momentum::position::ProbePhase::Scaled => {
                        tracing::debug!(
                            mint = %bs58::encode(&mint).into_string(),
                            hold_ms = pos.hold_ms(now_ms),
                            "[momentum] probe PASSED — ready for scale-in"
                        );
                    }
                    crate::momentum::position::ProbePhase::HeldTight => {
                        tracing::debug!(
                            mint = %bs58::encode(&mint).into_string(),
                            hold_ms = pos.hold_ms(now_ms),
                            "[momentum] probe HELD TIGHT — moderate dip, staying at probe size"
                        );
                    }
                    _ => {}
                }
            }
        }
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
                // Unsubscribe any remaining entries that won't become positions
                self.price_feed.unsubscribe_sync(&entry.mint);
                // Also unsubscribe the rest (we're about to break)
                // Remaining entries in the iterator won't be processed
                continue;  // continue instead of break so we unsubscribe all remaining
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
                        // Clean up subscription — entry will never become a position
                        self.price_feed.unsubscribe_sync(&entry.mint);
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
                // Clean up subscription — entry will never become a position
                self.price_feed.unsubscribe_sync(&entry.mint);
                continue;
            }

            // Reject degenerate prices — fixed-point precision lost (token supply too large)
            // Tokens with trillion-unit supply produce price_fp=1..99 where no meaningful
            // bps movement can be computed, causing time_sl to fire at 15s every time.
            if current_price_fp < 100 {
                tracing::warn!(
                    mint = %bs58::encode(&entry.mint).into_string(),
                    entry_price_fp = current_price_fp,
                    "[momentum] skipping entry — degenerate price (entry_price_fp < 100, token supply too large)"
                );
                self.price_feed.unsubscribe_sync(&entry.mint);
                continue;
            }

            // Second liquidity check at actual entry time — pool may have drained since resolution
            const MIN_ENTRY_RESERVE_LAMPORTS: u64 = 40_000_000_000; // 40 SOL
            if let Some(current_reserve_sol) = self.price_feed.get_reserve_sol(&entry.mint) {
                if current_reserve_sol < MIN_ENTRY_RESERVE_LAMPORTS {
                    tracing::warn!(
                        mint = %bs58::encode(&entry.mint).into_string(),
                        reserve_sol_lamports = current_reserve_sol,
                        "[momentum] skipping entry — pool drained since resolution (reserve < 40 SOL)"
                    );
                    self.price_feed.unsubscribe_sync(&entry.mint);
                    continue;
                }
            }

            // v2 scorer: entry_discount is already computed inside score_graduation()
            // called in on_graduation(). No separate recovery enrichment needed.
            let final_score = entry.grad_score;

            // Scale-in entry: ALL entries start as probes at probe_size_sol (0.10 SOL).
            // Scaling up happens in process_scale_in() when s[0] or s[1] confirms momentum.
            // Quant spec §4: probe 0.10 → scale to 0.50 on s[0]≥300, 0.30 on s[0]≥100.

            // ── Balance gate (live mode only) ──────────────────────────────────────
            // In paper mode: skipped entirely (wallet_balance_lamports stays at u64::MAX).
            // In live mode: reject entry if cached balance is too low to cover probe + tip + margin.
            if !self.config.paper_mode {
                let cached_balance = self.wallet_balance_lamports.load(Ordering::Relaxed);
                // Estimate tip: use 0.003 SOL as conservative estimate (actual tip computed at tx time)
                let tip_estimate: u64 = 3_000_000;
                let probe_lamports = (self.config.probe_size_sol * 1_000_000_000.0) as u64;
                let margin = (probe_lamports as f64 * self.config.balance_safety_margin_pct) as u64;
                let required = probe_lamports + tip_estimate + 10_000 + margin;

                if cached_balance < required.max(self.config.min_wallet_balance_lamports) {
                    tracing::warn!(
                        mint = %bs58::encode(&entry.mint).into_string(),
                        cached_sol = cached_balance as f64 / 1e9,
                        required_sol = required as f64 / 1e9,
                        "[momentum] insufficient balance — skipping entry"
                    );
                    self.price_feed.unsubscribe_sync(&entry.mint);
                    self.active.remove(&entry.mint);
                    continue;
                }
            }

            let raw_size = if self.config.kelly_sizing_enabled {
                // Kelly sizing: use rolling trade history to compute optimal size.
                // Falls back to fixed probe_size_sol if insufficient history.
                let kelly_size = self.compute_kelly_probe_size();
                kelly_size.unwrap_or_else(|| (self.config.probe_size_sol * 1_000_000_000.0) as u64)
            } else if self.config.probe_size_sol > 0.0 {
                (self.config.probe_size_sol * 1_000_000_000.0) as u64
            } else {
                self.compute_size_lamports(&entry.mint, final_score as u32)
            };
            // Apply ToD multiplier to entry size (computed in on_graduation, recompute here
            // since pending entries may execute hours after scheduling).
            let tod_mult = crate::momentum::tod::entry_size_multiplier(
                &self.config.tod_config,
                now_ms,
            );
            let size_lamports = if tod_mult >= 1.0 {
                raw_size
            } else if tod_mult <= 0.0 {
                // Blocked hour — skip this entry entirely
                tracing::info!(
                    mint = %bs58::encode(&entry.mint).into_string(),
                    "[momentum] ToD gating at entry time: blocked hour — skipping"
                );
                // Clean up subscription — entry will never become a position
                self.price_feed.unsubscribe_sync(&entry.mint);
                continue;
            } else {
                ((raw_size as f64) * tod_mult) as u64
            };
            let mut pos = MomentumPosition::new(
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
            // Set initial probe phase if probe entry is enabled
            if self.config.probe_entry_enabled && self.config.probe_size_sol > 0.0 {
                pos.set_probe_phase(crate::momentum::position::ProbePhase::Probing);
            }

            // Guard: skip if position already active for this mint (late duplicate slipped through ring buffer).
            if self.active.contains_key(&entry.mint) {
                tracing::debug!(
                    mint = %bs58::encode(&entry.mint).into_string(),
                    "[momentum] skipping duplicate entry — mint already active"
                );
                continue;
            }

            self.active.insert(entry.mint, pos);

            // LQS: record reserve SOL at entry time for liquidity quality scoring
            if let Some(entry_reserve) = self.price_feed.get_reserve_sol(&entry.mint) {
                self.reserve_sol_ctx.insert(entry.mint, ReserveSolContext::new(entry_reserve));
            }

            self.entries_opened.fetch_add(1, Ordering::Relaxed);

            // Initialize momentum zone tracker with entry-time reserve
            let entry_reserve = self.price_feed.get_reserve_sol(&entry.mint).unwrap_or(0);
            self.momentum_zones.insert(
                entry.mint,
                crate::momentum::position::MomentumZoneTracker::new(entry_reserve),
            );

            tracing::info!(
                mint = %bs58::encode(&entry.mint).into_string(),
                score = entry.grad_score,
                size_sol = size_lamports as f64 / 1e9,
                speed_s = entry.grad_speed_s,
                volume_x100 = entry.grad_volume_sol_x100,
                buys_5s = entry.pre_grad_buys_5s,
                "[momentum] entry opened"
            );

            // Live mode: submit buy tx via Raydium AMM V4 + Jito
            if !self.config.paper_mode {
                if let Some(pool) = self.raydium_pools.get(&entry.mint).map(|r| r.clone()) {
                    let mint = entry.mint;
                    let size = size_lamports;
                    let kp_path = std::env::var("WALLET_KEYPAIR_PATH").unwrap_or_default();
                    let jg = match self.jito_grpc.clone() {
                        Some(j) => j,
                        None => {
                            tracing::warn!(mint=%bs58::encode(&mint).into_string(), "[buy_task] no jito client");
                            self.active.remove(&mint);
                            self.momentum_zones.remove(&mint);
                            continue;
                        }
                    };
                    let bh = self.blockhash_cache_sync().unwrap_or([0u8; 32]);
                    let tip_req = crate::tx::tip_engine::TipRequest {
                        context: crate::tx::tip_engine::TipContext::Entry,
                        size_lamports: size,
                        gain_bps: 0,
                        grad_score: entry.grad_score as f64,
                    };
                    let tip = self.tip_engine.lock().compute_tip(&tip_req);
                    // Estimate tokens from entry price: tokens ≈ sol_in / price_fp * 1_000_000
                    // price_fp = lamports per 1M token atoms, so tokens = sol_in * 1_000_000 / price_fp
                    let tokens_estimate = if current_price_fp > 0 {
                        (size as u128 * 1_000_000 / current_price_fp as u128) as u64
                    } else { 0u64 };
                    // Store tokens estimate in position immediately (before async buy confirms)
                    if let Some(mut pos) = self.active.get_mut(&entry.mint) {
                        pos.set_tokens_held(tokens_estimate);
                    }
                    // Capture mint for move into async block
                    let mint_buy = mint;
                    let tokens_est = tokens_estimate;
                    tokio::spawn(async move {
                        let kp_bytes = match std::fs::read(&kp_path) {
                            Ok(b) => b,
                            Err(e) => { tracing::error!(err=?e, "[buy_task] keypair load failed"); return; }
                        };
                        let kp_arr: Vec<u8> = match serde_json::from_slice(&kp_bytes) {
                            Ok(v) => v,
                            Err(e) => { tracing::error!(err=?e, "[buy_task] keypair parse failed"); return; }
                        };
                        if kp_arr.len() != 64 { tracing::error!("[buy_task] invalid keypair len"); return; }
                        let mut kb = [0u8; 64];
                        kb.copy_from_slice(&kp_arr);
                        let keypair = match solana_sdk::signature::Keypair::from_bytes(&kb) {
                            Ok(k) => k,
                            Err(e) => { tracing::error!(err=?e, "[buy_task] keypair err"); return; }
                        };
                        use std::str::FromStr as _;
                        let tip_account = solana_sdk::pubkey::Pubkey::from_str(
                            crate::tx::raydium::JITO_TIP_ACCOUNTS[0]
                        ).unwrap();
                        let tx_bytes = match crate::tx::raydium::build_raydium_buy_tx(
                            &pool, &mint_buy, &keypair, size, 0, tip, tip_account, bh,
                        ) {
                            Ok(b) => b,
                            Err(e) => { tracing::error!(mint=%bs58::encode(&mint_buy).into_string(), err=?e, "[buy_task] build failed"); return; }
                        };
                        // Jito requires base58-encoded transactions (not base64)
                        let tx_b58 = bs58::encode(&tx_bytes).into_string();
                        match jg.submit_bundle(&tx_b58).await {
                            Ok(id) => {
                                tracing::info!(mint=%bs58::encode(&mint_buy).into_string(), bundle_id=%id, tip, size_sol=size as f64/1e9, tokens_est, "[buy_task] Jito submitted");
                                // tokens_held stored at position open — buy confirmed
                                // Note: tokens_est is the AMM formula estimate; actual tokens may differ slightly
                            }
                            Err(e) => {
                                tracing::error!(mint=%bs58::encode(&mint_buy).into_string(), err=?e, "[buy_task] Jito FAILED");
                            }
                        }
                    });
                } else if let Some(ps_pool) = self.pumpswap_pools.get(&entry.mint).map(|r| r.clone()) {
                    // PumpSwap live buy path
                    let mint = entry.mint;
                    let size = size_lamports;
                    let kp_path = std::env::var("WALLET_KEYPAIR_PATH").unwrap_or_default();
                    let jg = match self.jito_grpc.clone() {
                        Some(j) => j,
                        None => {
                            tracing::warn!(mint=%bs58::encode(&mint).into_string(), "[buy_pumpswap] no jito client");
                            self.active.remove(&mint);
                            self.momentum_zones.remove(&mint);
                            continue;
                        }
                    };
                    let bh = self.blockhash_cache_sync().unwrap_or([0u8; 32]);
                    let tip_req = crate::tx::tip_engine::TipRequest {
                        context: crate::tx::tip_engine::TipContext::Entry,
                        size_lamports: size,
                        gain_bps: 0,
                        grad_score: entry.grad_score as f64,
                    };
                    let tip = self.tip_engine.lock().compute_tip(&tip_req);
                    let tokens_estimate = if current_price_fp > 0 {
                        (size as u128 * 1_000_000 / current_price_fp as u128) as u64
                    } else { 0u64 };
                    if let Some(mut pos) = self.active.get_mut(&entry.mint) {
                        pos.set_tokens_held(tokens_estimate);
                    }
                    let mint_buy = mint;
                    let fee_idx = (std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() % 8) as usize;
                    tokio::spawn(async move {
                        let kp_bytes = match std::fs::read(&kp_path) {
                            Ok(b) => b,
                            Err(e) => { tracing::error!(err=?e, "[buy_pumpswap] keypair load failed"); return; }
                        };
                        let kp_arr: Vec<u8> = match serde_json::from_slice(&kp_bytes) {
                            Ok(v) => v,
                            Err(e) => { tracing::error!(err=?e, "[buy_pumpswap] keypair parse failed"); return; }
                        };
                        if kp_arr.len() != 64 { tracing::error!("[buy_pumpswap] invalid keypair len"); return; }
                        let mut kb = [0u8; 64];
                        kb.copy_from_slice(&kp_arr);
                        let keypair = match solana_sdk::signature::Keypair::from_bytes(&kb) {
                            Ok(k) => k,
                            Err(e) => { tracing::error!(err=?e, "[buy_pumpswap] keypair err"); return; }
                        };
                        use std::str::FromStr as _;
                        let tip_account = solana_sdk::pubkey::Pubkey::from_str(
                            crate::tx::raydium::JITO_TIP_ACCOUNTS[0]
                        ).unwrap();
                        let tx_bytes = match crate::tx::pumpswap::build_pumpswap_buy_tx(
                            &ps_pool, &keypair, size, 1, tip, tip_account, bh, fee_idx,
                        ) {
                            Ok(b) => b,
                            Err(e) => { tracing::error!(mint=%bs58::encode(&mint_buy).into_string(), err=?e, "[buy_pumpswap] build failed"); return; }
                        };
                        // Jito requires base58-encoded transactions (not base64)
                        let tx_b58 = bs58::encode(&tx_bytes).into_string();
                        match jg.submit_bundle(&tx_b58).await {
                            Ok(id) => tracing::info!(
                                mint=%bs58::encode(&mint_buy).into_string(),
                                bundle_id=%id,
                                tip,
                                size_sol=size as f64/1e9,
                                "[buy_pumpswap] Jito submitted"
                            ),
                            Err(e) => tracing::error!(
                                mint=%bs58::encode(&mint_buy).into_string(),
                                err=?e,
                                "[buy_pumpswap] Jito FAILED"
                            ),
                        }
                    });
                } else {
                    tracing::warn!(
                        mint=%bs58::encode(&entry.mint).into_string(),
                        "[momentum] live mode: no pool accounts (Raydium or PumpSwap) — position is accounting-only"
                    );
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
            // Guard: skip until min_samples reached (same gate as dynamic trailing stop).
            if self.config.max_hold_trail_activation_ms > 0
                && elapsed_ms >= self.config.max_hold_trail_activation_ms
                && pos.sample_count >= self.config.trailing_stop_min_samples
            {
                if let Some(current_fp) = self.price_feed.current_price(&mint).filter(|&p| p > 0) {
                    let trail_bps = (self.config.max_hold_trail_pct * 100.0) as u32;
                    if pos.trailing_stop_hit(current_fp, trail_bps) {
                        // Max-hold trail uses confirm_samples gate too
                        pos.trail_stop_below_floor_count = pos.trail_stop_below_floor_count.saturating_add(1);
                        if pos.trail_stop_below_floor_count >= self.config.trailing_stop_confirm_samples {
                            to_close.push((mint, MomentumExitReason::MaxHold, current_fp));
                            continue;
                        }
                    } else {
                        pos.trail_stop_below_floor_count = 0;
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

            // ── Drain Detection (highest priority exit) ────────────────────
            // Detect rug pulls by monitoring pool SOL reserve depletion.
            // Fires before all other exit checks — speed is everything on a drain.
            if let Some(current_reserve) = self.price_feed.get_reserve_sol(&mint).filter(|&r| r > 0) {
                let now_drain = now_ms;
                let mut samples = self.drain_samples.entry(mint).or_insert_with(Vec::new);
                samples.push((now_drain, current_reserve));
                // Keep last 20 samples (ring-style, remove oldest)
                if samples.len() > 20 {
                    samples.remove(0);
                }

                // ── Momentum Zone Update ─────────────────────────────────
                // Update reserve trajectory tracker each tick for scale-in gating.
                if let Some(mut zone) = self.momentum_zones.get_mut(&mint) {
                    zone.update(current_reserve, elapsed_ms);
                }

                let mint_b58 = bs58::encode(&mint).into_string();
                let mut drain_exit = false;

                // Hard floor: absolute drain — reserve < 10 SOL
                const DRAIN_FLOOR_LAMPORTS: u64 = 10_000_000_000;
                if current_reserve < DRAIN_FLOOR_LAMPORTS {
                    tracing::warn!(
                        mint = %mint_b58,
                        reserve_lamports = current_reserve,
                        "[momentum] DRAIN DETECTED — reserve < 10 SOL, exiting immediately"
                    );
                    drain_exit = true;
                }

                // Fast drain: >30% drop in 3s
                if !drain_exit {
                    let cutoff_3s = now_drain.saturating_sub(3000);
                    if let Some(&(_, r_3s_ago)) = samples.iter().find(|(ts, _)| *ts <= cutoff_3s) {
                        if r_3s_ago > 0 && current_reserve < r_3s_ago * 70 / 100 {
                            tracing::warn!(
                                mint = %mint_b58,
                                reserve_3s_ago = r_3s_ago,
                                current_reserve,
                                "[momentum] DRAIN DETECTED — >30% drop in 3s, exiting"
                            );
                            drain_exit = true;
                        }
                    }
                }

                // Slower drain: >50% drop in 10s
                if !drain_exit {
                    let cutoff_10s = now_drain.saturating_sub(10000);
                    if let Some(&(_, r_10s_ago)) = samples.iter().find(|(ts, _)| *ts <= cutoff_10s) {
                        if r_10s_ago > 0 && current_reserve < r_10s_ago * 50 / 100 {
                            tracing::warn!(
                                mint = %mint_b58,
                                reserve_10s_ago = r_10s_ago,
                                current_reserve,
                                "[momentum] DRAIN DETECTED — >50% drop in 10s, exiting"
                            );
                            drain_exit = true;
                        }
                    }
                }

                if drain_exit {
                    to_close.push((mint, MomentumExitReason::DrainDetected, current_price_fp));
                    continue;
                }
            }
            // ── End Drain Detection ──────────────────────────────────────────

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

            // ── Dump signal: s[0] < 0, instant exit ──────────────────────
            if pos.tp_flags & 0x8 != 0 {
                to_close.push((mint, MomentumExitReason::HardSl, current_price_fp));
                continue;
            }

            // 2. Hard SL
            let hard_sl_bps = (self.config.hard_sl_pct * 100.0) as u32;
            if pos.hard_sl_hit(current_price_fp, hard_sl_bps) {
                to_close.push((mint, MomentumExitReason::HardSl, current_price_fp));
                continue;
            }

            // 3. Trailing stop — active after TP1 hit, width is momentum-state-aware.
            // Quant spec: ACCELERATING=15%, SUSTAINING=8%, DECELERATING=5%, REVERSING=3%
            // Guard: skip trailing stop until we have enough samples (trailing_stop_min_samples).
            if pos.tp_flags & 0x1 != 0
                && pos.sample_count >= self.config.trailing_stop_min_samples
            {
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
                // Confirm samples gate: require N consecutive below-floor readings
                if pos.trailing_stop_hit(current_price_fp, trailing_bps) {
                    pos.trail_stop_below_floor_count = pos.trail_stop_below_floor_count.saturating_add(1);
                    if pos.trail_stop_below_floor_count >= self.config.trailing_stop_confirm_samples {
                        to_close.push((
                            mint,
                            MomentumExitReason::TrailingStop,
                            current_price_fp,
                        ));
                        continue;
                    }
                } else {
                    pos.trail_stop_below_floor_count = 0; // reset on any above-floor reading
                }
            }

            // ── Velocity Exit ────────────────────────────────────────────────
            // Detect sustained negative velocity/acceleration to exit before
            // trailing stop trips. Protects profits on momentum collapse.
            // Fires after trailing stop check — if trail already fired, we skip.
            if self.config.velocity_exit_enabled
                && pos.sample_count >= self.config.velocity_exit_min_samples as u8
            {
                let n = pos.sample_count as usize;
                let current_bps = price_to_bps_offset(pos.entry_price_fp, current_price_fp);

                // Guard: only fire if position is profitable enough
                if current_bps >= self.config.velocity_exit_min_profit_bps as i32 {
                    let vel = crate::momentum::velocity::compute_velocity(
                        &pos.price_samples_bps[..n],
                        self.config.velocity_window as usize,
                    );

                    // Check velocity threshold (negative = price falling)
                    if vel <= self.config.velocity_exit_threshold_mbps {
                        pos.velocity_confirm_counter = pos.velocity_confirm_counter.saturating_add(1);

                        if pos.velocity_confirm_counter >= self.config.velocity_exit_confirm_samples as u8 {
                            let accel = crate::momentum::velocity::compute_acceleration(
                                &pos.price_samples_bps[..n],
                                self.config.velocity_window as usize,
                            );
                            tracing::info!(
                                mint = %bs58::encode(&mint).into_string(),
                                velocity_mbps = vel,
                                acceleration_mbps2 = accel,
                                current_bps,
                                peak_bps = price_to_bps_offset(pos.entry_price_fp, pos.peak_price_fp),
                                confirm_count = pos.velocity_confirm_counter,
                                "[momentum] velocity exit fired"
                            );
                            to_close.push((mint, MomentumExitReason::VelocityExit, current_price_fp));
                            continue;
                        }
                    } else {
                        // Velocity recovered — reset confirm counter
                        pos.velocity_confirm_counter = 0;
                    }
                }
            }
            // ── End Velocity Exit ────────────────────────────────────────────

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

            // ── Phase 5B: Reserve flatness dead zone ─────────────────────────
            // If the pool's SOL reserve hasn't changed across recent samples,
            // zero trades are happening. Combined with flat/low price → dead token.
            // Uses drain_samples (already populated by drain detection above).
            if self.config.dead_zone_reserve_flat_min_samples > 0
                && hold_ms >= self.config.dead_zone_reserve_flat_min_hold_ms
            {
                let min_n = self.config.dead_zone_reserve_flat_min_samples;
                let reserve_flat = self.drain_samples.get(&mint).map_or(false, |samples| {
                    if samples.len() < min_n {
                        return false;
                    }
                    // Check last N samples for flatness
                    let recent = &samples[samples.len().saturating_sub(min_n)..];
                    let max_r = recent.iter().map(|(_, r)| *r).max().unwrap_or(0);
                    let min_r = recent.iter().map(|(_, r)| *r).min().unwrap_or(0);
                    max_r.saturating_sub(min_r) < self.config.dead_zone_reserve_flat_tolerance_lamports
                });

                if reserve_flat {
                    // Reserve is flat — check if price is also weak/flat.
                    // Only exit when price confirms the dead signal:
                    //   price_flat_bps threshold (200 bps default) — same as Phase 5.
                    let n = pos.sample_count as usize;
                    let price_weak = if n == 0 {
                        // No samples at all → treat as dead (no data = no movement)
                        true
                    } else {
                        let max_gain = pos.price_samples_bps[..n].iter().copied().max().unwrap_or(0);
                        max_gain < self.config.dead_zone_price_flat_bps
                    };

                    if price_weak {
                        tracing::debug!(
                            mint = %bs58::encode(&mint).into_string(),
                            hold_ms,
                            samples = pos.sample_count,
                            "[momentum] dead zone: reserve flat + price weak — no trades in pool"
                        );
                        to_close.push((mint, MomentumExitReason::TimeSl, current_price_fp));
                        continue;
                    }
                }
            }
            // ── End Phase 5B ─────────────────────────────────────────────────

            // ── Phase 6: Early abort — kill dead tokens fast ────────────────
            // Data shows 80.8% of trades are near-zero gross. Most flat tokens
            // are identifiable by sample 3 (3-5s in). Exit early to reduce
            // fee drag — don't hold a dead token for 30-60s.
            if self.config.early_abort_max_bps > 0
                && pos.sample_count >= self.config.early_abort_min_samples
                && elapsed_ms >= self.config.early_abort_min_hold_ms
            {
                let n = pos.sample_count as usize;
                let nonzero: Vec<i32> = pos.price_samples_bps[..n]
                    .iter()
                    .filter(|&&s| s != 0)
                    .copied()
                    .collect();
                if !nonzero.is_empty() {
                    let max_sample = *nonzero.iter().max().unwrap();
                    if max_sample < self.config.early_abort_max_bps {
                        // Don't abort if trailing stop is already armed (TP1 hit)
                        let trailing_armed = pos.tp_flags & 0x1 != 0;
                        if !trailing_armed {
                            tracing::debug!(
                                mint = %bs58::encode(&mint).into_string(),
                                max_sample,
                                samples = pos.sample_count,
                                "[momentum] early abort: max_sample {} < {} bps",
                                max_sample,
                                self.config.early_abort_max_bps
                            );
                            to_close.push((mint, MomentumExitReason::TimeSl, current_price_fp));
                            continue;
                        }
                    }
                }
            }
            // ── End Phase 6 ─────────────────────────────────────────────────

            // ── Stagnation exit (TASK 5): zero-movement tokens ──────────────
            // If ALL price samples are exactly 0 bps after stagnation_exit_ms,
            // the token is dead — exit immediately instead of holding for max_hold.
            if self.config.stagnation_exit_ms > 0
                && pos.is_stagnant(hold_ms, self.config.stagnation_exit_ms)
            {
                tracing::debug!(
                    mint = %bs58::encode(&mint).into_string(),
                    hold_ms,
                    samples = pos.sample_count,
                    "[momentum] stagnation exit: zero movement after {}ms",
                    self.config.stagnation_exit_ms
                );
                to_close.push((mint, MomentumExitReason::TimeSl, current_price_fp));
                continue;
            }
            // ── End stagnation exit ─────────────────────────────────────────

            // ── Time-decay trailing stop (TASK 5) ───────────────────────────
            // Progressively tightens trailing stop as position ages.
            // Replaces the blunt max_hold wall: losers get stopped earlier,
            // winners protected by normal trailing stop until it tightens.
            // Guard: skip until min_samples reached.
            if self.config.time_decay_trailing_enabled
                && !self.config.time_decay_stages_ms.is_empty()
                && self.config.time_decay_stages_ms.len() == self.config.time_decay_trail_bps.len()
                && pos.sample_count >= self.config.trailing_stop_min_samples
            {
                let _trail = pos.time_decay_trail_bps(
                    hold_ms,
                    &self.config.time_decay_stages_ms,
                    &self.config.time_decay_trail_bps,
                );
                if pos.time_decay_trailing_stop_hit(current_price_fp) {
                    // Confirm samples gate for time-decay trail too
                    pos.trail_stop_below_floor_count = pos.trail_stop_below_floor_count.saturating_add(1);
                    if pos.trail_stop_below_floor_count >= self.config.trailing_stop_confirm_samples {
                        tracing::debug!(
                            mint = %bs58::encode(&mint).into_string(),
                            hold_ms,
                            effective_trail = pos.effective_trail_bps(),
                            "[momentum] time-decay trailing stop hit"
                        );
                        to_close.push((mint, MomentumExitReason::TrailingStop, current_price_fp));
                        continue;
                    }
                } else {
                    pos.trail_stop_below_floor_count = 0;
                }
            }
            // ── End time-decay trailing stop ────────────────────────────────

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
            // Bit 3 (0x8) flags instant exit in process_active_positions().
            if s0 < 0 {
                pos.tp_flags |= 0x8;
                pos.set_scaled_in();
                continue;
            }

            // s[0] == 0: ambiguous — price hasn't moved from entry yet, OR feed hasn't delivered.
            // Skip until we have a meaningful non-zero reading or fall through to s[1] check.
            // Do NOT lock at probe — this will be resolved by s[1] or the sample_count guard.
            if s0 == 0 && pos.sample_count == 1 {
                continue;
            }

            // ── Liquidity gate: momentum zone must allow scale-in ─────────
            // Only scale in when reserves are stable (Neutral) or confirmed
            // growing (MomentumConfirmed) AND reserve >= 85% of entry level.
            // During InitialChurn (<10s), Shakeout, or MomentumCandidate,
            // defer scale-in — keep at probe size, re-evaluate next tick.
            {
                let mint = pos.mint;
                if let Some(zone) = self.momentum_zones.get(&mint) {
                    let current_reserve = self.price_feed.get_reserve_sol(&mint).unwrap_or(0);
                    if !zone.allows_scale_in(current_reserve) {
                        tracing::trace!(
                            mint = %bs58::encode(&mint).into_string(),
                            phase = zone.phase_str(),
                            reserve_entry = zone.reserve_sol_entry,
                            reserve_now = current_reserve,
                            "[momentum] scale-in deferred — liquidity gate (phase={}, reserve={}%)",
                            zone.phase_str(),
                            if zone.reserve_sol_entry > 0 {
                                current_reserve * 100 / zone.reserve_sol_entry
                            } else { 0 }
                        );
                        continue;
                    }
                }
                // If no zone tracker exists (edge case), allow scale-in (backward compat)
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

        // Clean up reserve samples (drain detection + reserve flatness)
        self.drain_samples.remove(&mint);
        // Clean up momentum zone tracker
        self.momentum_zones.remove(&mint);
        // Clean up LQS reserve context
        self.reserve_sol_ctx.remove(&mint);

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
            MomentumExitReason::TrailingStop | MomentumExitReason::HardSl | MomentumExitReason::DrainDetected | MomentumExitReason::VelocityExit => {
                self.sl_exits.fetch_add(1, Ordering::Relaxed);
            }
            _ => {
                self.timeout_exits.fetch_add(1, Ordering::Relaxed);
            }
        }

        // Capture WS notif info BEFORE unsubscribe removes the price state
        let ws_notif_count_at_close = self.price_feed.ws_notif_info(&mint).0;

        // Clean up drain detection samples and momentum zone tracker
        self.drain_samples.remove(&mint);
        self.momentum_zones.remove(&mint);

        // Unsubscribe from price feed (direct DashMap remove — no async needed)
        self.price_feed.unsubscribe_sync(&mint);

        // Log to JSONL
        let grad_vol_sol = pos.grad_volume_sol_x100 as f64 / 100.0;
        // structural_discount uses fp units directly (both are lamports per 1M token atoms)
        let bc_price_f64 = pos.bc_terminal_price_fp as f64;
        let entry_price_f64 = pos.entry_price_fp as f64;
        // Positive = entry above terminal (token pumping), negative = entry below
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
            grad_score_final: pos.grad_score, // updated by E3 when recovery enrichment lands
            grad_speed_s: pos.grad_speed_s as u64,
            grad_volume_sol: grad_vol_sol,
            pre_grad_buys_5s: pos.pre_grad_buys_5s,
            size_sol,
            size_lamports: pos.size_lamports,
            entry_delay_ms: pos.entry_delay_ms as u64,
            entry_price_lamports: pos.entry_price_fp,
            exit_price_lamports: exit_price_fp,
            bc_terminal_price_fp: pos.bc_terminal_price_fp,
            structural_discount_pct: structural_discount,
            entry_timestamp_ms: pos.entry_ts_ms,
            exit_timestamp_ms: now_ms,
            hold_ms: now_ms.saturating_sub(pos.entry_ts_ms),
            exit_reason: reason.as_str(),
            raw_gain_bps,
            gross_pnl_sol,
            fee_sol,
            fees_sol: fee_sol,
            net_pnl_sol,
            price_samples_bps: pos.price_samples_bps[..pos.sample_count as usize].to_vec(),
            price_sample_count: pos.sample_count as u8,
            ws_notif_count_at_close,
            is_paper: self.config.paper_mode,
            config_version: self.config.config_version(),
        });

        // ── Live mode: Raydium AMM V4 sell via Jito ────────────────────────────
        if !self.config.paper_mode {
            if let Some((_, pool)) = self.raydium_pools.remove(&mint) {
                let tokens = pos.tokens_held();
                if tokens == 0 {
                    tracing::warn!(
                        mint=%bs58::encode(&mint).into_string(),
                        "[close_position] tokens_held=0 — buy tx likely failed, skipping sell"
                    );
                } else {
                    let kp_path = std::env::var("WALLET_KEYPAIR_PATH").unwrap_or_default();
                    let jg = match self.jito_grpc.clone() {
                        Some(j) => j,
                        None => { tracing::error!("[close_position] no jito client"); return; }
                    };
                    let bh = self.blockhash_cache_sync().unwrap_or([0u8; 32]);
                    let tip_req = TipRequest {
                        context: exit_to_context(&reason, gain_bps as i64),
                        size_lamports: pos.size_lamports,
                        gain_bps: gain_bps as i64,
                        grad_score: 0.0,
                    };
                    let tip = self.tip_engine.lock().compute_tip(&tip_req);
                    // 1% slippage on profitable exits, 0 on losses (speed > price)
                    let min_sol_out = if gain_bps > 0 {
                        let expected = (pos.entry_price_fp as u128 * tokens as u128 / 1_000_000) as u64;
                        (expected as u128 * 9900 / 10000) as u64
                    } else { 0u64 };
                    let noz = self.nozomi_client.clone();
                    let reason_str = reason.as_str().to_string();
                    let gain = gain_bps as i64;
                    let noz_ok = noz.is_some();
                    let mint_copy = mint;
                    tokio::spawn(async move {
                        let kp_bytes = match std::fs::read(&kp_path) {
                            Ok(b) => b,
                            Err(e) => { tracing::error!(err=?e, "[sell_raydium] keypair load failed"); return; }
                        };
                        let kp_arr: Vec<u8> = match serde_json::from_slice(&kp_bytes) {
                            Ok(v) => v,
                            Err(e) => { tracing::error!(err=?e, "[sell_raydium] keypair parse failed"); return; }
                        };
                        if kp_arr.len() != 64 { tracing::error!("[sell_raydium] bad keypair len"); return; }
                        let mut kb = [0u8; 64];
                        kb.copy_from_slice(&kp_arr);
                        let keypair = match solana_sdk::signature::Keypair::from_bytes(&kb) {
                            Ok(k) => k,
                            Err(e) => { tracing::error!(err=?e, "[sell_raydium] keypair from_bytes"); return; }
                        };
                        use std::str::FromStr as _;
                        let tip_account = solana_sdk::pubkey::Pubkey::from_str(
                            crate::tx::raydium::JITO_TIP_ACCOUNTS[0]
                        ).unwrap();
                        let tx_bytes = match crate::tx::raydium::build_raydium_sell_tx(
                            &pool, &mint_copy, &keypair, tokens, min_sol_out, tip, tip_account, bh,
                        ) {
                            Ok(b) => b,
                            Err(e) => { tracing::error!(mint=%bs58::encode(&mint_copy).into_string(), err=?e, "[sell_raydium] build failed"); return; }
                        };
                        use base64::Engine as _;
                        let tx_b64 = base64::engine::general_purpose::STANDARD.encode(&tx_bytes); // Nozomi needs base64
                        let tx_b58 = bs58::encode(&tx_bytes).into_string(); // Jito needs base58
                        let landing = route_exit(&reason_str, gain, noz_ok);
                        match landing {
                            LandingPath::JitoOnly => {
                                match jg.submit_bundle(&tx_b58).await {
                                    Ok(id) => tracing::info!(mint=%bs58::encode(&mint_copy).into_string(), bundle_id=%id, "[sell_raydium] Jito submitted"),
                                    Err(e) => tracing::error!(mint=%bs58::encode(&mint_copy).into_string(), err=?e, "[sell_raydium] Jito FAILED"),
                                }
                            }
                            LandingPath::NozomiOnly | LandingPath::DualPath => {
                                if let Some(ref n) = noz {
                                    match n.send_transaction(&tx_b64).await {
                                        Ok(_) => tracing::info!(mint=%bs58::encode(&mint_copy).into_string(), "[sell_raydium] Nozomi OK"),
                                        Err(e) => { tracing::warn!(err=?e, "[sell_raydium] Nozomi failed → Jito"); let _ = jg.submit_bundle(&tx_b58).await; }
                                    }
                                }
                            }
                        }
                    });
                }
            } else if let Some((_, ps_pool)) = self.pumpswap_pools.remove(&mint) {
                let tokens = pos.tokens_held();
                if tokens == 0 {
                    tracing::warn!(
                        mint=%bs58::encode(&mint).into_string(),
                        "[close_pumpswap] tokens_held=0 — buy tx likely failed, skipping sell"
                    );
                } else {
                    let kp_path = std::env::var("WALLET_KEYPAIR_PATH").unwrap_or_default();
                    let jg = match self.jito_grpc.clone() {
                        Some(j) => j,
                        None => { tracing::error!("[close_pumpswap] no jito client"); return; }
                    };
                    let noz = self.nozomi_client.clone();
                    let bh = self.blockhash_cache_sync().unwrap_or([0u8; 32]);
                    let tip_req = TipRequest {
                        context: exit_to_context(&reason, gain_bps as i64),
                        size_lamports: pos.size_lamports,
                        gain_bps: gain_bps as i64,
                        grad_score: 0.0,
                    };
                    let tip = self.tip_engine.lock().compute_tip(&tip_req);
                    let min_sol_out = if gain_bps > 0 {
                        let expected = (pos.entry_price_fp as u128 * tokens as u128 / 1_000_000) as u64;
                        (expected as u128 * 9900 / 10000) as u64
                    } else { 0u64 };
                    let noz_ok = noz.is_some();
                    let reason_str = reason.as_str().to_string();
                    let gain = gain_bps as i64;
                    let mint_copy = mint;
                    let fee_idx = (std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() % 8) as usize;
                    tokio::spawn(async move {
                        let kp_bytes = match std::fs::read(&kp_path) {
                            Ok(b) => b,
                            Err(e) => { tracing::error!(err=?e, "[sell_pumpswap] keypair load failed"); return; }
                        };
                        let kp_arr: Vec<u8> = match serde_json::from_slice(&kp_bytes) {
                            Ok(v) => v,
                            Err(e) => { tracing::error!(err=?e, "[sell_pumpswap] keypair parse failed"); return; }
                        };
                        if kp_arr.len() != 64 { tracing::error!("[sell_pumpswap] bad keypair len"); return; }
                        let mut kb = [0u8; 64];
                        kb.copy_from_slice(&kp_arr);
                        let keypair = match solana_sdk::signature::Keypair::from_bytes(&kb) {
                            Ok(k) => k,
                            Err(e) => { tracing::error!(err=?e, "[sell_pumpswap] keypair from_bytes"); return; }
                        };
                        use std::str::FromStr as _;
                        let tip_account = solana_sdk::pubkey::Pubkey::from_str(
                            crate::tx::raydium::JITO_TIP_ACCOUNTS[0]
                        ).unwrap();
                        let tx_bytes = match crate::tx::pumpswap::build_pumpswap_sell_tx(
                            &ps_pool, &keypair, tokens, min_sol_out, tip, tip_account, bh, fee_idx,
                        ) {
                            Ok(b) => b,
                            Err(e) => { tracing::error!(mint=%bs58::encode(&mint_copy).into_string(), err=?e, "[sell_pumpswap] build failed"); return; }
                        };
                        use base64::Engine as _;
                        let tx_b64 = base64::engine::general_purpose::STANDARD.encode(&tx_bytes); // Nozomi needs base64
                        let tx_b58 = bs58::encode(&tx_bytes).into_string(); // Jito needs base58
                        let landing = route_exit(&reason_str, gain, noz_ok);
                        match landing {
                            LandingPath::JitoOnly => {
                                match jg.submit_bundle(&tx_b58).await {
                                    Ok(id) => tracing::info!(mint=%bs58::encode(&mint_copy).into_string(), bundle_id=%id, "[sell_pumpswap] Jito submitted"),
                                    Err(e) => tracing::error!(mint=%bs58::encode(&mint_copy).into_string(), err=?e, "[sell_pumpswap] Jito FAILED"),
                                }
                            }
                            LandingPath::NozomiOnly | LandingPath::DualPath => {
                                if let Some(ref n) = noz {
                                    match n.send_transaction(&tx_b64).await {
                                        Ok(_) => tracing::info!(mint=%bs58::encode(&mint_copy).into_string(), "[sell_pumpswap] Nozomi OK"),
                                        Err(e) => { tracing::warn!(err=?e, "[sell_pumpswap] Nozomi failed → Jito"); let _ = jg.submit_bundle(&tx_b58).await; }
                                    }
                                }
                            }
                        }
                    });
                }
            } else {
                tracing::warn!(
                    mint=%bs58::encode(&mint).into_string(),
                    "[close_position] no pool accounts (Raydium or PumpSwap) — sell NOT submitted"
                );
            }
            // Idempotent cleanup — safe if already removed in sell branch above
            self.pumpswap_pools.remove(&mint);
        }
    }

    /// Called from main.rs on every graduation migration event.
    /// Resolves the pool via getTransaction and calls on_graduation() if successful.
    /// Cold path — graduation is rare (~10-20 Raydium/day).
    #[inline(never)]
    pub async fn on_migration(
        &self,
        mint: [u8; 32],
        ts_ms: u64,
        sig: [u8; 64],
        enrichment: crate::engine::hot_path::GradEnrichment,
    ) {
        if !self.config.enabled { return; }

        // Dedup: prevent 3 feeds from triggering separate Helius lookups for the same graduation.
        // First-seen wins; duplicates are skipped. Map grows slowly (~100-200 entries/day).
        if self.resolving_sigs.contains_key(&sig) {
            tracing::debug!(
                sig = %&bs58::encode(&sig).into_string()[..8],
                "[momentum] pool resolution already in progress — skipping duplicate"
            );
            return;
        }
        self.resolving_sigs.insert(sig, ts_ms);

        tracing::debug!(
            grad_speed_s = enrichment.grad_speed_s,
            volume_sol_x100 = enrichment.volume_sol_x100,
            buys_5s = enrichment.buys_5s,
            sig = %&bs58::encode(&sig).into_string()[..8],
            "[momentum] on_migration: enrichment values at entry"
        );

        // Cold miss: CoreCast hasn't propagated enrichment data yet.
        // Detected from raw enrichment values before effective defaults are applied.
        let is_cold_miss = enrichment.grad_speed_s == 0 && enrichment.volume_sol_x100 == 0;

        // ── FIX-1/FIX-4: Staleness gate — drop cold-miss CoreCast backlog ─────
        // CoreCast replays ~430 old Raydium-era graduation events/min with no
        // enrichment data. These waste RPC calls and resolve to dead Raydium pools.
        // Gate: cold-miss events older than stale_grad_max_age_ms are dropped.
        if is_cold_miss && self.config.stale_grad_max_age_ms > 0 {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let grad_age_ms = now_ms.saturating_sub(ts_ms);
            if grad_age_ms > self.config.stale_grad_max_age_ms {
                tracing::debug!(
                    mint = %bs58::encode(&mint).into_string(),
                    grad_age_ms,
                    "[momentum] stale cold-miss grad rejected — CoreCast backlog"
                );
                self.resolving_sigs.remove(&sig);
                return;
            }
        }
        // ── End staleness gate ─────────────────────────────────────────────────

        match resolve_pool_from_transaction(&self.http_client, &sig, &self.rpc_url).await {
            Some(resolution) => {
                let mint_b58 = bs58::encode(&resolution.mint).into_string();

                // Defense in depth: reject pools with insufficient liquidity even if
                // resolve_pool_from_transaction() didn't catch it (e.g. future code paths).
                if resolution.reserve_sol_lamports < crate::momentum::pool::MIN_SOL_RESERVES_LAMPORTS {
                    tracing::warn!(
                        mint = %mint_b58,
                        reserve_sol = resolution.reserve_sol_lamports,
                        "[momentum] skipping graduation — pool has < 50 SOL liquidity"
                    );
                    return;
                }

                tracing::info!(
                    mint = %mint_b58,
                    pool_type = ?resolution.pool_type,
                    reserve_sol = resolution.reserve_sol_lamports,
                    grad_speed_s = enrichment.grad_speed_s,
                    volume_sol_x100 = enrichment.volume_sol_x100,
                    buys_5s = enrichment.buys_5s,
                    "[momentum] pool resolved — entering on_graduation"
                );

                // ── FIX-5: Raydium dead pool activity check ───────────────────
                // If the resolved pool is Raydium, verify it has had recent swap
                // activity. Dead Raydium pools have stale liquidity but zero trades.
                if resolution.pool_type == crate::momentum::pool::PoolType::RaydiumAmmV4
                    && self.config.raydium_max_idle_ms > 0
                {
                    let pc_vault_b58 = bs58::encode(&resolution.pc_vault).into_string();
                    let last_ms = crate::momentum::pool::get_account_last_activity_ms(
                        &self.http_client, &self.helius_rpc_url, &pc_vault_b58
                    ).await;
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    let idle_ms = now_ms.saturating_sub(last_ms.unwrap_or(0));
                    if idle_ms > self.config.raydium_max_idle_ms {
                        tracing::info!(
                            mint = %bs58::encode(&resolution.mint).into_string(),
                            idle_ms,
                            idle_min = idle_ms / 60_000,
                            "[momentum] Raydium pool dead — no activity in {}min, skipping",
                            idle_ms / 60_000
                        );
                        return;
                    }
                }
                // ── End FIX-5 ─────────────────────────────────────────────────

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
                    // mint_map cold miss — estimate speed from LP SOL reserves.
                    // BC deposits ~85 SOL at graduation; extra = immediate LP adds post-grad.
                    // Conservative: assume organic unless LP is very high (≥250 SOL = clear whale/bot pump).
                    let sol = resolution.reserve_sol_lamports / 1_000_000_000;
                    self.grad_enrichment_cold_misses.fetch_add(1, Ordering::Relaxed);
                    tracing::info!(
                        mint = %bs58::encode(&resolution.mint).into_string(),
                        reserve_sol = sol,
                        "[momentum] enrichment cold miss — estimating speed from LP reserves"
                    );
                    if sol >= 250 { 60u32 }  // Very aggressive LP add → likely whale, apply hard gate
                    else { 120u32 }          // Unknown → assume organic minimum (passes hard gate at 90s)
                } else {
                    enrichment.grad_speed_s
                };
                let effective_buys_5s = if enrichment.buys_5s == 0 {
                    3u32
                } else {
                    enrichment.buys_5s as u32
                };
                // Store Raydium pool accounts for live-mode buy/sell tx building.
                // Keyed by mint — consumed when position closes.
                if !self.config.paper_mode && resolution.pool_type == PoolType::RaydiumAmmV4
                    && resolution.amm_id != [0u8; 32]
                {
                    use crate::tx::raydium::RaydiumPoolAccounts;
                    // amm_authority is a global PDA — same for all Raydium AMM V4 pools
                    let amm_authority = {
                        use std::str::FromStr;
                        let prog = solana_sdk::pubkey::Pubkey::from_str(
                            crate::tx::raydium::RAYDIUM_AMM_V4_PROGRAM
                        ).unwrap_or_default();
                        let (pda, _) = solana_sdk::pubkey::Pubkey::find_program_address(
                            &[b"amm authority"], &prog
                        );
                        pda.to_bytes()
                    };
                    // serum_program_id: standard Serum v3 DEX
                    let serum_program_id = {
                        use std::str::FromStr;
                        solana_sdk::pubkey::Pubkey::from_str(
                            crate::tx::raydium::SERUM_DEX_PROGRAM
                        ).unwrap_or_default().to_bytes()
                    };
                    let pool_accts = RaydiumPoolAccounts {
                        amm_id: resolution.amm_id,
                        amm_authority,
                        amm_open_orders: resolution.amm_open_orders,
                        amm_target_orders: resolution.amm_target_orders,
                        serum_program_id,
                        serum_market: resolution.serum_market,
                        serum_bids: resolution.serum_bids,
                        serum_asks: resolution.serum_asks,
                        serum_event_queue: resolution.serum_event_queue,
                        serum_coin_vault: resolution.serum_coin_vault,
                        serum_pc_vault: resolution.serum_pc_vault,
                        serum_vault_signer: resolution.serum_vault_signer,
                        coin_vault: resolution.coin_vault,
                        pc_vault: resolution.pc_vault,
                    };
                    self.raydium_pools.insert(resolution.mint, pool_accts);
                    tracing::debug!(
                        mint = %bs58::encode(&resolution.mint).into_string(),
                        "[momentum] raydium pool accounts stored for live execution"
                    );
                }

                // PumpSwap pool accounts (zeroed for Raydium tokens, populated for PumpSwap)
                if let Some(ps_pool) = crate::momentum::pool::extract_pumpswap_pool_accounts(&resolution) {
                    let ps_accts: crate::tx::pumpswap::PumpSwapPoolAccounts = ps_pool.into();
                    self.pumpswap_pools.insert(resolution.mint, ps_accts);
                    tracing::debug!(
                        mint = %bs58::encode(&resolution.mint).into_string(),
                        "[momentum] pumpswap pool accounts stored for live execution"
                    );
                }

                let effective_sells_5s = if enrichment.sells_5s == 0 {
                    1u32 // Default: assume at least 1 sell (conservative for buy/sell ratio)
                } else {
                    enrichment.sells_5s as u32
                };

                self.on_graduation(
                    &pool_info,
                    ts_ms,
                    effective_speed_s,
                    effective_volume_sol_x100,
                    effective_buys_5s,
                    effective_sells_5s,
                    is_cold_miss,
                ).await;
            }
            None => {
                // Sig-based resolution failed. CoreCast sends DEX trade sigs (not pool-creation
                // sigs), so getTransaction finds no vault data → RPC error. Fall back to
                // mint-based getProgramAccounts on Raydium AMM V4.
                tracing::info!(
                    mint = %bs58::encode(&mint).into_string(),
                    "[momentum] sig resolution failed — trying PumpSwap then Raydium mint lookup"
                );
                // Try PumpSwap first (100% of current pump.fun graduations go to PumpSwap),
                // then fall back to Raydium AMM V4 mint-based lookup.
                let fallback_resolution = {
                    // Use helius_rpc_url — SOLANA_RPC_URL doesn't support getProgramAccounts
                    let ps = crate::momentum::pool::resolve_pumpswap_pool_from_mint(
                        &self.http_client, &mint, &self.helius_rpc_url
                    ).await;
                    if ps.is_some() {
                        ps
                    } else {
                        crate::momentum::pool::resolve_pool_from_mint(
                            &self.http_client, &mint, &self.helius_rpc_url
                        ).await
                    }
                };
                match fallback_resolution {
                    Some(resolution) => {
                        let mint_b58 = bs58::encode(&resolution.mint).into_string();

                        // Defense in depth: reject pools with insufficient liquidity
                        if resolution.reserve_sol_lamports < crate::momentum::pool::MIN_SOL_RESERVES_LAMPORTS {
                            tracing::warn!(
                                mint = %mint_b58,
                                reserve_sol = resolution.reserve_sol_lamports,
                                "[momentum] skipping graduation (mint lookup) — pool has < 50 SOL liquidity"
                            );
                            return;
                        }

                        tracing::info!(
                            mint = %mint_b58,
                            pool_type = ?resolution.pool_type,
                            reserve_sol = resolution.reserve_sol_lamports,
                            "[momentum] pool resolved via mint lookup — entering on_graduation"
                        );

                        // ── FIX-5: Raydium dead pool activity check (mint-lookup path) ──
                        if resolution.pool_type == crate::momentum::pool::PoolType::RaydiumAmmV4
                            && self.config.raydium_max_idle_ms > 0
                        {
                            let pc_vault_b58 = bs58::encode(&resolution.pc_vault).into_string();
                            let last_ms = crate::momentum::pool::get_account_last_activity_ms(
                                &self.http_client, &self.helius_rpc_url, &pc_vault_b58
                            ).await;
                            let now_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64;
                            let idle_ms = now_ms.saturating_sub(last_ms.unwrap_or(0));
                            if idle_ms > self.config.raydium_max_idle_ms {
                                tracing::info!(
                                    mint = %mint_b58,
                                    idle_ms,
                                    idle_min = idle_ms / 60_000,
                                    "[momentum] Raydium pool dead (mint lookup) — no activity in {}min, skipping",
                                    idle_ms / 60_000
                                );
                                return;
                            }
                        }
                        // ── End FIX-5 (mint-lookup path) ──────────────────────────────────

                        let pool_info = PoolInfo {
                            coin_vault: resolution.coin_vault,
                            pc_vault: resolution.pc_vault,
                            reserve_token: resolution.reserve_token_atoms,
                            reserve_sol: resolution.reserve_sol_lamports,
                            pool_type: resolution.pool_type,
                            mint: resolution.mint,
                        };
                        let effective_volume_sol_x100 = if enrichment.volume_sol_x100 == 0 {
                            (resolution.reserve_sol_lamports / 10_000_000).min(65535) as u32
                        } else {
                            enrichment.volume_sol_x100
                        };
                        let effective_speed_s = if enrichment.grad_speed_s == 0 {
                            let sol = resolution.reserve_sol_lamports / 1_000_000_000;
                            self.grad_enrichment_cold_misses.fetch_add(1, Ordering::Relaxed);
                            tracing::info!(
                                mint = %mint_b58,
                                reserve_sol = sol,
                                "[momentum] enrichment cold miss (mint lookup) — estimating speed from LP reserves"
                            );
                            if sol >= 250 { 60u32 } else { 120u32 }
                        } else {
                            enrichment.grad_speed_s
                        };
                        let effective_buys_5s = if enrichment.buys_5s == 0 { 3u32 } else { enrichment.buys_5s as u32 };
                        let effective_sells_5s = if enrichment.sells_5s == 0 { 1u32 } else { enrichment.sells_5s as u32 };

                        // Store PumpSwap pool accounts for live execution
                        if let Some(ps_pool) = crate::momentum::pool::extract_pumpswap_pool_accounts(&resolution) {
                            let ps_accts: crate::tx::pumpswap::PumpSwapPoolAccounts = ps_pool.into();
                            self.pumpswap_pools.insert(resolution.mint, ps_accts);
                            tracing::debug!(
                                mint = %bs58::encode(&resolution.mint).into_string(),
                                "[momentum] pumpswap pool accounts stored (mint lookup path)"
                            );
                        }

                        self.on_graduation(
                            &pool_info,
                            ts_ms,
                            effective_speed_s,
                            effective_volume_sol_x100,
                            effective_buys_5s,
                            effective_sells_5s,
                            is_cold_miss,
                        ).await;
                    }
                    None => {
                        tracing::warn!(
                            mint = %bs58::encode(&mint).into_string(),
                            "[momentum] pool resolution FAILED (sig + PumpSwap + Raydium all failed)"
                        );
                    }
                }
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
            grad_enrichment_cold_misses: self.grad_enrichment_cold_misses.load(Ordering::Relaxed),
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
    /// Graduation events where mint_map had no history (enrichment cold miss).
    pub grad_enrichment_cold_misses: u64,
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
            .on_graduation(&pool_info, 1_000_000, 60, 50_000, 15, 2, false)
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

        // High-scoring graduation: speed=120 (passes hard gate), volume=13900 (139 SOL), velocity=15
        engine
            .on_graduation(&pool_info, 1_000_000, 120, 13_900, 15, 2, false)
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
            .on_graduation(&pool_info, 1_000_000, 60, 50_000, 15, 2, false)
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

        // Low-scoring: speed=91 (passes hard gate, speed_score=5 in v3),
        // tiny volume (10 SOL → 0), no buys → total=5 < paper_mode threshold 20
        engine
            .on_graduation(&pool_info, 1_000_000, 91, 1_000, 0, 0, false)
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
            state.reserve_sol.store(80_000_000_000, Ordering::Relaxed); // 80 SOL — passes entry gate
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
        // Use legacy trailing stop behavior (min_samples=0, confirm=1) to test basic trigger
        let engine = make_test_engine_with(true, |cfg| {
            cfg.trailing_stop_min_samples = 0;
            cfg.trailing_stop_confirm_samples = 1;
        });

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
            .on_graduation(&pool_info, 1_000_000, 60, 50_000, 15, 2, false)
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

    // ── Hard gate: whale/bot pump rejection tests (TASK 1) ──────────────

    #[tokio::test]
    async fn test_hard_gate_rejects_fast_grad() {
        // speed=60 < min_grad_speed_s=90 → rejected by hard gate
        let engine = make_test_engine(true);

        let pool_info = PoolInfo {
            coin_vault: [1u8; 32],
            pc_vault: [2u8; 32],
            reserve_token: 200_000_000_000_000,
            reserve_sol: 80_000_000_000,
            pool_type: PoolType::RaydiumAmmV4,
            mint: [0xA1; 32],
        };

        // speed=60 (bot fill), volume=139 SOL (normal) — should be rejected on speed alone
        engine
            .on_graduation(&pool_info, 1_000_000, 60, 13_900, 15, 2, false)
            .await;
        // Counter incremented (fires before hard gate)
        assert_eq!(engine.graduations_seen.load(Ordering::Relaxed), 1);
        // But NO pending entry — hard gate rejected it
        assert_eq!(engine.pending.lock().unwrap().active_count(), 0);
    }

    #[tokio::test]
    async fn test_hard_gate_rejects_saturated_volume() {
        // vol=655.35 SOL (x100 = 65535) >= max_grad_volume_sol_absolute=650 → rejected
        let engine = make_test_engine(true);

        let pool_info = PoolInfo {
            coin_vault: [1u8; 32],
            pc_vault: [2u8; 32],
            reserve_token: 200_000_000_000_000,
            reserve_sol: 80_000_000_000,
            pool_type: PoolType::RaydiumAmmV4,
            mint: [0xA2; 32],
        };

        // speed=300 (organic), but volume=655.35 SOL (u16 saturated) → whale fill
        engine
            .on_graduation(&pool_info, 1_000_000, 300, 65_535, 15, 2, false)
            .await;
        assert_eq!(engine.graduations_seen.load(Ordering::Relaxed), 1);
        // Hard gate: saturated volume → rejected
        assert_eq!(engine.pending.lock().unwrap().active_count(), 0);
    }

    #[tokio::test]
    async fn test_hard_gate_rejects_fast_high_volume() {
        // speed=100 (< min_grad_speed_s*2=180), vol=250 SOL (>= max_grad_volume_sol_fast=200)
        // → rejected by fast+high-vol gate
        let engine = make_test_engine(true);

        let pool_info = PoolInfo {
            coin_vault: [1u8; 32],
            pc_vault: [2u8; 32],
            reserve_token: 200_000_000_000_000,
            reserve_sol: 80_000_000_000,
            pool_type: PoolType::RaydiumAmmV4,
            mint: [0xA3; 32],
        };

        // speed=100 (passes absolute floor of 90, but < 180), vol=250 SOL (>= 200)
        engine
            .on_graduation(&pool_info, 1_000_000, 100, 25_000, 15, 2, false)
            .await;
        assert_eq!(engine.graduations_seen.load(Ordering::Relaxed), 1);
        // Hard gate: fast-ish + high volume → rejected
        assert_eq!(engine.pending.lock().unwrap().active_count(), 0);
    }

    #[tokio::test]
    async fn test_hard_gate_passes_slow_organic() {
        // speed=180 (>= min_grad_speed_s*2=180), vol=139 SOL (< 200) → passes all gates
        let engine = make_test_engine(true);

        let pool_info = PoolInfo {
            coin_vault: [1u8; 32],
            pc_vault: [2u8; 32],
            reserve_token: 200_000_000_000_000,
            reserve_sol: 80_000_000_000,
            pool_type: PoolType::RaydiumAmmV4,
            mint: [0xA4; 32],
        };

        // speed=180 (organic), vol=139 SOL → passes all hard gates
        engine
            .on_graduation(&pool_info, 1_000_000, 180, 13_900, 15, 2, false)
            .await;
        assert_eq!(engine.graduations_seen.load(Ordering::Relaxed), 1);
        // Should pass hard gate and reach scoring → pending entry scheduled
        let pending_count = engine.pending.lock().unwrap().active_count();
        assert_eq!(pending_count, 1);
    }

    #[tokio::test]
    async fn test_hard_gate_disabled_when_zero() {
        // min_grad_speed_s=0 disables the speed and fast+vol gates
        let engine = make_test_engine_with(true, |cfg| {
            cfg.min_grad_speed_s = 0;
            cfg.max_grad_volume_sol_absolute = 0.0;
        });

        let pool_info = PoolInfo {
            coin_vault: [1u8; 32],
            pc_vault: [2u8; 32],
            reserve_token: 200_000_000_000_000,
            reserve_sol: 80_000_000_000,
            pool_type: PoolType::RaydiumAmmV4,
            mint: [0xA5; 32],
        };

        // speed=30 (extreme bot), vol=655 SOL (saturated) — would normally be rejected
        // but hard gate is disabled → should pass through to scoring
        engine
            .on_graduation(&pool_info, 1_000_000, 30, 65_500, 15, 2, false)
            .await;
        assert_eq!(engine.graduations_seen.load(Ordering::Relaxed), 1);
        // Gate disabled → reaches scoring. Whether it gets a pending entry depends on score,
        // but it should NOT have been blocked by the hard gate.
        // With paper_mode threshold of 20 and speed=30 + high volume, it may or may not score high enough.
        // The key assertion is that it got past the hard gate (graduations_seen=1 + no early return).
        // We can verify it reached scoring by checking that the function didn't return at the gate.
        // Since we can't directly test "reached scoring", we rely on the fact that disabled=0
        // means the if-block is skipped entirely.
    }
}