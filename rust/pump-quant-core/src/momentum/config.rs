//! Configuration for the post-graduation momentum engine.
//!
//! All fields have serde defaults so the momentum section can be
//! omitted entirely from canary.json (engine defaults to disabled).

use serde::{Deserialize, Serialize};

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
    /// Daily loss cap in SOL — circuit breaker.
    pub daily_loss_cap_sol: f64,
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
}

impl Default for MomentumConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            paper_mode: true,
            entry_delay_ms: 15_000,
            min_grad_score: 40,
            position_size_sol: 0.3,
            max_concurrent: 5,
            tp1_pct: 5.0,
            tp1_exit_pct: 0.0,
            tp2_pct: 15.0,
            tp2_exit_pct: 0.0,
            tp3_pct: 999.0,
            tp3_exit_pct: 0.40,
            trailing_stop_pct: 8.0,
            hard_sl_pct: 12.0,
            time_sl_ms: 60_000,
            max_hold_ms: 600_000,
            momentum_decay_min_hold_ms: 30_000,
            momentum_decay_threshold: -150.0,
            momentum_decay_window: 8,
            max_hold_trail_activation_ms: 200_000,
            max_hold_trail_pct: 5.0,
            check_ms: 150,
            daily_loss_cap_sol: 2.0,
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

            // Momentum state classification
            momentum_accel_threshold_bps: 100,
            momentum_decel_threshold_bps: -100,
            momentum_reversal_threshold_bps: -500,

            // Dynamic trailing stop
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
            dead_zone_ws_zero_ms: 3_000,
            dead_zone_ws_sparse_ms: 8_000,
            dead_zone_ws_active_ms: 15_000,
            dead_zone_ws_sparse_n: 3,
            dead_zone_ws_fallback_ms: 8_000,

            // Dead zone detection
            dead_zone_early_ms: 10_000,
            dead_zone_early_bps: 50,
            dead_zone_confirmed_ms: 60_000,
            dead_zone_confirmed_bps: 200,
            dead_zone_stagnant_bps: 300,
            dead_zone_stagnant_window_ms: 30_000,

            // Scale-in entry
            probe_size_sol: 0.10,
            scale_in_s0_strong_bps: 300,
            scale_in_s0_strong_sol: 0.40,
            scale_in_s0_moderate_bps: 100,
            scale_in_s0_moderate_sol: 0.20,
            scale_in_s1_moderate_bps: 200,
            scale_in_s1_sol: 0.15,
            max_total_size_sol: 0.50,
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
        assert_eq!(config.min_grad_score, 40);
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
        assert!((config.daily_loss_cap_sol - 2.0).abs() < f64::EPSILON);
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
}
