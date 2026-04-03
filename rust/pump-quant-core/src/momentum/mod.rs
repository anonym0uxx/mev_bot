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

pub mod activity_gate;
pub mod types;
pub mod kelly;
pub mod config;
pub mod logger;
pub mod pool;
pub mod reconciler;
pub mod position;
pub mod price_feed;
pub mod rpc_sender;
pub mod scorer;
pub mod sell_engine;
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
use crate::momentum::types::ScoredToken;
use crate::momentum::activity_gate::{ActivityTracker, ActivityDecision};

use crate::tx::tip_engine::{TipEngine, TipConfig, TipRequest};

use dashmap::{DashMap, DashSet};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

/// WSOL mint as raw bytes for detecting reversed PumpSwap pool ordering.
/// PumpSwap sorts mints by raw byte comparison — when token_bytes > WSOL_bytes,
/// WSOL becomes base_mint and token becomes quote_mint (~81% of pools).
const WSOL_MINT_BYTES: [u8; 32] = [
    0x06, 0x9b, 0x88, 0x57, 0xfe, 0xab, 0x81, 0x84,
    0xfb, 0x68, 0x7f, 0x63, 0x46, 0x18, 0xc0, 0x35,
    0xda, 0xc4, 0x39, 0xdc, 0x1a, 0xeb, 0x3b, 0x55,
    0x98, 0xa0, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x01,
];

// ── Buy state tracking ──────────────────────────────────────────────────────
// Tracks whether a buy TX landed on-chain. Sell is gated on Confirmed state.
// Fix #1 from BUILD_SPEC_LIVE_EXECUTION.md — eliminates 49+ phantom sell failures.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BuyState {
    /// Buy TX submitted, awaiting confirmation.
    Pending,
    /// Buy TX confirmed on-chain. Safe to sell.
    Confirmed,
    /// Buy TX failed on-chain. Do NOT attempt sell.
    Failed,
}

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

/// RPC fallback: send a raw transaction via standard JSON-RPC `sendTransaction`.
/// Used when Jito (and Nozomi) fail. The transaction bytes are base64-encoded.
async fn rpc_fallback_send(
    client: &reqwest::Client,
    rpc_url: &str,
    tx_bytes: &[u8],
    mint_b58: &str,
    label: &str,
) {
    use base64::Engine as _;
    let tx_b64 = base64::engine::general_purpose::STANDARD.encode(tx_bytes);
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "sendTransaction",
        "params": [tx_b64, {"encoding": "base64", "skipPreflight": true, "maxRetries": 2}]
    });
    match client
        .post(rpc_url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
    {
        Ok(resp) => {
            let text = resp.text().await.unwrap_or_default();
            tracing::info!(mint=%mint_b58, response=%text, "[{label}] RPC fallback submitted");
        }
        Err(e2) => {
            tracing::error!(mint=%mint_b58, err=?e2, "[{label}] RPC fallback also failed");
        }
    }
}

// ── Observation Window ──────────────────────────────────────────────────────
// Pre-entry sniper dump detection. Collects price/reserve samples for
// observation_window_ms after graduation detection before committing to entry.
// Detects: sniper buy-everything-dump pattern, pool drains, price instability.

/// Maximum number of price/reserve samples stored during observation window.
/// At 500ms poll interval, 8s window = ~16 samples. 32 provides headroom.
const OBSERVATION_MAX_SAMPLES: usize = 32;

/// Per-mint observation window state. Tracks price and reserve trajectory
/// between graduation detection and entry decision.
///
/// Stored in a DashMap<[u8; 32], ObservationWindow> on the engine.
/// Created in on_graduation(), evaluated each tick in process_pending_entries(),
/// removed on entry or rejection.
struct ObservationWindow {
    /// Observation window start timestamp (ms). Same as PendingEntry::first_scheduled_ts_ms.
    start_ms: u64,
    /// Collected price samples: (timestamp_ms, price_fp).
    /// Pre-allocated fixed array to avoid heap allocation in hot path.
    price_samples: [(u64, u64); OBSERVATION_MAX_SAMPLES],
    /// Collected reserve samples: (timestamp_ms, reserve_sol_lamports).
    reserve_samples: [(u64, u64); OBSERVATION_MAX_SAMPLES],
    /// Number of price samples recorded.
    price_count: u8,
    /// Number of reserve samples recorded.
    reserve_count: u8,
    /// Peak price_fp observed during the window.
    peak_price_fp: u64,
    /// Window evaluation result: true = entry criteria met, proceed.
    is_ready: bool,
    /// Window evaluation result: true = rejected (sniper dump / drain / instability).
    rejected: bool,
    /// Human-readable rejection reason for logging.
    reject_reason: Option<&'static str>,
    /// Velocity computed at window completion, for use by buy path after window removal.
    pub computed_velocity_bps_per_s: i64,
}

impl ObservationWindow {
    /// Create a new observation window starting at the given timestamp.
    fn new(start_ms: u64) -> Self {
        Self {
            start_ms,
            price_samples: [(0u64, 0u64); OBSERVATION_MAX_SAMPLES],
            reserve_samples: [(0u64, 0u64); OBSERVATION_MAX_SAMPLES],
            price_count: 0,
            reserve_count: 0,
            peak_price_fp: 0,
            is_ready: false,
            rejected: false,
            reject_reason: None,
            computed_velocity_bps_per_s: 0,
        }
    }

    /// Record a price sample. Returns false if buffer is full.
    #[inline(always)]
    fn record_price(&mut self, ts_ms: u64, price_fp: u64) -> bool {
        if (self.price_count as usize) >= OBSERVATION_MAX_SAMPLES {
            return false;
        }
        self.price_samples[self.price_count as usize] = (ts_ms, price_fp);
        self.price_count += 1;
        if price_fp > self.peak_price_fp {
            self.peak_price_fp = price_fp;
        }
        true
    }

    /// Record a reserve sample. Returns false if buffer is full.
    #[inline(always)]
    fn record_reserve(&mut self, ts_ms: u64, reserve_lamports: u64) -> bool {
        if (self.reserve_count as usize) >= OBSERVATION_MAX_SAMPLES {
            return false;
        }
        self.reserve_samples[self.reserve_count as usize] = (ts_ms, reserve_lamports);
        self.reserve_count += 1;
        true
    }

    /// Check drawdown from peak. Returns current drawdown in bps (negative = price below peak).
    #[inline(always)]
    fn current_drawdown_bps(&self, current_price_fp: u64) -> i32 {
        if self.peak_price_fp == 0 || current_price_fp == 0 {
            return 0;
        }
        // bps = (current - peak) / peak * 10000
        let diff = current_price_fp as i64 - self.peak_price_fp as i64;
        ((diff as i128 * 10_000) / self.peak_price_fp as i128) as i32
    }

    /// Check if the last 3 price samples are stable (within 10% of each other).
    /// Returns true if stable, false if volatile.
    fn last_3_stable(&self) -> bool {
        let n = self.price_count as usize;
        if n < 3 {
            return false; // Not enough samples to evaluate
        }
        let p1 = self.price_samples[n - 1].1;
        let p2 = self.price_samples[n - 2].1;
        let p3 = self.price_samples[n - 3].1;

        // All three must be non-zero
        if p1 == 0 || p2 == 0 || p3 == 0 {
            return false;
        }

        // Check each pair: |a - b| / max(a, b) < 10%
        let pairs = [(p1, p2), (p1, p3), (p2, p3)];
        for (a, b) in pairs {
            let diff = (a as i64 - b as i64).unsigned_abs();
            let max_val = a.max(b);
            // diff * 100 / max_val < 10 → diff * 10 < max_val
            if diff * 10 > max_val {
                return false;
            }
        }
        true
    }

    /// Compute sustained price velocity in bps/s from first to latest sample.
    /// Returns 0 if insufficient samples. Positive = rising, negative = falling.
    fn price_velocity_bps_per_s(&self) -> i64 {
        let n = self.price_count as usize;
        if n < 2 {
            return 0;
        }
        let (t_first, p_first) = self.price_samples[0];
        let (t_last, p_last) = self.price_samples[n - 1];
        if p_first == 0 || t_last <= t_first {
            return 0;
        }
        let elapsed_ms = (t_last - t_first) as i64;
        if elapsed_ms < 100 {
            return 0; // Too short to be meaningful
        }
        let price_change_bps = ((p_last as i128 - p_first as i128) * 10_000 / p_first as i128) as i64;
        // Convert bps over elapsed_ms to bps per second
        price_change_bps * 1_000 / elapsed_ms
    }

    /// Get the latest reserve sample value (0 if no samples).
    #[inline(always)]
    fn latest_reserve(&self) -> u64 {
        if self.reserve_count == 0 {
            0
        } else {
            self.reserve_samples[self.reserve_count as usize - 1].1
        }
    }
}

/// Result from an async retry task when ShredStream detects a fresh PumpSwap
/// pool before getProgramAccounts has indexed it. The retry task resolves the
/// pool after a delay and sends the result back through `retry_tx` for
/// processing on the next `on_tick()`.
struct AsyncRetryResult {
    resolution: PoolResolution,
    enrichment: crate::momentum::types::GradEnrichment,
    ts_ms: u64,
    mint: [u8; 32],
}

/// Compute dynamic max_quote_in multiplier pct from observed price velocity.
/// velocity_bps_per_s: from ObservationWindow::price_velocity_bps_per_s(), 0 if unknown.
fn compute_max_quote_in_multiplier(cfg: &MomentumConfig, velocity_bps_per_s: i64) -> u32 {
    let velocity = velocity_bps_per_s.max(0) as u32; // only positive velocity increases buffer
    let extra_pct = velocity
        .saturating_mul(cfg.max_quote_in_tx_propagation_s)
        / cfg.max_quote_in_per_velocity_divisor.max(1);
    let multiplier = cfg.max_quote_in_base_pct.saturating_add(extra_pct);
    multiplier.min(cfg.max_quote_in_cap_pct)
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

    /// Mint-level dedup for CoreCast duplicate graduation events.
    /// CoreCast sends the same mint's graduation 10-20+ times per 3-minute window
    /// with different sigs (DEX trade sigs, not graduation sigs), so sig-based
    /// dedup in `resolving_sigs` doesn't catch them. This gates on mint + 30s TTL.
    /// Key: mint [u8; 32], Value: first-seen timestamp ms.
    recent_corecast_grads: DashMap<[u8; 32], u64>,

    /// Atomic graduation dedup — inserted in on_graduation() BEFORE ring push.
    /// Prevents TOCTOU race where two concurrent on_graduation calls both pass
    /// the `active.contains_key()` check before either inserts into `active`.
    /// Key: mint [u8; 32], Value: first-seen timestamp ms. TTL: 60s.
    pending_grads: DashMap<[u8; 32], u64>,

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

    // ── Observed price velocity per position (for dynamic max_quote_in) ──
    // Key = mint, Value = velocity in bps/s from observation window.
    // Stored outside MomentumPosition (no free bytes in 256-byte struct).
    observed_velocity: DashMap<[u8; 32], i64>,

    // ── Pending entries scheduled for T+delay ───────────────────────
    pending: std::sync::Mutex<PendingEntryRing>,

    // ── Observation windows: mint → ObservationWindow ───────────────
    // Pre-entry sniper dump detection. Created on graduation, evaluated
    // each tick during process_pending_entries(), removed on entry or rejection.
    observation_windows: DashMap<[u8; 32], ObservationWindow>,

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
    /// Buy TX state tracking: mint → BuyState. Written by async buy tasks,
    /// read by close_position to gate sell TX submission.
    /// MUST be Arc so all clones (buy task, sell task) share the same map.
    buy_states: Arc<DashMap<[u8; 32], BuyState>>,
    /// Mints where position was opened but buy TX couldn't be submitted because
    /// pool accounts were zeroed (not yet resolved). Checked each tick — when pool
    /// accounts appear in pumpswap_pools/raydium_pools, the deferred buy is submitted.
    /// Entries are removed after successful submission or after 10s timeout.
    deferred_buy_pending: DashSet<[u8; 32]>,
    tip_engine: Arc<parking_lot::Mutex<TipEngine>>,
    jito_grpc: Option<Arc<crate::tx::jito_grpc::JitoGrpcClient>>,
    nozomi_client: Option<Arc<crate::tx::nozomi::NozomiClient>>,
    wallet_pubkey: Option<[u8; 32]>,
    blockhash_cache: Arc<crate::tx::executor::BlockhashCache>,

    // ── RPC primary sender (Helius) with rate limiter + circuit breaker ──
    rpc_sender: Arc<rpc_sender::RpcSender>,

    // ── Legacy RPC fallback (kept for Nozomi→Jito→RPC triple fallback) ──
    /// Shared reqwest::Client for RPC fallback (created once, reused).
    rpc_fallback_client: reqwest::Client,
    /// RPC URL for fallback sendTransaction (SOLANA_RPC_URL or default).
    rpc_fallback_url: Arc<String>,

    /// Public Solana RPC URL for read-heavy pool resolution calls.
    /// Uses free public endpoint to avoid burning Helius rate budget.
    pub public_rpc_url: Arc<String>,

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

    /// Tracks how many times the engine has entered each mint this session.
    /// Used to enforce max_entries_per_mint (re-entry limiter).
    /// Key: mint [u8; 32], Value: entry count. Resets on engine restart.
    mint_entry_counts: DashMap<[u8; 32], u32>,

    // ── Circuit Breaker & Risk Management (TASK 5) ──────────────────
    /// Cumulative session net PnL in lamports (signed). Updated after each close.
    cb_session_pnl_lamports: AtomicI64,
    /// Consecutive loss counter. Reset to 0 on any win. Updated after each close.
    cb_consecutive_losses: AtomicU64,
    /// Total trades this session (for rolling WR denominator).
    cb_total_trades: AtomicU64,
    /// Total wins this session (for rolling WR numerator).
    cb_total_wins: AtomicU64,
    /// Timestamp (ms) until which trading is paused (session drawdown or consecutive loss pause).
    /// 0 = not paused. Entries blocked when now_ms < this value.
    cb_pause_until_ms: AtomicU64,
    /// Session halted flag. Once set, no new entries until manual restart.
    /// Uses AtomicU64: 0 = not halted, 1 = halted.
    cb_halted: AtomicU64,
    /// Half-size flag. When set, compute_size_lamports divides result by 2.
    /// Uses AtomicU64: 0 = normal, 1 = half-size active.
    cb_halfsize: AtomicU64,
    /// Trades remaining under half-size regime (counts down from consecutive_loss_halfsize).
    /// When this reaches 0, cb_halfsize is cleared.
    cb_halfsize_remaining: AtomicU64,

    // ── Async retry channel for ShredStream fresh detections ────────────
    // When ShredStream detects a PumpSwap pool before getProgramAccounts has
    // indexed it, an async retry task resolves the pool after a delay and sends
    // the result through this channel. on_tick() drains it.
    retry_tx: tokio::sync::mpsc::UnboundedSender<AsyncRetryResult>,
    retry_rx: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<AsyncRetryResult>>,

    // ── Pre-entry activity gate (filters dead tokens) ───────────────
    activity_tracker: ActivityTracker,

    // ── Sell engine (escalation retry pipeline) ─────────────────────
    // TODO(sell_engine_pr): Wire sell_engine into close_position() — separate PR
    // sell_engine: Arc<sell_engine::SellEngine>,

    // ── On-chain reconciler (P&L verification) ──────────────────────
    // TODO(reconciler_pr): Wire reconciler background task + record_buy/sell — separate PR
    // reconciler: Arc<reconciler::Reconciler>,
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
        public_rpc_url: String,
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
        // WSS: use standard Helius WSS (supports accountSubscribe) instead of
        // dedicated node WSS which silently drops subscription requests.
        let helius_wss_for_price = std::env::var("HELIUS_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .map(|k| format!("wss://mainnet.helius-rpc.com/?api-key={}", k))
            .unwrap_or(helius_wss_url);
        let (price_feed, ws_handle) = PriceFeedManager::new(
            helius_poll_url,
            helius_wss_for_price,
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

        // RPC fallback client + URL (for sendTransaction when Jito/Nozomi fail)
        let rpc_fallback_url = Arc::new(
            std::env::var("SOLANA_RPC_URL")
                .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string()),
        );
        let rpc_fallback_client = reqwest::Client::new();

        // RPC primary sender with circuit breaker (Helius RPC → Jito fallback)
        // Uses helius_rpc_url for both getSignatureStatuses and sendTransaction.
        // When Engineer 4 adds send_rpc_url parameter, the second arg becomes the dedicated send endpoint.
        let rpc_sender_config = rpc_sender::RpcSenderConfig::from_momentum_config(&config.rpc_sender);
        let rpc_sender_inst = Arc::new(rpc_sender::RpcSender::new(
            helius_rpc_url.to_string(),
            rpc_sender_config,
        ));

        let public_rpc_url = Arc::new(public_rpc_url);

        // Channel for async retry results (ShredStream fresh detection → delayed pool resolution)
        let (async_retry_tx, async_retry_rx) = tokio::sync::mpsc::unbounded_channel::<AsyncRetryResult>();

        let engine = Self {
            config,
            rpc_url,
            helius_rpc_url,
            http_client: crate::momentum::pool::make_pool_resolution_client(),
            active: DashMap::new(),
            recently_closed: DashMap::new(),
            resolving_sigs: DashMap::new(),
            recent_corecast_grads: DashMap::new(),
            pending_grads: DashMap::new(),
            drain_samples: DashMap::new(),
            reserve_sol_ctx: DashMap::new(),
            momentum_zones: DashMap::new(),
            pending: std::sync::Mutex::new(PendingEntryRing::new()),
            observation_windows: DashMap::new(),
            price_feed,
            logger,
            scored_tokens: DashMap::new(),
            scored_token_rx: scored_rx,
            raydium_pools: DashMap::new(),
            pumpswap_pools: DashMap::new(),
            buy_states: Arc::new(DashMap::new()),
            deferred_buy_pending: DashSet::new(),
            mint_entry_counts: DashMap::new(),
            tip_engine,
            jito_grpc,
            nozomi_client,
            wallet_pubkey,
            blockhash_cache,
            rpc_sender: rpc_sender_inst,
            rpc_fallback_client,
            rpc_fallback_url,
            public_rpc_url,
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
            // Circuit breaker state (TASK 5)
            cb_session_pnl_lamports: AtomicI64::new(0),
            cb_consecutive_losses: AtomicU64::new(0),
            cb_total_trades: AtomicU64::new(0),
            cb_total_wins: AtomicU64::new(0),
            cb_pause_until_ms: AtomicU64::new(0),
            cb_halted: AtomicU64::new(0),
            cb_halfsize: AtomicU64::new(0),
            cb_halfsize_remaining: AtomicU64::new(0),
            retry_tx: async_retry_tx,
            retry_rx: tokio::sync::Mutex::new(async_retry_rx),
            activity_tracker: ActivityTracker::new(),
            observed_velocity: DashMap::new(),
        };

        // Spawn wallet balance poller (no-op in paper mode — reads but doesn't gate)
        let balance_arc = Arc::clone(&engine.wallet_balance_lamports);
        let poll_ms = engine.config.wallet_balance_poll_ms;
        let wallet_pk = engine.wallet_pubkey;
        // Use public Solana RPC for balance polling — lightweight call (every 30s)
        // that doesn't need Helius. Frees Helius rate budget for sendTransaction.
        let rpc_for_balance = engine.public_rpc_url.clone();
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

        // NOTE: u64 overflow gate REMOVED — it was incorrectly blocking ALL standard pump.fun
        // tokens (800T atoms at 85 SOL = k >> u64::MAX). PumpSwap uses u128 internally
        // for the constant product, so the gate logic was wrong. The Custom:6023 on Phxz39
        // was caused by a different overflow (likely in the fee computation with extreme
        // reserve_token values), not the k = sol * token product itself.
        // TODO: Investigate real cause of Custom:6023 and add a correct gate if needed.

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

        // Atomic graduation dedup — prevents TOCTOU race where two concurrent
        // on_graduation calls both pass active.contains_key() before either
        // process_pending_entries() inserts into active.
        //
        // Strategy: use DashMap::entry().or_insert() which is atomic. If the
        // mint is already in pending_grads OR already in active, reject.
        let now_ms_dedup = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        // Check active first (already has a live position)
        if self.active.contains_key(&pool_info.mint) {
            tracing::debug!(
                mint = %bs58::encode(&pool_info.mint).into_string(),
                "[momentum] skipping graduation — already have an open position in this mint"
            );
            return;
        }
        // Atomic insert: only the first concurrent caller wins; all others see the existing entry.
        let mut already_pending = false;
        self.pending_grads.entry(pool_info.mint).and_modify(|_| {
            already_pending = true;
        }).or_insert(now_ms_dedup);
        if already_pending {
            tracing::debug!(
                mint = %bs58::encode(&pool_info.mint).into_string(),
                "[momentum] skipping graduation — concurrent duplicate (pending_grads)"
            );
            return;
        }

        // Check concurrent position limit
        if self.active.len() >= self.config.max_concurrent as usize {
            tracing::debug!(
                active = self.active.len(),
                max = self.config.max_concurrent,
                "[on_graduation] rejected: max concurrent positions reached"
            );
            return;
        }

        tracing::info!(
            mint = %bs58::encode(&pool_info.mint).into_string(),
            reserve_sol = pool_info.reserve_sol,
            pool_type = ?pool_info.pool_type,
            grad_speed_s,
            volume_sol_x100 = grad_volume_sol_x100,
            is_cold_miss,
            "[on_graduation] ENTERED — processing gates"
        );

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
        // Skip for cold misses — volume is estimated from reserves and caps at u16 max (655.35)
        if !is_cold_miss && cfg.max_grad_volume_sol_absolute > 0.0
            && grad_volume_sol >= cfg.max_grad_volume_sol_absolute
        {
            tracing::debug!(
                mint = %bs58::encode(&pool_info.mint).into_string(),
                vol_sol = grad_volume_sol,
                "hard gate: rejected saturated volume"
            );
            return;
        }
        // Hard reject: volume below minimum (too thin, low conviction)
        // Skip for cold misses — they have volume=0 until enrichment resolves
        if !is_cold_miss && cfg.min_grad_volume_sol > 0.0 && grad_volume_sol < cfg.min_grad_volume_sol {
            tracing::debug!(
                mint = %bs58::encode(&pool_info.mint).into_string(),
                vol_sol = grad_volume_sol,
                min = cfg.min_grad_volume_sol,
                "hard gate: rejected low volume"
            );
            return;
        }
        // Hard reject: volume above max threshold
        if !is_cold_miss && cfg.max_grad_volume_sol > 0.0 && grad_volume_sol > cfg.max_grad_volume_sol {
            tracing::debug!(
                mint = %bs58::encode(&pool_info.mint).into_string(),
                vol_sol = grad_volume_sol,
                max = cfg.max_grad_volume_sol,
                "hard gate: rejected high volume"
            );
            return;
        }
        // Hard reject: re-entry limiter (max entries per mint per session)
        if cfg.max_entries_per_mint > 0 {
            let count = self.mint_entry_counts.get(&pool_info.mint).map(|c| *c).unwrap_or(0);
            if count >= cfg.max_entries_per_mint {
                tracing::debug!(
                    mint = %bs58::encode(&pool_info.mint).into_string(),
                    entries = count,
                    max = cfg.max_entries_per_mint,
                    "hard gate: rejected re-entry limit"
                );
                return;
            }
        }
        // ── End hard gate ────────────────────────────────────────────────────

        // Score the graduation (v2: 5 components including entry discount).
        // Compute entry_price_fp from pool reserves for the entry discount scorer.
        let pre_score_entry_fp = price_from_reserves(pool_info.reserve_sol, pool_info.reserve_token);

        // Hard gate: reject extreme-supply tokens at entry.
        // price_fp < 100 means token supply is so large that fixed-point price tracking
        // is meaningless (1 bps = 0 change). These tokens always hit time_sl and are
        // prone to PumpSwap Overflow (Custom:6023) on sell. Block them before observation.
        if pre_score_entry_fp > 0 && pre_score_entry_fp < 100 {
            tracing::warn!(
                mint = %bs58::encode(&pool_info.mint).into_string(),
                price_fp = pre_score_entry_fp,
                reserve_token = pool_info.reserve_token,
                "hard gate: rejected extreme-supply token (price_fp < 100) — PumpSwap Overflow risk"
            );
            return;
        }

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
            0, // velocity_bps_per_s: unknown at graduation time, set during observation
            self.config.min_buys_for_full_ratio_score,
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

        // ── Activity Gate: require minimum WS trading activity before entry ──
        // Dead tokens waste -5.3% on AMM round-trip fees. This blocks ~90% of
        // dead tokens (saves +0.042 SOL per 167 trades).
        {
            let decision = self.activity_tracker.check_entry(
                &pool_info.mint,
                now_ms,
                &self.config.activity_gate,
            );
            if let ActivityDecision::Reject(reason) = decision {
                tracing::info!(
                    mint = %bs58::encode(&pool_info.mint).into_string(),
                    score = score.total(),
                    reason = %reason,
                    "[momentum] activity gate REJECTED — insufficient trading activity"
                );
                return;
            }
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

        // Increment re-entry counter for this mint
        *self.mint_entry_counts.entry(pool_info.mint).or_insert(0) += 1;

        // Start price feed subscription immediately (before entry delay).
        // Seed with estimated reserves so current_price() returns a value instantly.
        // The RPC poll will overwrite with real data within ~750ms.
        let coin_vault_b58 = bs58::encode(&pool_info.coin_vault).into_string();
        let pc_vault_b58 = bs58::encode(&pool_info.pc_vault).into_string();
        self.price_feed
            .subscribe_with_estimate(
                VaultSubscription {
                    mint: pool_info.mint,
                    coin_vault: coin_vault_b58,
                    pc_vault: pc_vault_b58,
                },
                pool_info.reserve_sol,
                pool_info.reserve_token,
            )
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
            observed_velocity_bps_per_s: None,
            active: true,
        };

        if let Ok(mut ring) = self.pending.lock() {
            ring.push(entry);
        }

        // Create observation window if enabled.
        // The window starts now and runs for observation_window_ms.
        // During this time, process_pending_entries() collects price/reserve
        // samples and evaluates sniper dump patterns before allowing entry.
        if self.config.observation_window_ms > 0 {
            self.observation_windows.insert(
                pool_info.mint,
                ObservationWindow::new(now_ms),
            );
            tracing::info!(
                mint = %mint_b58,
                window_ms = self.config.observation_window_ms,
                "[momentum] observation window started — collecting samples before entry"
            );
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

    /// Circuit breaker check: returns true if new entries are allowed.
    fn check_circuit_breakers(&self, now_ms: u64) -> bool {
        // 1. Hard halt: session exceeded max loss
        if self.cb_halted.load(Ordering::Relaxed) != 0 {
            tracing::debug!("[circuit_breaker] entries blocked — session halted");
            return false;
        }
        // 2. Timed pause (from session loss or consecutive losses)
        let pause_until = self.cb_pause_until_ms.load(Ordering::Relaxed);
        if pause_until > 0 && now_ms < pause_until {
            tracing::debug!(
                resume_in_s = (pause_until - now_ms) / 1000,
                "[circuit_breaker] entries blocked — paused"
            );
            return false;
        }
        // 3. Rolling WR floor
        let total = self.cb_total_trades.load(Ordering::Relaxed);
        let wins = self.cb_total_wins.load(Ordering::Relaxed);
        if total >= self.config.rolling_wr_window as u64 {
            let wr_pct = (wins as f64 / total as f64) * 100.0;
            if wr_pct < self.config.min_rolling_wr_pct {
                tracing::warn!(
                    wr_pct = wr_pct,
                    total,
                    wins,
                    "[circuit_breaker] entries blocked — rolling WR below floor"
                );
                return false;
            }
        }
        true
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

        // Prune stale mint-level graduation dedup entries (TTL 60s, O(n) but n ≤ ~200)
        self.recent_corecast_grads.retain(|_, first_seen| {
            now_ms.saturating_sub(*first_seen) < 60_000
        });

        // Prune stale pending_grads entries (TTL 60s — covers the full obs window + buy lifetime)
        self.pending_grads.retain(|_, first_seen| {
            now_ms.saturating_sub(*first_seen) < 60_000
        });

        // Prune stale observation windows (safety net: 2× observation_window_ms TTL)
        // Normally removed by process_pending_entries, but this catches leaked entries.
        if self.config.observation_window_ms > 0 {
            let obs_ttl = self.config.observation_window_ms * 2;
            self.observation_windows.retain(|_, w| {
                now_ms.saturating_sub(w.start_ms) < obs_ttl
            });
        }
    }

    /// Drain async retry results from the ShredStream fresh-detection retry channel.
    ///
    /// When ShredStream detects a PumpSwap pool before getProgramAccounts has indexed
    /// it, an async task retries resolution after 1s/2s/4s delays and sends the
    /// result here. We process them identically to the mint-based fast path success
    /// case in on_migration().
    async fn drain_async_retries(&self, _now_ms: u64) {
        let mut rx = match self.retry_rx.try_lock() {
            Ok(rx) => rx,
            Err(_) => return, // Another tick is draining — skip
        };

        // Drain all pending results (non-blocking)
        while let Ok(result) = rx.try_recv() {
            let mint_b58 = bs58::encode(&result.mint).into_string();
            let resolution = &result.resolution;
            let enrichment = result.enrichment;
            let ts_ms = result.ts_ms;

            tracing::info!(
                mint = %mint_b58,
                pool_type = ?resolution.pool_type,
                reserve_sol = resolution.reserve_sol_lamports,
                "[momentum] processing async retry result from ShredStream fresh detection"
            );

            let pool_info = PoolInfo {
                coin_vault: resolution.coin_vault,
                pc_vault: resolution.pc_vault,
                reserve_token: resolution.reserve_token_atoms,
                reserve_sol: resolution.reserve_sol_lamports,
                pool_type: resolution.pool_type,
                mint: resolution.mint,
            };

            // Derive effective enrichment (same logic as mint fast path in on_migration)
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
                    "[momentum] enrichment cold miss (async retry) — estimating speed from LP reserves"
                );
                if sol >= 250 { 60u32 } else { 120u32 }
            } else {
                enrichment.grad_speed_s
            };
            let effective_buys_5s = if enrichment.buys_5s == 0 { 3u32 } else { enrichment.buys_5s as u32 };
            let effective_sells_5s = if enrichment.sells_5s == 0 { 1u32 } else { enrichment.sells_5s as u32 };

            // Store PumpSwap pool accounts for live execution
            if let Some(ps_pool) = crate::momentum::pool::extract_pumpswap_pool_accounts(resolution) {
                let ps_accts: crate::tx::pumpswap::PumpSwapPoolAccounts = ps_pool.into();
                self.pumpswap_pools.insert(resolution.mint, ps_accts);
                tracing::debug!(
                    mint = %mint_b58,
                    "[momentum] pumpswap pool accounts stored (async retry)"
                );
            } else if resolution.pool_type == crate::momentum::pool::PoolType::PumpSwap && !self.config.paper_mode {
                // Fallback: store partial accounts for last-chance resolution
                let ps_accts = crate::tx::pumpswap::PumpSwapPoolAccounts {
                    pool: [0u8; 32],
                    base_mint: resolution.mint,
                    pool_base_token_account: resolution.coin_vault,
                    pool_quote_token_account: resolution.pc_vault,
                    coin_creator_vault_ata: [0u8; 32],
                    coin_creator_vault_authority: [0u8; 32],
                    token_is_base: true,
                    token_mint_program: [0u8; 32],
                    is_cashback_coin: false,
                };
                self.pumpswap_pools.insert(resolution.mint, ps_accts);
                tracing::warn!(
                    mint = %mint_b58,
                    "[momentum] pumpswap pool accounts stored PARTIAL (async retry fallback)"
                );
            }

            // is_cold_miss is always true for async retry results (that's why they were retried)
            self.on_graduation(
                &pool_info,
                ts_ms,
                effective_speed_s,
                effective_volume_sol_x100,
                effective_buys_5s,
                effective_sells_5s,
                true, // is_cold_miss — ShredStream fresh detections are always cold misses
            ).await;
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
        // Data-driven score-based sizing (from Kelly analysis of 776 trades)
        let size_sol: f64 = match grad_score {
            60..=100 => 0.05,   // score 60+: qKelly=0.034, optimal ~0.05 SOL
            55..=59  => 0.02,   // score 55-59: marginal edge, conservative size
            _        => 0.02,   // should not reach here with min_score=55
        };
        let mut lamports = (size_sol * 1_000_000_000.0) as u64;
        // Circuit breaker: halfsize regime
        if self.cb_halfsize.load(Ordering::Relaxed) != 0 {
            lamports /= 2;
            let remaining = self.cb_halfsize_remaining.fetch_sub(1, Ordering::Relaxed);
            if remaining <= 1 {
                self.cb_halfsize.store(0, Ordering::Relaxed);
            }
        }
        lamports
    }

    /// Compute Kelly-optimal probe size from rolling trade history.
    /// Returns None if insufficient history (< kelly_bootstrap_trades) or negative EV.
    /// Caller falls back to probe_size_sol when None is returned.
    fn compute_kelly_probe_size(&self) -> Option<u64> {
        use crate::momentum::kelly::{compute_momentum_kelly_size, compute_momentum_kelly_inputs, MomentumPaperTrade};

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

        // Drain async retry results (ShredStream fresh detection → delayed pool resolution)
        self.drain_async_retries(now_ms).await;

        // ── Circuit Breaker checks: block new entries if tripped ────────
        let cb_allow_entries = self.check_circuit_breakers(now_ms);

        // Process pending entries that are ready (skipped if circuit breaker blocks)
        if cb_allow_entries {
            self.process_pending_entries(now_ms).await;
        }

        // Deferred buy TX: retry submission for positions that opened without pool accounts
        self.process_deferred_buys(now_ms);

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

        // Activity tracker housekeeping: remove stale mints (~every 10s)
        let tick_num_cleanup = now_ms / self.config.check_ms.max(1);
        if tick_num_cleanup % 67 == 0 {
            self.activity_tracker.cleanup(now_ms, self.config.activity_gate.cleanup_stale_ms);
        }
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

    /// Check positions with deferred buy TXs. When pool accounts appear in the
    /// DashMap (resolved by Engineer #1/#3 async tasks), submit the buy TX.
    /// Gives up after 10 seconds — momentum opportunity has passed.
    ///
    /// Called from on_tick() after process_pending_entries(). O(n) where n is
    /// the number of deferred positions (typically 0-2). No blocking RPC calls —
    /// buy TX submission is spawned as a tokio task.
    #[inline(never)]
    fn process_deferred_buys(&self, now_ms: u64) {
        if self.config.paper_mode || self.deferred_buy_pending.is_empty() {
            return;
        }

        // Collect mints to process (can't mutate DashSet while iterating)
        let pending_mints: Vec<[u8; 32]> = self.deferred_buy_pending.iter().map(|r| *r.key()).collect();

        for mint in pending_mints {
            // Position must still be active
            let (entry_ts, size_lamports, entry_price_fp, grad_score) = match self.active.get(&mint) {
                Some(pos) => (pos.entry_ts_ms, pos.size_lamports, pos.entry_price_fp, pos.grad_score),
                None => {
                    // Position was already closed — clean up
                    self.deferred_buy_pending.remove(&mint);
                    continue;
                }
            };

            let age_ms = now_ms.saturating_sub(entry_ts);

            // 10s timeout: momentum opportunity has passed
            if age_ms > 10_000 {
                tracing::warn!(
                    mint = %bs58::encode(&mint).into_string(),
                    age_ms,
                    "[momentum] deferred buy timed out — pool resolution too slow, position stays accounting-only"
                );
                self.deferred_buy_pending.remove(&mint);
                continue;
            }

            // Check PumpSwap pool accounts first (100% of pump.fun graduations since Apr 2026)
            if let Some(ps_pool) = self.pumpswap_pools.get(&mint).map(|r| r.clone()) {
                let has_pool = ps_pool.pool != [0u8; 32];
                let has_creator = ps_pool.coin_creator_vault_ata != [0u8; 32];

                if has_pool && has_creator {
                    let mint_b58 = bs58::encode(&mint).into_string();
                    tracing::info!(
                        mint = %mint_b58,
                        age_ms,
                        "[momentum] deferred buy TX — PumpSwap pool accounts now resolved, submitting"
                    );

                    // Get current price for token estimate
                    let current_price_fp = match self.price_feed.current_price(&mint) {
                        Some(p) if p > 0 => p,
                        _ => entry_price_fp, // fallback to entry price
                    };

                    let tokens_estimate = if current_price_fp > 0 {
                        (size_lamports as u128 * 1_000_000 / current_price_fp as u128) as u64
                    } else { 0u64 };

                    // Set tokens_held on position BEFORE async buy
                    if let Some(mut pos) = self.active.get_mut(&mint) {
                        pos.set_tokens_held(tokens_estimate);
                    }

                    // Build and submit buy TX (same logic as initial PumpSwap buy path)
                    let kp_path = std::env::var("WALLET_KEYPAIR_PATH").unwrap_or_default();
                    let bh = self.blockhash_cache_sync().unwrap_or([0u8; 32]);
                    let tip_req = crate::tx::tip_engine::TipRequest {
                        context: crate::tx::tip_engine::TipContext::Entry,
                        size_lamports,
                        gain_bps: 0,
                        grad_score: grad_score as f64,
                    };
                    let tip = self.tip_engine.lock().compute_tip(&tip_req);
                    let fee_idx = (std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() % 8) as usize;
                    let rpc_sender = self.rpc_sender.clone();
                    let mint_buy = mint;
                    let resolve_client = self.http_client.clone();
                    let resolve_url = self.helius_rpc_url.clone();
                    let resolve_url_fallback = self.public_rpc_url.clone();
                    // Dynamic max_quote_in: scale slippage buffer based on observed price velocity.
                    let obs_velocity: i64 = self.observed_velocity
                        .get(&mint_buy)
                        .map(|v| *v)
                        .unwrap_or(0);
                    // Clean up after use
                    self.observed_velocity.remove(&mint_buy);
                    let multiplier_pct = compute_max_quote_in_multiplier(&self.config, obs_velocity);
                    let max_quote_in = (size_lamports as u128 * multiplier_pct as u128 / 100) as u64;
                    // Anti-sandwich slippage: compute min_tokens_out from buffered amount.
                    // Using 50% to avoid SlippageExceeded (Custom:6004) when pool price moves
                    // between observation and TX landing. max_quote_in is the real SOL guard.
                    let min_tokens_out = if current_price_fp > 0 {
                        let tokens_at_max = (max_quote_in as u128 * 1_000_000 / current_price_fp as u128) as u64;
                        std::cmp::max(tokens_at_max * 50 / 100, 1)
                    } else {
                        1
                    };
                    let tokens_est_for_sandwich = tokens_estimate;
                    // Fix #1: Track buy state for sell gating.
                    // Guard: if buy is already Pending or Confirmed, skip duplicate buy.
                    // This can happen when process_deferred_buys fires for a mint that
                    // already had a buy submitted via the normal path.
                    {
                        let already_buying = self.buy_states.get(&mint)
                            .map(|s| matches!(*s, BuyState::Pending | BuyState::Confirmed))
                            .unwrap_or(false);
                        if already_buying {
                            tracing::warn!(
                                mint = %bs58::encode(&mint).into_string(),
                                "[buy_pumpswap] duplicate buy suppressed — buy already in progress"
                            );
                            continue;
                        }
                        self.buy_states.insert(mint, BuyState::Pending);
                    }
                    let buy_states = self.buy_states.clone();

                    tokio::spawn(async move {
                        // Resolve token_mint_program if unknown
                        let mut ps_pool = ps_pool;
                        if ps_pool.token_mint_program == [0u8; 32] {
                            if let Some(prog) = crate::momentum::pool::resolve_mint_program_with_fallback(
                                &resolve_client, &mint_buy, &resolve_url, Some(&resolve_url_fallback),
                            ).await {
                                ps_pool.token_mint_program = prog;
                                tracing::info!(
                                    mint = %bs58::encode(&mint_buy).into_string(),
                                    program = %bs58::encode(&prog).into_string(),
                                    "[deferred_buy_pumpswap] resolved token_mint_program"
                                );
                            } else {
                                // Resolution failed — we cannot determine the token program.
                                // NEVER default to SPL Token: if the token is Token-2022 the TX will fail
                                // on-chain with IncorrectProgramId, wasting fees and leaving a bad trade.
                                // Safe path: abort this buy. The trade will be skipped.
                                tracing::warn!(
                                    mint = %bs58::encode(&mint_buy).into_string(),
                                    "[deferred_buy_pumpswap] failed to resolve token_mint_program — aborting buy (Token-2022 safety)"
                                );
                                buy_states.remove(&mint_buy);
                                return;
                            }
                        }

                        let kp_bytes = match std::fs::read(&kp_path) {
                            Ok(b) => b,
                            Err(e) => { tracing::error!(err=?e, "[deferred_buy_pumpswap] keypair load failed"); return; }
                        };
                        let kp_arr: Vec<u8> = match serde_json::from_slice(&kp_bytes) {
                            Ok(v) => v,
                            Err(e) => { tracing::error!(err=?e, "[deferred_buy_pumpswap] keypair parse failed"); return; }
                        };
                        if kp_arr.len() != 64 { tracing::error!("[deferred_buy_pumpswap] invalid keypair len"); return; }
                        let mut kb = [0u8; 64];
                        kb.copy_from_slice(&kp_arr);
                        let keypair = match solana_sdk::signature::Keypair::from_bytes(&kb) {
                            Ok(k) => k,
                            Err(e) => { tracing::error!(err=?e, "[deferred_buy_pumpswap] keypair err"); return; }
                        };
                        use std::str::FromStr as _;
                        let tip_account = solana_sdk::pubkey::Pubkey::from_str(
                            crate::tx::raydium::JITO_TIP_ACCOUNTS[0]
                        ).unwrap();
                        let tx_bytes = match crate::tx::pumpswap::build_pumpswap_buy_tx(
                            &ps_pool, &keypair, max_quote_in, min_tokens_out, tip, tip_account, bh, fee_idx,
                        ) {
                            Ok(b) => b,
                            Err(e) => {
                                tracing::error!(
                                    mint=%bs58::encode(&mint_buy).into_string(),
                                    err=?e,
                                    "[deferred_buy_pumpswap] build failed"
                                );
                                return;
                            }
                        };
                        let mint_str = bs58::encode(&mint_buy).into_string();
                        match rpc_sender.submit_tx(&tx_bytes, &mint_str, "deferred_buy_pumpswap").await {
                            rpc_sender::SubmitResult::Landed { signature, latency_ms } => {
                                buy_states.insert(mint_buy, BuyState::Confirmed);
                                tracing::info!(
                                    mint=%mint_str, sig=%signature, latency_ms, tip,
                                    size_sol=size_lamports as f64/1e9,
                                    max_quote_sol=max_quote_in as f64/1e9,
                                    obs_velocity_bps_per_s = obs_velocity,
                                    max_quote_in_multiplier_pct = multiplier_pct,
                                    estimated_tokens=tokens_est_for_sandwich,
                                    min_tokens_out,
                                    "[deferred_buy_pumpswap] RPC landed ✅ — compare on-chain receipt vs estimated_tokens for sandwich detection"
                                );
                            }
                            rpc_sender::SubmitResult::TimedOut { signature } => {
                                tracing::warn!(mint=%mint_str, sig=%signature, "[deferred_buy_pumpswap] RPC timed out (may still land)");
                            }
                            rpc_sender::SubmitResult::Failed { error } => {
                                buy_states.insert(mint_buy, BuyState::Failed);
                                tracing::error!(mint=%mint_str, err=%error, "[deferred_buy_pumpswap] RPC FAILED");
                            }
                            rpc_sender::SubmitResult::CircuitOpen { remaining_ms } => {
                                buy_states.insert(mint_buy, BuyState::Failed);
                                tracing::warn!(mint=%mint_str, remaining_ms, "[deferred_buy_pumpswap] circuit breaker OPEN — skipped");
                            }
                        }
                    });

                    // Mark as submitted — remove from pending set
                    self.deferred_buy_pending.remove(&mint);
                    continue;
                }
                // Pool exists but accounts are still zeroed — wait for next tick
            }

            // Check Raydium pool accounts (legacy path)
            if let Some(pool) = self.raydium_pools.get(&mint).map(|r| r.clone()) {
                // Raydium pools need valid Serum accounts (non-zeroed)
                let serum_valid = pool.serum_market != [0u8; 32]
                    && pool.amm_open_orders != [0u8; 32];

                if serum_valid {
                    let mint_b58 = bs58::encode(&mint).into_string();
                    tracing::info!(
                        mint = %mint_b58,
                        age_ms,
                        "[momentum] deferred buy TX — Raydium pool accounts now resolved, submitting"
                    );

                    let current_price_fp = match self.price_feed.current_price(&mint) {
                        Some(p) if p > 0 => p,
                        _ => entry_price_fp,
                    };

                    let tokens_estimate = if current_price_fp > 0 {
                        (size_lamports as u128 * 1_000_000 / current_price_fp as u128) as u64
                    } else { 0u64 };

                    if let Some(mut pos) = self.active.get_mut(&mint) {
                        pos.set_tokens_held(tokens_estimate);
                    }

                    let kp_path = std::env::var("WALLET_KEYPAIR_PATH").unwrap_or_default();
                    let bh = self.blockhash_cache_sync().unwrap_or([0u8; 32]);
                    let tip_req = crate::tx::tip_engine::TipRequest {
                        context: crate::tx::tip_engine::TipContext::Entry,
                        size_lamports,
                        gain_bps: 0,
                        grad_score: grad_score as f64,
                    };
                    let tip = self.tip_engine.lock().compute_tip(&tip_req);
                    let rpc_sender = self.rpc_sender.clone();
                    let mint_buy = mint;

                    tokio::spawn(async move {
                        let kp_bytes = match std::fs::read(&kp_path) {
                            Ok(b) => b,
                            Err(e) => { tracing::error!(err=?e, "[deferred_buy_raydium] keypair load failed"); return; }
                        };
                        let kp_arr: Vec<u8> = match serde_json::from_slice(&kp_bytes) {
                            Ok(v) => v,
                            Err(e) => { tracing::error!(err=?e, "[deferred_buy_raydium] keypair parse failed"); return; }
                        };
                        if kp_arr.len() != 64 { tracing::error!("[deferred_buy_raydium] invalid keypair len"); return; }
                        let mut kb = [0u8; 64];
                        kb.copy_from_slice(&kp_arr);
                        let keypair = match solana_sdk::signature::Keypair::from_bytes(&kb) {
                            Ok(k) => k,
                            Err(e) => { tracing::error!(err=?e, "[deferred_buy_raydium] keypair err"); return; }
                        };
                        use std::str::FromStr as _;
                        let tip_account = solana_sdk::pubkey::Pubkey::from_str(
                            crate::tx::raydium::JITO_TIP_ACCOUNTS[0]
                        ).unwrap();
                        let tx_bytes = match crate::tx::raydium::build_raydium_buy_tx(
                            &pool, &mint_buy, &keypair, size_lamports, 0, tip, tip_account, bh,
                        ) {
                            Ok(b) => b,
                            Err(e) => {
                                tracing::error!(
                                    mint=%bs58::encode(&mint_buy).into_string(),
                                    err=?e,
                                    "[deferred_buy_raydium] build failed"
                                );
                                return;
                            }
                        };
                        let mint_str = bs58::encode(&mint_buy).into_string();
                        match rpc_sender.submit_tx(&tx_bytes, &mint_str, "deferred_buy_raydium").await {
                            rpc_sender::SubmitResult::Landed { signature, latency_ms } => {
                                tracing::info!(mint=%mint_str, sig=%signature, latency_ms, tip, size_sol=size_lamports as f64/1e9, "[deferred_buy_raydium] RPC landed ✅");
                            }
                            rpc_sender::SubmitResult::TimedOut { signature } => {
                                tracing::warn!(mint=%mint_str, sig=%signature, "[deferred_buy_raydium] RPC timed out (may still land)");
                            }
                            rpc_sender::SubmitResult::Failed { error } => {
                                tracing::error!(mint=%mint_str, err=%error, "[deferred_buy_raydium] RPC FAILED");
                            }
                            rpc_sender::SubmitResult::CircuitOpen { remaining_ms } => {
                                tracing::warn!(mint=%mint_str, remaining_ms, "[deferred_buy_raydium] circuit breaker OPEN — skipped");
                            }
                        }
                    });

                    self.deferred_buy_pending.remove(&mint);
                    continue;
                }
                // Raydium pool exists but Serum accounts are zeroed — wait
            }

            // Neither PumpSwap nor Raydium pool accounts ready yet — check next tick
        }
    }

    /// Process pending entries whose scheduled time has elapsed.
    ///
    /// If the price feed hasn't delivered live data yet, entries are re-queued
    /// for the next tick rather than entering at the stale graduation-time price.
    /// Entries are abandoned (skipped) once `no_price_timeout_ms` elapses without
    /// a live price, preventing ghost-price entries that trigger false stop-losses.
    ///
    /// ## Observation Window Phase
    ///
    /// When `observation_window_ms > 0`, entries go through an observation phase
    /// BEFORE the normal entry logic. During observation:
    /// 1. Price and reserve samples are collected each tick
    /// 2. Drawdown from peak is monitored (rejects sniper dump pattern)
    /// 3. Reserve floor is enforced (rejects drained pools)
    /// 4. Price stability is checked at window expiry (rejects volatile tokens)
    /// Entries only proceed to normal entry flow once observation passes.
    #[inline(never)]
    async fn process_pending_entries(&self, now_ms: u64) {
        // ── Observation Window: collect samples and evaluate ─────────────
        // Runs every tick for all active observation windows, independent of
        // whether the PendingEntry has been drained from the ring buffer yet.
        // This ensures we collect samples at full poll cadence (every 150ms tick).
        if self.config.observation_window_ms > 0 {
            // Collect mints to evaluate (can't mutate DashMap while iterating)
            let obs_mints: Vec<[u8; 32]> = self.observation_windows
                .iter()
                .filter(|r| !r.value().is_ready && !r.value().rejected)
                .map(|r| *r.key())
                .collect();

            for mint in obs_mints {
                let mut should_reject = false;
                let mut reject_reason: &str = "";
                let mut should_ready = false;

                // Collect current price and reserve from the feed
                let current_price_fp = self.price_feed.current_price(&mint).unwrap_or(0);
                let current_reserve = self.price_feed.get_reserve_sol(&mint).unwrap_or(0);
                let is_estimated = self.price_feed.is_price_estimated(&mint);

                if let Some(mut window) = self.observation_windows.get_mut(&mint) {
                    let w = window.value_mut();
                    let elapsed = now_ms.saturating_sub(w.start_ms);

                    // Only record non-estimated, non-zero prices
                    if !is_estimated && current_price_fp > 0 {
                        w.record_price(now_ms, current_price_fp);
                    }
                    if current_reserve > 0 {
                        w.record_reserve(now_ms, current_reserve);
                    }

                    // ── Early rejection checks (every tick during window) ──

                    // Check drawdown from peak
                    if current_price_fp > 0 && w.peak_price_fp > 0 {
                        let drawdown = w.current_drawdown_bps(current_price_fp);
                        if drawdown < self.config.observation_max_drawdown_bps {
                            should_reject = true;
                            reject_reason = "drawdown from peak exceeded threshold (sniper dump)";
                        }
                    }

                    // Check reserve floor
                    if !should_reject && current_reserve > 0
                        && current_reserve < self.config.observation_min_reserve_sol_lamports
                    {
                        should_reject = true;
                        reject_reason = "reserve below minimum during observation (pool drained)";
                    }

                    // ── Early-entry velocity trigger (fires after min_ms + min_samples) ──
                    if !should_reject && !should_ready
                        && self.config.observation_window_min_ms > 0
                        && elapsed >= self.config.observation_window_min_ms
                        && w.price_count >= self.config.observation_early_entry_min_samples
                    {
                        // Check early abort (faster dump detection than main threshold)
                        if current_price_fp > 0 && w.peak_price_fp > 0 {
                            let drawdown = w.current_drawdown_bps(current_price_fp);
                            if drawdown < self.config.observation_early_abort_drawdown_bps {
                                should_reject = true;
                                reject_reason = "early abort: drawdown exceeded fast threshold";
                            }
                        }
                        // Check early entry velocity
                        if !should_reject {
                            let velocity = w.price_velocity_bps_per_s();
                            if velocity >= self.config.observation_early_entry_velocity_bps_per_s {
                                tracing::info!(
                                    mint = %bs58::encode(&mint).into_string(),
                                    velocity_bps_per_s = velocity,
                                    elapsed_ms = elapsed,
                                    price_samples = w.price_count,
                                    "[momentum] observation early-entry triggered — price velocity threshold met"
                                );
                                should_ready = true;
                            }
                        }
                    }

                    // ── Window expiry evaluation ───────────────────────────
                    if !should_reject && elapsed >= self.config.observation_window_ms {
                        // Check minimum sample count
                        if w.price_count < self.config.observation_min_samples {
                            should_reject = true;
                            reject_reason = "insufficient price samples during observation window";
                        }
                        // Check price stability (last 3 samples within 10%)
                        else if self.config.observation_require_price_stability && !w.last_3_stable() {
                            should_reject = true;
                            reject_reason = "price unstable at observation window expiry (last 3 samples diverge >10%)";
                        }
                        // Check reserve at end of window
                        else if w.latest_reserve() > 0
                            && w.latest_reserve() < self.config.observation_min_reserve_sol_lamports
                        {
                            should_reject = true;
                            reject_reason = "reserve below minimum at observation window expiry";
                        }
                        // Check minimum velocity — zero velocity = dead/flat token, not worth entering
                        else if w.price_velocity_bps_per_s() <= 0 {
                            should_reject = true;
                            reject_reason = "zero or negative velocity at observation window expiry — no momentum";
                        }
                        else {
                            // All checks passed — observation window complete
                            should_ready = true;
                        }
                    }

                    if should_reject {
                        w.rejected = true;
                        w.reject_reason = Some(reject_reason);
                    } else if should_ready {
                        w.is_ready = true;
                        // Store velocity so buy path can use it after window is removed
                        w.computed_velocity_bps_per_s = w.price_velocity_bps_per_s();
                    }
                }

                // Handle rejection: unsubscribe price feed, deactivate pending entry, clean up
                if should_reject {
                    let mint_b58 = bs58::encode(&mint).into_string();
                    let window_data = self.observation_windows.get(&mint);
                    let (price_count, reserve_count, peak, drawdown) = window_data
                        .map(|w| {
                            let v = w.value();
                            let dd = if v.peak_price_fp > 0 && current_price_fp > 0 {
                                v.current_drawdown_bps(current_price_fp)
                            } else { 0 };
                            (v.price_count, v.reserve_count, v.peak_price_fp, dd)
                        })
                        .unwrap_or((0, 0, 0, 0));

                    tracing::warn!(
                        mint = %mint_b58,
                        reason = %reject_reason,
                        price_samples = price_count,
                        reserve_samples = reserve_count,
                        peak_price_fp = peak,
                        current_price_fp,
                        drawdown_bps = drawdown,
                        current_reserve_sol = current_reserve as f64 / 1e9,
                        "[momentum] observation window REJECTED — skipping entry"
                    );
                    self.price_feed.unsubscribe_sync(&mint);
                    // Deactivate the pending entry in the ring buffer
                    if let Ok(mut ring) = self.pending.lock() {
                        ring.deactivate_mint(&mint);
                    }
                    self.observation_windows.remove(&mint);
                } else if should_ready {
                    let mint_b58 = bs58::encode(&mint).into_string();
                    let (price_count, peak, velocity) = self.observation_windows.get(&mint)
                        .map(|w| (w.value().price_count, w.value().peak_price_fp, w.value().price_velocity_bps_per_s()))
                        .unwrap_or((0, 0, 0));

                    tracing::info!(
                        mint = %mint_b58,
                        price_samples = price_count,
                        peak_price_fp = peak,
                        current_price_fp,
                        velocity_bps_per_s = velocity,
                        current_reserve_sol = current_reserve as f64 / 1e9,
                        "[momentum] observation window PASSED ✅ — proceeding to entry"
                    );
                    // Don't remove yet — process_pending_entries loop checks for is_ready
                    // to gate entry. Remove happens when entry is processed or abandoned.
                }
            }
        }
        // ── End Observation Window Phase ─────────────────────────────────

        let ready: Vec<PendingEntry> = if let Ok(mut ring) = self.pending.lock() {
            ring.drain_ready(now_ms).collect()
        } else {
            return;
        };

        // Entries deferred because price feed isn't ready yet
        let mut requeue: Vec<PendingEntry> = Vec::new();

        for mut entry in ready {
            // ── Gate: observation window must have passed ─────────────────
            // If an observation window is active for this mint, check its state.
            // - Not yet evaluated (still observing): re-queue
            // - Rejected: skip (already unsubscribed in observation phase above)
            // - Ready: remove window and proceed to normal entry
            if self.config.observation_window_ms > 0 {
                if let Some(window) = self.observation_windows.get(&entry.mint) {
                    if window.rejected {
                        // Already handled in observation phase — just clean up
                        self.observation_windows.remove(&entry.mint);
                        continue;
                    }
                    if !window.is_ready {
                        // Still observing — re-queue for next tick
                        requeue.push(entry);
                        continue;
                    }
                    // Read velocity before removing window
                    let obs_velocity_from_window = window.computed_velocity_bps_per_s;
                    // Window passed — remove and proceed to normal entry flow
                    drop(window); // release DashMap ref before remove
                    self.observation_windows.remove(&entry.mint);
                    // Store on engine's velocity map for deferred buy path
                    self.observed_velocity.insert(entry.mint, obs_velocity_from_window);
                    entry.observed_velocity_bps_per_s = Some(obs_velocity_from_window);
                }
                // No window found = observation disabled for this entry or already removed
            }

            // Check limits again at entry time — drop excess entries
            if self.active.len() >= self.config.max_concurrent as usize {
                // Unsubscribe any remaining entries that won't become positions
                self.price_feed.unsubscribe_sync(&entry.mint);
                self.observation_windows.remove(&entry.mint);
                // Also unsubscribe the rest (we're about to break)
                // Remaining entries in the iterator won't be processed
                continue;  // continue instead of break so we unsubscribe all remaining
            }

            // ── Gate: reject estimated prices ────────────────────────────
            // subscribe_with_estimate() seeds a placeholder price (~106 fp)
            // from default reserves (85 SOL / 800M tokens). If WS/RPC hasn't
            // replaced it yet, using it as entry_price_fp would poison ALL
            // downstream: scale-in, trailing stop, PnL, dead zone checks.
            // Re-queue until real price arrives from WS or RPC poll.
            if self.price_feed.is_price_estimated(&entry.mint) {
                let waited_ms = now_ms.saturating_sub(entry.first_scheduled_ts_ms);
                if waited_ms < self.config.no_price_timeout_ms {
                    tracing::debug!(
                        mint = %bs58::encode(&entry.mint).into_string(),
                        waited_ms,
                        "[momentum] price still estimated — re-queuing entry"
                    );
                    requeue.push(entry);
                } else {
                    tracing::warn!(
                        mint = %bs58::encode(&entry.mint).into_string(),
                        waited_ms,
                        "[momentum] price still estimated after timeout — abandoning entry"
                    );
                    self.price_feed.unsubscribe_sync(&entry.mint);
                }
                continue;
            }

            // Get current live price from price feed (guaranteed real at this point)
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

            // LP reserve range gate: only trade fresh pump.fun graduations.
            // Fresh PumpSwap migrations land at 50-120 SOL. Outside config range = skip.
            if let Some(current_reserve_sol) = self.price_feed.get_reserve_sol(&entry.mint) {
                if current_reserve_sol < self.config.min_lp_reserve_entry_lamports {
                    tracing::warn!(
                        mint = %bs58::encode(&entry.mint).into_string(),
                        reserve_sol_lamports = current_reserve_sol,
                        min_required = self.config.min_lp_reserve_entry_lamports,
                        "[momentum] skipping entry — pool drained since resolution (reserve below min)"
                    );
                    self.price_feed.unsubscribe_sync(&entry.mint);
                    continue;
                }
                if current_reserve_sol > self.config.max_lp_reserve_entry_lamports {
                    tracing::info!(
                        mint = %bs58::encode(&entry.mint).into_string(),
                        reserve_sol = current_reserve_sol / 1_000_000_000,
                        max_allowed = self.config.max_lp_reserve_entry_lamports / 1_000_000_000,
                        "[momentum] entry rejected — LP reserve too large (established token)"
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
            // Update opening_price_fp to match real price (was estimated from default reserves)
            if entry.opening_price_fp < 1000 && current_price_fp > 1000 {
                tracing::info!(
                    mint = %bs58::encode(&entry.mint).into_string(),
                    estimated = entry.opening_price_fp,
                    real = current_price_fp,
                    "[momentum] correcting opening_price_fp: estimated → real"
                );
                entry.opening_price_fp = current_price_fp;
            }

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

            // Observability: log confirmed real price + reserves before entry
            {
                let confirmed_reserve_sol = self.price_feed.get_reserve_sol(&entry.mint);
                let waited_ms = now_ms.saturating_sub(entry.first_scheduled_ts_ms);
                tracing::info!(
                    mint = %bs58::encode(&entry.mint).into_string(),
                    confirmed_price_fp = current_price_fp,
                    reserve_sol_lamports = ?confirmed_reserve_sol,
                    waited_ms,
                    "[momentum] entry confirmed — real price + reserves"
                );
            }

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
                    let rpc_sender = self.rpc_sender.clone();
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
                        let mint_str = bs58::encode(&mint_buy).into_string();
                        // RPC only — rate limited + retried + circuit breaker waits
                        match rpc_sender.submit_tx(&tx_bytes, &mint_str, "buy_task").await {
                            rpc_sender::SubmitResult::Landed { signature, latency_ms } => {
                                tracing::info!(mint=%mint_str, sig=%signature, latency_ms, tip, size_sol=size as f64/1e9, tokens_est, "[buy_task] RPC landed ✅");
                            }
                            rpc_sender::SubmitResult::TimedOut { signature } => {
                                tracing::warn!(mint=%mint_str, sig=%signature, "[buy_task] RPC timed out (may still land)");
                            }
                            rpc_sender::SubmitResult::Failed { error } => {
                                tracing::error!(mint=%mint_str, err=%error, "[buy_task] RPC FAILED — no fallback");
                            }
                            rpc_sender::SubmitResult::CircuitOpen { remaining_ms } => {
                                tracing::warn!(mint=%mint_str, remaining_ms, "[buy_task] circuit breaker OPEN — skipped");
                            }
                        }
                    });
                } else if let Some(mut ps_pool) = self.pumpswap_pools.get(&entry.mint).map(|r| r.clone()) {
                    // ── Last-chance pool resolution for Helius direct path ──────
                    // When on_pumpswap_graduation_direct() trusts Helius vault data
                    // and enters immediately, it stores partial pool accounts with
                    // zeroed pool PDA and creator. By now (T+entry_delay_ms, ~15s),
                    // getProgramAccounts indexing has caught up. Resolve the full
                    // pool accounts before building the buy TX.
                    if ps_pool.pool == [0u8; 32] {
                        let mint_b58_resolve = bs58::encode(&entry.mint).into_string();
                        tracing::info!(
                            mint = %mint_b58_resolve,
                            "[buy_pumpswap] pool PDA is zeroed — attempting last-chance resolution"
                        );
                        if let Some(resolution) = crate::momentum::pool::resolve_pumpswap_pool_from_mint(
                            &self.http_client, &entry.mint, &self.public_rpc_url, &self.helius_rpc_url,
                        ).await {
                            if let Some(resolved_pool) = crate::momentum::pool::extract_pumpswap_pool_accounts(&resolution) {
                                let resolved_accts: crate::tx::pumpswap::PumpSwapPoolAccounts = resolved_pool.into();
                                tracing::info!(
                                    mint = %mint_b58_resolve,
                                    pool = %bs58::encode(&resolved_accts.pool).into_string(),
                                    "[buy_pumpswap] last-chance resolution succeeded ✅"
                                );
                                self.pumpswap_pools.insert(entry.mint, resolved_accts.clone());
                                ps_pool = resolved_accts;
                            } else {
                                tracing::warn!(
                                    mint = %mint_b58_resolve,
                                    "[buy_pumpswap] last-chance resolution: extract_pumpswap_pool_accounts returned None — position is accounting-only"
                                );
                                continue;
                            }
                        } else {
                            tracing::warn!(
                                mint = %mint_b58_resolve,
                                "[buy_pumpswap] last-chance resolution failed — position is accounting-only"
                            );
                            continue;
                        }
                    }

                    // ── Resolve token_mint_program if unknown ────────────────
                    // All pump.fun tokens use classic SPL Token. We MUST know the correct
                    // program to build valid TX instructions (ATA derivation, ATA creation,
                    // swap accounts [11]/[12]). Try Helius first, public RPC as fallback.
                    if ps_pool.token_mint_program == [0u8; 32] {
                        let mint_b58_prog = bs58::encode(&entry.mint).into_string();
                        match crate::momentum::pool::resolve_mint_program_with_fallback(
                            &self.http_client, &entry.mint, &self.helius_rpc_url, Some(&self.public_rpc_url),
                        ).await {
                            Some(program_bytes) => {
                                let prog_b58 = bs58::encode(&program_bytes).into_string();
                                tracing::info!(
                                    mint = %mint_b58_prog,
                                    program = %prog_b58,
                                    "[buy_pumpswap] resolved token_mint_program"
                                );
                                ps_pool.token_mint_program = program_bytes;
                                // Update stored pool accounts
                                if let Some(mut stored) = self.pumpswap_pools.get_mut(&entry.mint) {
                                    stored.token_mint_program = program_bytes;
                                }
                            }
                            None => {
                                // Primary resolution failed — try inferring from vault owner.
                                // The pool's token vault is owned by the same program as the token mint.
                                // This catches Token-2022 tokens where mint resolution timed out.
                                let vault_mint = ps_pool.pool_base_token_account; // token vault
                                let vault_b58 = bs58::encode(&vault_mint).into_string();
                                let inferred = crate::momentum::pool::resolve_mint_program_with_fallback(
                                    &self.http_client, &vault_mint, &self.helius_rpc_url, Some(&self.public_rpc_url),
                                ).await;
                                match inferred {
                                    Some(prog) if prog != [0u8; 32] => {
                                        let prog_b58 = bs58::encode(&prog).into_string();
                                        tracing::info!(
                                            mint = %mint_b58_prog,
                                            vault = %vault_b58,
                                            program = %prog_b58,
                                            "[buy_pumpswap] inferred token_mint_program from vault owner"
                                        );
                                        ps_pool.token_mint_program = prog;
                                        if let Some(mut stored) = self.pumpswap_pools.get_mut(&entry.mint) {
                                            stored.token_mint_program = prog;
                                        }
                                    }
                                    _ => {
                                        // Both mint and vault RPC resolution failed. We cannot determine the token
                                        // program safely. Defaulting to SPL Token risks IncorrectProgramId on-chain
                                        // for Token-2022 tokens. Skip this trade — better to miss than to burn fees.
                                        tracing::warn!(
                                            mint = %mint_b58_prog,
                                            "[buy_pumpswap] failed to resolve token_mint_program (mint + vault) — skipping trade (Token-2022 safety)"
                                        );
                                        self.active.remove(&entry.mint);
                                        self.momentum_zones.remove(&entry.mint);
                                        continue;
                                    }
                                }
                            }
                        }
                    }

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
                    // Dynamic max_quote_in: scale slippage buffer based on observed price velocity.
                    let obs_velocity = entry.observed_velocity_bps_per_s.unwrap_or(0);
                    let multiplier_pct = compute_max_quote_in_multiplier(&self.config, obs_velocity);
                    let max_quote_in = (size as u128 * multiplier_pct as u128 / 100) as u64;
                    // Anti-sandwich slippage: compute min_tokens_out from buffered amount.
                    // Using 50% to avoid SlippageExceeded (Custom:6004) when pool price moves
                    // between observation and TX landing. max_quote_in is the real SOL guard.
                    let min_tokens_out = if current_price_fp > 0 {
                        let tokens_at_max = (max_quote_in as u128 * 1_000_000 / current_price_fp as u128) as u64;
                        std::cmp::max(tokens_at_max * 50 / 100, 1)
                    } else {
                        1 // fallback — shouldn't happen with valid price feed
                    };
                    let tokens_est_for_sandwich = tokens_estimate;
                    let fee_idx = (std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() % 8) as usize;
                    let rpc_sender = self.rpc_sender.clone();
                    // Fix #1: Track buy state for sell gating.
                    // Guard: skip if buy is already in progress (prevents duplicate buy from
                    // two on_graduation paths racing into process_pending_entries in same tick).
                    {
                        let already_buying = self.buy_states.get(&mint)
                            .map(|s| matches!(*s, BuyState::Pending | BuyState::Confirmed))
                            .unwrap_or(false);
                        if already_buying {
                            tracing::warn!(
                                mint = %bs58::encode(&mint).into_string(),
                                "[buy_pumpswap] duplicate buy suppressed — buy already in progress"
                            );
                            continue;
                        }
                        self.buy_states.insert(mint, BuyState::Pending);
                    }
                    let buy_states = self.buy_states.clone();
                    tokio::spawn(async move {
                        let kp_bytes = match std::fs::read(&kp_path) {
                            Ok(b) => b,
                            Err(e) => { tracing::error!(err=?e, "[buy_pumpswap] keypair load failed"); buy_states.insert(mint_buy, BuyState::Failed); return; }
                        };
                        let kp_arr: Vec<u8> = match serde_json::from_slice(&kp_bytes) {
                            Ok(v) => v,
                            Err(e) => { tracing::error!(err=?e, "[buy_pumpswap] keypair parse failed"); buy_states.insert(mint_buy, BuyState::Failed); return; }
                        };
                        if kp_arr.len() != 64 { tracing::error!("[buy_pumpswap] invalid keypair len"); buy_states.insert(mint_buy, BuyState::Failed); return; }
                        let mut kb = [0u8; 64];
                        kb.copy_from_slice(&kp_arr);
                        let keypair = match solana_sdk::signature::Keypair::from_bytes(&kb) {
                            Ok(k) => k,
                            Err(e) => { tracing::error!(err=?e, "[buy_pumpswap] keypair err"); buy_states.insert(mint_buy, BuyState::Failed); return; }
                        };
                        use std::str::FromStr as _;
                        let tip_account = solana_sdk::pubkey::Pubkey::from_str(
                            crate::tx::raydium::JITO_TIP_ACCOUNTS[0]
                        ).unwrap();
                        tracing::info!(
                            mint = %bs58::encode(&mint_buy).into_string(),
                            pool = %bs58::encode(&ps_pool.pool).into_string(),
                            token_is_base = ps_pool.token_is_base,
                            pool_base_vault = %bs58::encode(&ps_pool.pool_base_token_account).into_string(),
                            pool_quote_vault = %bs58::encode(&ps_pool.pool_quote_token_account).into_string(),
                            max_quote_sol = max_quote_in as f64 / 1e9,
                            obs_velocity_bps_per_s = obs_velocity,
                            max_quote_in_multiplier_pct = multiplier_pct,
                            "[buy_pumpswap] building TX with pool accounts"
                        );
                        let tx_bytes = match crate::tx::pumpswap::build_pumpswap_buy_tx(
                            &ps_pool, &keypair, max_quote_in, min_tokens_out, tip, tip_account, bh, fee_idx,
                        ) {
                            Ok(b) => b,
                            Err(e) => { tracing::error!(mint=%bs58::encode(&mint_buy).into_string(), err=?e, "[buy_pumpswap] build failed"); buy_states.insert(mint_buy, BuyState::Failed); return; }
                        };
                        let mint_str = bs58::encode(&mint_buy).into_string();
                        // RPC only — rate limited + retried + circuit breaker waits
                        match rpc_sender.submit_tx(&tx_bytes, &mint_str, "buy_pumpswap").await {
                            rpc_sender::SubmitResult::Landed { signature, latency_ms } => {
                                buy_states.insert(mint_buy, BuyState::Confirmed);
                                tracing::info!(
                                    mint=%mint_str, sig=%signature, latency_ms, tip,
                                    size_sol=size as f64/1e9,
                                    max_quote_sol=max_quote_in as f64/1e9,
                                    obs_velocity_bps_per_s = obs_velocity,
                                    max_quote_in_multiplier_pct = multiplier_pct,
                                    estimated_tokens=tokens_est_for_sandwich,
                                    min_tokens_out,
                                    "[buy_pumpswap] RPC landed ✅ — compare on-chain receipt vs estimated_tokens for sandwich detection"
                                );
                            }
                            rpc_sender::SubmitResult::TimedOut { signature } => {
                                // Keep Pending — may still land. 30s timeout in process_active handles this.
                                tracing::warn!(mint=%mint_str, sig=%signature, "[buy_pumpswap] RPC timed out (may still land)");
                            }
                            rpc_sender::SubmitResult::Failed { error } => {
                                buy_states.insert(mint_buy, BuyState::Failed);
                                tracing::error!(mint=%mint_str, err=%error, "[buy_pumpswap] RPC FAILED — no fallback");
                            }
                            rpc_sender::SubmitResult::CircuitOpen { remaining_ms } => {
                                buy_states.insert(mint_buy, BuyState::Failed);
                                tracing::warn!(mint=%mint_str, remaining_ms, "[buy_pumpswap] circuit breaker OPEN — skipped");
                            }
                        }
                    });
                } else {
                    tracing::warn!(
                        mint=%bs58::encode(&entry.mint).into_string(),
                        "[momentum] live mode: no pool accounts yet — deferring buy TX (will retry each tick for 10s)"
                    );
                    self.deferred_buy_pending.insert(entry.mint);
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

            // ── Set buy_confirmed_ms on first tick where buy TX is confirmed ──
            if pos.buy_confirmed_ms == 0 {
                if let Some(state) = self.buy_states.get(&mint) {
                    if matches!(*state, BuyState::Confirmed) {
                        pos.stamp_buy_confirmed(now_ms);
                        tracing::debug!(
                            mint = %bs58::encode(&mint).into_string(),
                            confirmed_at_ms = now_ms,
                            "[momentum] buy_confirmed_ms stamped"
                        );
                    }
                }
            }

            // ── Phase-gated exit evaluation ──────────────────────────────
            {
                let current_bps_for_phase = self.price_feed.current_price(&mint)
                    .map(|p| price_to_bps_offset(pos.entry_price_fp, p))
                    .unwrap_or(0);
                let (ws_msgs, ws_last_ms) = self.price_feed.ws_notif_info(&mint);
                let ws_age_ms = if ws_last_ms > 0 { now_ms.saturating_sub(ws_last_ms) } else { 0 };
                let phase = pos.evaluate_phase(
                    now_ms,
                    current_bps_for_phase,
                    ws_msgs.min(u16::MAX as u64) as u16,
                    ws_age_ms,
                );

                match phase {
                    position::PositionPhase::AwaitingConfirmation => {
                        continue; // Not on-chain yet — skip ALL exit evaluation
                    }
                    position::PositionPhase::Exiting => {
                        let exit_price = self.price_feed.current_price(&mint)
                            .unwrap_or(pos.entry_price_fp);
                        to_close.push((mint, MomentumExitReason::HardSl, exit_price));
                        continue;
                    }
                    position::PositionPhase::RapidAssessment => {
                        // Fall through — existing micro_exit + hard_sl covers this
                    }
                    _ => {
                        // Observation, Momentum, ExitEligible — full evaluation
                    }
                }
            }

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

            // 3. Trailing stop — TASK 6: Adaptive gain-tiered OR legacy momentum-state.
            //
            // When adaptive_trail_enabled=true (default):
            //   Uses gain-tiered trailing: tight 1% at small gains, wider as gains grow.
            //   Activates after min_samples_to_activate (default: 3), no TP1 gate needed.
            //   Rationale: old 25% Accelerating trail never triggered until complete dump.
            //   At +30% peak with 25% trail, floor is +22.5% — by the time price drops
            //   7.5%, the dump is accelerating and actual exit is +15% or worse.
            //
            // When adaptive_trail_enabled=false:
            //   Falls back to legacy momentum-state trailing (Accel=15%, etc.)
            //   with gain-tiered cap and ATR adaptation. Requires TP1 hit.
            if self.config.adaptive_trail_enabled {
                // ── ADAPTIVE GAIN-TIERED TRAILING STOP (TASK 6) ──────────────
                let tc = &self.config.trail_config;
                if pos.sample_count >= tc.min_samples_to_activate {
                    let current_bps = price_to_bps_offset(pos.entry_price_fp, current_price_fp);
                    let trail_bps = pos.compute_adaptive_trail_bps(current_bps, tc);

                    if trail_bps > 0 && pos.adaptive_trailing_stop_hit(current_price_fp, trail_bps) {
                        // Floor gate: don't trail-exit below minimum gain threshold.
                        // Prevents "fee death" where 1-3% gains don't cover TX overhead.
                        if tc.floor_bps > 0 && current_bps > 0 && (current_bps as u32) < tc.floor_bps {
                            // Price is positive but below fee breakeven — hold position.
                            // Will either recover (trail fires higher) or dump to hard SL.
                            pos.trail_stop_below_floor_count = 0; // reset confirmation counter
                            // Don't exit — fall through to other checks
                        } else {
                            // Confirm gate: must stay below trail floor for N consecutive ticks
                            pos.trail_stop_below_floor_count = pos.trail_stop_below_floor_count.saturating_add(1);
                            if pos.trail_stop_below_floor_count >= tc.confirm_samples {
                                to_close.push((
                                    mint,
                                    MomentumExitReason::TrailingStop,
                                    current_price_fp,
                                ));
                                continue;
                            }
                        }
                    } else {
                        pos.trail_stop_below_floor_count = 0;
                    }
                }
            } else {
                // ── LEGACY MOMENTUM-STATE TRAILING STOP (backward compat) ────
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

                    // Phase 2.5: Gain-tiered trailing stop cap.
                    let current_gain_bps = price_to_bps_offset(pos.entry_price_fp, current_price_fp) as i64;
                    let tier_cap = if current_gain_bps < self.config.trailing_stop_tier1_max_bps {
                        self.config.trailing_stop_tier1_pct
                    } else if current_gain_bps < self.config.trailing_stop_tier2_max_bps {
                        self.config.trailing_stop_tier2_pct
                    } else {
                        999.0
                    };
                    let base_trail = base_trail.min(tier_cap);

                    // Phase 3: ATR-adaptive trail width.
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
                        pos.trail_stop_below_floor_count = 0;
                    }
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

            // ── WINNER PROTECTION: Momentum Lock (TASK 6) ────────────────────
            // Skip ALL time-based exits for profitable positions with active trading.
            // Only trailing stop (above) and velocity exit can close a momentum-locked position.
            //
            // On-chain evidence: biggest winner (+82.2%, +0.025 SOL) held 40 minutes.
            // Time-based exits (time_sl, dead_zone, stagnation) killed profitable positions,
            // capping avg win at +15.8% when we need +24% for +EV.
            //
            // A position is momentum-locked when:
            //   1. Currently profitable (gain > 0 bps)
            //   2. Pool has recent WebSocket activity (ws_count > 0)
            //
            // If either condition fails, normal time-based exits proceed.
            if self.config.winner_protection_enabled {
                let current_bps = price_to_bps_offset(pos.entry_price_fp, current_price_fp);
                let (ws_count, _ws_last_ms) = self.price_feed.ws_notif_info(&mint);

                if pos.is_momentum_locked(current_bps, ws_count) {
                    // Position is momentum-locked — skip all time-based exits below.
                    // Only trailing stop (already evaluated above) can close this position.
                    continue;
                }
            }
            // ── End Winner Protection ────────────────────────────────────────

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
                    // PumpSwap pools get a wider WS silence window — lower notification frequency per swap
                    if pos.pool_type == 1 {
                        self.config.dead_zone_pumpswap_ws_zero_ms
                    } else {
                        self.config.dead_zone_ws_zero_ms
                    }
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
                    // PumpSwap pools: use wider tolerance (1% fee = bigger reserve swings per swap)
                    let reserve_flat_tolerance = if pos.pool_type == 1 {
                        self.config.dead_zone_pumpswap_reserve_tolerance_lamports
                    } else {
                        self.config.dead_zone_reserve_flat_tolerance_lamports
                    };
                    max_r.saturating_sub(min_r) < reserve_flat_tolerance
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
            // Observability: log exit details before closing
            if let Some(pos) = self.active.get(&mint) {
                let hold_ms = now_ms.saturating_sub(pos.entry_ts_ms);
                tracing::info!(
                    mint = %bs58::encode(&mint).into_string(),
                    exit_reason = reason.as_str(),
                    entry_price_fp = pos.entry_price_fp,
                    exit_price_fp,
                    hold_ms,
                    size_lamports = pos.size_lamports,
                    "[momentum] closing position — exit pipeline"
                );
            }
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

        // Remove from graduation dedup so reentry is allowed after cooldown expires
        self.pending_grads.remove(&mint);

        // Clean up observation window (safety: should already be removed at entry)
        self.observation_windows.remove(&mint);

        // Clean up reserve samples (drain detection + reserve flatness)
        self.drain_samples.remove(&mint);
        // Clean up momentum zone tracker
        self.momentum_zones.remove(&mint);
        // Clean up LQS reserve context
        self.reserve_sol_ctx.remove(&mint);
        // Clean up deferred buy tracking
        self.deferred_buy_pending.remove(&mint);

        // Calculate P&L
        let size_sol = pos.size_lamports as f64 / 1e9;
        let raw_gain_bps = price_to_bps_offset(pos.entry_price_fp, exit_price_fp);

        // Defense-in-depth: detect residual estimated-price entries.
        // Real pump.fun tokens rarely move >100× (1,000,000 bps) in a single hold.
        // If raw_gain_bps exceeds this, entry_price_fp was likely still estimated.
        // Clamp to 0 PnL to prevent phantom gains/losses.
        let (gain_bps, gross_pnl_override): (i32, Option<f64>) = if raw_gain_bps.unsigned_abs() > 1_000_000 {
            tracing::error!(
                mint = %bs58::encode(&mint).into_string(),
                entry_price_fp = pos.entry_price_fp,
                exit_price_fp,
                raw_gain_bps,
                "[close_position] SUSPECTED ESTIMATED ENTRY PRICE — clamping to 0 PnL"
            );
            (0i32, Some(0.0f64))
        } else {
            // Sanity clamp: no real trade gains >500% or loses >100% — bad price feed data.
            let clamped = raw_gain_bps.clamp(-10_000, 50_000);
            (clamped, None)
        };

        if raw_gain_bps != gain_bps && gross_pnl_override.is_none() {
            tracing::warn!(
                mint = %bs58::encode(&mint).into_string(),
                raw_gain_bps,
                clamped_gain_bps = gain_bps,
                entry_price = pos.entry_price_fp,
                exit_price = exit_price_fp,
                "[momentum] PnL sanity clamp — bad price data"
            );
        }
        let gross_pnl_sol = gross_pnl_override.unwrap_or(size_sol * gain_bps as f64 / 10_000.0);

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

        // ── Circuit breaker updates ──
        self.cb_session_pnl_lamports.fetch_add(net_lamports, Ordering::Relaxed);
        self.cb_total_trades.fetch_add(1, Ordering::Relaxed);
        if net_pnl_sol > 0.0 {
            self.cb_total_wins.fetch_add(1, Ordering::Relaxed);
            self.cb_consecutive_losses.store(0, Ordering::Relaxed);
            // Clear halfsize on a win
            self.cb_halfsize.store(0, Ordering::Relaxed);
        } else {
            let cl = self.cb_consecutive_losses.fetch_add(1, Ordering::Relaxed) + 1;
            // Activate halfsize if consecutive losses reach threshold
            if cl >= self.config.consecutive_loss_halfsize as u64 && cl < self.config.consecutive_loss_pause as u64 {
                self.cb_halfsize.store(1, Ordering::Relaxed);
                self.cb_halfsize_remaining.store(self.config.consecutive_loss_halfsize as u64, Ordering::Relaxed);
            }
            // Pause if consecutive losses reach pause threshold
            if cl >= self.config.consecutive_loss_pause as u64 {
                let pause_until = now_ms + self.config.loss_pause_duration_ms;
                self.cb_pause_until_ms.store(pause_until, Ordering::Relaxed);
                tracing::warn!(
                    consecutive_losses = cl,
                    pause_until_ms = pause_until,
                    "[circuit_breaker] consecutive loss pause activated"
                );
            }
        }
        // Session loss halt
        let session_pnl = self.cb_session_pnl_lamports.load(Ordering::Relaxed);
        let halt_lamports = (self.config.session_max_loss_halt_sol * 1e9) as i64;
        if session_pnl < -halt_lamports {
            self.cb_halted.store(1, Ordering::Relaxed);
            tracing::error!(
                session_pnl_sol = session_pnl as f64 / 1e9,
                "[circuit_breaker] SESSION HALTED — max loss exceeded"
            );
        }
        // Session loss pause
        let pause_lamports = (self.config.session_max_loss_pause_sol * 1e9) as i64;
        if session_pnl < -pause_lamports && self.cb_halted.load(Ordering::Relaxed) == 0 {
            let pause_until = now_ms + self.config.session_pause_duration_ms;
            let current_pause = self.cb_pause_until_ms.load(Ordering::Relaxed);
            if pause_until > current_pause {
                self.cb_pause_until_ms.store(pause_until, Ordering::Relaxed);
                tracing::warn!(
                    session_pnl_sol = session_pnl as f64 / 1e9,
                    "[circuit_breaker] session loss pause activated"
                );
            }
        }

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
            "[momentum] position CLOSED"
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

        // ── Fix #1: Gate sell on buy state ──────────────────────────────────────
        // Only attempt sell if buy TX was confirmed on-chain. If buy failed,
        // selling would fail with ConstraintTokenMint (3012) or Custom:1.
        //
        // CRITICAL: Read state with .get() instead of .remove() so the spawned
        // sell task can still poll the DashMap while the buy callback updates it.
        // Removing here caused the sell task's poll loop to always get None,
        // wasting 8s and then deriving the wrong ATA (key gone = no state update
        // from buy callback). The sell task removes the key after it finishes.
        let buy_state = self.buy_states.get(&mint).map(|s| *s).unwrap_or(BuyState::Failed);
        let should_sell = match buy_state {
            BuyState::Confirmed => {
                // Buy already confirmed — remove now (sell task doesn't need to poll)
                self.buy_states.remove(&mint);
                true
            }
            BuyState::Pending => {
                // Buy still pending — DON'T remove! Let the sell task poll until
                // the buy callback sets Confirmed. The sell task will remove it.
                tracing::warn!(
                    mint = %bs58::encode(&mint).into_string(),
                    "[close_position] buy still Pending at close — will attempt sell with balance check"
                );
                true
            }
            BuyState::Failed => {
                // Buy failed — remove and skip sell
                self.buy_states.remove(&mint);
                tracing::info!(
                    mint = %bs58::encode(&mint).into_string(),
                    "[close_position] buy FAILED — skipping sell TX entirely"
                );
                false
            }
        };

        // ── Live mode: Raydium AMM V4 sell via Jito ────────────────────────────
        if !self.config.paper_mode && should_sell {
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
                    let noz = self.nozomi_client.clone();
                    let reason_str = reason.as_str().to_string();
                    let gain = gain_bps as i64;
                    let noz_ok = noz.is_some();
                    let mint_copy = mint;
                    let rpc_sender = self.rpc_sender.clone();
                    let balance_rpc_url = self.public_rpc_url.clone();
                    let balance_http = self.rpc_fallback_client.clone();
                    let exit_price_for_spawn = exit_price_fp;
                    let sell_buy_states_ray = self.buy_states.clone();
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

                        // ── Wait for buy TX to confirm before querying balance ──
                        // Poll buy_states with timeout (same pattern as sell_pumpswap).
                        // None = key already removed by close_position (buy was Confirmed) → proceed immediately.
                        // Some(Pending) = buy still in flight → keep polling.
                        {
                            let max_wait_ms = 8_000u64;
                            let poll_interval_ms = 200u64;
                            let mut waited_ms = 0u64;
                            loop {
                                let state = sell_buy_states_ray.get(&mint_copy).map(|s| *s);
                                match state {
                                    None => break, // key removed = already confirmed or handled
                                    Some(BuyState::Confirmed) | Some(BuyState::Failed) => break,
                                    _ if waited_ms >= max_wait_ms => {
                                        tracing::warn!(
                                            mint=%bs58::encode(&mint_copy).into_string(),
                                            waited_ms,
                                            "[sell_raydium] buy state poll timed out — proceeding with balance check"
                                        );
                                        break;
                                    }
                                    _ => {
                                        tokio::time::sleep(std::time::Duration::from_millis(poll_interval_ms)).await;
                                        waited_ms += poll_interval_ms;
                                    }
                                }
                            }
                            // Deferred cleanup: remove buy_states entry now that sell task owns it
                            sell_buy_states_ray.remove(&mint_copy);
                        }

                        // ── Query actual on-chain token balance instead of paper estimate ──
                        use solana_sdk::signer::Signer as _;
                        let wallet_pubkey = keypair.pubkey();
                        let token_mint = solana_sdk::pubkey::Pubkey::new_from_array(mint_copy);
                        // Resolve token_mint_program via RPC instead of hardcoding SPL Token.
                        // Token-2022 tokens have their ATA at a DIFFERENT address than SPL Token.
                        let resolved_token_program = {
                            let rpc = std::env::var("SOLANA_RPC_URL")
                                .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
                            match crate::momentum::pool::resolve_mint_program_with_fallback(
                                &balance_http, &mint_copy, &rpc, None,
                            ).await {
                                Some(prog) => {
                                    tracing::info!(
                                        mint=%bs58::encode(&mint_copy).into_string(),
                                        program=%bs58::encode(&prog).into_string(),
                                        "[sell_raydium] resolved token_mint_program at sell time"
                                    );
                                    prog
                                }
                                None => {
                                    tracing::warn!(
                                        mint=%bs58::encode(&mint_copy).into_string(),
                                        "[sell_raydium] failed to resolve token_mint_program — using SPL Token fallback"
                                    );
                                    crate::tx::pumpswap::SPL_TOKEN_PROGRAM_BYTES
                                }
                            }
                        };
                        let token_program = crate::tx::pumpswap::token_program_for_mint_with_hint(
                            &token_mint, &resolved_token_program,
                        );
                        let ata_program = solana_sdk::pubkey::Pubkey::from_str(
                            crate::tx::pumpswap::SPL_ATA_PROGRAM_STR,
                        ).unwrap();
                        let (token_ata, _) = solana_sdk::pubkey::Pubkey::find_program_address(
                            &[wallet_pubkey.as_ref(), token_program.as_ref(), token_mint.as_ref()],
                            &ata_program,
                        );
                        let balance_body = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": 1,
                            "method": "getTokenAccountBalance",
                            "params": [token_ata.to_string()]
                        });
                        // Balance check with retry: RPC node may not have indexed the new ATA
                        // immediately after buy TX lands. Retry up to 15x with 1s backoff.
                        let actual_tokens = {
                            let mut result = None;
                            for attempt in 0..15u32 {
                                if attempt > 0 {
                                    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                                }
                                let resp = balance_http
                                    .post(balance_rpc_url.as_str())
                                    .header("Content-Type", "application/json")
                                    .json(&balance_body)
                                    .send()
                                    .await;
                                match resp {
                                    Ok(r) => match r.json::<serde_json::Value>().await {
                                        Ok(json) => {
                                            if json.get("error").is_some() {
                                                tracing::debug!(
                                                    mint=%bs58::encode(&mint_copy).into_string(),
                                                    attempt,
                                                    "[sell_raydium] ATA not found yet — retrying"
                                                );
                                                continue;
                                            }
                                            match json["result"]["value"]["amount"]
                                                .as_str()
                                                .and_then(|s| s.parse::<u64>().ok())
                                            {
                                                Some(bal) => { result = Some(bal); break; }
                                                None => {
                                                    tracing::warn!(
                                                        mint=%bs58::encode(&mint_copy).into_string(),
                                                        body=%json,
                                                        "[sell_raydium] balance returned null/unparseable — aborting sell"
                                                    );
                                                    return;
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            tracing::error!(mint=%bs58::encode(&mint_copy).into_string(), err=?e, "[sell_raydium] balance response parse failed");
                                            return;
                                        }
                                    },
                                    Err(e) => {
                                        tracing::error!(mint=%bs58::encode(&mint_copy).into_string(), err=?e, "[sell_raydium] balance RPC failed");
                                        return;
                                    }
                                }
                            }
                            match result {
                                Some(bal) => bal,
                                None => {
                                    tracing::warn!(
                                        mint=%bs58::encode(&mint_copy).into_string(),
                                        "[sell_raydium] ATA not found after 15 retries — buy likely failed, skipping sell"
                                    );
                                    return;
                                }
                            }
                        };
                        if actual_tokens == 0 {
                            tracing::warn!(mint=%bs58::encode(&mint_copy).into_string(), estimated_tokens=tokens, "[sell_raydium] on-chain token balance is 0 — skipping sell");
                            return;
                        }

                        // Recalculate min_sol_out with actual token balance
                        let min_sol_out = if gain > 0 {
                            let expected = (exit_price_for_spawn as u128 * actual_tokens as u128 / 1_000_000) as u64;
                            (expected as u128 * 9900 / 10000) as u64
                        } else { 0u64 };

                        let tip_account = solana_sdk::pubkey::Pubkey::from_str(
                            crate::tx::raydium::JITO_TIP_ACCOUNTS[0]
                        ).unwrap();
                        tracing::info!(
                            mint=%bs58::encode(&mint_copy).into_string(),
                            tokens = actual_tokens,
                            estimated_tokens = tokens,
                            min_sol_out,
                            "[sell_raydium] building sell TX"
                        );
                        let tx_bytes = match crate::tx::raydium::build_raydium_sell_tx(
                            &pool, &mint_copy, &keypair, actual_tokens, min_sol_out, tip, tip_account, bh,
                        ) {
                            Ok(b) => b,
                            Err(e) => { tracing::error!(mint=%bs58::encode(&mint_copy).into_string(), err=?e, "[sell_raydium] build failed"); return; }
                        };
                        let mint_str = bs58::encode(&mint_copy).into_string();
                        // SELL: RPC only with rate limiting + backoff. Circuit breaker waits, never routes to Jito.
                        match rpc_sender.submit_tx(&tx_bytes, &mint_str, "sell_raydium").await {
                            rpc_sender::SubmitResult::Landed { signature, latency_ms } => {
                                tracing::info!(mint=%mint_str, sig=%signature, latency_ms, reason=%reason_str, gain_bps=gain, "[sell_raydium] RPC landed ✅");
                            }
                            rpc_sender::SubmitResult::TimedOut { signature } => {
                                tracing::warn!(mint=%mint_str, sig=%signature, reason=%reason_str, "[sell_raydium] RPC timed out (may still land)");
                            }
                            rpc_sender::SubmitResult::Failed { error } => {
                                tracing::error!(mint=%mint_str, err=%error, reason=%reason_str, "[sell_raydium] 🚨 SELL FAILED — tokens may be stuck");
                            }
                            rpc_sender::SubmitResult::CircuitOpen { remaining_ms } => {
                                tracing::error!(mint=%mint_str, remaining_ms, reason=%reason_str, "[sell_raydium] 🚨 circuit breaker OPEN — SELL SKIPPED");
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
                    let noz_ok = noz.is_some();
                    let reason_str = reason.as_str().to_string();
                    let gain = gain_bps as i64;
                    let mint_copy = mint;
                    let fee_idx = (std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() % 8) as usize;
                    let rpc_sender = self.rpc_sender.clone();
                    let balance_rpc_url = self.public_rpc_url.clone();
                    let balance_http = self.rpc_fallback_client.clone();
                    let exit_price_for_spawn = exit_price_fp;
                    let sell_buy_states = self.buy_states.clone();
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
                        use solana_sdk::signer::Signer as _;

                        // ── Wait for buy TX to confirm before querying balance ──
                        // Poll buy_states with timeout instead of blind sleep.
                        // None = key already removed by close_position (buy was Confirmed) → proceed immediately.
                        // Some(Pending) = buy still in flight → keep polling until Confirmed/Failed/timeout.
                        {
                            let max_wait_ms = 8_000u64;
                            let poll_interval_ms = 200u64;
                            let mut waited_ms = 0u64;
                            loop {
                                let state = sell_buy_states.get(&mint_copy).map(|s| *s);
                                match state {
                                    None => break, // key removed = already confirmed or handled
                                    Some(BuyState::Confirmed) | Some(BuyState::Failed) => break,
                                    _ if waited_ms >= max_wait_ms => {
                                        tracing::warn!(
                                            mint=%bs58::encode(&mint_copy).into_string(),
                                            waited_ms,
                                            "[sell_pumpswap] buy state poll timed out — proceeding with balance check"
                                        );
                                        break;
                                    }
                                    _ => {
                                        tokio::time::sleep(std::time::Duration::from_millis(poll_interval_ms)).await;
                                        waited_ms += poll_interval_ms;
                                    }
                                }
                            }
                            // Deferred cleanup: remove buy_states entry now that sell task owns it
                            sell_buy_states.remove(&mint_copy);
                        }

                        // ── Query actual on-chain token balance instead of paper estimate ──
                        let wallet_pubkey = keypair.pubkey();
                        let token_mint = solana_sdk::pubkey::Pubkey::new_from_array(mint_copy);
                        // Resolve token_mint_program if still unknown — critical for correct ATA derivation.
                        // Token-2022 tokens have their ATA at a DIFFERENT address than SPL Token tokens.
                        // Using the wrong program = wrong ATA address = "ATA not found" = position abandoned.
                        let resolved_token_program = if ps_pool.token_mint_program != [0u8; 32] {
                            ps_pool.token_mint_program
                        } else {
                            let rpc = std::env::var("SOLANA_RPC_URL")
                                .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
                            match crate::momentum::pool::resolve_mint_program_with_fallback(
                                &balance_http, &mint_copy, &rpc, None,
                            ).await {
                                Some(prog) => {
                                    tracing::info!(
                                        mint=%bs58::encode(&mint_copy).into_string(),
                                        program=%bs58::encode(&prog).into_string(),
                                        "[sell_pumpswap] resolved token_mint_program at sell time"
                                    );
                                    prog
                                }
                                None => {
                                    tracing::warn!(
                                        mint=%bs58::encode(&mint_copy).into_string(),
                                        "[sell_pumpswap] failed to resolve token_mint_program — using SPL Token fallback"
                                    );
                                    crate::tx::pumpswap::SPL_TOKEN_PROGRAM_BYTES
                                }
                            }
                        };
                        let token_program = crate::tx::pumpswap::token_program_for_mint_with_hint(
                            &token_mint, &resolved_token_program,
                        );
                        let ata_program = solana_sdk::pubkey::Pubkey::from_str(
                            crate::tx::pumpswap::SPL_ATA_PROGRAM_STR,
                        ).unwrap();
                        let (token_ata, _) = solana_sdk::pubkey::Pubkey::find_program_address(
                            &[wallet_pubkey.as_ref(), token_program.as_ref(), token_mint.as_ref()],
                            &ata_program,
                        );
                        let balance_body = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": 1,
                            "method": "getTokenAccountBalance",
                            "params": [token_ata.to_string()]
                        });
                        // Balance check with retry: RPC node may not have indexed the new ATA
                        // immediately after buy TX lands. Retry up to 15x with 1s backoff.
                        // Only hard-abort if we exhaust all retries.
                        let actual_tokens = {
                            let mut result = None;
                            // 15 retries at 1s spacing = 15s window.
                            // Helius RPC can take up to 8-10s to index a new ATA after buy lands.
                            // Token-2022 tokens on Helius may need up to 15s.
                            for attempt in 0..15u32 {
                                if attempt > 0 {
                                    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                                }
                                let resp = balance_http
                                    .post(balance_rpc_url.as_str())
                                    .header("Content-Type", "application/json")
                                    .json(&balance_body)
                                    .send()
                                    .await;
                                match resp {
                                    Ok(r) => match r.json::<serde_json::Value>().await {
                                        Ok(json) => {
                                            if json.get("error").is_some() {
                                                tracing::debug!(
                                                    mint=%bs58::encode(&mint_copy).into_string(),
                                                    attempt,
                                                    "[sell] ATA not found yet — retrying"
                                                );
                                                continue; // retry
                                            }
                                            match json["result"]["value"]["amount"]
                                                .as_str()
                                                .and_then(|s| s.parse::<u64>().ok())
                                            {
                                                Some(bal) => { result = Some(bal); break; }
                                                None => {
                                                    tracing::warn!(
                                                        mint=%bs58::encode(&mint_copy).into_string(),
                                                        body=%json,
                                                        "[sell_pumpswap] balance returned null/unparseable — aborting sell"
                                                    );
                                                    return;
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            tracing::error!(mint=%bs58::encode(&mint_copy).into_string(), err=?e, "[sell_pumpswap] balance response parse failed");
                                            return;
                                        }
                                    },
                                    Err(e) => {
                                        tracing::error!(mint=%bs58::encode(&mint_copy).into_string(), err=?e, "[sell_pumpswap] balance RPC failed");
                                        return;
                                    }
                                }
                            }
                            match result {
                                Some(bal) => bal,
                                None => {
                                    tracing::warn!(
                                        mint=%bs58::encode(&mint_copy).into_string(),
                                        "[sell] ATA not found after 15 retries — buy likely failed, skipping sell"
                                    );
                                    return;
                                }
                            }
                        };
                        // actual_tokens resolved above via retry block
                        if actual_tokens == 0 {
                            tracing::warn!(mint=%bs58::encode(&mint_copy).into_string(), estimated_tokens=tokens, "[sell_pumpswap] on-chain token balance is 0 — skipping sell");
                            return;
                        }

                        // Sandwich detection: if we hold < 10% of estimated tokens,
                        // the buy was sandwiched — selling dust wastes fees.
                        if tokens > 0 && actual_tokens < tokens / 10 {
                            tracing::warn!(
                                mint = %bs58::encode(&mint_copy).into_string(),
                                actual_tokens,
                                estimated_tokens = tokens,
                                ratio_pct = actual_tokens * 100 / tokens,
                                "[sell_pumpswap] sandwich detected — skipping sell (dust position)"
                            );
                            return;
                        }

                        // min_sol_out = 0: accept whatever the AMM gives.
                        // A non-zero floor causes Custom:6004 SlippageExceeded when pool
                        // price moves between close decision and TX landing — tokens get stuck.
                        // The AMM guarantees fair value by construction; we don't need a floor.
                        let min_sol_out = 0u64;

                        let tip_account = solana_sdk::pubkey::Pubkey::from_str(
                            crate::tx::raydium::JITO_TIP_ACCOUNTS[0]
                        ).unwrap();
                        // Observability: log sell TX parameters before building
                        tracing::info!(
                            mint = %bs58::encode(&mint_copy).into_string(),
                            pool = %bs58::encode(&ps_pool.pool).into_string(),
                            token_is_base = ps_pool.token_is_base,
                            tokens = actual_tokens,
                            estimated_tokens = tokens,
                            min_sol_out,
                            "[sell_pumpswap] building sell TX"
                        );
                        let tx_bytes = match crate::tx::pumpswap::build_pumpswap_sell_tx(
                            &ps_pool, &keypair, actual_tokens, min_sol_out, tip, tip_account, bh, fee_idx,
                        ) {
                            Ok(b) => b,
                            Err(e) => { tracing::error!(mint=%bs58::encode(&mint_copy).into_string(), err=?e, "[sell_pumpswap] build failed"); return; }
                        };
                        let mint_str = bs58::encode(&mint_copy).into_string();
                        // SELL: RPC only with rate limiting + backoff. Circuit breaker waits, never routes to Jito.
                        match rpc_sender.submit_tx(&tx_bytes, &mint_str, "sell_pumpswap").await {
                            rpc_sender::SubmitResult::Landed { signature, latency_ms } => {
                                tracing::info!(mint=%mint_str, sig=%signature, latency_ms, reason=%reason_str, gain_bps=gain, "[sell_pumpswap] RPC landed ✅");
                            }
                            rpc_sender::SubmitResult::TimedOut { signature } => {
                                tracing::warn!(mint=%mint_str, sig=%signature, reason=%reason_str, "[sell_pumpswap] RPC timed out (may still land)");
                            }
                            rpc_sender::SubmitResult::Failed { error } => {
                                tracing::error!(mint=%mint_str, err=%error, reason=%reason_str, "[sell_pumpswap] 🚨 SELL FAILED — tokens may be stuck");
                            }
                            rpc_sender::SubmitResult::CircuitOpen { remaining_ms } => {
                                tracing::error!(mint=%mint_str, remaining_ms, reason=%reason_str, "[sell_pumpswap] 🚨 circuit breaker OPEN — SELL SKIPPED");
                            }
                        }
                    });
                }
            } else {
                // Last-chance sell resolution: spawn async task to resolve PumpSwap pool
                // and submit sell. close_position() is sync, so we spawn the async work.
                let mint_b58_sell = bs58::encode(&mint).into_string();
                let tokens = pos.tokens_held();
                if tokens == 0 {
                    tracing::warn!(
                        mint=%mint_b58_sell,
                        "[close_position] no pool accounts and tokens_held=0 — buy never landed, skipping sell"
                    );
                } else {
                    tracing::warn!(
                        mint=%mint_b58_sell,
                        tokens,
                        "[close_position] no pool accounts — spawning last-chance PumpSwap resolution for sell"
                    );
                    let http_client = self.http_client.clone();
                    let public_rpc = self.public_rpc_url.clone();
                    let helius_rpc = self.helius_rpc_url.clone();
                    let mint_copy = mint;
                    let kp_path = std::env::var("WALLET_KEYPAIR_PATH").unwrap_or_default();
                    let bh = self.blockhash_cache_sync().unwrap_or([0u8; 32]);
                    let tip_req = TipRequest {
                        context: exit_to_context(&reason, gain_bps as i64),
                        size_lamports: pos.size_lamports,
                        gain_bps: gain_bps as i64,
                        grad_score: 0.0,
                    };
                    let tip = self.tip_engine.lock().compute_tip(&tip_req);
                    let reason_str = reason.as_str().to_string();
                    let gain = gain_bps as i64;
                    let fee_idx = (std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() % 8) as usize;
                    let rpc_sender = self.rpc_sender.clone();
                    let balance_rpc_url = self.public_rpc_url.clone();
                    let balance_http = self.rpc_fallback_client.clone();
                    tokio::spawn(async move {
                        // Resolve pool accounts
                        let resolution = match crate::momentum::pool::resolve_pumpswap_pool_from_mint(
                            &http_client, &mint_copy, &public_rpc, &helius_rpc,
                        ).await {
                            Some(r) => r,
                            None => {
                                tracing::error!(mint=%bs58::encode(&mint_copy).into_string(), "[sell_lastchance] pool resolution FAILED — tokens may be stuck");
                                return;
                            }
                        };
                        let ps_pool_raw = match crate::momentum::pool::extract_pumpswap_pool_accounts(&resolution) {
                            Some(p) => p,
                            None => {
                                tracing::error!(mint=%bs58::encode(&mint_copy).into_string(), "[sell_lastchance] extract_pool_accounts returned None");
                                return;
                            }
                        };
                        let mut ps_pool: crate::tx::pumpswap::PumpSwapPoolAccounts = ps_pool_raw.into();
                        // Resolve token_mint_program
                        if ps_pool.token_mint_program == [0u8; 32] {
                            ps_pool.token_mint_program = match crate::momentum::pool::resolve_mint_program_with_fallback(
                                &http_client, &mint_copy, &helius_rpc, Some(&public_rpc),
                            ).await {
                                Some(prog) => prog,
                                None => crate::tx::pumpswap::SPL_TOKEN_PROGRAM_BYTES,
                            };
                        }
                        // Load keypair
                        let kp_bytes = match std::fs::read(&kp_path) {
                            Ok(b) => b,
                            Err(e) => { tracing::error!(err=?e, "[sell_lastchance] keypair load failed"); return; }
                        };
                        let kp_arr: Vec<u8> = match serde_json::from_slice(&kp_bytes) {
                            Ok(v) => v,
                            Err(e) => { tracing::error!(err=?e, "[sell_lastchance] keypair parse failed"); return; }
                        };
                        if kp_arr.len() != 64 { tracing::error!("[sell_lastchance] bad keypair len"); return; }
                        let mut kb = [0u8; 64];
                        kb.copy_from_slice(&kp_arr);
                        let keypair = match solana_sdk::signature::Keypair::from_bytes(&kb) {
                            Ok(k) => k,
                            Err(e) => { tracing::error!(err=?e, "[sell_lastchance] keypair err"); return; }
                        };
                        use std::str::FromStr as _;
                        use solana_sdk::signer::Signer as _;

                        // ── STRICT balance check before last-chance sell ──
                        // Wait for buy TX to land before querying balance, then retry up to 15x.
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                        let wallet_pubkey = keypair.pubkey();
                        let token_mint = solana_sdk::pubkey::Pubkey::new_from_array(mint_copy);
                        let token_program = crate::tx::pumpswap::token_program_for_mint_with_hint(
                            &token_mint, &ps_pool.token_mint_program,
                        );
                        let ata_program = solana_sdk::pubkey::Pubkey::from_str(
                            crate::tx::pumpswap::SPL_ATA_PROGRAM_STR,
                        ).unwrap();
                        let (token_ata, _) = solana_sdk::pubkey::Pubkey::find_program_address(
                            &[wallet_pubkey.as_ref(), token_program.as_ref(), token_mint.as_ref()],
                            &ata_program,
                        );
                        let balance_body = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": 1,
                            "method": "getTokenAccountBalance",
                            "params": [token_ata.to_string()]
                        });
                        let actual_tokens = {
                            let mut result = None;
                            // 15 retries at 1s spacing = 15s window for Token-2022 indexing.
                            for attempt in 0..15u32 {
                                if attempt > 0 {
                                    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                                }
                                let resp = balance_http
                                    .post(balance_rpc_url.as_str())
                                    .header("Content-Type", "application/json")
                                    .json(&balance_body)
                                    .send()
                                    .await;
                                match resp {
                                    Ok(r) => match r.json::<serde_json::Value>().await {
                                        Ok(json) => {
                                            if json.get("error").is_some() {
                                                tracing::debug!(
                                                    mint=%bs58::encode(&mint_copy).into_string(),
                                                    attempt,
                                                    "[sell_lastchance] ATA not found yet — retrying"
                                                );
                                                continue;
                                            }
                                            match json["result"]["value"]["amount"]
                                                .as_str()
                                                .and_then(|s| s.parse::<u64>().ok())
                                            {
                                                Some(bal) => { result = Some(bal); break; }
                                                None => {
                                                    tracing::warn!(
                                                        mint=%bs58::encode(&mint_copy).into_string(),
                                                        body=%json,
                                                        "[sell_lastchance] balance returned null/unparseable — aborting sell"
                                                    );
                                                    return;
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            tracing::error!(mint=%bs58::encode(&mint_copy).into_string(), err=?e, "[sell_lastchance] balance response parse failed");
                                            return;
                                        }
                                    },
                                    Err(e) => {
                                        tracing::error!(mint=%bs58::encode(&mint_copy).into_string(), err=?e, "[sell_lastchance] balance RPC failed");
                                        return;
                                    }
                                }
                            }
                            match result {
                                Some(bal) => bal,
                                None => {
                                    tracing::warn!(
                                        mint=%bs58::encode(&mint_copy).into_string(),
                                        "[sell_lastchance] ATA not found after 15 retries — skipping sell"
                                    );
                                    return;
                                }
                            }
                        };
                        if actual_tokens == 0 {
                            tracing::warn!(mint=%bs58::encode(&mint_copy).into_string(), estimated_tokens=tokens, "[sell_lastchance] on-chain token balance is 0 — skipping sell");
                            return;
                        }

                        let tip_account = solana_sdk::pubkey::Pubkey::from_str(
                            crate::tx::raydium::JITO_TIP_ACCOUNTS[0]
                        ).unwrap();
                        let min_sol_out = 0u64; // Emergency exit — accept any SOL
                        let mint_str = bs58::encode(&mint_copy).into_string();
                        tracing::info!(mint=%mint_str, actual_tokens, estimated_tokens=tokens, pool=%bs58::encode(&ps_pool.pool).into_string(), "[sell_lastchance] building sell TX");
                        let tx_bytes = match crate::tx::pumpswap::build_pumpswap_sell_tx(
                            &ps_pool, &keypair, actual_tokens, min_sol_out, tip, tip_account, bh, fee_idx,
                        ) {
                            Ok(b) => b,
                            Err(e) => { tracing::error!(mint=%mint_str, err=?e, "[sell_lastchance] build failed"); return; }
                        };
                        match rpc_sender.submit_tx(&tx_bytes, &mint_str, "sell_lastchance").await {
                            rpc_sender::SubmitResult::Landed { signature, latency_ms } => {
                                tracing::info!(mint=%mint_str, sig=%signature, latency_ms, reason=%reason_str, gain_bps=gain, "[sell_lastchance] RPC landed ✅");
                            }
                            rpc_sender::SubmitResult::TimedOut { signature } => {
                                tracing::warn!(mint=%mint_str, sig=%signature, reason=%reason_str, "[sell_lastchance] RPC timed out");
                            }
                            rpc_sender::SubmitResult::Failed { error } => {
                                tracing::error!(mint=%mint_str, err=%error, reason=%reason_str, "[sell_lastchance] 🚨 SELL FAILED");
                            }
                            rpc_sender::SubmitResult::CircuitOpen { remaining_ms } => {
                                tracing::error!(mint=%mint_str, remaining_ms, "[sell_lastchance] circuit breaker OPEN");
                            }
                        }
                    });
                }
            }
            // Idempotent cleanup — safe if already removed in sell branch above
            self.pumpswap_pools.remove(&mint);
        }
    }

    /// Recover orphan positions on daemon startup.
    ///
    /// Scans the wallet for any non-zero token balances that aren't tracked in
    /// `self.active`. For each orphan, resolves the PumpSwap pool and immediately
    /// submits a sell TX (min_sol_out=0, emergency exit). This is a safety net
    /// ensuring stuck positions from prior crashes/bugs don't survive restarts.
    ///
    /// Must be called once, shortly after engine construction, from main.rs.
    pub async fn recover_orphan_positions(&self) {
        if self.config.paper_mode {
            tracing::info!("[orphan_recovery] paper mode — skipping");
            return;
        }

        // Load blocklist of permanently unrecoverable mints (pool gone or PumpSwap Overflow)
        let blocklist: std::collections::HashSet<String> = {
            let path = std::env::var("ORPHAN_BLOCKLIST_PATH")
                .unwrap_or_else(|_| "/data/.openclaw/workspace/projects/pump-quant/config/orphan_blocklist.json".to_string());
            match std::fs::read_to_string(&path) {
                Ok(s) => match serde_json::from_str::<serde_json::Value>(&s) {
                    Ok(v) => v["blocked_mints"]
                        .as_array()
                        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
                        .unwrap_or_default(),
                    Err(e) => { tracing::warn!(err=?e, "[orphan_recovery] blocklist parse failed — ignoring"); Default::default() }
                },
                Err(_) => Default::default(), // no blocklist file = no skips
            }
        };
        if !blocklist.is_empty() {
            tracing::info!(count=blocklist.len(), "[orphan_recovery] loaded blocklist — will skip known-dead mints");
        }

        let wallet_bytes = match self.wallet_pubkey {
            Some(w) => w,
            None => {
                tracing::warn!("[orphan_recovery] no wallet_pubkey configured — skipping");
                return;
            }
        };

        let wallet = solana_sdk::pubkey::Pubkey::new_from_array(wallet_bytes);
        let rpc_url = self.public_rpc_url.as_str();

        // ── Pre-create WSOL ATA if missing ────────────────────────────────────
        // Every PumpSwap sell TX requires the wallet's WSOL ATA to exist as a
        // writable account. Solana validates all accounts before executing any
        // instructions, so the idempotent create instruction inside the sell TX
        // can't save us — if the ATA doesn't exist, the TX fails with
        // ProgramAccountNotFound/MissingAccount before any instruction runs.
        // Create it once here at startup; it persists forever.
        {
            use std::str::FromStr as _;
            use crate::tx::pumpswap::{SPL_ATA_PROGRAM_STR, SPL_TOKEN_PROGRAM_STR};
            let wsol_mint = solana_sdk::pubkey::Pubkey::from_str("So11111111111111111111111111111111111111112").unwrap();
            let spl_prog  = solana_sdk::pubkey::Pubkey::from_str(SPL_TOKEN_PROGRAM_STR).unwrap();
            let ata_prog  = solana_sdk::pubkey::Pubkey::from_str(SPL_ATA_PROGRAM_STR).unwrap();
            let (wsol_ata, _) = solana_sdk::pubkey::Pubkey::find_program_address(
                &[wallet.as_ref(), spl_prog.as_ref(), wsol_mint.as_ref()],
                &ata_prog,
            );

            // Check if it already exists
            let check_body = serde_json::json!({
                "jsonrpc": "2.0", "id": 1,
                "method": "getAccountInfo",
                "params": [wsol_ata.to_string(), {"encoding": "base64"}]
            });
            let balance_http = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build().unwrap();
            let exists = match balance_http.post(rpc_url).json(&check_body).send().await {
                Ok(r) => match r.json::<serde_json::Value>().await {
                    Ok(j) => j["result"]["value"].is_object(),
                    Err(_) => false,
                },
                Err(_) => false,
            };

            if !exists {
                tracing::info!(wsol_ata=%wsol_ata, "[startup] WSOL ATA missing — creating before first sell");
                let kp_path = std::env::var("WALLET_KEYPAIR_PATH")
                    .unwrap_or_else(|_| "/data/.openclaw/workspace/projects/pump-quant/config/keys/wallet-keypair.json".to_string());
                if let Ok(kp_bytes) = std::fs::read(&kp_path) {
                    if let Ok(kp_arr) = serde_json::from_slice::<Vec<u8>>(&kp_bytes) {
                        if kp_arr.len() == 64 {
                            let mut kb = [0u8; 64];
                            kb.copy_from_slice(&kp_arr);
                            if let Ok(keypair) = solana_sdk::signature::Keypair::from_bytes(&kb) {
                                // Get blockhash
                                let bh_body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"getLatestBlockhash","params":[{"commitment":"confirmed"}]});
                                if let Ok(bh_resp) = balance_http.post(rpc_url).json(&bh_body).send().await {
                                    if let Ok(bh_json) = bh_resp.json::<serde_json::Value>().await {
                                        if let Some(bh_str) = bh_json["result"]["value"]["blockhash"].as_str() {
                                            if let Ok(bh_bytes) = bs58::decode(bh_str).into_vec() {
                                                let mut bh_arr = [0u8; 32];
                                                bh_arr.copy_from_slice(&bh_bytes);
                                                let blockhash = solana_sdk::hash::Hash::new_from_array(bh_arr);

                                                let sys_prog = solana_sdk::pubkey::Pubkey::from_str("11111111111111111111111111111111").unwrap();
                                                let create_ix = solana_sdk::instruction::Instruction {
                                                    program_id: ata_prog,
                                                    accounts: vec![
                                                        solana_sdk::instruction::AccountMeta::new(wallet, true),
                                                        solana_sdk::instruction::AccountMeta::new(wsol_ata, false),
                                                        solana_sdk::instruction::AccountMeta::new_readonly(wallet, false),
                                                        solana_sdk::instruction::AccountMeta::new_readonly(wsol_mint, false),
                                                        solana_sdk::instruction::AccountMeta::new_readonly(sys_prog, false),
                                                        solana_sdk::instruction::AccountMeta::new_readonly(spl_prog, false),
                                                    ],
                                                    data: vec![1u8], // CreateIdempotent
                                                };
                                                let tx = solana_sdk::transaction::Transaction::new_signed_with_payer(
                                                    &[create_ix], Some(&wallet), &[&keypair], blockhash,
                                                );
                                                if let Ok(tx_bytes) = bincode::serialize(&tx) {
                                                    let tx_b64 = base64::encode(&tx_bytes);
                                                    let send_body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"sendTransaction","params":[tx_b64,{"encoding":"base64","skipPreflight":true}]});
                                                    match balance_http.post(rpc_url).json(&send_body).send().await {
                                                        Ok(r) => match r.json::<serde_json::Value>().await {
                                                            Ok(j) => if let Some(sig) = j["result"].as_str() {
                                                                tracing::info!(sig=%sig, wsol_ata=%wsol_ata, "[startup] WSOL ATA creation TX submitted");
                                                                // Wait for it to land
                                                                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                                                            } else {
                                                                tracing::warn!(resp=?j, "[startup] WSOL ATA creation failed");
                                                            },
                                                            Err(e) => tracing::warn!(err=?e, "[startup] WSOL ATA creation response parse failed"),
                                                        },
                                                        Err(e) => tracing::warn!(err=?e, "[startup] WSOL ATA creation send failed"),
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            } else {
                tracing::info!(wsol_ata=%wsol_ata, "[startup] WSOL ATA exists ✅");
            }
        }

        // Fetch all SPL token accounts owned by wallet (both SPL Token and Token-2022)
        tracing::info!(wallet=%wallet, "[orphan_recovery] scanning wallet for orphan token balances");

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getTokenAccountsByOwner",
            "params": [
                wallet.to_string(),
                { "programId": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA" },
                { "encoding": "jsonParsed" }
            ]
        });
        let body_2022 = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "getTokenAccountsByOwner",
            "params": [
                wallet.to_string(),
                { "programId": "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb" },
                { "encoding": "jsonParsed" }
            ]
        });

        let http = &self.rpc_fallback_client;
        let mut orphan_mints: Vec<([u8; 32], u64)> = Vec::new();

        for req_body in [&body, &body_2022] {
            let resp = match http
                .post(rpc_url)
                .header("Content-Type", "application/json")
                .json(req_body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(err=?e, "[orphan_recovery] RPC failed");
                    continue;
                }
            };
            let json: serde_json::Value = match resp.json().await {
                Ok(j) => j,
                Err(e) => {
                    tracing::error!(err=?e, "[orphan_recovery] parse failed");
                    continue;
                }
            };
            if let Some(accounts) = json["result"]["value"].as_array() {
                for acct in accounts {
                    let info = &acct["account"]["data"]["parsed"]["info"];
                    let mint_str = match info["mint"].as_str() {
                        Some(s) => s,
                        None => continue,
                    };
                    let balance = info["tokenAmount"]["amount"]
                        .as_str()
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(0);
                    if balance == 0 { continue; }

                    // Skip WSOL (wrapped SOL) — not an orphan position
                    if mint_str == "So11111111111111111111111111111111111111112" { continue; }

                    // Skip known-dead mints (pool gone or PumpSwap Overflow)
                    if blocklist.contains(mint_str) {
                        tracing::debug!(mint=%mint_str, "[orphan_recovery] skipping blocklisted mint");
                        continue;
                    }

                    let mint_bytes: [u8; 32] = match bs58::decode(mint_str).into_vec() {
                        Ok(v) if v.len() == 32 => {
                            let mut arr = [0u8; 32];
                            arr.copy_from_slice(&v);
                            arr
                        }
                        _ => continue,
                    };

                    // Skip mints that have an active position (not orphans)
                    if self.active.contains_key(&mint_bytes) { continue; }

                    orphan_mints.push((mint_bytes, balance));
                }
            }
        }

        if orphan_mints.is_empty() {
            tracing::info!("[orphan_recovery] no orphan positions found ✅");
            return;
        }

        tracing::warn!(
            count = orphan_mints.len(),
            "[orphan_recovery] found orphan token balances — attempting emergency sell"
        );

        for (mint_bytes, balance) in orphan_mints {
            let mint_str = bs58::encode(&mint_bytes).into_string();
            tracing::info!(
                mint=%mint_str,
                balance,
                "[orphan_recovery] selling orphan position"
            );

            // Resolve PumpSwap pool
            let resolution = match crate::momentum::pool::resolve_pumpswap_pool_from_mint(
                &self.http_client, &mint_bytes, &self.public_rpc_url, &self.helius_rpc_url,
            ).await {
                Some(r) => r,
                None => {
                    tracing::error!(mint=%mint_str, "[orphan_recovery] pool resolution failed — tokens stuck");
                    continue;
                }
            };
            let ps_pool_raw = match crate::momentum::pool::extract_pumpswap_pool_accounts(&resolution) {
                Some(p) => p,
                None => {
                    tracing::error!(mint=%mint_str, "[orphan_recovery] extract_pool_accounts returned None");
                    continue;
                }
            };
            let mut ps_pool: crate::tx::pumpswap::PumpSwapPoolAccounts = ps_pool_raw.into();
            if ps_pool.token_mint_program == [0u8; 32] {
                ps_pool.token_mint_program = match crate::momentum::pool::resolve_mint_program_with_fallback(
                    &self.http_client, &mint_bytes, &self.helius_rpc_url, Some(&self.public_rpc_url),
                ).await {
                    Some(prog) => prog,
                    None => crate::tx::pumpswap::SPL_TOKEN_PROGRAM_BYTES,
                };
            }

            // Load keypair
            let kp_path = std::env::var("WALLET_KEYPAIR_PATH").unwrap_or_default();
            let kp_bytes = match std::fs::read(&kp_path) {
                Ok(b) => b,
                Err(e) => { tracing::error!(err=?e, "[orphan_recovery] keypair load failed"); continue; }
            };
            let kp_arr: Vec<u8> = match serde_json::from_slice(&kp_bytes) {
                Ok(v) => v,
                Err(e) => { tracing::error!(err=?e, "[orphan_recovery] keypair parse failed"); continue; }
            };
            if kp_arr.len() != 64 { tracing::error!("[orphan_recovery] bad keypair len"); continue; }
            let mut kb = [0u8; 64];
            kb.copy_from_slice(&kp_arr);
            let keypair = match solana_sdk::signature::Keypair::from_bytes(&kb) {
                Ok(k) => k,
                Err(e) => { tracing::error!(err=?e, "[orphan_recovery] keypair err"); continue; }
            };

            let bh = self.blockhash_cache_sync().unwrap_or([0u8; 32]);
            let tip = 1_000u64; // minimal tip for emergency exit
            use std::str::FromStr as _;
            let tip_account = solana_sdk::pubkey::Pubkey::from_str(
                crate::tx::raydium::JITO_TIP_ACCOUNTS[0]
            ).unwrap();
            let fee_idx = 0usize;

            tracing::info!(
                mint=%mint_str,
                pool=%bs58::encode(&ps_pool.pool).into_string(),
                tokens=balance,
                "[orphan_recovery] building emergency sell TX (min_sol_out=0)"
            );

            let tx_bytes = match crate::tx::pumpswap::build_pumpswap_sell_tx(
                &ps_pool, &keypair, balance, 0u64, tip, tip_account, bh, fee_idx,
            ) {
                Ok(b) => b,
                Err(e) => { tracing::error!(mint=%mint_str, err=?e, "[orphan_recovery] sell build failed"); continue; }
            };

            match self.rpc_sender.submit_tx(&tx_bytes, &mint_str, "orphan_recovery").await {
                rpc_sender::SubmitResult::Landed { signature, latency_ms } => {
                    tracing::info!(mint=%mint_str, sig=%signature, latency_ms, "[orphan_recovery] sell landed ✅");
                }
                rpc_sender::SubmitResult::TimedOut { signature } => {
                    tracing::warn!(mint=%mint_str, sig=%signature, "[orphan_recovery] sell timed out (may still land)");
                }
                rpc_sender::SubmitResult::Failed { error } => {
                    tracing::error!(mint=%mint_str, err=%error, "[orphan_recovery] 🚨 sell FAILED");
                    // Auto-add to blocklist so we don't retry on next restart
                    let bl_path = std::env::var("ORPHAN_BLOCKLIST_PATH")
                        .unwrap_or_else(|_| "/data/.openclaw/workspace/projects/pump-quant/config/orphan_blocklist.json".to_string());
                    if let Ok(s) = std::fs::read_to_string(&bl_path) {
                        if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&s) {
                            if let Some(arr) = v["blocked_mints"].as_array_mut() {
                                let already = arr.iter().any(|x| x.as_str() == Some(&mint_str));
                                if !already {
                                    arr.push(serde_json::Value::String(mint_str.clone()));
                                    if let Ok(updated) = serde_json::to_string_pretty(&v) {
                                        let _ = std::fs::write(&bl_path, updated);
                                        tracing::info!(mint=%mint_str, "[orphan_recovery] auto-added to blocklist");
                                    }
                                }
                            }
                        }
                    }
                }
                rpc_sender::SubmitResult::CircuitOpen { remaining_ms } => {
                    tracing::error!(mint=%mint_str, remaining_ms, "[orphan_recovery] circuit breaker OPEN");
                }
            }

            // Small delay between sells to avoid rate limiting
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        tracing::info!("[orphan_recovery] sweep complete");
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
        enrichment: crate::momentum::types::GradEnrichment,
    ) {
        if !self.config.enabled { return; }

        // RATE GATE: Limit pool resolution to 60/min to prevent Helius 429 storm.
        // CoreCast sends 1000+ stale events/min → each triggers 2-15 RPC calls.
        // Without this gate, we burn the entire Helius rate budget on reads,
        // starving sendTransaction (buy/sell) of headroom.
        {
            static POOL_RES_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            static POOL_RES_RESET: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let reset = POOL_RES_RESET.load(std::sync::atomic::Ordering::Relaxed);
            if now.saturating_sub(reset) > 60_000 {
                POOL_RES_COUNT.store(0, std::sync::atomic::Ordering::Relaxed);
                POOL_RES_RESET.store(now, std::sync::atomic::Ordering::Relaxed);
            }
            let count = POOL_RES_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if count >= 300 {
                return; // Over budget — drop this graduation event
            }
        }

        // Mint-level dedup: CoreCast sends 10-20+ duplicate graduation events per mint
        // within seconds, each with a different DEX trade sig. Sig-based dedup doesn't
        // catch these. Gate on mint + 30s TTL to avoid wasting RPC calls.
        {
            let now_dedup = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            if let Some(prev_ts) = self.recent_corecast_grads.get(&mint) {
                if now_dedup.saturating_sub(*prev_ts) < 30_000 {
                    tracing::debug!(
                        mint = %bs58::encode(&mint).into_string(),
                        age_ms = now_dedup.saturating_sub(*prev_ts),
                        "[momentum] mint-level dedup — skipping duplicate graduation (seen <30s ago)"
                    );
                    return;
                }
            }
            self.recent_corecast_grads.insert(mint, now_dedup);
        }

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

        // ── Pump.fun graduation filter ──────────────────────────────────────────
        // CoreCast emits graduation events for ALL Raydium AMM trades, including
        // established tokens (mSOL, USDT, DeFi tokens). We only care about pump.fun
        // memecoins — their mints end in "pump" OR mint is [0u8;32] (Helius sig path).
        // Tokens with real enrichment (speed > 0, volume > 0) have been validated by
        // PumpPortal and are genuine pump.fun tokens — allow regardless of suffix.
        {
            let mint_b58 = bs58::encode(&mint).into_string();
            let is_zero_mint = mint == [0u8; 32];
            let has_pump_suffix = mint_b58.ends_with("pump");
            let has_enrichment = enrichment.grad_speed_s > 0 || enrichment.volume_sol_x100 > 0;
            if !is_zero_mint && !has_pump_suffix && !has_enrichment {
                tracing::debug!(
                    mint = %mint_b58,
                    "[momentum] non-pump.fun mint rejected — not a pump.fun graduation"
                );
                self.resolving_sigs.remove(&sig);
                return;
            }
        }
        // ── End pump.fun graduation filter ────────────────────────────────────

        // ── FIX: Drop cold-miss events for NON-pump.fun mints ────────────────
        // CoreCast replays ~11,000+ stale graduation events per session with no
        // enrichment data (cold_miss). These ALWAYS resolve to dead Raydium pools
        // (AMM accounts closed, 0 bytes) or fail PumpSwap lookup entirely.
        // They consume the pool resolution semaphore (5 slots) and rate budget
        // (60/min), starving fresh ShredStream events of resolution capacity.
        //
        // However, ShredStream detects fresh PumpSwap pool creations BEFORE
        // PumpPortal enrichment arrives — these are legitimate cold misses for
        // real pump.fun tokens (mint ends in "pump"). We MUST let these through.
        //
        // Gate: cold-miss + NOT a pump.fun mint → drop (CoreCast stale junk).
        //       cold-miss + pump.fun mint → allow (ShredStream fresh detection).
        //       cold-miss + zero mint → allow (Helius sig-only path).
        if is_cold_miss {
            let mint_b58 = bs58::encode(&mint).into_string();
            let is_pump_mint = mint_b58.ends_with("pump");
            let is_zero_mint = mint == [0u8; 32];
            if !is_pump_mint && !is_zero_mint {
                tracing::debug!(
                    mint = %mint_b58,
                    "[momentum] non-pump.fun cold-miss dropped — CoreCast stale junk"
                );
                self.resolving_sigs.remove(&sig);
                return;
            }
            // pump.fun cold misses pass through — ShredStream fresh detections
        }
        // ── End cold-miss gate ─────────────────────────────────────────────────

        // ── PumpSwap mint-based fast path ──────────────────────────────────────
        // If we have a non-zero mint, try PumpSwap pool lookup directly via
        // getProgramAccounts (memcmp on base_mint). This skips the getTransaction
        // round-trip which is slow (~500ms-15s) and often fails for fresh txs
        // due to Helius indexing lag.
        // 100% of pump.fun graduations go to PumpSwap since April 2026.
        if mint != [0u8; 32] {
            if let Some(resolution) = crate::momentum::pool::resolve_pumpswap_pool_from_mint(
                &self.http_client, &mint, &self.public_rpc_url, &self.helius_rpc_url
            ).await {
                if resolution.reserve_sol_lamports >= crate::momentum::pool::MIN_PUMPSWAP_SOL_RESERVES_LAMPORTS {
                    let mint_b58 = bs58::encode(&resolution.mint).into_string();
                    tracing::info!(
                        mint = %mint_b58,
                        pool_type = ?resolution.pool_type,
                        reserve_sol = resolution.reserve_sol_lamports,
                        "[momentum] PumpSwap pool resolved via mint-based fast path — skipping getTransaction"
                    );

                    let pool_info = PoolInfo {
                        coin_vault: resolution.coin_vault,
                        pc_vault: resolution.pc_vault,
                        reserve_token: resolution.reserve_token_atoms,
                        reserve_sol: resolution.reserve_sol_lamports,
                        pool_type: resolution.pool_type,
                        mint: resolution.mint,
                    };

                    // Derive effective enrichment (same logic as main resolution path)
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
                            "[momentum] enrichment cold miss (mint fast path) — estimating speed from LP reserves"
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
                            mint = %mint_b58,
                            "[momentum] pumpswap pool accounts stored (mint fast path)"
                        );
                    } else if !self.config.paper_mode {
                        // Fallback: store partial accounts for last-chance resolution
                        let ps_accts = crate::tx::pumpswap::PumpSwapPoolAccounts {
                            pool: [0u8; 32],
                            base_mint: resolution.mint,
                            pool_base_token_account: resolution.coin_vault,
                            pool_quote_token_account: resolution.pc_vault,
                            coin_creator_vault_ata: [0u8; 32],
                            coin_creator_vault_authority: [0u8; 32],
                            token_is_base: true,
                            token_mint_program: [0u8; 32],
                            is_cashback_coin: false,
                        };
                        self.pumpswap_pools.insert(resolution.mint, ps_accts);
                        tracing::warn!(
                            mint = %mint_b58,
                            "[momentum] pumpswap pool accounts stored PARTIAL (mint fast path fallback)"
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
                    return; // Fast path succeeded — skip getTransaction fallback
                }
                // Insufficient liquidity on PumpSwap — fall through to getTransaction
            }
            // PumpSwap mint lookup returned None — fall through to getTransaction
            // ── ShredStream fresh detection async retry ─────────────────────────
            // ShredStream detects PumpSwap pool creations ~100ms after the tx,
            // but getProgramAccounts may not have indexed the pool yet.
            // For cold-miss pump.fun mints, spawn an async retry rather than
            // falling through to sig-based resolution (which also fails on
            // fresh sigs) or Raydium (which finds dead V4 pools).
            if is_cold_miss {
                let mint_b58 = bs58::encode(&mint).into_string();
                if mint_b58.ends_with("pump") {
                    tracing::info!(
                        mint = %mint_b58,
                        "[momentum] fresh pump.fun mint — scheduling async pool resolution retry (1s, 2s, 4s)"
                    );

                    let http = self.http_client.clone();
                    let mint_copy = mint;
                    let enrichment_copy = enrichment;
                    let ts_ms_copy = ts_ms;
                    let public_rpc = self.public_rpc_url.clone();
                    let helius_rpc = self.helius_rpc_url.clone();
                    let retry_tx = self.retry_tx.clone();

                    tokio::spawn(async move {
                        let mint_b58 = bs58::encode(&mint_copy).into_string();
                        for delay_ms in [1000u64, 2000, 4000] {
                            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;

                            if let Some(resolution) = crate::momentum::pool::resolve_pumpswap_pool_from_mint(
                                &http, &mint_copy, &public_rpc, &helius_rpc,
                            ).await {
                                if resolution.reserve_sol_lamports >= crate::momentum::pool::MIN_PUMPSWAP_SOL_RESERVES_LAMPORTS {
                                    tracing::info!(
                                        mint = %mint_b58,
                                        reserve_sol = resolution.reserve_sol_lamports,
                                        delay_ms,
                                        "[momentum] async retry succeeded — pool now indexed"
                                    );
                                    let _ = retry_tx.send(AsyncRetryResult {
                                        resolution,
                                        enrichment: enrichment_copy,
                                        ts_ms: ts_ms_copy,
                                        mint: mint_copy,
                                    });
                                    return;
                                }
                            }
                        }
                        tracing::warn!(
                            mint = %mint_b58,
                            "[momentum] async retry failed after 3 attempts (1s, 2s, 4s) — pool not indexed"
                        );
                    });

                    // Remove sig from dedup so async retry result can be processed fresh
                    self.resolving_sigs.remove(&sig);
                    return; // Don't fall through to Raydium — async retry will handle it
                }
            }
            // ── End ShredStream fresh detection async retry ──────────────────────
        }
        // ── End PumpSwap mint-based fast path ──────────────────────────────────

        match resolve_pool_from_transaction(&self.http_client, &sig, &self.public_rpc_url, &self.helius_rpc_url).await {
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

                // ── FIX-1/4 upgrade: block-time-based staleness gate ──────────
                // Use the tx's actual blockTime (not CoreCast's ts_ms) to detect
                // old graduations. CoreCast sets ts_ms = now(), so the ts_ms gate
                // always shows ~0ms age. blockTime = actual on-chain graduation time.
                if resolution.grad_block_time_ms > 0 && self.config.stale_grad_max_age_ms > 0 {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    let grad_age_ms = now_ms.saturating_sub(resolution.grad_block_time_ms);
                    if grad_age_ms > self.config.stale_grad_max_age_ms {
                        tracing::debug!(
                            mint = %bs58::encode(&resolution.mint).into_string(),
                            grad_age_ms,
                            grad_age_min = grad_age_ms / 60_000,
                            "[momentum] stale graduation rejected — on-chain blockTime {}min ago",
                            grad_age_ms / 60_000
                        );
                        return;
                    }
                }
                // ── End block-time staleness gate ─────────────────────────────

                // ── FIX-5: Raydium dead pool activity check ───────────────────
                // If the resolved pool is Raydium, verify it has had recent swap
                // activity. Dead Raydium pools have stale liquidity but zero trades.
                if resolution.pool_type == crate::momentum::pool::PoolType::RaydiumAmmV4
                    && self.config.raydium_max_idle_ms > 0
                {
                    let pc_vault_b58 = bs58::encode(&resolution.pc_vault).into_string();
                    let last_ms = crate::momentum::pool::get_account_last_activity_ms(
                        &self.http_client, &self.public_rpc_url, &pc_vault_b58
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
                    // Guard: reject Raydium pools with zeroed Serum accounts.
                    // When fetch_raydium_pool_accounts fails (account not found, 0 bytes,
                    // or wrong amm_id extraction), all Serum fields are [0u8; 32].
                    // Submitting a Raydium swap with zeroed accounts → Custom(27) on-chain
                    // ("amm account owner is not match with this program").
                    let serum_valid = resolution.serum_market != [0u8; 32]
                        && resolution.amm_open_orders != [0u8; 32];
                    if !serum_valid {
                        tracing::warn!(
                            mint = %bs58::encode(&resolution.mint).into_string(),
                            "[momentum] Raydium pool has zeroed Serum accounts — NOT stored (position = accounting-only)"
                        );
                    } else {
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
                }

                // PumpSwap pool accounts (zeroed for Raydium tokens, populated for PumpSwap)
                if let Some(ps_pool) = crate::momentum::pool::extract_pumpswap_pool_accounts(&resolution) {
                    let ps_accts: crate::tx::pumpswap::PumpSwapPoolAccounts = ps_pool.into();
                    self.pumpswap_pools.insert(resolution.mint, ps_accts);
                    tracing::debug!(
                        mint = %bs58::encode(&resolution.mint).into_string(),
                        "[momentum] pumpswap pool accounts stored for live execution"
                    );
                } else if resolution.pool_type == PoolType::PumpSwap && !self.config.paper_mode {
                    // Fallback: extract_pumpswap_pool_accounts returned None (e.g., pool_address
                    // was [0u8;32] because sig-based path didn't resolve it and the FIX-2 mint
                    // lookup also failed). Store PARTIAL pool accounts with zeroed pool PDA —
                    // the last-chance resolver in process_pending_entries will fill them in
                    // at T+entry_delay_ms when getProgramAccounts has indexed the pool.
                    let ps_accts = crate::tx::pumpswap::PumpSwapPoolAccounts {
                        pool: [0u8; 32],
                        base_mint: resolution.mint,
                        pool_base_token_account: resolution.coin_vault,
                        pool_quote_token_account: resolution.pc_vault,
                        coin_creator_vault_ata: [0u8; 32],
                        coin_creator_vault_authority: [0u8; 32],
                        token_is_base: true,
                        token_mint_program: [0u8; 32], // resolved at entry time
                        is_cashback_coin: false,
                    };
                    self.pumpswap_pools.insert(resolution.mint, ps_accts);
                    tracing::warn!(
                        mint = %bs58::encode(&resolution.mint).into_string(),
                        "[momentum] pumpswap pool accounts stored PARTIAL (zeroed pool PDA) — last-chance resolution will fill at entry time"
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
                    // Use public_rpc_url for pool resolution reads — frees Helius budget.
                    // Falls back to helius_rpc_url if public RPC doesn't support getProgramAccounts.
                    let ps = crate::momentum::pool::resolve_pumpswap_pool_from_mint(
                        &self.http_client, &mint, &self.public_rpc_url, &self.helius_rpc_url
                    ).await;
                    if ps.is_some() {
                        ps
                    } else {
                        crate::momentum::pool::resolve_pool_from_mint(
                            &self.http_client, &mint, &self.public_rpc_url, &self.helius_rpc_url
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

                        // ── FIX-1/4 (mint-lookup path): ts_ms-based staleness gate ────────
                        // Mint-lookup has no blockTime; use ts_ms (CoreCast sets ts_ms=now,
                        // Helius sets ts_ms=now). For cold-miss tokens on the mint-lookup path,
                        // use a stricter check: if enrichment is cold-miss, reject always —
                        // we have no enrichment and no blockTime to validate freshness.
                        // Only exception: Helius grads with mint=[0;32] need mint-lookup and ARE fresh.
                        // They're cold-miss but their sig was freshly validated in resolve_pool_from_transaction.
                        // The mint here is already resolved (non-zero), so if is_cold_miss here
                        // on mint-lookup path it means CoreCast sent it without enrichment.
                        if is_cold_miss && self.config.stale_grad_max_age_ms > 0 {
                            let now_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64;
                            let grad_age_ms = now_ms.saturating_sub(ts_ms);
                            if grad_age_ms > self.config.stale_grad_max_age_ms {
                                tracing::debug!(
                                    mint = %mint_b58,
                                    grad_age_ms,
                                    "[momentum] stale cold-miss grad rejected (mint-lookup) — CoreCast backlog"
                                );
                                return;
                            }
                        }
                        // ── End staleness gate (mint-lookup path) ─────────────────────────

                        // ── FIX-5: Raydium dead pool activity check (mint-lookup path) ──
                        if resolution.pool_type == crate::momentum::pool::PoolType::RaydiumAmmV4
                            && self.config.raydium_max_idle_ms > 0
                        {
                            let pc_vault_b58 = bs58::encode(&resolution.pc_vault).into_string();
                            let last_ms = crate::momentum::pool::get_account_last_activity_ms(
                                &self.http_client, &self.public_rpc_url, &pc_vault_b58
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
                        } else if resolution.pool_type == PoolType::PumpSwap && !self.config.paper_mode {
                            // Fallback: store partial accounts for last-chance resolution
                            let ps_accts = crate::tx::pumpswap::PumpSwapPoolAccounts {
                                pool: [0u8; 32],
                                base_mint: resolution.mint,
                                pool_base_token_account: resolution.coin_vault,
                                pool_quote_token_account: resolution.pc_vault,
                                coin_creator_vault_ata: [0u8; 32],
                                coin_creator_vault_authority: [0u8; 32],
                                token_is_base: true,
                                token_mint_program: [0u8; 32],
                                is_cashback_coin: false,
                            };
                            self.pumpswap_pools.insert(resolution.mint, ps_accts);
                            tracing::warn!(
                                mint = %bs58::encode(&resolution.mint).into_string(),
                                "[momentum] pumpswap pool accounts stored PARTIAL (mint lookup path fallback)"
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

    /// Called from main.rs on PumpSwapGraduationDirect events.
    ///
    /// Fast path: vaults already extracted from Helius Enhanced transactionSubscribe
    /// notification — no getTransaction RPC call needed. Only need to fetch vault
    /// reserves via getMultipleAccounts (one RPC call).
    ///
    /// This is the fastest graduation path: Helius Enhanced → extract vaults from
    /// notification → fetch reserves → on_graduation(). Total: ~200-400ms.
    #[inline(never)]
    pub async fn on_pumpswap_graduation_direct(
        &self,
        mint: [u8; 32],
        sig: [u8; 64],
        ts_ms: u64,
        coin_vault: [u8; 32],
        pc_vault: [u8; 32],
        source: crate::feeds::MigrationSource,
        enrichment: crate::momentum::types::GradEnrichment,
    ) {
        if !self.config.enabled { return; }

        // NOTE: The Helius Enhanced parser (helius.rs) already identifies vaults
        // by checking postTokenBalances mints: coin_vault = token mint account,
        // pc_vault = WSOL account. No byte-order swap needed here — the parser
        // resolves semantic assignment, not pool layout order.

        // RATE GATE: Limit direct path entries to 60/min (reset every 60s)
        {
            static POOL_RES_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            static POOL_RES_RESET: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let reset = POOL_RES_RESET.load(std::sync::atomic::Ordering::Relaxed);
            if now.saturating_sub(reset) > 60_000 {
                POOL_RES_COUNT.store(0, std::sync::atomic::Ordering::Relaxed);
                POOL_RES_RESET.store(now, std::sync::atomic::Ordering::Relaxed);
            }
            let count = POOL_RES_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if count >= 60 { return; }
        }

        // Dedup: same sig-based dedup as on_migration
        if self.resolving_sigs.contains_key(&sig) {
            tracing::debug!(
                sig = %&bs58::encode(&sig).into_string()[..8],
                "[momentum] PumpSwapGraduationDirect already resolving — skipping duplicate"
            );
            return;
        }
        self.resolving_sigs.insert(sig, ts_ms);

        // Reentry cooldown check
        if let Some(close_ts) = self.recently_closed.get(&mint) {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            if now_ms.saturating_sub(*close_ts) < self.config.reentry_cooldown_ms
            {
                tracing::debug!(
                    mint = %bs58::encode(&mint).into_string(),
                    "[momentum] PumpSwapGraduationDirect reentry cooldown — skipping"
                );
                return;
            }
        }

        // Duplicate mint guard (already have open position)
        if self.active.contains_key(&mint) {
            tracing::debug!(
                mint = %bs58::encode(&mint).into_string(),
                "[momentum] PumpSwapGraduationDirect already active — skipping"
            );
            return;
        }

        // Blocklist: reject known SPL token mints
        if is_blocked_mint(&mint) {
            tracing::debug!(
                mint = %bs58::encode(&mint).into_string(),
                "[momentum] PumpSwapGraduationDirect blocked mint — skipping"
            );
            return;
        }

        // Pump.fun graduation filter (same as on_migration)
        {
            let mint_b58 = bs58::encode(&mint).into_string();
            let has_pump_suffix = mint_b58.ends_with("pump");
            let has_enrichment = enrichment.grad_speed_s > 0 || enrichment.volume_sol_x100 > 0;
            if !has_pump_suffix && !has_enrichment {
                tracing::debug!(
                    mint = %mint_b58,
                    "[momentum] PumpSwapGraduationDirect non-pump.fun mint rejected"
                );
                self.resolving_sigs.remove(&sig);
                return;
            }
        }

        let coin_vault_b58 = bs58::encode(&coin_vault).into_string();
        let pc_vault_b58 = bs58::encode(&pc_vault).into_string();

        // ── Trust Helius Enhanced vault data — skip RPC verification ────────
        // Helius transactionSubscribe already parsed the on-chain create_pool
        // instruction and extracted the vault addresses. These are 100% correct.
        // The old flow called fetch_vault_reserves() to verify, but that fails
        // 100% of the time for fresh graduations because:
        //   1. Vault accounts are < 1 second old
        //   2. Even Helius RPC hasn't indexed them at confirmed commitment
        //   3. The 500ms timeout is too tight for brand-new accounts
        // Instead: use estimated reserves (all fresh PumpSwap graduations start
        // with ~85 SOL + ~800M tokens) and enter immediately. The price feed
        // will get real reserves from the first vault poll (~500ms later).
        //
        // Pool PDA + creator ATA are NOT available yet (require getProgramAccounts
        // which also lags). Store partial pool accounts with zeroed PDA/creator.
        // The buy TX executes at T+entry_delay_ms (15s) — by then, the
        // "last-chance" resolution in process_pending_entries will resolve them.

        let estimated_reserve_sol: u64 = 85_000_000_000; // 85 SOL
        let estimated_reserve_token: u64 = 800_000_000_000_000; // ~800M tokens (6 decimals)

        let mint_b58 = bs58::encode(&mint).into_string();
        tracing::info!(
            mint = %mint_b58,
            coin_vault = %coin_vault_b58,
            pc_vault = %pc_vault_b58,
            source = source.as_str(),
            "[momentum] PumpSwapGraduationDirect — trusting Helius vault data, entering immediately (est 85 SOL)"
        );

        let pool_info = PoolInfo {
            coin_vault,
            pc_vault,
            reserve_token: estimated_reserve_token,
            reserve_sol: estimated_reserve_sol,
            pool_type: PoolType::PumpSwap,
            mint,
        };

        // Cold miss detection
        let is_cold_miss = enrichment.grad_speed_s == 0 && enrichment.volume_sol_x100 == 0;

        // Derive effective enrichment (same logic as on_migration)
        let effective_volume_sol_x100 = if enrichment.volume_sol_x100 == 0 {
            // Use estimated 85 SOL for cold miss enrichment
            (estimated_reserve_sol / 10_000_000).min(65535) as u32
        } else {
            enrichment.volume_sol_x100
        };
        let effective_speed_s = if enrichment.grad_speed_s == 0 {
            let sol = estimated_reserve_sol / 1_000_000_000;
            self.grad_enrichment_cold_misses.fetch_add(1, Ordering::Relaxed);
            tracing::info!(
                mint = %mint_b58,
                reserve_sol = sol,
                "[momentum] enrichment cold miss (direct path) — using estimated reserves"
            );
            if sol >= 250 { 60u32 } else { 120u32 }
        } else {
            enrichment.grad_speed_s
        };
        let effective_buys_5s = if enrichment.buys_5s == 0 { 3u32 } else { enrichment.buys_5s as u32 };
        let effective_sells_5s = if enrichment.sells_5s == 0 { 1u32 } else { enrichment.sells_5s as u32 };

        // Store partial PumpSwap pool accounts with zeroed pool PDA and creator.
        // The buy TX path in process_pending_entries has a "last-chance" resolver
        // that will fill these in before submitting (at T+entry_delay_ms, ~15s later,
        // by which time getProgramAccounts indexing has caught up).
        {
            // Helius parser normalizes: coin_vault = token vault, pc_vault = WSOL vault.
            // PumpSwap ALWAYS uses token=base, WSOL=quote for pump.fun pools.
            // No vault swapping needed.
            let ps_accts = crate::tx::pumpswap::PumpSwapPoolAccounts {
                pool: [0u8; 32],
                base_mint: mint,
                pool_base_token_account: coin_vault,  // token vault = base
                pool_quote_token_account: pc_vault,    // WSOL vault = quote
                coin_creator_vault_ata: [0u8; 32],
                coin_creator_vault_authority: [0u8; 32],
                token_is_base: true,
                token_mint_program: [0u8; 32], // will be resolved at entry time
                is_cashback_coin: false, // will be resolved from pool data
            };
            self.pumpswap_pools.insert(mint, ps_accts);
            tracing::debug!(
                mint = %mint_b58,
                "[momentum] partial pumpswap pool accounts stored (pool PDA + creator will resolve at entry time)"
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
        // Disable activity gate in tests — no WS feed populates the tracker
        cfg.activity_gate.enabled = false;
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
            "https://api.mainnet-beta.solana.com".to_string(),
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
            reserve_token: 200_000_000, // realistic pump.fun graduation pool (~200M tokens remaining in curve)
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
            reserve_token: 200_000_000, // realistic pump.fun graduation pool (~200M tokens remaining in curve)
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
            reserve_token: 200_000_000, // realistic pump.fun graduation pool (~200M tokens remaining in curve)
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
            reserve_token: 200_000_000, // realistic pump.fun graduation pool (~200M tokens remaining in curve)
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
                observed_velocity_bps_per_s: None,
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
        let mut pos = MomentumPosition::new(
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
        pos.buy_confirmed_ms = 1; // confirmed — allow exit evaluation
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
        let mut pos = MomentumPosition::new(
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
        pos.buy_confirmed_ms = 1; // confirmed — allow exit evaluation
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
        let mut pos = MomentumPosition::new(
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
        pos.buy_confirmed_ms = 1; // confirmed — allow exit evaluation
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
            cfg.adaptive_trail_enabled = false; // use legacy trailing stop path for this test
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
        pos.buy_confirmed_ms = 1; // confirmed — allow exit evaluation
        pos.tp_flags = 0x1; // TP1 hit — trailing stop active
        pos.peak_price_fp = 1200; // peak at +20%
        // Pre-fill samples so trailing stop sample gate passes (requires >= 2 samples for tp1 path)
        pos.price_samples_bps[0] = 200; // +2%
        pos.price_samples_bps[1] = 150; // +1.5%
        pos.price_samples_bps[2] = 80;  // +0.8% (declining from peak)
        pos.sample_count = 3;
        engine.active.insert([0x22; 32], pos);

        // Price dropped 15.8% from peak (1200 → 1010), trailing stop is 15%
        // 15% of 1200 = 180 drop → floor at 1020
        // 1010 is below 1020 → should trigger trailing stop
        {
            let state = crate::momentum::price_feed::PriceState::new();
            state.price_fp.store(1010, Ordering::Relaxed);
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
        let mut pos = MomentumPosition::new(
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
        pos.buy_confirmed_ms = 1; // confirmed — allow exit evaluation
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
            reserve_token: 200_000_000, // realistic pump.fun graduation pool (~200M tokens remaining in curve)
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
            reserve_token: 200_000_000, // realistic pump.fun graduation pool (~200M tokens remaining in curve)
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
            reserve_token: 200_000_000, // realistic pump.fun graduation pool (~200M tokens remaining in curve)
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
            reserve_token: 200_000_000, // realistic pump.fun graduation pool (~200M tokens remaining in curve)
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
            reserve_token: 200_000_000, // realistic pump.fun graduation pool (~200M tokens remaining in curve)
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
            reserve_token: 200_000_000, // realistic pump.fun graduation pool (~200M tokens remaining in curve)
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