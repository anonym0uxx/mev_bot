//! Configuration for the post-graduation momentum engine.
//!
//! All fields have serde defaults so the momentum section can be
//! omitted entirely from canary.json (engine defaults to disabled).

use serde::{Deserialize, Serialize};
use super::tod::MomentumTodConfig;
use super::activity_gate::ActivityGateConfig;
use super::position::TrailConfig;

// ── RPC sender default functions ─────────────────────────────────────────────

fn default_priority_fee() -> u64 { 1_000 }
fn default_max_retries() -> u32 { 3 }
fn default_retry_delay() -> u64 { 500 }
fn default_confirm_timeout() -> u64 { 30_000 }
fn default_skip_preflight() -> bool { true }
fn default_cb_threshold() -> u32 { 5 }
fn default_cb_cooldown() -> u64 { 120_000 }
fn default_jito_fallback_tip() -> u64 { 100_000 }
fn default_rpc_primary() -> bool { true }

/// Configuration for RPC transaction sending: priority fees, retries,
/// circuit breaker, and Jito fallback.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RpcSenderConfig {
    /// Compute unit price in micro-lamports. Default: 1000.
    #[serde(default = "default_priority_fee")]
    pub priority_fee_microlamports: u64,
    /// Maximum send attempts before giving up. Default: 3.
    #[serde(default = "default_max_retries")]
    pub max_send_retries: u32,
    /// Delay between retry attempts (ms). Default: 500.
    #[serde(default = "default_retry_delay")]
    pub retry_delay_ms: u64,
    /// How long to wait for transaction confirmation (ms). Default: 30000.
    #[serde(default = "default_confirm_timeout")]
    pub confirm_timeout_ms: u64,
    /// Skip preflight simulation before sending. Default: true.
    #[serde(default = "default_skip_preflight")]
    pub skip_preflight: bool,
    /// Consecutive failures before circuit breaker opens. Default: 5.
    #[serde(default = "default_cb_threshold")]
    pub circuit_breaker_threshold: u32,
    /// Cooldown (ms) before circuit breaker resets. Default: 120000.
    #[serde(default = "default_cb_cooldown")]
    pub circuit_breaker_cooldown_ms: u64,
    /// Jito bundle tip (lamports) when falling back to Jito. Default: 100000.
    #[serde(default = "default_jito_fallback_tip")]
    pub jito_fallback_tip: u64,
    /// Whether RPC is the primary send path (true) or Jito is primary (false). Default: true.
    #[serde(default = "default_rpc_primary")]
    pub rpc_primary: bool,
}

impl Default for RpcSenderConfig {
    fn default() -> Self {
        Self {
            priority_fee_microlamports: default_priority_fee(),
            max_send_retries: default_max_retries(),
            retry_delay_ms: default_retry_delay(),
            confirm_timeout_ms: default_confirm_timeout(),
            skip_preflight: default_skip_preflight(),
            circuit_breaker_threshold: default_cb_threshold(),
            circuit_breaker_cooldown_ms: default_cb_cooldown(),
            jito_fallback_tip: default_jito_fallback_tip(),
            rpc_primary: default_rpc_primary(),
        }
    }
}

/// Configuration for the momentum trading engine.
///
/// Loaded from the `momentum` section of canary.json.
/// All fields default via `#[serde(default)]` — omitting the section
/// entirely yields a disabled engine with safe paper-mode defaults.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct MomentumConfig {
    /// Master toggle. Must be true to process any graduation events.
    pub enabled: bool,
    /// Paper mode — log trades but do not submit transactions.
    pub paper_mode: bool,
    /// Delay in ms between graduation detection and entry (allows price discovery).
    pub entry_delay_ms: u64,
    /// Minimum graduation score (0-100, excl. recovery at filter time) to schedule entry.
    pub min_grad_score: u8,
    /// Position size in SOL per entry.
    pub position_size_sol: f64,
    /// Maximum concurrent open positions.
    pub max_concurrent: u8,
    /// Take-profit tier 1: trigger at this % gain.
    pub tp1_pct: f64,
    /// Take-profit tier 1: fraction of position to exit (0.0–1.0).
    pub tp1_exit_pct: f64,
    /// Take-profit tier 2: trigger at this % gain.
    pub tp2_pct: f64,
    /// Take-profit tier 2: fraction of position to exit (0.0–1.0).
    pub tp2_exit_pct: f64,
    /// Take-profit tier 3 (ceiling): trigger at this % gain.
    pub tp3_pct: f64,
    /// Take-profit tier 3: fraction of position to exit (0.0–1.0).
    pub tp3_exit_pct: f64,
    /// Trailing stop: exit when price drops this % below peak (active after TP2).
    pub trailing_stop_pct: f64,
    /// Tiered trailing stop: gain threshold for tier 1 (tight trail). Default: 200 bps.
    pub trailing_stop_tier1_max_bps: i64,
    /// Tiered trailing stop: trail % for tier 1 (small gains). Default: 8.0%.
    pub trailing_stop_tier1_pct: f64,
    /// Tiered trailing stop: gain threshold for tier 2 (medium trail). Default: 500 bps.
    pub trailing_stop_tier2_max_bps: i64,
    /// Tiered trailing stop: trail % for tier 2 (medium gains). Default: 12.0%.
    pub trailing_stop_tier2_pct: f64,
    /// Hard stop-loss: immediate full exit at this % loss.
    pub hard_sl_pct: f64,
    /// Time-based stop-loss: exit if still losing after this many ms.
    pub time_sl_ms: u64,
    /// Maximum hold time before forced exit (ms).
    pub max_hold_ms: u64,
    /// Momentum decay exit: minimum hold time before decay exit can trigger. Default: 30000ms.
    /// Before this, early noise would give false signals.
    pub momentum_decay_min_hold_ms: u64,
    /// Momentum decay exit: score threshold. Exit when score drops below this. Default: -150.0.
    /// Score = exponentially weighted sum of recent bps deltas (decay=0.5 per tick).
    pub momentum_decay_threshold: f64,
    /// Momentum decay exit: window of recent ticks to consider. Default: 8 (~8.4s).
    pub momentum_decay_window: usize,
    /// After this many ms, activate a tight trailing stop instead of holding blindly.
    /// Set to 0 to disable (use original max_hold behavior). Default: 200_000ms (200s).
    pub max_hold_trail_activation_ms: u64,
    /// Trailing stop percentage applied after max_hold_trail_activation_ms.
    /// Tighter than the regular trailing_stop_pct to take profits aggressively near maturity.
    /// Default: 3.0% — exit if price drops 3% from peak after 200s.
    pub max_hold_trail_pct: f64,
    /// Tick interval: check positions every this many ms.
    pub check_ms: u64,
    /// Daily loss cap as fraction of current wallet balance (0.0–1.0).
    /// Circuit breaker trips when |daily_pnl_lamports| > wallet_balance_lamports * daily_loss_cap_pct.
    /// Scales automatically with wallet size. Default: 0.10 (10%).
    /// At 1.5 SOL wallet: trips at 0.15 SOL daily loss.
    /// At 5.0 SOL wallet: trips at 0.50 SOL daily loss.
    /// Set to 1.0 to effectively disable (never trips).
    pub daily_loss_cap_pct: f64,
    /// Raydium AMM fee in basis points.
    pub raydium_fee_bps: u32,
    /// PumpSwap fee in basis points.
    pub pumpswap_fee_bps: u32,
    /// How many ticks between price samples (default 7 ≈ 1.05s at 150ms tick).
    /// Lower = more samples captured per trade. PRICE_SAMPLES cap is 30 slots.
    pub sample_interval_ticks: u64,
    /// Tighter stop-loss applied during the first `micro_sl_ticks` ticks.
    /// Catches immediate dump-on-graduation tokens early. Default: 8.0%.
    pub micro_sl_pct: f64,
    /// Number of ticks during which micro_sl_pct applies (default: 20 ≈ 3s at 150ms).
    pub micro_sl_ticks: u64,
    /// Micro exit: only active for this many ms after entry. Default: 4500ms (4 samples at 1050ms cadence).
    pub micro_exit_window_ms: u64,
    /// Micro exit: per-sample-tick velocity threshold in bps. Default: -200.
    /// Exit if 2 consecutive ticks both have delta < this value.
    pub micro_exit_velocity_bps: i32,
    /// Micro exit: number of consecutive below-threshold ticks required. Default: 2.
    pub micro_exit_n_consecutive: u8,
    /// Max ms to wait for live price feed before skipping an entry.
    /// If price hasn't arrived within this window, the entry is abandoned
    /// rather than entering at the stale graduation reserve price.
    /// Default: 2000ms (gives Helius WSS ~2s to deliver live price).
    pub no_price_timeout_ms: u64,
    /// Position size in SOL for tier-0 entries (first price reading, no momentum signal yet).
    /// Set to 0.0 to disable tier-0 sizing (use regular grad_score tiers).
    /// Default: 0.10 SOL — reduces blind entry exposure by 2/3.
    pub tier0_size_sol: f64,
    /// Cooldown period before a mint can be re-entered after close.
    /// Prevents CoreCast duplicate graduation events from causing phantom re-entries.
    /// Default: 300_000ms (5 minutes).
    pub reentry_cooldown_ms: u64,
    /// How often to poll Helius RPC for vault account prices (ms). Default: 500.
    pub price_poll_interval_ms: u64,

    // ══════════════════════════════════════════════════════════
    // OBSERVATION WINDOW (T+5-8s sniper dump detection)
    // ══════════════════════════════════════════════════════════

    /// Duration of the observation window after graduation detection (ms).
    /// During this window, price and reserve samples are collected to detect
    /// sniper bot dump patterns before committing to entry.
    /// Set to 0 to disable (legacy behavior: enter immediately after entry_delay_ms).
    /// Default: 6000 (6 seconds).
    pub observation_window_ms: u64,

    /// Minimum price samples required during observation window before entry decision.
    /// If fewer samples arrive (price feed lag), the entry is rejected.
    /// At 500ms poll interval, 6s window yields ~12 samples; 5 is a safe floor.
    /// Default: 5.
    pub observation_min_samples: u8,

    /// Maximum allowed drawdown from peak price during observation window (basis points, negative).
    /// If price drops more than this from the observed peak, entry is rejected (sniper dump).
    /// -2000 bps = -20% from peak. Default: -2000.
    pub observation_max_drawdown_bps: i32,

    /// Minimum SOL reserve in pool at the end of the observation window (lamports).
    /// Pools drained below this threshold during observation are rejected.
    /// Default: 50_000_000_000 (50 SOL).
    pub observation_min_reserve_sol_lamports: u64,

    /// Require price stability at the end of the observation window.
    /// When true, the last 3 price samples must be within 10% of each other.
    /// Catches volatile tokens still mid-dump at window expiry.
    /// Default: true.
    pub observation_require_price_stability: bool,
    /// Minimum observation window before early triggers can fire (ms).
    pub observation_window_min_ms: u64,
    /// Early entry trigger: if price velocity >= this (bps/s), skip remaining window and enter.
    pub observation_early_entry_velocity_bps_per_s: i64,
    /// Minimum price samples before early entry trigger can fire.
    pub observation_early_entry_min_samples: u8,
    /// Early abort trigger: if drawdown from peak < this (bps), abort immediately.
    /// Should be less negative than observation_max_drawdown_bps to abort faster.
    pub observation_early_abort_drawdown_bps: i32,


    // ══════════════════════════════════════════════════════════
    // MOMENTUM STATE CLASSIFICATION
    // ══════════════════════════════════════════════════════════

    /// d(bps/s) above which state = ACCELERATING. Default: 100.
    pub momentum_accel_threshold_bps: i32,
    /// d(bps/s) below which state = DECELERATING (requires 2 consecutive). Default: -100.
    pub momentum_decel_threshold_bps: i32,
    /// d(bps/s) below which state = REVERSING (single sample). Default: -500.
    pub momentum_reversal_threshold_bps: i32,

    // ══════════════════════════════════════════════════════════
    // DYNAMIC TRAILING STOP (state-aware widths)
    // ══════════════════════════════════════════════════════════

    /// Trail width when ACCELERATING — wide to avoid shakeouts. Default: 15.0%.
    pub trailing_stop_accel_pct: f64,
    /// Trail width when DECELERATING — tighter to protect gains. Default: 5.0%.
    pub trailing_stop_decel_pct: f64,
    /// Trail width when REVERSING — near-immediate exit. Default: 3.0%.
    pub trailing_stop_reversal_pct: f64,
    /// Minimum price samples before trailing stop evaluation begins.
    /// Prevents premature exits on explosive tokens where the first few samples
    /// show discontinuous price action (gap-downs of 70%+ in one poll).
    /// Default: 5. Set to 0 to disable (evaluate from first sample).
    pub trailing_stop_min_samples: u8,
    /// Number of consecutive below-floor readings required before trailing stop fires.
    /// Eliminates single-poll noise exits — price must stay below trail floor
    /// for N consecutive samples before triggering exit.
    /// Default: 2. Set to 1 for legacy behavior (fire immediately).
    pub trailing_stop_confirm_samples: u8,

    /// ATR multiplier for adaptive trail width. trail_pct = max(base, k * ATR / 100).
    /// Default: 2.5 — trail = 2.5× average tick volatility.
    pub trail_atr_multiplier: f64,
    /// ATR window: number of samples to compute ATR over. Default: 10 (~10.5s lookback).
    pub trail_atr_window: usize,
    /// Minimum sample count before ATR adaptive trail activates. Default: 4.
    pub trail_min_samples_for_atr: u8,

    // ══════════════════════════════════════════════════════════
    // TOP DETECTION
    // ══════════════════════════════════════════════════════════

    /// Number of concurrent top signals needed to trigger exit (of 5 possible). Default: 2.
    pub top_detection_strong_signals: u8,
    /// Percentage of position to exit on strong top signal. Default: 75.
    pub top_detection_exit_pct: u8,
    /// Trail width for remaining position after top exit. Default: 3.0%.
    pub top_detection_trail_pct: f64,

    // ══════════════════════════════════════════════════════════
    // DEAD ZONE DETECTION
    // ══════════════════════════════════════════════════════════

    /// Adaptive dead zone: ms of WS silence before exit when 0 WS notifications received.
    pub dead_zone_ws_zero_ms: u64,
    /// Adaptive dead zone: ms of WS silence before exit when 1..=sparse_n notifications received.
    pub dead_zone_ws_sparse_ms: u64,
    /// Adaptive dead zone: ms of WS silence before exit when > sparse_n notifications received.
    pub dead_zone_ws_active_ms: u64,
    /// Boundary between sparse and active notification tiers.
    pub dead_zone_ws_sparse_n: u16,
    /// Fallback dead zone ms when no WS data exists (ws_notif_last_ms == 0).
    pub dead_zone_ws_fallback_ms: u64,

    /// Time window for Phase 1 dead zone check (ms). Default: 10_000 (10s).
    pub dead_zone_early_ms: u64,
    /// Minimum cumulative bps to survive Phase 1. Default: 50.
    pub dead_zone_early_bps: i32,
    /// Time window for Phase 2 dead zone check (ms). Default: 60_000 (60s).
    pub dead_zone_confirmed_ms: u64,
    /// Minimum cumulative bps to survive Phase 2. Default: 200.
    pub dead_zone_confirmed_bps: i32,
    /// Rolling window bps threshold for stagnation. Default: 300.
    pub dead_zone_stagnant_bps: i32,
    /// Rolling window size for stagnation check (ms). Default: 30_000.
    pub dead_zone_stagnant_window_ms: u64,

    // ══════════════════════════════════════════════════════════
    // PRICE-DIRECT DEAD ZONE (Phase 5)
    // ══════════════════════════════════════════════════════════

    /// Max gain threshold below which a priced token is considered flat/dead. Default: 200 bps.
    pub dead_zone_price_flat_bps: i32,
    /// Minimum non-zero samples required before Phase 5 fires. Default: 3.
    pub dead_zone_price_flat_min_samples: u8,
    /// Minimum hold time before Phase 5 can fire (ms). Default: 8_000.
    pub dead_zone_price_flat_min_hold_ms: u64,
    /// If max(samples) < this value (all negative), exit as hard_sl. Default: -100 bps.
    pub dead_zone_price_always_down_bps: i32,

    // ══════════════════════════════════════════════════════════
    // RESERVE FLATNESS DEAD ZONE (Phase 5B)
    // ══════════════════════════════════════════════════════════

    /// Minimum drain_samples entries required to evaluate reserve flatness. Default: 5.
    /// Set to 0 to disable reserve flatness detection entirely.
    pub dead_zone_reserve_flat_min_samples: usize,
    /// Maximum allowed spread (lamports) between min and max reserve across recent samples.
    /// If spread < this, reserve is considered flat (no trades happening). Default: 100_000 (0.0001 SOL).
    pub dead_zone_reserve_flat_tolerance_lamports: u64,
    /// Minimum hold time (ms) before reserve flatness can fire. Default: 3_000 (3s).
    pub dead_zone_reserve_flat_min_hold_ms: u64,

    // ══════════════════════════════════════════════════════════
    // EARLY ABORT (Phase 6)
    // ══════════════════════════════════════════════════════════

    /// Phase 6: Early abort. If max(nonzero_samples) < this threshold after
    /// early_abort_min_samples samples AND hold >= early_abort_min_hold_ms → exit.
    /// Default: 30 bps. Set to 0 to disable.
    pub early_abort_max_bps: i32,

    /// Minimum samples before early abort fires. Default: 3.
    pub early_abort_min_samples: u8,

    /// Minimum hold time (ms) before early abort fires. Default: 3_000.
    pub early_abort_min_hold_ms: u64,

    // ══════════════════════════════════════════════════════════
    // PROBE-THEN-SCALE ENTRY (TASK 2: Hard SL Reduction)
    // ══════════════════════════════════════════════════════════

    /// Master toggle for probe-then-scale entry. When enabled, entries start
    /// at probe_size_sol and only scale up after probe_hold_ms if price is stable.
    /// Targets 60 trades that dump <1s of entry (avg -55.85 mSOL → ~-2.5 mSOL).
    pub probe_entry_enabled: bool,
    /// How long to hold the probe position before evaluating scale-in (ms).
    /// Default: 2000ms (2s). Trades dumping <1s exit at probe size.
    pub probe_hold_ms: u64,
    /// Bps threshold for immediate probe exit (dump detection).
    /// If price drops more than this during probe phase, exit immediately.
    /// Default: -500 bps (-5%). At 0.05 SOL probe, max loss = ~2.5 mSOL.
    pub probe_dump_threshold_bps: i32,
    /// Minimum bps gain required to scale up after probe_hold_ms.
    /// If gain < this value, stay at probe size with tight SL.
    /// Default: -300 bps (-3%). Between -3% and -5% = stay at probe.
    pub probe_scale_min_bps: i32,
    /// Whether to require at least one price sample before scaling up.
    /// If true and 0 samples after probe_hold_ms, stay at probe size.
    /// Default: true (don't scale blind).
    pub probe_scale_require_price: bool,

    // ══════════════════════════════════════════════════════════
    // SCALE-IN ENTRY
    // ══════════════════════════════════════════════════════════

    /// Initial probe entry size. Default: 0.10 SOL.
    pub probe_size_sol: f64,
    /// s[0] bps threshold for strong conviction scale-in. Default: 300.
    pub scale_in_s0_strong_bps: i32,
    /// SOL to add when s[0] shows strong momentum (≥300 bps). Default: 0.40.
    pub scale_in_s0_strong_sol: f64,
    /// s[0] bps threshold for moderate conviction. Default: 100.
    pub scale_in_s0_moderate_bps: i32,
    /// SOL to add when s[0] shows moderate momentum (100-299 bps). Default: 0.20.
    pub scale_in_s0_moderate_sol: f64,
    /// s[1] bps threshold for second confirmation. Default: 200.
    pub scale_in_s1_moderate_bps: i32,
    /// SOL to add on s[1] confirmation. Default: 0.15.
    pub scale_in_s1_sol: f64,
    /// Absolute max position size (probe + all scale-ins). Default: 0.50 SOL.
    pub max_total_size_sol: f64,

    // ══════════════════════════════════════════════════════════
    // SCALE-IN GATES (TASK 3 + TASK 4)
    // ══════════════════════════════════════════════════════════

    /// Minimum WebSocket notification count required before scale-in is allowed.
    /// ws_notif measures realized trading activity on the Raydium/PumpSwap pool.
    /// ws_notif=0 → 0.0% WR (165 trades). ws_notif≥10 → 27.2% WR (371 trades).
    /// Set to 0 to disable. Default: 10.
    pub min_ws_notif_for_scale_in: u16,

    /// Minimum bps at price_samples_bps[1] (second sample, ~2s after entry)
    /// required before scale-in is allowed.
    /// s[1]=0: WR=6.8% (676 trades). s[1]>0: WR=50.9% (118 trades).
    /// s[0] is always 0 (entry baseline), so s[1] is the first informative sample.
    /// Set to i32::MIN to disable. Default: 1 (any positive movement).
    pub scale_in_min_s1_bps: i32,

    // ══════════════════════════════════════════════════════════
    // SCORE-AWARE SCALE-IN
    // ══════════════════════════════════════════════════════════

    /// Grad score threshold for high-conviction scale-in (lower s0 required). Default: 65.
    pub scale_in_high_score_threshold: u8,
    /// s[0] bps required for strong scale-in when grad_score >= high threshold. Default: 200.
    pub scale_in_high_score_s0_bps: i32,
    /// Grad score threshold below which scale-in requires higher s0. Default: 35.
    pub scale_in_low_score_threshold: u8,
    /// s[0] bps required for strong scale-in when grad_score < low threshold. Default: 400.
    pub scale_in_low_score_s0_bps: i32,

    // ══════════════════════════════════════════════════════════
    // TIME-OF-DAY GATING (TASK 4)
    // ══════════════════════════════════════════════════════════

    /// Time-of-day configuration for entry sizing.
    pub tod_config: MomentumTodConfig,

    // ══════════════════════════════════════════════════════════
    // TIME-DECAY TRAILING STOP (TASK 5)
    // ══════════════════════════════════════════════════════════

    /// Enable time-decay trailing stop. Default: true.
    pub time_decay_trailing_enabled: bool,
    /// Hold durations (ms) at which trailing stop tightens. Must be ascending.
    pub time_decay_stages_ms: Vec<u64>,
    /// Trail widths (bps) corresponding to each stage. Must be descending (tighter over time).
    pub time_decay_trail_bps: Vec<u16>,

    // ══════════════════════════════════════════════════════════
    // ADAPTIVE TRAILING STOP + WINNER MANAGEMENT (TASK 6)
    // ══════════════════════════════════════════════════════════

    /// Enable adaptive gain-tiered trailing stop. When true, replaces
    /// momentum-state-based trailing (Accel=25%, Sustain=15%, etc.) with
    /// gain-tiered trailing calibrated for memecoin dynamics.
    /// Default: true. Set to false to revert to legacy behavior.
    pub adaptive_trail_enabled: bool,

    /// Adaptive trailing stop configuration: gain tiers, confirm samples, etc.
    /// Only used when `adaptive_trail_enabled` is true.
    #[serde(default)]
    pub trail_config: TrailConfig,

    /// Pre-entry activity gate configuration.
    /// Blocks dead tokens by requiring minimum WS activity before entry.
    /// Omit section to use defaults (enabled, 5 notifs, 2s stale, 1 buy, 50bps range).
    #[serde(default)]
    pub activity_gate: ActivityGateConfig,

    /// Enable winner protection (momentum lock). When true, profitable positions
    /// with ongoing WebSocket activity are protected from ALL time-based exits
    /// (time_sl, dead_zone, stagnation, early_abort, etc.). Only the trailing
    /// stop can close a momentum-locked position.
    ///
    /// On-chain evidence: biggest winner (+82.2%) held 40 minutes. Time-based
    /// exits kill winners early, capping avg win at +15.8% (need +24% for +EV).
    /// Default: true.
    pub winner_protection_enabled: bool,

    // ══════════════════════════════════════════════════════════
    // STAGNATION EXIT (TASK 5)
    // ══════════════════════════════════════════════════════════

    /// Hold time (ms) after which a stagnant position is exited. Default: 60_000 (60s).
    /// A position is stagnant if ALL price_samples_bps are zero after this time.
    pub stagnation_exit_ms: u64,

    // ══════════════════════════════════════════════════════════
    // DEAD TOKEN FAST EXIT (TASK 5B)
    // ══════════════════════════════════════════════════════════

    /// Enable fast exit for dead tokens with zero trading activity.
    /// Detects: ws_notif_count=0 AND all price_samples flat AND hold≥min_hold_ms.
    /// Frees position slot 3-4x faster than waiting for time_sl.
    /// Default: true.
    pub dead_token_fast_exit_enabled: bool,

    /// Minimum hold time (ms) before dead token fast exit can fire.
    /// Must wait long enough for price samples to populate (~1s each).
    /// Default: 5000ms (5 samples at 1050ms cadence).
    pub dead_token_fast_exit_min_hold_ms: u64,

    /// Minimum price sample count before flat detection fires.
    /// Needs enough samples to confirm flatness, not just early data gap.
    /// Default: 5.
    pub dead_token_fast_exit_min_samples: u8,

    // ══════════════════════════════════════════════════════════
    // HARD GATE: WHALE/BOT PUMP REJECTION (TASK 1)
    // ══════════════════════════════════════════════════════════

    /// Hard gate: minimum graduation speed (seconds) to accept.
    /// Tokens graduating faster than this are bot/whale bonding curve fills
    /// with near-zero post-graduation momentum (speed≤90s: 7.3% WR in backtest).
    /// Default: 90. Set to 0 to disable.
    pub min_grad_speed_s: u32,

    /// Hard gate: max graduation volume (SOL) when grad is fast (speed < min_grad_speed_s * 2).
    /// Fast-ish + high volume = bot pump. Default: 200.0.
    /// Applied when: grad_speed_s < min_grad_speed_s * 2 AND grad_volume_sol >= this value.
    pub max_grad_volume_sol_fast: f64,

    /// Hard gate: absolute max graduation volume (SOL) regardless of speed.
    /// u16 saturation value is 655.35 SOL — anything at/above this is a confirmed whale fill.
    /// Default: 650.0. Set to 0.0 to disable.
    pub max_grad_volume_sol_absolute: f64,

    /// Entry filter: minimum graduation volume (SOL) to accept.
    /// Tokens with volume < 50 SOL have 2.2% WR and net -0.167 SOL (massive loser bucket).
    /// Default: 50.0. Set to 0.0 to disable.
    pub min_grad_volume_sol: f64,

    /// Entry filter: maximum graduation volume (SOL) to accept.
    /// Tokens with volume > 200 SOL have 2.6% WR and net -0.012 SOL (marginal loser).
    /// Sweet spot is 50-200 SOL (combined 13.6% WR, +0.422 SOL).
    /// Default: 200.0. Set to 0.0 to disable.
    pub max_grad_volume_sol: f64,

    /// Maximum entries per mint per engine session.
    /// Re-entries on the same token are net losers beyond 2 entries.
    /// Counter resets on engine restart. Default: 2. Set to 0 to disable.
    pub max_entries_per_mint: u32,

    // ══════════════════════════════════════════════════════════
    // VELOCITY EXIT
    // ══════════════════════════════════════════════════════════

    /// Enable/disable the velocity exit system entirely. Default: true
    pub velocity_exit_enabled: bool,

    /// Velocity threshold in milli-bps/sample (×1000 scale).
    /// Signal fires when regression slope ≤ this. Must be negative.
    /// Default: -150_000 (= -150 bps/sample)
    pub velocity_exit_threshold_mbps: i64,

    /// Acceleration threshold in milli-bps/sample² (×1000 scale).
    /// Fires when acceleration ≤ this while velocity already negative.
    /// Default: -100_000
    pub accel_exit_threshold_mbps: i64,

    /// Minimum peak bps before MomentumCollapse can fire.
    /// Default: 200
    pub momentum_collapse_min_peak_bps: i32,

    /// Drop threshold (bps, negative) from local peak to trigger MomentumCollapse.
    /// Default: -200
    pub momentum_collapse_drop_threshold_bps: i32,

    /// Max samples after local peak for MomentumCollapse (gap-down detector).
    /// Default: 2
    pub momentum_collapse_max_samples: u32,

    /// Lookback window for MomentumCollapse pattern detection.
    /// Must be >= momentum_collapse_max_samples + 2. Default: 5
    pub momentum_collapse_lookback: u32,

    /// Regression window size for velocity. Default: 3
    pub velocity_window: u32,

    /// Window for acceleration (split into halves). Must be >= 4. Default: 4
    pub accel_window: u32,

    /// Minimum samples before any velocity exit fires. Default: 5
    pub velocity_exit_min_samples: u32,

    /// Consecutive ticks condition must hold before VelocityThreshold/AccelCollapse fires.
    /// Default: 2
    pub velocity_exit_confirm_samples: u32,

    /// Minimum current price in bps above entry for velocity exit to fire.
    /// Below this, trailing stop handles it. Default: 50
    pub velocity_exit_min_profit_bps: i32,

    // ══════════════════════════════════════════════════════════
    // WALLET BALANCE MONITOR + KELLY SIZING
    // ══════════════════════════════════════════════════════════

    /// How often to poll wallet balance (ms). Default: 30_000 (30s).
    pub wallet_balance_poll_ms: u64,
    /// Minimum balance (lamports) to allow entries. Below this, engine pauses.
    /// Default: 100_000_000 (0.1 SOL).
    pub min_wallet_balance_lamports: u64,
    /// Extra margin fraction on top of required entry+tip. Default: 0.05 (5%).
    pub balance_safety_margin_pct: f64,
    /// Master toggle for Kelly position sizing. Default: false (fixed probe_size_sol until bootstrap).
    pub kelly_sizing_enabled: bool,
    /// Kelly fraction (0.0–1.0). 1.0 = full Kelly, 0.5 = half Kelly. Default: 0.25.
    pub kelly_fraction: f64,
    /// Use fixed probe_size_sol until this many clean trades. Default: 30.
    pub kelly_bootstrap_trades: usize,
    /// Rolling window of trades for Kelly WR/avgwin/avgloss. Default: 50.
    pub kelly_lookback_trades: usize,
    /// Minimum allowed Kelly-computed size. Default: 0.02.
    pub min_probe_size_sol: f64,
    /// Maximum allowed Kelly-computed size. Default: 0.20.
    pub max_probe_size_sol: f64,
    /// On engine init, scan active positions and force-close any with 0 token balance.
    /// Default: true.
    pub ghost_position_cleanup_enabled: bool,

    // ── Pool resolution gates (FIX-1/4/5) ─────────────────────────────────
    /// Max age (ms) for cold-miss graduation events before rejecting as stale CoreCast backlog.
    /// Cold-miss = grad_speed_s==0 AND volume_sol_x100==0.
    /// CoreCast replays ~430 old Raydium-era graduations/min — this gates them out early.
    /// Set to 0 to disable. Default: 120_000 (2 minutes).
    pub stale_grad_max_age_ms: u64,

    /// Max idle time (ms) for a Raydium pc_vault before treating the pool as dead (FIX-5).
    /// Queries getSignaturesForAddress to find last swap. If older than this → skip.
    /// Set to 0 to disable. Default: 300_000 (5 minutes).
    pub raydium_max_idle_ms: u64,

    /// Minimum LP reserve (lamports) at entry time. Fresh PumpSwap grads land with ~85 SOL.
    /// Default: 40_000_000_000 (40 SOL) — rejects drained/abnormal pools at entry.
    pub min_lp_reserve_entry_lamports: u64,

    /// Maximum LP reserve (lamports) at entry time. Pools >200 SOL are established tokens
    /// with dampened momentum. Default: 200_000_000_000 (200 SOL).
    pub max_lp_reserve_entry_lamports: u64,

    /// Dead zone reserve flat tolerance (lamports) for PumpSwap pools specifically.
    /// PumpSwap has 1% fee → reserve changes per swap are larger than Raydium.
    /// Default: 2_000_000 (0.002 SOL) — wider than Raydium's 1_000_000.
    pub dead_zone_pumpswap_reserve_tolerance_lamports: u64,

    /// Dead zone WS zero timeout for PumpSwap pools (ms).
    /// PumpSwap WS notifications fire less frequently per swap volume.
    /// Default: 10_000 (10s) — looser than Raydium's 8s.
    pub dead_zone_pumpswap_ws_zero_ms: u64,

    // ══════════════════════════════════════════════════════════
    // RPC SENDER CONFIG
    // ══════════════════════════════════════════════════════════

    /// **DEPRECATED** — superseded by dynamic velocity-based fields below.
    /// Kept for backward compat; ignored when max_quote_in_base_pct > 0.
    /// Max SOL overspend multiplier for PumpSwap buy TX (basis: 100 = exact, 115 = 15% buffer).
    pub max_quote_in_multiplier_pct: u32,

    /// Base max_quote_in multiplier when price is stable (pct of position_size, e.g. 110 = 110%).
    pub max_quote_in_base_pct: u32,
    /// Per velocity bps divisor: for each bps/s of observed price velocity,
    /// add (velocity_bps_per_s * propagation_s / this_value)% to the multiplier.
    /// e.g. 5 means 100 bps/s velocity with 4s propagation → +80% multiplier (capped).
    pub max_quote_in_per_velocity_divisor: u32,
    /// Hard cap on max_quote_in multiplier regardless of velocity (pct).
    pub max_quote_in_cap_pct: u32,
    /// Estimated TX propagation time in seconds (used to extrapolate price at landing).
    pub max_quote_in_tx_propagation_s: u32,

    /// RPC transaction sender configuration: priority fees, retries,
    /// circuit breaker, and Jito fallback settings.
    #[serde(default)]
    pub rpc_sender: RpcSenderConfig,

    // ══════════════════════════════════════════════════════════
    // SESSION CIRCUIT BREAKER & RISK MANAGEMENT (TASK 5)
    // ══════════════════════════════════════════════════════════

    /// Session drawdown: pause trading for session_pause_duration_ms when
    /// cumulative session net PnL drops below -session_max_loss_pause_sol.
    /// Default: 0.10 SOL. Set to 0.0 to disable.
    pub session_max_loss_pause_sol: f64,

    /// Session drawdown: halt all trading (manual resume required) when
    /// cumulative session net PnL drops below -session_max_loss_halt_sol.
    /// Default: 0.20 SOL. Set to 0.0 to disable.
    pub session_max_loss_halt_sol: f64,

    /// Duration (ms) to pause trading after session_max_loss_pause_sol is hit.
    /// Default: 1_800_000 (30 minutes).
    pub session_pause_duration_ms: u64,

    /// After this many consecutive losses, reduce position size by 50%.
    /// Default: 5. Set to 0 to disable.
    pub consecutive_loss_halfsize: u32,

    /// After this many consecutive losses, pause trading for loss_pause_duration_ms.
    /// Default: 10. Set to 0 to disable.
    pub consecutive_loss_pause: u32,

    /// Duration (ms) to pause trading after consecutive_loss_pause is hit.
    /// Default: 900_000 (15 minutes).
    pub loss_pause_duration_ms: u64,

    /// If rolling win rate (last rolling_wr_window trades) drops below this %, pause trading.
    /// Default: 5.0. Set to 0.0 to disable.
    pub min_rolling_wr_pct: f64,

    /// Window size for rolling win rate calculation.
    /// Default: 50.
    pub rolling_wr_window: u32,
}

impl Default for MomentumConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            paper_mode: true,
            entry_delay_ms: 15_000,
            min_grad_score: 30,
            position_size_sol: 0.3,
            max_concurrent: 5,
            tp1_pct: 5.0,
            tp1_exit_pct: 0.0,
            tp2_pct: 15.0,
            tp2_exit_pct: 0.0,
            tp3_pct: 999.0,
            tp3_exit_pct: 0.40,
            trailing_stop_pct: 8.0,
            trailing_stop_tier1_max_bps: 200,
            trailing_stop_tier1_pct: 8.0,
            trailing_stop_tier2_max_bps: 500,
            trailing_stop_tier2_pct: 12.0,
            hard_sl_pct: 12.0,
            time_sl_ms: 60_000,
            max_hold_ms: 600_000,
            momentum_decay_min_hold_ms: 30_000,
            momentum_decay_threshold: -150.0,
            momentum_decay_window: 8,
            max_hold_trail_activation_ms: 200_000,
            max_hold_trail_pct: 5.0,
            check_ms: 150,
            daily_loss_cap_pct: 0.10,
            raydium_fee_bps: 25,
            pumpswap_fee_bps: 100,
            sample_interval_ticks: 7,
            micro_sl_pct: 8.0,
            micro_sl_ticks: 20,
            micro_exit_window_ms: 4_500,
            micro_exit_velocity_bps: -200,
            micro_exit_n_consecutive: 2,
            no_price_timeout_ms: 2_000,
            tier0_size_sol: 0.10,
            reentry_cooldown_ms: 300_000,
            price_poll_interval_ms: 500,

            // Observation window (T+5-8s sniper dump detection)
            observation_window_ms: 6_000,
            observation_min_samples: 5,
            observation_max_drawdown_bps: -2_000,
            observation_min_reserve_sol_lamports: 50_000_000_000, // 50 SOL
            observation_require_price_stability: true,
            observation_window_min_ms: 2_000,
            observation_early_entry_velocity_bps_per_s: 150,
            observation_early_entry_min_samples: 3,
            observation_early_abort_drawdown_bps: -500,

            // Momentum state classification
            momentum_accel_threshold_bps: 100,
            momentum_decel_threshold_bps: -100,
            momentum_reversal_threshold_bps: -500,

            // Dynamic trailing stop
            trailing_stop_min_samples: 5,
            trailing_stop_confirm_samples: 2,
            trailing_stop_accel_pct: 15.0,
            trailing_stop_decel_pct: 5.0,
            trailing_stop_reversal_pct: 3.0,
            trail_atr_multiplier: 2.5,
            trail_atr_window: 10,
            trail_min_samples_for_atr: 4,

            // Top detection
            top_detection_strong_signals: 2,
            top_detection_exit_pct: 75,
            top_detection_trail_pct: 3.0,

            // Adaptive dead zone (WS-based)
            dead_zone_ws_zero_ms: 8_000,
            dead_zone_ws_sparse_ms: 12_000,
            dead_zone_ws_active_ms: 15_000,
            dead_zone_ws_sparse_n: 3,
            dead_zone_ws_fallback_ms: 10_000,

            // Dead zone detection
            dead_zone_early_ms: 10_000,
            dead_zone_early_bps: 100,
            dead_zone_confirmed_ms: 60_000,
            dead_zone_confirmed_bps: 200,
            dead_zone_stagnant_bps: 300,
            dead_zone_stagnant_window_ms: 30_000,

            // Price-direct dead zone (Phase 5)
            dead_zone_price_flat_bps: 200,
            dead_zone_price_flat_min_samples: 6,
            dead_zone_price_flat_min_hold_ms: 12_000,
            dead_zone_price_always_down_bps: -100,

            // Reserve flatness dead zone (Phase 5B)
            dead_zone_reserve_flat_min_samples: 5,
            dead_zone_reserve_flat_tolerance_lamports: 1_000_000, // 0.001 SOL — PumpSwap 1% fee on 0.05 SOL = ~500K lamports
            dead_zone_reserve_flat_min_hold_ms: 8_000,

            // Early abort (Phase 6)
            early_abort_max_bps: 30,
            early_abort_min_samples: 6,
            early_abort_min_hold_ms: 8_000,

            // Probe-then-scale entry (TASK 2)
            probe_entry_enabled: true,
            probe_hold_ms: 2_000,
            probe_dump_threshold_bps: -500,
            probe_scale_min_bps: -300,
            probe_scale_require_price: true,

            // Scale-in entry
            probe_size_sol: 0.10,
            scale_in_s0_strong_bps: 300,
            scale_in_s0_strong_sol: 0.40,
            scale_in_s0_moderate_bps: 100,
            scale_in_s0_moderate_sol: 0.20,
            scale_in_s1_moderate_bps: 200,
            scale_in_s1_sol: 0.15,
            max_total_size_sol: 0.50,

            // Scale-in gates (Task 3 + Task 4)
            min_ws_notif_for_scale_in: 10,
            scale_in_min_s1_bps: 1,

            // Score-aware scale-in
            scale_in_high_score_threshold: 65,
            scale_in_high_score_s0_bps: 200,
            scale_in_low_score_threshold: 35,
            scale_in_low_score_s0_bps: 400,

            // Time-of-day gating (TASK 4)
            tod_config: MomentumTodConfig::default(),

            // Time-decay trailing stop (TASK 5)
            time_decay_trailing_enabled: true,
            time_decay_stages_ms: vec![30_000, 60_000, 120_000, 180_000, 240_000],
            time_decay_trail_bps: vec![800, 500, 300, 200, 100],

            // Adaptive trailing stop + winner management (TASK 6)
            adaptive_trail_enabled: true,
            trail_config: TrailConfig::default(),
            activity_gate: ActivityGateConfig::default(),
            winner_protection_enabled: true,

            // Stagnation exit (TASK 5)
            stagnation_exit_ms: 60_000,

            // Dead token fast exit (TASK 5B)
            dead_token_fast_exit_enabled: true,
            dead_token_fast_exit_min_hold_ms: 5_000,
            dead_token_fast_exit_min_samples: 5,

            // Hard gate: whale/bot pump rejection (TASK 1)
            min_grad_speed_s: 90,
            max_grad_volume_sol_fast: 200.0,
            max_grad_volume_sol_absolute: 650.0,
            min_grad_volume_sol: 50.0,
            max_grad_volume_sol: 200.0,
            max_entries_per_mint: 2,

            // Velocity exit
            velocity_exit_enabled: true,
            velocity_exit_threshold_mbps: -150_000,
            accel_exit_threshold_mbps: -100_000,
            momentum_collapse_min_peak_bps: 200,
            momentum_collapse_drop_threshold_bps: -200,
            momentum_collapse_max_samples: 2,
            momentum_collapse_lookback: 5,
            velocity_window: 3,
            accel_window: 4,
            velocity_exit_min_samples: 5,
            velocity_exit_confirm_samples: 2,
            velocity_exit_min_profit_bps: 50,

            // Wallet balance monitor + Kelly sizing
            wallet_balance_poll_ms: 30_000,
            min_wallet_balance_lamports: 100_000_000,
            balance_safety_margin_pct: 0.05,
            kelly_sizing_enabled: false,
            kelly_fraction: 0.25,
            kelly_bootstrap_trades: 30,
            kelly_lookback_trades: 50,
            min_probe_size_sol: 0.02,
            max_probe_size_sol: 0.20,
            ghost_position_cleanup_enabled: true,

            // Pool resolution gates
            stale_grad_max_age_ms: 120_000,  // 2 minutes
            raydium_max_idle_ms: 300_000,    // 5 minutes

            min_lp_reserve_entry_lamports: 40_000_000_000,  // 40 SOL
            max_lp_reserve_entry_lamports: 200_000_000_000, // 200 SOL

            dead_zone_pumpswap_reserve_tolerance_lamports: 2_000_000,
            dead_zone_pumpswap_ws_zero_ms: 10_000,

            // PumpSwap buy TX slippage buffer (deprecated static, kept for compat)
            max_quote_in_multiplier_pct: 115,
            // Dynamic velocity-based max_quote_in
            max_quote_in_base_pct: 110,
            max_quote_in_per_velocity_divisor: 5,
            max_quote_in_cap_pct: 175,
            max_quote_in_tx_propagation_s: 4,

            // RPC sender config
            rpc_sender: RpcSenderConfig::default(),

            // Session circuit breaker & risk management (TASK 5)
            session_max_loss_pause_sol: 0.10,
            session_max_loss_halt_sol: 0.20,
            session_pause_duration_ms: 1_800_000,
            consecutive_loss_halfsize: 5,
            consecutive_loss_pause: 10,
            loss_pause_duration_ms: 900_000,
            min_rolling_wr_pct: 5.0,
            rolling_wr_window: 50,
        }
    }
}

impl MomentumConfig {
    /// Generate a config version string for paper trade logging.
    /// Format: `"mom-v{position_size_sol:.2}sol_{entry_delay_ms}ms"`
    pub fn config_version(&self) -> String {
        format!(
            "mom-v{:.2}sol_{}ms",
            self.position_size_sol, self.entry_delay_ms
        )
    }

    /// Validate daily loss cap configuration.
    pub fn validate_daily_loss_cap(&self) -> Result<(), String> {
        if !(0.0 < self.daily_loss_cap_pct && self.daily_loss_cap_pct <= 1.0) {
            return Err(format!(
                "daily_loss_cap_pct must be in (0.0, 1.0], got {}",
                self.daily_loss_cap_pct
            ));
        }
        Ok(())
    }

    /// Validate observation window configuration.
    pub fn validate_observation_config(&self) -> Result<(), String> {
        if self.observation_window_ms > 0 {
            if self.observation_max_drawdown_bps > 0 {
                return Err(format!(
                    "observation_max_drawdown_bps must be <= 0, got {}",
                    self.observation_max_drawdown_bps
                ));
            }
            if self.observation_min_samples == 0 {
                return Err("observation_min_samples must be > 0 when window is enabled".into());
            }
            if self.observation_window_min_ms >= self.observation_window_ms {
                return Err("observation_window_min_ms must be < observation_window_ms".into());
            }
            if self.observation_early_abort_drawdown_bps > 0 {
                return Err("observation_early_abort_drawdown_bps must be <= 0".into());
            }
        }
        Ok(())
    }

    /// Validate wallet balance monitor + Kelly sizing configuration.
    pub fn validate_balance_config(&self) -> Result<(), String> {
        if self.min_wallet_balance_lamports == 0 {
            return Err("min_wallet_balance_lamports must be > 0".into());
        }
        if !(0.0..=1.0).contains(&self.kelly_fraction) {
            return Err(format!("kelly_fraction must be 0.0–1.0, got {}", self.kelly_fraction));
        }
        if self.kelly_bootstrap_trades < 10 {
            return Err(format!("kelly_bootstrap_trades must be >= 10, got {}", self.kelly_bootstrap_trades));
        }
        if self.min_probe_size_sol >= self.max_probe_size_sol {
            return Err(format!("min_probe_size_sol ({}) must be < max_probe_size_sol ({})", self.min_probe_size_sol, self.max_probe_size_sol));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_momentum_config_defaults() {
        let config = MomentumConfig::default();
        assert!(!config.enabled);
        assert!(config.paper_mode);
        assert_eq!(config.entry_delay_ms, 15_000);
        assert_eq!(config.min_grad_score, 30);
        assert!((config.position_size_sol - 0.3).abs() < f64::EPSILON);
        assert_eq!(config.max_concurrent, 5);
        assert!((config.tp1_pct - 5.0).abs() < f64::EPSILON);
        assert!((config.tp1_exit_pct - 0.0).abs() < f64::EPSILON);
        assert!((config.tp2_pct - 15.0).abs() < f64::EPSILON);
        assert!((config.tp2_exit_pct - 0.0).abs() < f64::EPSILON);
        assert!((config.tp3_pct - 999.0).abs() < f64::EPSILON);
        assert!((config.tp3_exit_pct - 0.40).abs() < f64::EPSILON);
        assert!((config.trailing_stop_pct - 8.0).abs() < f64::EPSILON);
        assert!((config.hard_sl_pct - 12.0).abs() < f64::EPSILON);
        assert_eq!(config.time_sl_ms, 60_000);
        assert_eq!(config.max_hold_ms, 600_000);
        assert_eq!(config.max_hold_trail_activation_ms, 200_000);
        assert!((config.max_hold_trail_pct - 5.0).abs() < f64::EPSILON);
        assert_eq!(config.check_ms, 150);
        assert!((config.daily_loss_cap_pct - 0.10).abs() < f64::EPSILON);
        assert_eq!(config.raydium_fee_bps, 25);
        assert_eq!(config.pumpswap_fee_bps, 100);
        assert_eq!(config.sample_interval_ticks, 7);
        assert!((config.micro_sl_pct - 8.0).abs() < f64::EPSILON);
        assert_eq!(config.micro_sl_ticks, 20);
        assert_eq!(config.no_price_timeout_ms, 2_000);
        assert!((config.tier0_size_sol - 0.10).abs() < f64::EPSILON);
    }

    #[test]
    fn test_momentum_config_serde_roundtrip() {
        let config = MomentumConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let parsed: MomentumConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.entry_delay_ms, config.entry_delay_ms);
        assert_eq!(parsed.min_grad_score, config.min_grad_score);
        assert!((parsed.position_size_sol - config.position_size_sol).abs() < f64::EPSILON);
    }

    #[test]
    fn test_validate_daily_loss_cap_ok() {
        let config = MomentumConfig::default();
        assert!(config.validate_daily_loss_cap().is_ok());
    }

    #[test]
    fn test_validate_daily_loss_cap_invalid() {
        let mut config = MomentumConfig::default();
        config.daily_loss_cap_pct = 0.0;
        assert!(config.validate_daily_loss_cap().is_err());
        config.daily_loss_cap_pct = 1.5;
        assert!(config.validate_daily_loss_cap().is_err());
    }

    #[test]
    fn test_validate_balance_config_ok() {
        let config = MomentumConfig::default();
        assert!(config.validate_balance_config().is_ok());
    }

    #[test]
    fn test_validate_balance_config_bad_kelly_fraction() {
        let mut config = MomentumConfig::default();
        config.kelly_fraction = 1.5;
        assert!(config.validate_balance_config().is_err());
    }

    #[test]
    fn test_validate_balance_config_bad_probe_sizes() {
        let mut config = MomentumConfig::default();
        config.min_probe_size_sol = 0.5;
        config.max_probe_size_sol = 0.1;
        assert!(config.validate_balance_config().is_err());
    }

    #[test]
    fn test_validate_balance_config_zero_min_balance() {
        let mut config = MomentumConfig::default();
        config.min_wallet_balance_lamports = 0;
        assert!(config.validate_balance_config().is_err());
    }
}
