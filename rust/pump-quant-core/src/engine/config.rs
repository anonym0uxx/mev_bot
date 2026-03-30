//! Configuration loader for the MEV engine.
//!
//! Reads canary.json, extracts the `mev` section, and maps it into
//! `GateConfig`, `ScoreConfig`, and `PositionConfig` structs.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::gates::GateConfig;
use super::health::HealthConfig;
use super::positions::{PositionConfig, SizeTier, TpSlTier};
use super::scorer::ScoreConfig;
use crate::feeds::FeedSource;

// ── JSON schema for the `mev` section of canary.json ─────────────────────────

#[derive(Deserialize, Debug)]
pub struct MevJsonConfig {
    pub enabled: Option<bool>,
    pub paper_mode: Option<bool>,

    // Gate thresholds (SOL floats → lamports)
    pub trigger_min_buy_sol: Option<f64>,
    pub trigger_max_buy_sol: Option<f64>,
    pub min_vsol_in_curve: Option<f64>,
    pub max_vsol_in_curve: Option<f64>,
    pub max_token_age_s: Option<u64>,
    pub min_unique_buyers: Option<u16>,

    // Pre-trigger gates
    pub pre_trigger_min_buys_1s: Option<u16>,
    pub pre_trigger_min_buys_2s: Option<u16>,
    pub pre_trigger_min_buys_5s: Option<u16>,
    pub pre_trigger_max_gap_ms: Option<u64>,
    pub pre_trigger_min_vsol_accel: Option<f64>,
    pub pre_trigger_min_sell_count_5s: Option<u16>,
    pub pre_trigger_max_vsol_delta_3s: Option<f64>,
    pub pre_trigger_min_volume_5s: Option<f64>,
    pub max_trigger_isolation: Option<f64>,

    // Score threshold
    pub trigger_min_score: Option<f64>,

    // Position management
    pub max_hold_ms: Option<u64>,
    pub ride_max_hold_ms: Option<u64>,
    pub max_concurrent_positions: Option<usize>,
    pub entry_size_sol: Option<f64>,
    pub max_entry_size_sol: Option<f64>,
    pub take_profit_pct: Option<f64>,
    pub stop_loss_pct: Option<f64>,
    pub size_variance_pct: Option<f64>,
    pub jito_tip_lamports: Option<u64>,

    // Next-buyer exit
    pub next_buyer_exit: Option<bool>,
    pub next_buyer_aggregate_flow_ratio: Option<f64>,
    pub next_buyer_count_threshold: Option<u32>,
    pub next_buyer_single_buy_ratio: Option<f64>,
    pub next_buyer_profit_exit_pct: Option<f64>,

    // Momentum decay
    pub momentum_decay_check_ms: Option<u64>,
    pub momentum_decay_min_mfe_pct: Option<f64>,
    pub momentum_decay_max_drawdown_pct: Option<f64>,

    // Intra-hold trailing stop
    pub intra_hold_trailing_stop_pct: Option<f64>,
    pub intra_hold_trailing_stop_min_mfe_pct: Option<f64>,

    // Tiers
    pub tp_tiers: Option<Vec<TpSlTierJson>>,
    pub size_tiers: Option<Vec<SizeTierJson>>,

    // ToD config
    pub tod_config: Option<TodConfigJson>,

    // Blocked sources
    pub blocked_trigger_sources: Option<Vec<String>>,

    // Logging
    pub log_file: Option<String>,

    // Safety / circuit breakers
    pub daily_loss_cap_sol: Option<f64>,
    pub paper_daily_loss_cap_sol: Option<f64>,
    pub live_daily_loss_cap_sol: Option<f64>,
    pub consecutive_stop_pause_count: Option<u32>,
    pub consecutive_stop_pause_ms: Option<u64>,

    // Min hold before NB exit (ms)
    pub min_hold_before_exit_ms: Option<u64>,

    // Creator sell TTL (ms)
    pub creator_sell_ttl_ms: Option<u64>,

    // Master toggle for TOD gate. When false, blocked_hours_utc is ignored.
    // Use false in paper mode to collect data 24/7.
    pub tod_gate_enabled: Option<bool>,

    // Entry randomizer config (anti-fingerprinting)
    pub jitter_ms_min: Option<u32>,
    pub jitter_ms_max: Option<u32>,
    // size_variance_pct already declared above (position management section)

    // ── Scaled entry config (SPEC 3) ────────────────────────────────
    // When enabled, golden segment entries use a two-phase scaled entry:
    // Phase 1: enter at initial_pct of full size, wait for confirmation buy.
    // Phase 2: on confirmation, scale up to full size; on timeout, keep partial.
    // TODO: Full implementation deferred pending PositionManager API extension.
    // Currently stub-only: config fields parsed, JSONL schema emitted, logic is no-op.
    pub scaled_entry_enabled: Option<bool>,
    pub scaled_entry_initial_pct: Option<f64>,
    pub scaled_entry_confirmation_window_ms: Option<u64>,
    pub scaled_entry_confirmation_min_sol: Option<f64>,

    // ── Graduation arb config (SPEC 4) ──────────────────────────────
    // Infrastructure for graduation arbitrage between bonding curve terminal
    // price and Raydium AMM opening price. Disabled by default — requires
    // ShredStream for competitive latency.
    pub graduation_arb_enabled: Option<bool>,
    pub graduation_arb_max_sol: Option<f64>,
    pub graduation_arb_min_spread_pct: Option<f64>,
    pub graduation_arb_tp_pct: Option<f64>,
    pub graduation_arb_sl_pct: Option<f64>,
    pub graduation_arb_max_hold_ms: Option<u64>,
    pub graduation_arb_jito_tip_sol: Option<f64>,

    // ── Entry curve progress cap ─────────────────────────────────────
    // Rejects entries when bonding curve progress exceeds threshold.
    // Tokens near graduation produce max_hold exits with ~3.3% WR.
    pub max_curve_progress: Option<f64>,

    // ── Buy/sell ratio floor (anti-flat filter) ──────────────────────
    // Minimum buy/sell count ratio to qualify for entry.
    // Eliminates momentum_decay_flat exits caused by weak follow-through.
    pub min_buy_sell_ratio_5s: Option<f64>,

    // ── Flow concentration gate (Amihud-style) ──────────────────────
    // Minimum flow concentration: volume_5s / unique_buyers_30s.
    // High FC = concentrated informed flow. Low FC = dispersed retail noise.
    pub min_flow_concentration: Option<f64>,

    // ── Max unique buyers gate ──────────────────────────────────────
    // Maximum unique buyers in 30s window. Too many = dispersed retail.
    pub max_unique_buyers_30s: Option<u16>,

    // ── Exit state machine config (signal-based exits) ──────────────
    pub confirmation_window_ms: Option<u64>,
    pub stall_no_buy_ms: Option<u64>,
    pub stall_fade_pct: Option<f64>,
    pub stall_conviction_no_buy_ms: Option<u64>,
    pub stall_conviction_fade_pct: Option<f64>,
    pub max_hold_safety_ms: Option<u64>,
    pub trail_min_conviction: Option<u8>,
    pub trail_activation_pct_of_base_tp: Option<u8>,
    pub trail_distance_pct: Option<f64>,
    pub tp_sl_tiers_v2: Option<Vec<TpSlTierJsonV2>>,

    // ── Kelly bankroll config ───────────────────────────────────────
    /// Paper bankroll starting balance in SOL (default: 5.0).
    pub paper_bankroll_sol: Option<f64>,

    // ── Entry engine / ride / risk (v2 pipeline) ────────────────────
    pub entry_engine: Option<EntryEngineJsonConfig>,
    pub ride: Option<RideJsonConfig>,
    pub risk: Option<RiskJsonConfig>,

    // ── Dual signal mode (Bayesian + shadow composite) ──────────────
    pub signal: Option<SignalConfigJson>,
}

#[derive(Deserialize, Debug)]
pub struct TpSlTierJson {
    pub trigger_max_sol: f64,
    pub tp_pct: f64,
    pub sl_pct: f64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TpSlTierJsonV2 {
    pub trigger_max_sol: f64,
    pub unconfirmed_tp_pct: f64,
    pub unconfirmed_sl_pct: f64,
    pub confirmed_tp_pct: f64,
    pub confirmed_sl_pct: f64,
}

#[derive(Deserialize, Debug)]
pub struct SizeTierJson {
    pub trigger_max_sol: f64,
    pub size_sol: f64,
}

#[derive(Deserialize, Debug)]
pub struct TodConfigJson {
    pub blocked_hours_utc: Option<Vec<u8>>,
    pub boosted_hours_utc: Option<Vec<u8>>,
    pub reduced_hours_utc: Option<Vec<u8>>,
}

// ── Entry engine JSON config (v2 pipeline) ───────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct EntryEngineJsonConfig {
    pub hard_gate: Option<HardGateJsonConfig>,
    pub scoring: Option<ScoringJsonConfig>,
    pub magnitude: Option<MagnitudeJsonConfig>,
    pub position_sizing: Option<SizingJsonConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HardGateJsonConfig {
    pub min_buy_count_1s: Option<u16>,            // default: 5
    pub min_volume_sol_5s: Option<f64>,            // default: 5.0 (SOL, converted to lamports)
    pub max_time_since_last_buy_ms: Option<u64>,   // default: 500
    pub curve_pct_min: Option<f64>,                // default: 20.0
    pub curve_pct_max: Option<f64>,                // default: 60.0
    pub max_unique_buyers_30s: Option<u16>,         // default: 30
    pub max_sell_ratio_x100: Option<u16>,           // default: 50 (= 0.50)
    pub min_history_age_ms: Option<u64>,            // default: 2000
    pub creator_sell_cooldown_ms: Option<u64>,      // default: 30000
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScoringJsonConfig {
    // Entry feature weights (sum to 1.0)
    pub w_buy_burst: Option<f64>,        // default: 0.30
    pub w_volume: Option<f64>,           // default: 0.20
    pub w_curve: Option<f64>,            // default: 0.15
    pub w_concentration: Option<f64>,    // default: 0.10
    pub w_acceleration: Option<f64>,     // default: 0.10
    pub w_avg_size: Option<f64>,         // default: 0.05
    pub w_sell_absence: Option<f64>,     // default: 0.05
    pub w_recency: Option<f64>,          // default: 0.05
    // Sigmoid params
    pub buy_burst_center: Option<f64>,   // default: 7.0
    pub buy_burst_steep: Option<f64>,    // default: 0.8
    pub volume_norm_sol: Option<f64>,    // default: 10.0
    pub curve_mean: Option<f64>,         // default: 43.0
    pub curve_sigma: Option<f64>,        // default: 12.0
    pub accel_center: Option<f64>,       // default: 10.0
    pub accel_steep: Option<f64>,        // default: 0.15
}

#[derive(Debug, Clone, Deserialize)]
pub struct MagnitudeJsonConfig {
    pub w_fill_rate: Option<f64>,        // default: 0.20
    pub w_accel: Option<f64>,            // default: 0.20
    pub w_wallet_quality: Option<f64>,   // default: 0.15
    pub w_curve_remaining: Option<f64>,  // default: 0.15
    pub w_volume_intensity: Option<f64>, // default: 0.15
    pub w_sell_vacuum: Option<f64>,      // default: 0.10
    pub w_token_age: Option<f64>,        // default: 0.05
    pub fill_rate_center: Option<f64>,   // default: 15.0 (in LUT index scale)
    pub fill_rate_steep: Option<f64>,    // default: 0.25
}

#[derive(Debug, Clone, Deserialize)]
pub struct SizingJsonConfig {
    pub min_entry_score: Option<f64>,        // default: 50.0
    pub min_magnitude_for_ride: Option<f64>, // default: 40.0
    // SCALP fields removed — all positions are RIDE
    pub ride_size_min_sol: Option<f64>,      // default: 0.10
    pub ride_size_max_sol: Option<f64>,      // default: 0.15
}

// ── Signal weights JSON config (v2 pipeline — RideState v2) ──────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct SignalWeightsJson {
    #[serde(default = "default_w_buy_rate_1s")]
    pub w_buy_rate_1s: i8,
    #[serde(default = "default_w_buy_rate_5s")]
    pub w_buy_rate_5s: i8,
    #[serde(default = "default_w_sell_rate_5s")]
    pub w_sell_rate_5s: i8,
    #[serde(default = "default_w_vol_accel_shift")]
    pub w_vol_accel_shift: u8,
    #[serde(default = "default_w_buy_gap_divisor")]
    pub w_buy_gap_divisor: u16,
    #[serde(default = "default_w_sell_pressure_shift")]
    pub w_sell_pressure_shift: u8,
    #[serde(default = "default_w_pnl_shift")]
    pub w_pnl_shift: u8,
    #[serde(default = "default_w_time_since_peak_divisor")]
    pub w_time_since_peak_divisor: u16,
    #[serde(default = "default_w_unique_wallets")]
    pub w_unique_wallets: i8,
    #[serde(default = "default_w_confirm_vol_shift")]
    pub w_confirm_vol_shift: u8,
}

impl Default for SignalWeightsJson {
    fn default() -> Self {
        Self {
            w_buy_rate_1s: 24,
            w_buy_rate_5s: 16,
            w_sell_rate_5s: -20,
            w_vol_accel_shift: 6,
            w_buy_gap_divisor: 150,
            w_sell_pressure_shift: 2,
            w_pnl_shift: 3,
            w_time_since_peak_divisor: 200,
            w_unique_wallets: 14,
            w_confirm_vol_shift: 8,
        }
    }
}

fn default_w_buy_rate_1s() -> i8 { 24 }
fn default_w_buy_rate_5s() -> i8 { 16 }
fn default_w_sell_rate_5s() -> i8 { -20 }
fn default_w_vol_accel_shift() -> u8 { 6 }
fn default_w_buy_gap_divisor() -> u16 { 150 }
fn default_w_sell_pressure_shift() -> u8 { 2 }
fn default_w_pnl_shift() -> u8 { 3 }
fn default_w_time_since_peak_divisor() -> u16 { 200 }
fn default_w_unique_wallets() -> i8 { 14 }
fn default_w_confirm_vol_shift() -> u8 { 8 }
fn default_signal_weights() -> SignalWeightsJson { SignalWeightsJson::default() }

// ── Dual signal mode JSON config (Bayesian + shadow composite) ───────────────

/// JSON deserialization struct for the `signal` section of `mev` config.
/// All fields are optional — defaults produce a system that behaves identically
/// to pre-Bayesian (composite-only) operation.
#[derive(Debug, Clone, Deserialize)]
pub struct SignalConfigJson {
    /// Use Bayesian signal for exit decisions on NEW positions. Default: false.
    #[serde(default, rename = "useBayesianSignal")]
    pub use_bayesian_signal: bool,

    /// Compute composite score as shadow (for comparison logging).
    /// Default: true. Disable in live mode for ~30ns/tick savings.
    #[serde(default = "default_shadow_composite_enabled", rename = "shadowCompositeEnabled")]
    pub shadow_composite_enabled: bool,

    /// Bayesian time-decay rate (0–65535). 240 = half-life ≈ 5s.
    /// Applied as: α,β *= decay_rate/256 per tick.
    #[serde(default = "default_bayesian_decay_rate", rename = "bayesianDecayRate")]
    pub bayesian_decay_rate: u16,

    /// Beta prior strengths for [LOW, MED, HIGH] conviction tiers.
    /// Sum of α₀+β₀ (in units, before ×16 scaling).
    #[serde(default = "default_bayesian_prior_strength", rename = "bayesianPriorStrength")]
    pub bayesian_prior_strength: [u8; 3],

    /// Alert threshold: log warning if divergence_count exceeds this
    /// in the last 50 positions. Default: 10.
    #[serde(default = "default_divergence_alert_threshold", rename = "divergenceAlertThreshold")]
    pub divergence_alert_threshold: u8,
}

fn default_shadow_composite_enabled() -> bool { true }
fn default_bayesian_decay_rate() -> u16 { 240 }
fn default_bayesian_prior_strength() -> [u8; 3] { [6, 9, 13] }
fn default_divergence_alert_threshold() -> u8 { 10 }

impl Default for SignalConfigJson {
    fn default() -> Self {
        Self {
            use_bayesian_signal: false,
            shadow_composite_enabled: true,
            bayesian_decay_rate: 240,
            bayesian_prior_strength: [6, 9, 13],
            divergence_alert_threshold: 10,
        }
    }
}

// ── Signal runtime config ────────────────────────────────────────────────────

/// Runtime signal config built from `SignalConfigJson`.
/// Passed by value (Copy) on the hot path.
#[derive(Debug, Clone, Copy)]
pub struct SignalConfig {
    /// When true, Bayesian f̂*(t) drives exit decisions for new positions.
    pub use_bayesian_signal: bool,
    /// When true, compute composite score as a shadow signal alongside primary.
    pub shadow_composite_enabled: bool,
    /// Time-decay rate: α,β *= decay_rate/256 per tick. 240 → half-life ≈ 5s.
    pub bayesian_decay_rate: u16,
    /// Prior strengths for [LOW, MED, HIGH] conviction tiers.
    pub bayesian_prior_strength: [u8; 3],
    /// Divergence alert threshold (last 50 positions).
    pub divergence_alert_threshold: u8,
}

impl Default for SignalConfig {
    fn default() -> Self {
        Self {
            use_bayesian_signal: false,
            shadow_composite_enabled: true,
            bayesian_decay_rate: 240,
            bayesian_prior_strength: [6, 9, 13],
            divergence_alert_threshold: 10,
        }
    }
}

impl From<&SignalConfigJson> for SignalConfig {
    fn from(json: &SignalConfigJson) -> Self {
        Self {
            use_bayesian_signal: json.use_bayesian_signal,
            shadow_composite_enabled: json.shadow_composite_enabled,
            bayesian_decay_rate: json.bayesian_decay_rate,
            bayesian_prior_strength: json.bayesian_prior_strength,
            divergence_alert_threshold: json.divergence_alert_threshold,
        }
    }
}

// ── SignalMode enum ──────────────────────────────────────────────────────────

/// Which signal system drives exit decisions for a position.
/// Stored per-position at open time — not affected by runtime flag changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SignalMode {
    Composite = 0,
    Bayesian  = 1,
}

impl SignalMode {
    #[inline(always)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Composite => "composite",
            Self::Bayesian  => "bayesian",
        }
    }
}

// ── Bayesian auto-revert tracker ─────────────────────────────────────────────

/// Tracks Bayesian signal performance for auto-revert safety.
/// Lives in HotPath (session-scoped, not persisted).
///
/// When WR drops below 20% on ≥20 trades, `check_revert()` returns true
/// once — the caller should flip `use_bayesian_signal` to false at runtime.
pub struct BayesianRevertTracker {
    pub bayesian_trades: u16,
    pub bayesian_wins: u16,
    pub reverted: bool,
}

impl Default for BayesianRevertTracker {
    fn default() -> Self {
        Self {
            bayesian_trades: 0,
            bayesian_wins: 0,
            reverted: false,
        }
    }
}

impl BayesianRevertTracker {
    /// Record a closed Bayesian-mode position.
    #[inline]
    pub fn record(&mut self, win: bool) {
        self.bayesian_trades = self.bayesian_trades.saturating_add(1);
        if win {
            self.bayesian_wins = self.bayesian_wins.saturating_add(1);
        }
    }

    /// Check if auto-revert should fire.
    /// Returns true exactly once: when WR < 20% on ≥ 20 trades.
    /// After firing, `reverted` is set — subsequent calls return false.
    #[inline]
    pub fn check_revert(&mut self) -> bool {
        if self.reverted {
            return false;
        }
        if self.bayesian_trades < 20 {
            return false;
        }
        // Integer WR: wins * 100 / trades — avoid fp
        let wr_pct = (self.bayesian_wins as u32 * 100) / self.bayesian_trades as u32;
        if wr_pct < 20 {
            self.reverted = true;
            return true;
        }
        false
    }
}

// ── Ride JSON config (v2 pipeline) ───────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct RideJsonConfig {
    pub min_confirming_buys: Option<u16>,        // default: 2
    pub min_confirming_sol: Option<f64>,          // default: 0.3
    pub min_gain_pct: Option<f64>,               // default: 1.5
    pub max_curve_pct: Option<f64>,              // default: 80.0
    pub early_trail_pct: Option<f64>,            // default: 8.0 (price space, converted to vSOL bp)
    pub momentum_trail_pct: Option<f64>,         // default: 6.0
    pub tighten_trail_pct: Option<f64>,          // default: 4.0
    pub emergency_trail_pct: Option<f64>,        // default: 2.0
    pub early_to_momentum_ms: Option<u64>,       // default: 15000
    pub momentum_to_tighten_ms: Option<u64>,     // default: 60000
    pub max_hold_ms: Option<u64>,                // default: 300000
    pub gain_momentum_pct: Option<f64>,          // default: 15.0
    pub gain_tighten_pct: Option<f64>,           // default: 50.0
    pub hard_floor_gain_pct: Option<f64>,        // default: 1.0
    pub whale_exit_sol: Option<f64>,             // default: 1.0
    pub buy_gap_tighten_ms: Option<u64>,         // default: 5000
    pub buy_gap_exit_ms: Option<u64>,            // default: 10000
    pub sell_cascade_count: Option<u8>,          // default: 3
    pub sell_pressure_tighten_pct: Option<f64>,  // default: 2.0

    // ── Signal-driven exit thresholds (RideState v2) ────────────────
    #[serde(default = "default_signal_strong_pump")]
    pub signal_strong_pump_threshold: u16,
    #[serde(default = "default_signal_sustained")]
    pub signal_sustained_threshold: u16,
    #[serde(default = "default_signal_weakening")]
    pub signal_weakening_threshold: u16,

    // Signal weights (integer scoring)
    #[serde(default = "default_signal_weights")]
    pub signal_weights: SignalWeightsJson,

    // Kelly parameters
    #[serde(default = "default_kelly_baseline_f")]
    pub kelly_baseline_f_permille: u16,
    #[serde(default = "default_kelly_min_trail_bp")]
    pub kelly_min_trail_bp: u16,
    #[serde(default = "default_kelly_max_trail_bp")]
    pub kelly_max_trail_bp: u16,

    // Lifecycle phase thresholds
    #[serde(default = "default_lifecycle_accel_buys")]
    pub lifecycle_accel_min_buys: u16,
    #[serde(default = "default_lifecycle_accel_sol_msol")]
    pub lifecycle_accel_min_sol_msol: u32,
    #[serde(default = "default_lifecycle_momentum_buys")]
    pub lifecycle_momentum_min_buys: u16,
    #[serde(default = "default_lifecycle_momentum_sol_msol")]
    pub lifecycle_momentum_min_sol_msol: u32,

    // Dynamic trail base distances per signal state (vSOL bp)
    #[serde(default = "default_trail_strong_bp")]
    pub trail_strong_pump_bp: u16,
    #[serde(default = "default_trail_sustained_bp")]
    pub trail_sustained_bp: u16,
    #[serde(default = "default_trail_weakening_bp")]
    pub trail_weakening_bp: u16,
}

// ── Risk JSON config (v2 pipeline) ───────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct RiskJsonConfig {
    pub daily_loss_limit_sol: Option<f64>,     // default: 1.5
    pub consecutive_loss_limit: Option<u8>,    // default: 5
    pub pause_duration_ms: Option<u64>,        // default: 300000
    pub daily_trade_limit: Option<u32>,        // default: 60
    pub loss_cooldown_ms: Option<u64>,         // default: 5000
    pub max_concurrent_scalp: Option<u8>,      // default: 5
    pub max_concurrent_ride: Option<u8>,       // default: 3
    pub max_concurrent_total: Option<u8>,      // default: 8
}

// ── Serde default functions for RideJsonConfig v2 fields ─────────────────────

fn default_signal_strong_pump() -> u16 { 700 }
fn default_signal_sustained() -> u16 { 400 }
fn default_signal_weakening() -> u16 { 200 }
fn default_kelly_baseline_f() -> u16 { 671 }
fn default_kelly_min_trail_bp() -> u16 { 50 }
fn default_kelly_max_trail_bp() -> u16 { 800 }
fn default_lifecycle_accel_buys() -> u16 { 5 }
fn default_lifecycle_accel_sol_msol() -> u32 { 2000 }
fn default_lifecycle_momentum_buys() -> u16 { 15 }
fn default_lifecycle_momentum_sol_msol() -> u32 { 10000 }
fn default_trail_strong_bp() -> u16 { 500 }
fn default_trail_sustained_bp() -> u16 { 350 }
fn default_trail_weakening_bp() -> u16 { 200 }

// ── Ride runtime config (v2 pipeline) ────────────────────────────────────────

/// Runtime ride config with vSOL-space basis points and lamports.
/// All price-space percentages from JSON are converted at build time.
#[derive(Debug, Clone, Copy)]
pub struct RideConfig {
    pub min_confirming_buys: u16,
    pub min_confirming_lamports: u64,
    pub min_gain_vsol_fp: u16,           // vSOL fixed-point (×10000)
    pub max_curve_pct_x100: u16,         // 80.0 → 8000
    pub early_trail_bp: u16,             // vSOL basis points
    pub momentum_trail_bp: u16,
    pub tighten_trail_bp: u16,
    pub emergency_trail_bp: u16,
    pub early_to_momentum_ms: u64,
    pub momentum_to_tighten_ms: u64,
    pub max_hold_ms: u64,
    pub gain_momentum_vsol_fp: u16,      // vSOL fixed-point (×10000)
    pub gain_tighten_vsol_fp: u16,
    pub hard_floor_vsol_fp: u16,
    pub whale_exit_lamports: u64,
    /// Average historical loss in basis points (for Bayesian R̂ update).
    pub avg_loss_bp: u16,
    pub buy_gap_tighten_ms: u64,
    pub buy_gap_exit_ms: u64,
    pub sell_cascade_count: u8,
    pub sell_pressure_tighten_bp: u16,   // vSOL basis points

    // ── Signal-driven fields (RideState v2) ─────────────────────────
    pub signal_strong_threshold: u16,
    pub signal_sustained_threshold: u16,
    pub signal_weakening_threshold: u16,

    // Signal weights (flattened for L1 residency)
    pub w_buy_rate_1s: i8,
    pub w_buy_rate_5s: i8,
    pub w_sell_rate_5s: i8,
    pub w_vol_accel_shift: u8,
    pub w_buy_gap_divisor: u16,
    pub w_sell_pressure_shift: u8,
    pub w_pnl_shift: u8,
    pub w_time_since_peak_divisor: u16,
    pub w_unique_wallets: i8,
    pub w_confirm_vol_shift: u8,

    // Kelly parameters
    pub kelly_baseline_f_permille: u16,
    pub kelly_min_trail_bp: u16,
    pub kelly_max_trail_bp: u16,

    // Lifecycle thresholds
    pub lifecycle_accel_min_buys: u16,
    pub lifecycle_accel_min_sol_msol: u32,
    pub lifecycle_momentum_min_buys: u16,
    pub lifecycle_momentum_min_sol_msol: u32,

    // Trail distances per signal state (vSOL bp)
    pub trail_strong_pump_bp: u16,
    pub trail_sustained_bp: u16,
    pub trail_weakening_bp: u16,
}

impl Default for RideConfig {
    fn default() -> Self {
        Self {
            min_confirming_buys: 2,
            min_confirming_lamports: 500_000_000,
            min_gain_vsol_fp: 10200,
            max_curve_pct_x100: 8000,
            early_trail_bp: 408,
            momentum_trail_bp: 305,
            tighten_trail_bp: 202,
            emergency_trail_bp: 101,
            early_to_momentum_ms: 15_000,
            momentum_to_tighten_ms: 60_000,
            max_hold_ms: 300_000,
            gain_momentum_vsol_fp: 10724,
            gain_tighten_vsol_fp: 12247,
            hard_floor_vsol_fp: 9800,
            whale_exit_lamports: 2_000_000_000,
            avg_loss_bp: 200,
            buy_gap_tighten_ms: 5_000,
            buy_gap_exit_ms: 10_000,
            sell_cascade_count: 3,
            sell_pressure_tighten_bp: 100,
            // Signal v2
            signal_strong_threshold: 700,
            signal_sustained_threshold: 400,
            signal_weakening_threshold: 200,
            w_buy_rate_1s: 24,
            w_buy_rate_5s: 16,
            w_sell_rate_5s: -20,
            w_vol_accel_shift: 6,
            w_buy_gap_divisor: 150,
            w_sell_pressure_shift: 2,
            w_pnl_shift: 3,
            w_time_since_peak_divisor: 200,
            w_unique_wallets: 14,
            w_confirm_vol_shift: 8,
            kelly_baseline_f_permille: 671,
            kelly_min_trail_bp: 50,
            kelly_max_trail_bp: 800,
            lifecycle_accel_min_buys: 5,
            lifecycle_accel_min_sol_msol: 2000,
            lifecycle_momentum_min_buys: 15,
            lifecycle_momentum_min_sol_msol: 10000,
            trail_strong_pump_bp: 500,
            trail_sustained_bp: 350,
            trail_weakening_bp: 200,
        }
    }
}

// ── Risk runtime config (v2 pipeline) ────────────────────────────────────────

/// Runtime risk config with lamports and integer thresholds.
#[derive(Debug, Clone, Copy)]
pub struct RiskConfig {
    pub daily_loss_limit_lamports: u64,
    pub consecutive_loss_limit: u8,
    pub pause_duration_ms: u64,
    pub daily_trade_limit: u32,
    pub loss_cooldown_ms: u64,
    pub max_concurrent_scalp: u8,
    pub max_concurrent_ride: u8,
    pub max_concurrent_total: u8,
}

// ── Exit state machine config (runtime structs) ──────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct TpSlTierV2 {
    pub trigger_max_lamports: u64,
    pub unconfirmed_tp_fp: u32, // fixed-point: actual_pct = value / 100_000
    pub unconfirmed_sl_fp: u32,
    pub confirmed_tp_fp: u32,
    pub confirmed_sl_fp: u32,
}

impl Default for TpSlTierV2 {
    fn default() -> Self {
        Self {
            trigger_max_lamports: 0,
            unconfirmed_tp_fp: 2000,
            unconfirmed_sl_fp: 1000,
            confirmed_tp_fp: 3000,
            confirmed_sl_fp: 1500,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExitConfig {
    pub confirmation_window_ms: u64,
    pub stall_no_buy_ms: u64,
    pub stall_fade_fp: u32,
    pub stall_conviction_no_buy_ms: u64,
    pub stall_conviction_fade_fp: u32,
    pub max_hold_safety_ms: u64,
    pub conviction_tp_multipliers: [u16; 5], // [100, 100, 140, 180, 220]
    pub trail_min_conviction: u8,
    pub trail_activation_pct_of_base_tp: u8,
    pub trail_distance_fp: u32,
    /// Precomputed: 1.0 - (trail_distance_fp / 100_000.0) — eliminates hot-path division
    pub trail_keep_mult: f64,
    /// Precomputed: trail_activation_pct_of_base_tp as f64 / 100.0
    pub trail_activation_mult: f64,
    pub tp_sl_tiers: [TpSlTierV2; 8],
    pub tp_sl_tier_count: u8,
}

// ── Parsed engine config ─────────────────────────────────────────────────────

/// All engine configuration, parsed from the `mev` section.
pub struct EngineConfig {
    pub gate: GateConfig,
    pub score: ScoreConfig,
    pub position: PositionConfig,
    pub health: HealthConfig,
    pub paper_mode: bool,
    pub log_file: String,
    /// Daily loss cap in lamports (mode-aware: paper vs live).
    pub daily_loss_cap_lamports: u64,
    /// Number of consecutive stop-loss exits before pausing.
    pub consecutive_stop_pause_count: u32,
    /// Duration (ms) to pause after consecutive stop breaker fires.
    pub consecutive_stop_pause_ms: u64,
    /// UTC hours that get ToD boost (loaded from config).
    pub boosted_hours_utc: Vec<u8>,
    /// ToD boost multiplier for boosted hours (default 1.25).
    pub tod_boost_multiplier: f64,
    /// Entry randomizer config (anti-fingerprinting for live mode).
    pub randomizer: super::entry_randomizer::RandomizerConfig,

    // ── Scaled entry (SPEC 3) — stub config, logic deferred ─────────
    /// Master toggle for scaled entry on golden segment trades.
    pub scaled_entry_enabled: bool,
    /// Fraction of entry_size_sol for the initial (unconfirmed) position (0.0–1.0).
    pub scaled_entry_initial_pct: f64,
    /// Milliseconds to wait for a follow-on confirmation buy before keeping partial size.
    pub scaled_entry_confirmation_window_ms: u64,
    /// Minimum SOL of the follow-on buy to count as confirmation.
    pub scaled_entry_confirmation_min_sol: f64,

    // ── Graduation arb config (SPEC 4) ──────────────────────────────
    /// Whether graduation arb is enabled (default: false).
    pub graduation_arb_enabled: bool,
    /// Max SOL per arb trade (default: 0.30).
    pub graduation_arb_max_sol: f64,
    /// Min spread % between BC terminal price and Raydium opening price (default: 3.0).
    pub graduation_arb_min_spread_pct: f64,
    /// Take-profit % for arb positions (default: 0.03).
    pub graduation_arb_tp_pct: f64,
    /// Stop-loss % for arb positions (default: 0.02).
    pub graduation_arb_sl_pct: f64,
    /// Max hold time in ms for arb positions (default: 5000).
    pub graduation_arb_max_hold_ms: u64,
    /// Jito tip in SOL for arb bundles (default: 0.003).
    pub graduation_arb_jito_tip_sol: f64,

    // ── Momentum engine config (SPEC 5) ─────────────────────────────
    /// Post-graduation momentum engine configuration.
    pub momentum: crate::momentum::MomentumConfig,

    // ── Kelly bankroll ────────────────────────────────────────────
    /// Paper bankroll initial balance in lamports (default: 5 SOL).
    pub paper_bankroll_lamports: u64,

    // ── V2 pipeline configs ────────────────────────────────────────
    /// Entry engine config, built from `mev.entry_engine` JSON section.
    /// Always populated (defaults used when JSON section is absent).
    pub entry_engine_config: Option<crate::engine::entry_engine::EntryEngineConfig>,
    /// Ride exit state machine config, built from `mev.ride` JSON section.
    pub ride_config: Option<crate::engine::ride_state::RideConfig>,
    /// Risk manager config, built from `mev.risk` JSON section.
    pub risk_config: Option<crate::engine::risk_manager::RiskConfig>,

    // ── Dual signal mode config ────────────────────────────────────
    /// Signal engine configuration (Bayesian + shadow composite).
    /// Built from `mev.signal` JSON section; defaults when absent.
    pub signal: SignalConfig,
}

impl EngineConfig {
    /// Returns the time-of-day size multiplier for the given UTC hour.
    /// Returns `tod_boost_multiplier` (e.g. 1.25) if the hour is in `boosted_hours_utc`,
    /// otherwise returns 1.0.
    pub fn get_tod_multiplier(&self, hour_utc: u8) -> f64 {
        if self.boosted_hours_utc.contains(&hour_utc) {
            self.tod_boost_multiplier
        } else {
            1.0
        }
    }
}

// ── Loader ───────────────────────────────────────────────────────────────────

fn sol_to_lamports(sol: f64) -> u64 {
    (sol * 1_000_000_000.0) as u64
}

// ── vSOL conversion helpers (bonding curve math) ─────────────────────────────

/// Convert a price-space trail percentage to vSOL-space basis points.
/// A price drop of X% corresponds to a vSOL drop of (1 - sqrt(1 - X/100)).
fn price_pct_to_vsol_bp(price_pct: f64) -> u16 {
    let vsol_trail = 1.0 - (1.0 - price_pct / 100.0).sqrt();
    (vsol_trail * 10_000.0).round() as u16
}

/// Convert a price-space gain percentage to vSOL fixed-point (×10000).
/// A price gain of X% corresponds to vSOL ratio sqrt(1 + X/100).
fn gain_pct_to_vsol_fp(gain_pct: f64) -> u16 {
    ((1.0 + gain_pct / 100.0).sqrt() * 10_000.0).round() as u16
}

/// Build the ride runtime config from JSON, converting price-space %
/// to vSOL-space basis points / fixed-point.
pub fn build_ride_config(json: &RideJsonConfig) -> RideConfig {
    RideConfig {
        min_confirming_buys: json.min_confirming_buys.unwrap_or(2),
        min_confirming_lamports: sol_to_lamports(json.min_confirming_sol.unwrap_or(0.3)),
        min_gain_vsol_fp: gain_pct_to_vsol_fp(json.min_gain_pct.unwrap_or(1.5)),
        max_curve_pct_x100: (json.max_curve_pct.unwrap_or(80.0) * 100.0).round() as u16,
        early_trail_bp: price_pct_to_vsol_bp(json.early_trail_pct.unwrap_or(8.0)),
        momentum_trail_bp: price_pct_to_vsol_bp(json.momentum_trail_pct.unwrap_or(6.0)),
        tighten_trail_bp: price_pct_to_vsol_bp(json.tighten_trail_pct.unwrap_or(4.0)),
        emergency_trail_bp: price_pct_to_vsol_bp(json.emergency_trail_pct.unwrap_or(2.0)),
        early_to_momentum_ms: json.early_to_momentum_ms.unwrap_or(15_000),
        momentum_to_tighten_ms: json.momentum_to_tighten_ms.unwrap_or(60_000),
        max_hold_ms: json.max_hold_ms.unwrap_or(300_000),
        gain_momentum_vsol_fp: gain_pct_to_vsol_fp(json.gain_momentum_pct.unwrap_or(15.0)),
        gain_tighten_vsol_fp: gain_pct_to_vsol_fp(json.gain_tighten_pct.unwrap_or(50.0)),
        hard_floor_vsol_fp: gain_pct_to_vsol_fp(json.hard_floor_gain_pct.unwrap_or(1.0)),
        whale_exit_lamports: sol_to_lamports(json.whale_exit_sol.unwrap_or(1.0)),
        avg_loss_bp: 200,
        buy_gap_tighten_ms: json.buy_gap_tighten_ms.unwrap_or(5_000),
        buy_gap_exit_ms: json.buy_gap_exit_ms.unwrap_or(10_000),
        sell_cascade_count: json.sell_cascade_count.unwrap_or(3),
        sell_pressure_tighten_bp: price_pct_to_vsol_bp(
            json.sell_pressure_tighten_pct.unwrap_or(2.0),
        ),

        // Signal-driven fields (v2) — already integer, pass through directly
        signal_strong_threshold: json.signal_strong_pump_threshold,
        signal_sustained_threshold: json.signal_sustained_threshold,
        signal_weakening_threshold: json.signal_weakening_threshold,

        w_buy_rate_1s: json.signal_weights.w_buy_rate_1s,
        w_buy_rate_5s: json.signal_weights.w_buy_rate_5s,
        w_sell_rate_5s: json.signal_weights.w_sell_rate_5s,
        w_vol_accel_shift: json.signal_weights.w_vol_accel_shift,
        w_buy_gap_divisor: json.signal_weights.w_buy_gap_divisor,
        w_sell_pressure_shift: json.signal_weights.w_sell_pressure_shift,
        w_pnl_shift: json.signal_weights.w_pnl_shift,
        w_time_since_peak_divisor: json.signal_weights.w_time_since_peak_divisor,
        w_unique_wallets: json.signal_weights.w_unique_wallets,
        w_confirm_vol_shift: json.signal_weights.w_confirm_vol_shift,

        kelly_baseline_f_permille: json.kelly_baseline_f_permille,
        kelly_min_trail_bp: json.kelly_min_trail_bp,
        kelly_max_trail_bp: json.kelly_max_trail_bp,

        lifecycle_accel_min_buys: json.lifecycle_accel_min_buys,
        lifecycle_accel_min_sol_msol: json.lifecycle_accel_min_sol_msol,
        lifecycle_momentum_min_buys: json.lifecycle_momentum_min_buys,
        lifecycle_momentum_min_sol_msol: json.lifecycle_momentum_min_sol_msol,

        trail_strong_pump_bp: json.trail_strong_pump_bp,
        trail_sustained_bp: json.trail_sustained_bp,
        trail_weakening_bp: json.trail_weakening_bp,
    }
}

/// Build the risk runtime config from JSON, converting SOL to lamports.
pub fn build_risk_config(json: &RiskJsonConfig) -> RiskConfig {
    RiskConfig {
        daily_loss_limit_lamports: sol_to_lamports(json.daily_loss_limit_sol.unwrap_or(1.5)),
        consecutive_loss_limit: json.consecutive_loss_limit.unwrap_or(5),
        pause_duration_ms: json.pause_duration_ms.unwrap_or(300_000),
        daily_trade_limit: json.daily_trade_limit.unwrap_or(60),
        loss_cooldown_ms: json.loss_cooldown_ms.unwrap_or(5_000),
        max_concurrent_scalp: json.max_concurrent_scalp.unwrap_or(5),
        max_concurrent_ride: json.max_concurrent_ride.unwrap_or(3),
        max_concurrent_total: json.max_concurrent_total.unwrap_or(8),
    }
}

/// Build the ride_state::RideConfig from the JSON ride section,
/// converting price-space percentages to vSOL basis points / mSOL.
pub fn build_ride_state_config(json: &RideJsonConfig) -> crate::engine::ride_state::RideConfig {
    use crate::engine::ride_state::RideConfig as RideStateCfg;
    let mut cfg = RideStateCfg::default();

    if let Some(v) = json.early_to_momentum_ms {
        cfg.early_to_momentum_ms = v;
    }
    if let Some(v) = json.momentum_to_tighten_ms {
        cfg.momentum_to_tighten_ms = v;
    }
    if let Some(v) = json.max_hold_ms {
        cfg.max_hold_ms = v;
    }
    if let Some(v) = json.early_trail_pct {
        cfg.early_trail_bp = price_pct_to_vsol_bp(v);
    }
    if let Some(v) = json.momentum_trail_pct {
        cfg.momentum_trail_bp = price_pct_to_vsol_bp(v);
    }
    if let Some(v) = json.tighten_trail_pct {
        cfg.tighten_trail_bp = price_pct_to_vsol_bp(v);
    }
    if let Some(v) = json.emergency_trail_pct {
        cfg.emergency_trail_bp = price_pct_to_vsol_bp(v);
    }
    if let Some(v) = json.whale_exit_sol {
        cfg.whale_exit_lamports = (v * 1_000_000_000.0) as u64;
    }
    // whale_dump_exit_msol: no direct JSON field, keep default
    if let Some(v) = json.sell_cascade_count {
        cfg.sell_cascade_count = v;
    }
    if let Some(v) = json.sell_pressure_tighten_pct {
        cfg.sell_pressure_tighten_bp = price_pct_to_vsol_bp(v);
    }
    if let Some(v) = json.buy_gap_tighten_ms {
        cfg.buy_gap_tighten_ms = v;
    }
    if let Some(v) = json.buy_gap_exit_ms {
        cfg.buy_gap_exit_ms = v;
    }
    // Gain thresholds: convert price % to vSOL ratio FP
    if let Some(v) = json.gain_momentum_pct {
        cfg.gain_momentum_vsol_fp = gain_pct_to_vsol_fp(v);
    }
    if let Some(v) = json.gain_tighten_pct {
        cfg.gain_tighten_vsol_fp = gain_pct_to_vsol_fp(v);
    }

    cfg
}

/// Build the risk_manager::RiskConfig from the JSON risk section,
/// converting SOL to lamports (signed i64 for daily loss limit).
pub fn build_risk_manager_config(json: &RiskJsonConfig) -> crate::engine::risk_manager::RiskConfig {
    crate::engine::risk_manager::RiskConfig {
        daily_loss_limit_lamports: -(sol_to_lamports(
            json.daily_loss_limit_sol.unwrap_or(1.5),
        ) as i64),
        consecutive_loss_limit: json.consecutive_loss_limit.unwrap_or(5),
        pause_duration_ms: json.pause_duration_ms.unwrap_or(300_000),
        daily_trade_limit: json.daily_trade_limit.unwrap_or(60),
        loss_cooldown_ms: json.loss_cooldown_ms.unwrap_or(5_000),
        max_concurrent_scalp: json.max_concurrent_scalp.unwrap_or(5),
        max_concurrent_ride: json.max_concurrent_ride.unwrap_or(3),
        max_concurrent_total: json.max_concurrent_total.unwrap_or(8),
    }
}

/// Build the exit state machine config from JSON fields.
/// Prefers `tp_sl_tiers_v2` when present; falls back to legacy `tp_tiers`
/// with unconfirmed == confirmed for backward compatibility.
pub fn build_exit_config(mev: &MevJsonConfig) -> ExitConfig {
    let tiers: Vec<TpSlTierV2> = if let Some(tiers_v2) = &mev.tp_sl_tiers_v2 {
        tiers_v2
            .iter()
            .map(|t| TpSlTierV2 {
                trigger_max_lamports: (t.trigger_max_sol * 1_000_000_000.0) as u64,
                unconfirmed_tp_fp: (t.unconfirmed_tp_pct * 100_000.0) as u32,
                unconfirmed_sl_fp: (t.unconfirmed_sl_pct * 100_000.0) as u32,
                confirmed_tp_fp: (t.confirmed_tp_pct * 100_000.0) as u32,
                confirmed_sl_fp: (t.confirmed_sl_pct * 100_000.0) as u32,
            })
            .collect()
    } else {
        // Fallback: use existing tp_tiers mapped to both confirmed and unconfirmed
        mev.tp_tiers
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|t| TpSlTierV2 {
                trigger_max_lamports: (t.trigger_max_sol * 1_000_000_000.0) as u64,
                unconfirmed_tp_fp: (t.tp_pct * 100_000.0) as u32,
                unconfirmed_sl_fp: (t.sl_pct * 100_000.0) as u32,
                confirmed_tp_fp: (t.tp_pct * 100_000.0) as u32,
                confirmed_sl_fp: (t.sl_pct * 100_000.0) as u32,
            })
            .collect()
    };

    let count = tiers.len().min(8);
    let mut arr = [TpSlTierV2::default(); 8];
    arr[..count].copy_from_slice(&tiers[..count]);

    ExitConfig {
        confirmation_window_ms: mev.confirmation_window_ms.unwrap_or(200),
        stall_no_buy_ms: mev.stall_no_buy_ms.unwrap_or(500),
        stall_fade_fp: (mev.stall_fade_pct.unwrap_or(0.01) * 100_000.0) as u32,
        stall_conviction_no_buy_ms: mev.stall_conviction_no_buy_ms.unwrap_or(800),
        stall_conviction_fade_fp: (mev.stall_conviction_fade_pct.unwrap_or(0.015) * 100_000.0)
            as u32,
        max_hold_safety_ms: mev.max_hold_safety_ms.unwrap_or(5000),
        conviction_tp_multipliers: [100, 100, 140, 180, 220],
        trail_min_conviction: mev.trail_min_conviction.unwrap_or(2),
        trail_activation_pct_of_base_tp: mev.trail_activation_pct_of_base_tp.unwrap_or(60),
        trail_distance_fp: (mev.trail_distance_pct.unwrap_or(0.015) * 100_000.0) as u32,
        trail_keep_mult: 1.0 - mev.trail_distance_pct.unwrap_or(0.015),
        trail_activation_mult: mev.trail_activation_pct_of_base_tp.unwrap_or(60) as f64 / 100.0,
        tp_sl_tiers: arr,
        tp_sl_tier_count: count as u8,
    }
}

/// Build an EntryEngineConfig from the JSON entry_engine section.
/// Falls back to defaults for any missing fields.
pub fn build_entry_engine_config(json: &EntryEngineJsonConfig) -> crate::engine::entry_engine::EntryEngineConfig {
    let mut cfg = crate::engine::entry_engine::EntryEngineConfig::default();

    // Hard gate overrides
    if let Some(ref hg) = json.hard_gate {
        if let Some(v) = hg.min_buy_count_1s { cfg.min_buy_count_1s = v; }
        if let Some(v) = hg.min_volume_sol_5s { cfg.min_volume_sol_5s = v; }
        if let Some(v) = hg.max_time_since_last_buy_ms { cfg.max_time_since_last_buy_ms = v; }
        if let Some(v) = hg.curve_pct_min {
            cfg.curve_pct_min = v;
            cfg.min_vsol_reserves_lamports = ((30.0 + v / 100.0 * 85.0) * 1e9) as u64;
        }
        if let Some(v) = hg.curve_pct_max {
            cfg.curve_pct_max = v;
            cfg.max_vsol_reserves_lamports = ((30.0 + v / 100.0 * 85.0) * 1e9) as u64;
        }
        if let Some(v) = hg.max_unique_buyers_30s { cfg.max_unique_buyers_30s = v; }
        if let Some(v) = hg.min_history_age_ms { cfg.min_history_age_ms = v; }
        if let Some(v) = hg.creator_sell_cooldown_ms { cfg.creator_sell_cooldown_ms = v; }
    }

    // Scoring weight overrides
    if let Some(ref sc) = json.scoring {
        if let Some(v) = sc.w_buy_burst { cfg.weights.w_buy_burst = v; }
        if let Some(v) = sc.w_volume { cfg.weights.w_volume = v; }
        if let Some(v) = sc.w_curve { cfg.weights.w_curve_position = v; }
        if let Some(v) = sc.w_concentration { cfg.weights.w_buyer_concentration = v; }
        if let Some(v) = sc.w_acceleration { cfg.weights.w_buy_acceleration = v; }
        if let Some(v) = sc.w_avg_size { cfg.weights.w_avg_buy_size = v; }
        if let Some(v) = sc.w_sell_absence { cfg.weights.w_sell_absence = v; }
        if let Some(v) = sc.w_recency { cfg.weights.w_recency = v; }
    }

    // Magnitude weight overrides
    if let Some(ref mag) = json.magnitude {
        if let Some(v) = mag.w_fill_rate { cfg.weights.w_fill_rate = v; }
        if let Some(v) = mag.w_accel { cfg.weights.w_buy_velocity_accel = v; }
        if let Some(v) = mag.w_wallet_quality { cfg.weights.w_wallet_quality = v; }
        if let Some(v) = mag.w_curve_remaining { cfg.weights.w_curve_remaining = v; }
        if let Some(v) = mag.w_volume_intensity { cfg.weights.w_volume_intensity = v; }
        if let Some(v) = mag.w_sell_vacuum { cfg.weights.w_sell_vacuum = v; }
        if let Some(v) = mag.w_token_age { cfg.weights.w_token_age = v; }
    }

    // Decision threshold overrides (SCALP fields removed — all positions are RIDE)
    if let Some(ref sizing) = json.position_sizing {
        if let Some(v) = sizing.min_entry_score { cfg.decision.min_entry_score = v; }
        if let Some(v) = sizing.min_magnitude_for_ride { cfg.decision.min_magnitude_for_ride = v; }
        // ride_size_min/max removed — sizing is now Kelly-derived from wallet balance
        let _ = sizing.ride_size_min_sol; // kept in JSON for backward compat, ignored
        let _ = sizing.ride_size_max_sol;
    }

    cfg
}

/// Load canary.json from the given path, parse the `mev` section,
/// and return a fully-constructed `EngineConfig`.
pub fn load_config(path: &Path) -> Result<EngineConfig> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config at {}", path.display()))?;

    let root: serde_json::Value =
        serde_json::from_str(&raw).context("failed to parse canary.json as JSON")?;

    let mev_val = root
        .get("mev")
        .context("canary.json missing 'mev' section")?;

    let mev: MevJsonConfig =
        serde_json::from_value(mev_val.clone()).context("failed to deserialize 'mev' section")?;

    // ── Build GateConfig ────────────────────────────────────────────
    let blocked_sources: Vec<FeedSource> = mev
        .blocked_trigger_sources
        .as_ref()
        .map(|v| {
            v.iter()
                .filter_map(|s| match s.as_str() {
                    "corecast" | "Corecast" => None, // Not a FeedSource variant
                    "helius" | "Helius" => Some(FeedSource::Helius),
                    "pumpportal" | "PumpPortal" => Some(FeedSource::PumpPortal),
                    "shredstream" | "ShredStream" => Some(FeedSource::ShredStream),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    let gate = GateConfig {
        trigger_min_buy_lamports: sol_to_lamports(mev.trigger_min_buy_sol.unwrap_or(0.1)),
        trigger_max_buy_lamports: sol_to_lamports(mev.trigger_max_buy_sol.unwrap_or(10.0)),
        min_vsol_lamports: sol_to_lamports(mev.min_vsol_in_curve.unwrap_or(3.0)),
        max_vsol_lamports: sol_to_lamports(mev.max_vsol_in_curve.unwrap_or(85.0)),
        max_token_age_ms: mev.max_token_age_s.unwrap_or(120) * 1000,
        min_unique_buyers: mev.min_unique_buyers.unwrap_or(4),
        pre_trigger_min_buys_1s: mev.pre_trigger_min_buys_1s.unwrap_or(1),
        pre_trigger_min_buys_2s: mev.pre_trigger_min_buys_2s.unwrap_or(2),
        pre_trigger_min_buys_5s: mev.pre_trigger_min_buys_5s.unwrap_or(3),
        pre_trigger_max_gap_ms: mev.pre_trigger_max_gap_ms.unwrap_or(3000),
        pre_trigger_min_vsol_accel: sol_to_lamports(
            mev.pre_trigger_min_vsol_accel.unwrap_or(0.1),
        ),
        pre_trigger_min_sell_count_5s: mev.pre_trigger_min_sell_count_5s.unwrap_or(0),
        pre_trigger_max_vsol_delta_3s: sol_to_lamports(
            mev.pre_trigger_max_vsol_delta_3s.unwrap_or(30.0),
        ),
        creator_sell_ttl_ms: mev.creator_sell_ttl_ms.unwrap_or(30_000),
        pre_trigger_min_volume_5s_lamports: sol_to_lamports(
            mev.pre_trigger_min_volume_5s.unwrap_or(0.5),
        ),
        max_trigger_isolation: mev.max_trigger_isolation.unwrap_or(0.5),
        trigger_min_score: mev.trigger_min_score.unwrap_or(0.65),
        blocked_sources,
        large_trigger_lamports: 1_500_000_000,
        large_trigger_min_unique_buyers: 5,
        blocked_hours_utc: mev
            .tod_config
            .as_ref()
            .and_then(|tod| tod.blocked_hours_utc.clone())
            .unwrap_or_default(),
        boosted_hours_utc: mev
            .tod_config
            .as_ref()
            .and_then(|tod| tod.boosted_hours_utc.clone())
            .unwrap_or_default(),
        tod_gate_enabled: mev.tod_gate_enabled.unwrap_or(true),
        regime_config: super::regime::RegimeConfig::default(),
        max_curve_progress: mev.max_curve_progress.unwrap_or(1.0),
        min_buy_sell_ratio_5s: mev.min_buy_sell_ratio_5s.unwrap_or(0.0),
        // Precomputed fields — set to 0 here, GateStack::new() recomputes from Vec/f64 fields.
        blocked_hours_bitmask: 0,
        boosted_hours_bitmask: 0,
        min_buy_sell_ratio_x10: 0,
        max_vtoken_threshold: 0,
        min_flow_concentration_x100: (mev.min_flow_concentration.unwrap_or(0.0) * 100.0) as u16,
        max_unique_buyers_30s: mev.max_unique_buyers_30s.unwrap_or(0),
    };

    // ── Build ScoreConfig (defaults — no JSON overrides yet) ────────
    let score = ScoreConfig::default();

    // ── Build PositionConfig ────────────────────────────────────────
    let tp_tiers: Vec<TpSlTier> = mev
        .tp_tiers
        .as_ref()
        .map(|tiers| {
            tiers
                .iter()
                .map(|t| TpSlTier {
                    trigger_max_lamports: sol_to_lamports(t.trigger_max_sol),
                    tp_pct: t.tp_pct,
                    sl_pct: t.sl_pct,
                })
                .collect()
        })
        .unwrap_or_else(|| {
            vec![TpSlTier {
                trigger_max_lamports: u64::MAX,
                tp_pct: mev.take_profit_pct.unwrap_or(0.025),
                sl_pct: mev.stop_loss_pct.unwrap_or(0.015),
            }]
        });

    let size_tiers: Vec<SizeTier> = mev
        .size_tiers
        .as_ref()
        .map(|tiers| {
            tiers
                .iter()
                .map(|t| SizeTier {
                    trigger_max_lamports: sol_to_lamports(t.trigger_max_sol),
                    size_lamports: sol_to_lamports(t.size_sol),
                })
                .collect()
        })
        .unwrap_or_else(|| {
            vec![SizeTier {
                trigger_max_lamports: u64::MAX,
                size_lamports: sol_to_lamports(mev.entry_size_sol.unwrap_or(0.1)),
            }]
        });

    let boosted_hours_utc = mev
        .tod_config
        .as_ref()
        .and_then(|tod| tod.boosted_hours_utc.clone())
        .unwrap_or_else(|| vec![14, 15]);

    let position = PositionConfig {
        // All positions are RIDE — max_hold_ms kept for struct compat, defaults to ride value
        max_hold_ms: mev.max_hold_ms.unwrap_or(60_000),
        ride_max_hold_ms: mev.ride_max_hold_ms.unwrap_or(60_000), // 60s default for RIDE
        momentum_decay_check_ms: mev.momentum_decay_check_ms.unwrap_or(50),
        momentum_decay_min_mfe_pct: mev.momentum_decay_min_mfe_pct.unwrap_or(0.001),
        momentum_decay_max_drawdown_pct: mev.momentum_decay_max_drawdown_pct.unwrap_or(0.003),
        intra_hold_trailing_stop_pct: mev.intra_hold_trailing_stop_pct.unwrap_or(1.0),
        intra_hold_trailing_stop_min_mfe_pct: mev
            .intra_hold_trailing_stop_min_mfe_pct
            .unwrap_or(1.0),
        next_buyer_profit_exit_pct: mev.next_buyer_profit_exit_pct.unwrap_or(0.01),
        next_buyer_aggregate_flow_ratio: mev.next_buyer_aggregate_flow_ratio.unwrap_or(0.35),
        next_buyer_count_threshold: mev.next_buyer_count_threshold.unwrap_or(3),
        next_buyer_single_buy_ratio: mev.next_buyer_single_buy_ratio.unwrap_or(0.25),
        tp_tiers,
        size_tiers,
        max_concurrent_positions: mev.max_concurrent_positions.unwrap_or(10),
        max_entry_size_lamports: sol_to_lamports(mev.max_entry_size_sol.unwrap_or(0.25)),
        size_variance_pct: mev.size_variance_pct.unwrap_or(0.2),
        jito_tip_lamports: mev.jito_tip_lamports.unwrap_or(50_000),
        min_hold_before_exit_ms: mev.min_hold_before_exit_ms.unwrap_or(300),
        tod_boost_multiplier: 1.25,
        boosted_hours_utc,
        exit_config: build_exit_config(&mev),
        ride_config: mev.ride.as_ref()
            .map(build_ride_state_config)
            .unwrap_or_else(crate::engine::ride_state::RideConfig::default),
    };

    let paper_mode = mev.paper_mode.unwrap_or(true);
    let log_file = mev
        .log_file
        .unwrap_or_else(|| "data/backrun_paper_trades.jsonl".to_string());

    // ── Safety / circuit breaker config ─────────────────────────────
    // Daily loss cap: paper mode uses paper_daily_loss_cap_sol, live uses live_daily_loss_cap_sol,
    // both fall back to daily_loss_cap_sol, then to 5.0 SOL.
    let daily_loss_cap_sol = if paper_mode {
        mev.paper_daily_loss_cap_sol
            .or(mev.daily_loss_cap_sol)
            .unwrap_or(5.0)
    } else {
        mev.live_daily_loss_cap_sol
            .or(mev.daily_loss_cap_sol)
            .unwrap_or(0.18)
    };
    let daily_loss_cap_lamports = sol_to_lamports(daily_loss_cap_sol);

    let consecutive_stop_pause_count = mev.consecutive_stop_pause_count.unwrap_or(3);
    let consecutive_stop_pause_ms = mev.consecutive_stop_pause_ms.unwrap_or(180_000);

    // ── Build HealthConfig from top-level `health` section ──────────
    let health = if let Some(health_val) = root.get("health") {
        let market_feed_stale_s: u64 = health_val
            .get("market_feed_stale_s")
            .and_then(|v| v.as_u64())
            .unwrap_or(45);
        let auto_pause_on_degraded: bool = health_val
            .get("auto_pause_on_degraded")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        HealthConfig {
            market_feed_stale_ms: market_feed_stale_s * 1000,
            auto_pause_on_degraded,
        }
    } else {
        HealthConfig::default()
    };

    // ── ToD multiplier config ──────────────────────────────────────
    let tod_boosted_hours = mev
        .tod_config
        .as_ref()
        .and_then(|tod| tod.boosted_hours_utc.clone())
        .unwrap_or_default();
    let tod_boost_multiplier = 1.25_f64; // hardcoded per spec; config override possible later

    Ok(EngineConfig {
        gate,
        score,
        position,
        health,
        paper_mode,
        log_file,
        daily_loss_cap_lamports,
        consecutive_stop_pause_count,
        consecutive_stop_pause_ms,
        boosted_hours_utc: tod_boosted_hours,
        tod_boost_multiplier,
        randomizer: super::entry_randomizer::RandomizerConfig {
            jitter_ms_min: mev.jitter_ms_min.unwrap_or(50),
            jitter_ms_max: mev.jitter_ms_max.unwrap_or(200),
            size_variance_pct: mev.size_variance_pct.unwrap_or(0.20),
            base_entry_lamports: sol_to_lamports(mev.entry_size_sol.unwrap_or(0.12)),
        },
        // Scaled entry (SPEC 3) — config parsed, logic is stub-only for now
        scaled_entry_enabled: mev.scaled_entry_enabled.unwrap_or(false),
        scaled_entry_initial_pct: mev.scaled_entry_initial_pct.unwrap_or(0.40),
        scaled_entry_confirmation_window_ms: mev.scaled_entry_confirmation_window_ms.unwrap_or(400),
        scaled_entry_confirmation_min_sol: mev.scaled_entry_confirmation_min_sol.unwrap_or(0.10),
        // Graduation arb (SPEC 4) — disabled by default, infrastructure only
        graduation_arb_enabled: mev.graduation_arb_enabled.unwrap_or(false),
        graduation_arb_max_sol: mev.graduation_arb_max_sol.unwrap_or(0.30),
        graduation_arb_min_spread_pct: mev.graduation_arb_min_spread_pct.unwrap_or(3.0),
        graduation_arb_tp_pct: mev.graduation_arb_tp_pct.unwrap_or(0.03),
        graduation_arb_sl_pct: mev.graduation_arb_sl_pct.unwrap_or(0.02),
        graduation_arb_max_hold_ms: mev.graduation_arb_max_hold_ms.unwrap_or(5000),
        graduation_arb_jito_tip_sol: mev.graduation_arb_jito_tip_sol.unwrap_or(0.003),
        // Momentum engine config — loaded from top-level "momentum" section
        // Falls back to MomentumConfig::default() if section is missing.
        momentum: root
            .get("momentum")
            .and_then(|v| serde_json::from_value::<crate::momentum::MomentumConfig>(v.clone()).ok())
            .unwrap_or_default(),

        // Kelly bankroll
        paper_bankroll_lamports: sol_to_lamports(mev.paper_bankroll_sol.unwrap_or(5.0)),

        // V2 pipeline configs — entry engine always populated (defaults when absent)
        entry_engine_config: Some(
            mev.entry_engine.as_ref()
                .map(build_entry_engine_config)
                .unwrap_or_else(crate::engine::entry_engine::EntryEngineConfig::default)
        ),
        // Ride / Risk runtime configs — built from mev.ride / mev.risk sections
        ride_config: mev.ride.as_ref().map(build_ride_state_config),
        risk_config: mev.risk.as_ref().map(build_risk_manager_config),

        // Dual signal mode — defaults when mev.signal is absent
        signal: mev.signal.as_ref()
            .map(SignalConfig::from)
            .unwrap_or_default(),
    })
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a minimal MevJsonConfig with all fields None.
    fn minimal_mev() -> MevJsonConfig {
        serde_json::from_str::<MevJsonConfig>("{}").unwrap()
    }

    // Test 1: tp_sl_tiers_v2 deserializes correctly from JSON
    #[test]
    fn test_tp_sl_tiers_v2_deserializes() {
        let json = r#"{
            "tp_sl_tiers_v2": [
                {
                    "trigger_max_sol": 0.6,
                    "unconfirmed_tp_pct": 0.020,
                    "unconfirmed_sl_pct": 0.010,
                    "confirmed_tp_pct": 0.030,
                    "confirmed_sl_pct": 0.015
                },
                {
                    "trigger_max_sol": 5.0,
                    "unconfirmed_tp_pct": 0.050,
                    "unconfirmed_sl_pct": 0.012,
                    "confirmed_tp_pct": 0.070,
                    "confirmed_sl_pct": 0.015
                }
            ]
        }"#;
        let mev: MevJsonConfig = serde_json::from_str(json).unwrap();
        let exit = build_exit_config(&mev);

        assert_eq!(exit.tp_sl_tier_count, 2);
        // Tier 0: 0.6 SOL = 600_000_000 lamports
        assert_eq!(exit.tp_sl_tiers[0].trigger_max_lamports, 600_000_000);
        // 0.020 * 100_000 = 2000
        assert_eq!(exit.tp_sl_tiers[0].unconfirmed_tp_fp, 2000);
        assert_eq!(exit.tp_sl_tiers[0].unconfirmed_sl_fp, 1000);
        assert_eq!(exit.tp_sl_tiers[0].confirmed_tp_fp, 3000);
        assert_eq!(exit.tp_sl_tiers[0].confirmed_sl_fp, 1500);
        // Tier 1: 5.0 SOL
        assert_eq!(exit.tp_sl_tiers[1].trigger_max_lamports, 5_000_000_000);
        assert_eq!(exit.tp_sl_tiers[1].unconfirmed_tp_fp, 5000);
        assert_eq!(exit.tp_sl_tiers[1].confirmed_tp_fp, 7000);
    }

    // Test 2: Missing optional fields use correct defaults
    #[test]
    fn test_missing_fields_use_defaults() {
        let mev = minimal_mev();
        let exit = build_exit_config(&mev);

        assert_eq!(exit.confirmation_window_ms, 200);
        assert_eq!(exit.stall_no_buy_ms, 500);
        assert_eq!(exit.stall_fade_fp, (0.01_f64 * 100_000.0) as u32); // 1000
        assert_eq!(exit.stall_conviction_no_buy_ms, 800);
        assert_eq!(exit.stall_conviction_fade_fp, (0.015_f64 * 100_000.0) as u32); // 1500
        assert_eq!(exit.max_hold_safety_ms, 5000);
        assert_eq!(exit.trail_min_conviction, 2);
        assert_eq!(exit.trail_activation_pct_of_base_tp, 60);
        assert_eq!(exit.trail_distance_fp, (0.015_f64 * 100_000.0) as u32); // 1500
        assert_eq!(exit.tp_sl_tier_count, 0); // No tiers when both v2 and legacy absent
    }

    // Test 3: conviction_tp_multipliers always = [100,100,140,180,220]
    #[test]
    fn test_conviction_tp_multipliers_hardcoded() {
        let mev = minimal_mev();
        let exit = build_exit_config(&mev);
        assert_eq!(exit.conviction_tp_multipliers, [100, 100, 140, 180, 220]);

        // Also verify with populated config — still hardcoded
        let json = r#"{ "confirmation_window_ms": 999 }"#;
        let mev2: MevJsonConfig = serde_json::from_str(json).unwrap();
        let exit2 = build_exit_config(&mev2);
        assert_eq!(exit2.conviction_tp_multipliers, [100, 100, 140, 180, 220]);
    }

    // Test 4: Backward compat — old tp_tiers JSON still loads (mapped to confirmed fields)
    #[test]
    fn test_backward_compat_legacy_tp_tiers() {
        let json = r#"{
            "tp_tiers": [
                { "trigger_max_sol": 1.0, "tp_pct": 0.025, "sl_pct": 0.015 }
            ]
        }"#;
        let mev: MevJsonConfig = serde_json::from_str(json).unwrap();
        let exit = build_exit_config(&mev);

        assert_eq!(exit.tp_sl_tier_count, 1);
        assert_eq!(exit.tp_sl_tiers[0].trigger_max_lamports, 1_000_000_000);
        // Legacy: unconfirmed == confirmed (both mapped from tp_pct/sl_pct)
        assert_eq!(exit.tp_sl_tiers[0].unconfirmed_tp_fp, 2500);
        assert_eq!(exit.tp_sl_tiers[0].confirmed_tp_fp, 2500);
        assert_eq!(exit.tp_sl_tiers[0].unconfirmed_sl_fp, 1500);
        assert_eq!(exit.tp_sl_tiers[0].confirmed_sl_fp, 1500);
    }

    // Test: price_pct_to_vsol_bp conversion accuracy
    #[test]
    fn test_price_pct_to_vsol_bp() {
        assert_eq!(price_pct_to_vsol_bp(8.0), 408);
        assert_eq!(price_pct_to_vsol_bp(6.0), 305);
        assert_eq!(price_pct_to_vsol_bp(4.0), 202);
        assert_eq!(price_pct_to_vsol_bp(2.0), 101);
    }

    // ── Signal config tests ────────────────────────────────────────

    // Test: SignalConfigJson defaults when signal section is absent
    #[test]
    fn test_signal_config_absent_uses_defaults() {
        let json = r#"{}"#;
        let mev: MevJsonConfig = serde_json::from_str(json).unwrap();
        assert!(mev.signal.is_none());

        let signal = mev.signal.as_ref()
            .map(SignalConfig::from)
            .unwrap_or_default();
        assert!(!signal.use_bayesian_signal);
        assert!(signal.shadow_composite_enabled);
        assert_eq!(signal.bayesian_decay_rate, 240);
        assert_eq!(signal.bayesian_prior_strength, [6, 9, 13]);
        assert_eq!(signal.divergence_alert_threshold, 10);
    }

    // Test: SignalConfigJson parses with all fields present
    #[test]
    fn test_signal_config_full_parse() {
        let json = r#"{
            "signal": {
                "useBayesianSignal": true,
                "shadowCompositeEnabled": false,
                "bayesianDecayRate": 200,
                "bayesianPriorStrength": [4, 7, 11],
                "divergenceAlertThreshold": 5
            }
        }"#;
        let mev: MevJsonConfig = serde_json::from_str(json).unwrap();
        let signal_json = mev.signal.unwrap();
        assert!(signal_json.use_bayesian_signal);
        assert!(!signal_json.shadow_composite_enabled);
        assert_eq!(signal_json.bayesian_decay_rate, 200);
        assert_eq!(signal_json.bayesian_prior_strength, [4, 7, 11]);
        assert_eq!(signal_json.divergence_alert_threshold, 5);
    }

    // Test: SignalConfigJson partial parse — missing fields get defaults
    #[test]
    fn test_signal_config_partial_parse() {
        let json = r#"{
            "signal": {
                "useBayesianSignal": true
            }
        }"#;
        let mev: MevJsonConfig = serde_json::from_str(json).unwrap();
        let signal_json = mev.signal.unwrap();
        assert!(signal_json.use_bayesian_signal);
        // All others should be defaults
        assert!(signal_json.shadow_composite_enabled);
        assert_eq!(signal_json.bayesian_decay_rate, 240);
        assert_eq!(signal_json.bayesian_prior_strength, [6, 9, 13]);
        assert_eq!(signal_json.divergence_alert_threshold, 10);
    }

    // Test: SignalMode enum
    #[test]
    fn test_signal_mode_enum() {
        assert_eq!(SignalMode::Composite as u8, 0);
        assert_eq!(SignalMode::Bayesian as u8, 1);
        assert_eq!(SignalMode::Composite.as_str(), "composite");
        assert_eq!(SignalMode::Bayesian.as_str(), "bayesian");
    }

    // ── BayesianRevertTracker tests ─────────────────────────────────

    // Test: Revert fires when WR < 20% on ≥ 20 trades
    #[test]
    fn test_revert_tracker_fires_on_low_wr() {
        let mut tracker = BayesianRevertTracker::default();

        // 21 trades: 3 wins, 18 losses → WR = 14.2%
        for _ in 0..3 { tracker.record(true); }
        for _ in 0..18 { tracker.record(false); }

        assert_eq!(tracker.bayesian_trades, 21);
        assert_eq!(tracker.bayesian_wins, 3);
        assert!(tracker.check_revert());
        // Second call should NOT fire again
        assert!(!tracker.check_revert());
        assert!(tracker.reverted);
    }

    // Test: Revert does NOT fire when WR ≥ 20%
    #[test]
    fn test_revert_tracker_no_fire_good_wr() {
        let mut tracker = BayesianRevertTracker::default();

        // 20 trades: 4 wins, 16 losses → WR = 20% (exactly at threshold)
        for _ in 0..4 { tracker.record(true); }
        for _ in 0..16 { tracker.record(false); }

        assert_eq!(tracker.bayesian_trades, 20);
        assert!(!tracker.check_revert()); // 20% is NOT < 20%, so no revert
    }

    // Test: Revert does NOT fire with < 20 trades
    #[test]
    fn test_revert_tracker_no_fire_few_trades() {
        let mut tracker = BayesianRevertTracker::default();

        // 19 trades, all losses — but under the 20-trade minimum
        for _ in 0..19 { tracker.record(false); }

        assert_eq!(tracker.bayesian_trades, 19);
        assert!(!tracker.check_revert());
    }

    // Test: Revert tracker default state
    #[test]
    fn test_revert_tracker_default() {
        let tracker = BayesianRevertTracker::default();
        assert_eq!(tracker.bayesian_trades, 0);
        assert_eq!(tracker.bayesian_wins, 0);
        assert!(!tracker.reverted);
    }

    // Test 5: Deprecated fields don't cause parse errors if present
    #[test]
    fn test_deprecated_fields_no_parse_error() {
        let json = r#"{
            "max_hold_ms": 1500,
            "next_buyer_exit": true,
            "next_buyer_aggregate_flow_ratio": 0.35,
            "next_buyer_count_threshold": 3,
            "next_buyer_single_buy_ratio": 0.25,
            "next_buyer_profit_exit_pct": 0.01,
            "momentum_decay_check_ms": 150,
            "momentum_decay_min_mfe_pct": 0.005,
            "momentum_decay_max_drawdown_pct": 0.008,
            "intra_hold_trailing_stop_pct": 1.0,
            "intra_hold_trailing_stop_min_mfe_pct": 1.0,
            "confirmation_window_ms": 300,
            "tp_sl_tiers_v2": [
                {
                    "trigger_max_sol": 0.6,
                    "unconfirmed_tp_pct": 0.020,
                    "unconfirmed_sl_pct": 0.010,
                    "confirmed_tp_pct": 0.030,
                    "confirmed_sl_pct": 0.015
                }
            ]
        }"#;
        // Must not panic — deprecated fields coexist with new ones
        let mev: MevJsonConfig = serde_json::from_str(json).unwrap();
        let exit = build_exit_config(&mev);
        // Verify the new field was picked up
        assert_eq!(exit.confirmation_window_ms, 300);
        assert_eq!(exit.tp_sl_tier_count, 1);
    }
}
