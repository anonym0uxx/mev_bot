//! Time-of-Day gating for momentum engine entry sizing.
//!
//! Applies a UTC hour-based multiplier to entry size so the engine
//! reduces exposure during historically unprofitable hours.
//!
//! Data basis (24h sample, 4,440 trades):
//!   08-17 UTC → +67 SOL net (profitable, full size)
//!   18-05 UTC → ~0 SOL net across 2,500+ trades (dead hours, half size)

use serde::{Deserialize, Serialize};

/// Time-of-Day configuration for momentum entry sizing.
///
/// Loaded from the `momentum_tod` section of canary.json.
/// When `enabled` is false (default), `entry_size_multiplier` always returns 1.0.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct MomentumTodConfig {
    /// Master toggle. When false, all hours return multiplier 1.0.
    pub enabled: bool,

    /// UTC hours where entry is completely blocked (multiplier = 0.0).
    /// Example: `[3, 4, 5]` blocks 03:00-05:59 UTC.
    pub blocked_hours_utc: Vec<u8>,

    /// UTC hours where entry size is reduced (multiplier = `reduced_size_multiplier`).
    /// Default: 18-23, 0-5 (dead hours from data analysis).
    pub reduced_hours_utc: Vec<u8>,

    /// UTC hours where entry gets full size (multiplier = 1.0).
    /// Default: 8-17 (profitable hours from data analysis).
    /// Hours not in any list also get 1.0.
    pub boosted_hours_utc: Vec<u8>,

    /// Multiplier applied during reduced hours. Default: 0.5.
    pub reduced_size_multiplier: f64,
}

impl Default for MomentumTodConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            blocked_hours_utc: Vec::new(),
            reduced_hours_utc: vec![18, 19, 20, 21, 22, 23, 0, 1, 2, 3, 4, 5],
            boosted_hours_utc: vec![8, 9, 10, 11, 12, 13, 14, 15, 16, 17],
            reduced_size_multiplier: 0.5,
        }
    }
}

/// Returns the entry size multiplier for the given epoch timestamp.
///
/// Priority order:
///   1. `blocked_hours_utc` → 0.0
///   2. `reduced_hours_utc` → `config.reduced_size_multiplier`
///   3. Everything else (including `boosted_hours_utc`) → 1.0
///
/// If `config.enabled` is false, always returns 1.0.
///
/// # Panics
/// Does not panic. Invalid hour values (≥24) in config are simply never matched.
#[inline]
pub fn entry_size_multiplier(config: &MomentumTodConfig, epoch_ms: u64) -> f64 {
    if !config.enabled {
        return 1.0;
    }

    // Convert epoch_ms → UTC hour (0-23).
    // 86_400_000 ms/day, 3_600_000 ms/hour.
    let hour_utc = ((epoch_ms % 86_400_000) / 3_600_000) as u8;

    if config.blocked_hours_utc.contains(&hour_utc) {
        0.0
    } else if config.reduced_hours_utc.contains(&hour_utc) {
        config.reduced_size_multiplier
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms_for_utc_hour(hour: u8) -> u64 {
        // Pick a known epoch base (2024-01-01 00:00:00 UTC = 1704067200000 ms)
        // and add hours.
        1_704_067_200_000 + (hour as u64) * 3_600_000
    }

    #[test]
    fn test_disabled_returns_one() {
        let mut cfg = MomentumTodConfig::default();
        cfg.enabled = false;
        // Even a blocked hour returns 1.0 when disabled.
        assert_eq!(entry_size_multiplier(&cfg, ms_for_utc_hour(3)), 1.0);
    }

    #[test]
    fn test_boosted_hours_return_one() {
        let cfg = MomentumTodConfig::default();
        for h in 8..=17 {
            let m = entry_size_multiplier(&cfg, ms_for_utc_hour(h));
            assert_eq!(m, 1.0, "hour {h} should be boosted (1.0)");
        }
    }

    #[test]
    fn test_reduced_hours_return_half() {
        let cfg = MomentumTodConfig::default();
        for h in [18, 19, 20, 21, 22, 23, 0, 1, 2, 3, 4, 5] {
            let m = entry_size_multiplier(&cfg, ms_for_utc_hour(h));
            assert_eq!(m, 0.5, "hour {h} should be reduced (0.5)");
        }
    }

    #[test]
    fn test_hours_6_7_are_default_full() {
        // Hours 6 and 7 are in neither reduced nor boosted → default 1.0.
        let cfg = MomentumTodConfig::default();
        assert_eq!(entry_size_multiplier(&cfg, ms_for_utc_hour(6)), 1.0);
        assert_eq!(entry_size_multiplier(&cfg, ms_for_utc_hour(7)), 1.0);
    }

    #[test]
    fn test_blocked_hours_return_zero() {
        let mut cfg = MomentumTodConfig::default();
        cfg.blocked_hours_utc = vec![3, 4, 5];
        // Blocked takes priority over reduced.
        assert_eq!(entry_size_multiplier(&cfg, ms_for_utc_hour(3)), 0.0);
        assert_eq!(entry_size_multiplier(&cfg, ms_for_utc_hour(4)), 0.0);
        assert_eq!(entry_size_multiplier(&cfg, ms_for_utc_hour(5)), 0.0);
        // Hour 2 is still reduced (not blocked).
        assert_eq!(entry_size_multiplier(&cfg, ms_for_utc_hour(2)), 0.5);
    }

    #[test]
    fn test_custom_reduced_multiplier() {
        let mut cfg = MomentumTodConfig::default();
        cfg.reduced_size_multiplier = 0.25;
        assert_eq!(entry_size_multiplier(&cfg, ms_for_utc_hour(22)), 0.25);
    }

    #[test]
    fn test_mid_hour_timestamp() {
        // 14:30 UTC should still be hour 14 (boosted).
        let cfg = MomentumTodConfig::default();
        let ts = ms_for_utc_hour(14) + 30 * 60_000; // +30 minutes
        assert_eq!(entry_size_multiplier(&cfg, ts), 1.0);
    }

    #[test]
    fn test_real_epoch_value() {
        // 2025-03-31 20:00:00 UTC = 1743451200000 ms
        // Hour 20 is in reduced_hours_utc.
        let cfg = MomentumTodConfig::default();
        assert_eq!(entry_size_multiplier(&cfg, 1_743_451_200_000), 0.5);
    }

    #[test]
    fn test_serde_roundtrip() {
        let cfg = MomentumTodConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let cfg2: MomentumTodConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg.enabled, cfg2.enabled);
        assert_eq!(cfg.blocked_hours_utc, cfg2.blocked_hours_utc);
        assert_eq!(cfg.reduced_hours_utc, cfg2.reduced_hours_utc);
        assert_eq!(cfg.boosted_hours_utc, cfg2.boosted_hours_utc);
        assert!((cfg.reduced_size_multiplier - cfg2.reduced_size_multiplier).abs() < f64::EPSILON);
    }

    #[test]
    fn test_empty_config_all_full() {
        // All lists empty → everything returns 1.0.
        let cfg = MomentumTodConfig {
            enabled: true,
            blocked_hours_utc: vec![],
            reduced_hours_utc: vec![],
            boosted_hours_utc: vec![],
            reduced_size_multiplier: 0.5,
        };
        for h in 0..24 {
            assert_eq!(entry_size_multiplier(&cfg, ms_for_utc_hour(h)), 1.0);
        }
    }
}
