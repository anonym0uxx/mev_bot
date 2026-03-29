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
    /// Tick interval: check positions every this many ms.
    pub check_ms: u64,
    /// Daily loss cap in SOL — circuit breaker.
    pub daily_loss_cap_sol: f64,
    /// Raydium AMM fee in basis points.
    pub raydium_fee_bps: u32,
    /// PumpSwap fee in basis points.
    pub pumpswap_fee_bps: u32,
}

impl Default for MomentumConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            paper_mode: true,
            entry_delay_ms: 15_000,
            min_grad_score: 40,
            position_size_sol: 0.3,
            max_concurrent: 3,
            tp1_pct: 5.0,
            tp1_exit_pct: 0.30,
            tp2_pct: 15.0,
            tp2_exit_pct: 0.30,
            tp3_pct: 50.0,
            tp3_exit_pct: 0.40,
            trailing_stop_pct: 8.0,
            hard_sl_pct: 12.0,
            time_sl_ms: 60_000,
            max_hold_ms: 300_000,
            check_ms: 150,
            daily_loss_cap_sol: 2.0,
            raydium_fee_bps: 25,
            pumpswap_fee_bps: 100,
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
        assert_eq!(config.max_concurrent, 3);
        assert!((config.tp1_pct - 5.0).abs() < f64::EPSILON);
        assert!((config.tp1_exit_pct - 0.30).abs() < f64::EPSILON);
        assert!((config.tp2_pct - 15.0).abs() < f64::EPSILON);
        assert!((config.tp2_exit_pct - 0.30).abs() < f64::EPSILON);
        assert!((config.tp3_pct - 50.0).abs() < f64::EPSILON);
        assert!((config.tp3_exit_pct - 0.40).abs() < f64::EPSILON);
        assert!((config.trailing_stop_pct - 8.0).abs() < f64::EPSILON);
        assert!((config.hard_sl_pct - 12.0).abs() < f64::EPSILON);
        assert_eq!(config.time_sl_ms, 60_000);
        assert_eq!(config.max_hold_ms, 300_000);
        assert_eq!(config.check_ms, 150);
        assert!((config.daily_loss_cap_sol - 2.0).abs() < f64::EPSILON);
        assert_eq!(config.raydium_fee_bps, 25);
        assert_eq!(config.pumpswap_fee_bps, 100);
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
