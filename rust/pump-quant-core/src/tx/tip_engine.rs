//! Dynamic Jito tip engine — conviction-aware tip sizing.
//! All computation integer-only (lamports). Zero f64 on hot path.

/// Tip tier based on exit context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TipContext {
    /// SCALP exit: low-value, minimize tip
    Scalp,
    /// RIDE early phase exit (10-20% gain)
    RideEarly,
    /// RIDE momentum phase exit (20-50% gain)
    RideMomentum,
    /// RIDE tighten phase exit (50%+ gain)
    RideTighten,
    /// Emergency exit (whale dump, sell cascade) — must land THIS slot
    RideEmergency,
}

/// Configuration for dynamic tip computation.
pub struct TipConfig {
    /// Base tip for SCALP exits (lamports)
    pub scalp_tip: u64,
    /// Base tip for RIDE early phase exits (lamports)
    pub ride_early_tip: u64,
    /// Base tip for RIDE momentum phase exits (lamports)
    pub ride_momentum_tip: u64,
    /// Base tip for RIDE tighten phase exits (lamports)
    pub ride_tighten_tip: u64,
    /// Base tip for emergency exits (lamports)
    pub ride_emergency_tip: u64,
    /// Fraction of gross profit to tip (basis points, e.g., 500 = 5%)
    pub profit_fraction_bp: u32,
    /// Absolute maximum tip (lamports)
    pub max_tip: u64,
    /// Absolute minimum tip (lamports) — Jito floor
    pub min_tip: u64,
    /// Congestion multiplier applied when landing rate < 80% (basis points, e.g., 15000 = 1.5x)
    pub congestion_multiplier_bp: u32,
}

impl Default for TipConfig {
    fn default() -> Self {
        Self {
            scalp_tip: 500_000,              // 500 μSOL
            ride_early_tip: 1_000_000,       // 1 mSOL
            ride_momentum_tip: 2_000_000,    // 2 mSOL
            ride_tighten_tip: 3_000_000,     // 3 mSOL
            ride_emergency_tip: 5_000_000,   // 5 mSOL
            profit_fraction_bp: 500,         // 5% of profit
            max_tip: 5_000_000,              // 5 mSOL
            min_tip: 200_000,                // 200 μSOL
            congestion_multiplier_bp: 15_000, // 1.5x
        }
    }
}

/// Conviction-aware Jito tip sizing engine.
///
/// Tracks recent bundle landing rates via a circular buffer and computes
/// optimal tips based on exit context, profit magnitude, and network
/// congestion — all in integer arithmetic (lamports).
pub struct TipEngine {
    config: TipConfig,
    /// Circular buffer tracking last 32 bundle results (true = landed).
    recent_results: [bool; 32],
    /// Write head index into `recent_results`.
    result_head: u8,
    /// Number of results recorded so far (saturates at 32).
    results_count: u8,
}

impl TipEngine {
    /// Create a new `TipEngine` with the given configuration.
    pub fn new(config: TipConfig) -> Self {
        Self {
            config,
            recent_results: [false; 32],
            result_head: 0,
            results_count: 0,
        }
    }

    /// Compute optimal tip for this exit.
    ///
    /// `gross_profit_lamports`: expected gross PnL (can be 0 or negative for SL exits).
    /// `context`: what kind of exit this is.
    ///
    /// All integer arithmetic — zero f64 on the hot path.
    #[inline(always)]
    pub fn compute_tip(&self, gross_profit_lamports: i64, context: TipContext) -> u64 {
        // 1. Base tip from context
        let base = match context {
            TipContext::Scalp => self.config.scalp_tip,
            TipContext::RideEarly => self.config.ride_early_tip,
            TipContext::RideMomentum => self.config.ride_momentum_tip,
            TipContext::RideTighten => self.config.ride_tighten_tip,
            TipContext::RideEmergency => self.config.ride_emergency_tip,
        };

        // 2. Profit-proportional tip (only if profit > 0)
        let profit_tip = if gross_profit_lamports > 0 {
            (gross_profit_lamports as u64 * self.config.profit_fraction_bp as u64) / 10_000
        } else {
            0
        };

        // 3. Take the larger of base and profit-proportional
        let tip = base.max(profit_tip);

        // 4. Apply congestion multiplier if landing rate < 80%
        let tip = if self.landing_rate_pct() < 80 {
            (tip * self.config.congestion_multiplier_bp as u64) / 10_000
        } else {
            tip
        };

        // 5. Clamp to [min, max]
        tip.max(self.config.min_tip).min(self.config.max_tip)
    }

    /// Record a bundle result (landed or not).
    ///
    /// Maintains a circular buffer of the last 32 results for landing-rate
    /// computation.
    pub fn record_result(&mut self, landed: bool) {
        self.recent_results[self.result_head as usize] = landed;
        self.result_head = (self.result_head + 1) & 31; // mod 32
        if self.results_count < 32 {
            self.results_count += 1;
        }
    }

    /// Current landing rate as integer percentage (0–100).
    ///
    /// Returns 100 when no results have been recorded (assume healthy until
    /// proven otherwise — avoids inflating tips on cold start).
    fn landing_rate_pct(&self) -> u8 {
        if self.results_count == 0 {
            return 100; // optimistic default
        }
        let landed = self.recent_results[..self.results_count as usize]
            .iter()
            .filter(|&&r| r)
            .count() as u32;
        ((landed * 100) / self.results_count as u32) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_engine() -> TipEngine {
        TipEngine::new(TipConfig::default())
    }

    /// Helper: fill engine with specific landing rate.
    fn engine_with_landing_rate(rate_pct: u8) -> TipEngine {
        let mut engine = default_engine();
        let landed_count = (32u8 * rate_pct) / 100;
        for i in 0..32u8 {
            engine.record_result(i < landed_count);
        }
        engine
    }

    #[test]
    fn test_scalp_tip_minimum() {
        // Scalp with zero profit should return base scalp tip (500k),
        // which is above min_tip (200k). No congestion (fresh engine = 100% rate).
        let engine = default_engine();
        let tip = engine.compute_tip(0, TipContext::Scalp);
        assert_eq!(tip, 500_000, "Scalp with 0 profit should use base scalp tip");
    }

    #[test]
    fn test_ride_tighten_profit_proportional() {
        // 100 SOL gross profit = 100_000_000_000 lamports.
        // profit_fraction_bp = 500 → 5% = 5_000_000_000 lamports.
        // That's way above base (3 mSOL) AND above max_tip (5 mSOL),
        // so it gets clamped to max_tip.
        //
        // Use a smaller profit: 0.5 SOL = 500_000_000 lamports.
        // 5% of 500M = 25_000_000 → still above max.
        //
        // Use 0.05 SOL = 50_000_000 lamports.
        // 5% of 50M = 2_500_000. Base ride_tighten = 3_000_000.
        // max(3M, 2.5M) = 3M. No congestion. Clamp → 3M.
        let engine = default_engine();
        let tip = engine.compute_tip(50_000_000, TipContext::RideTighten);
        assert_eq!(tip, 3_000_000, "Base ride_tighten should win over small profit fraction");

        // Now with larger profit where profit_tip wins:
        // 0.8 SOL = 800_000_000 lamports. 5% = 40_000_000 → clamped to 5M max.
        let tip2 = engine.compute_tip(800_000_000, TipContext::RideTighten);
        assert_eq!(tip2, 5_000_000, "Large profit tip should be clamped to max_tip");

        // Sweet spot: profit where profit_tip > base but < max.
        // Need: profit_tip > 3M and < 5M.
        // profit_tip = profit * 500 / 10000 = profit / 20.
        // 3M < profit/20 < 5M → 60M < profit < 100M.
        // Use 80M lamports (0.08 SOL). profit_tip = 80M/20 = 4M.
        let tip3 = engine.compute_tip(80_000_000, TipContext::RideTighten);
        assert_eq!(tip3, 4_000_000, "Profit-proportional tip should win when it exceeds base");
    }

    #[test]
    fn test_emergency_tip_max() {
        // Emergency with negative profit (stop-loss).
        // Base = 5M = max_tip. Profit_tip = 0. Result = 5M.
        let engine = default_engine();
        let tip = engine.compute_tip(-50_000_000, TipContext::RideEmergency);
        assert_eq!(tip, 5_000_000, "Emergency SL should use full emergency base tip");

        // Emergency with high profit — still clamped to max.
        let tip2 = engine.compute_tip(1_000_000_000, TipContext::RideEmergency);
        assert_eq!(tip2, 5_000_000, "Emergency tip should be clamped to max");
    }

    #[test]
    fn test_congestion_increases_tip() {
        // Set landing rate to ~50% (below 80% threshold).
        let mut engine = default_engine();
        // Record 16 landed, 16 failed → 50% rate.
        for i in 0..32 {
            engine.record_result(i < 16);
        }
        assert!(engine.landing_rate_pct() < 80, "Landing rate should be below 80%");

        // Scalp with 0 profit: base = 500k.
        // Congestion multiplier: 500k * 15000 / 10000 = 750k.
        // Clamp: max(200k, 750k) = 750k, min(750k, 5M) = 750k.
        let tip = engine.compute_tip(0, TipContext::Scalp);
        assert_eq!(tip, 750_000, "Congestion should apply 1.5x multiplier");

        // Compare with a healthy engine (100% landing rate, no congestion).
        let healthy_engine = default_engine();
        let healthy_tip = healthy_engine.compute_tip(0, TipContext::Scalp);
        assert_eq!(healthy_tip, 500_000);

        assert!(tip > healthy_tip, "Congested tip must exceed healthy tip");
    }

    #[test]
    fn test_clamp_to_max() {
        // Create config with a low max to test clamping.
        let config = TipConfig {
            max_tip: 1_000_000, // 1 mSOL max
            ..TipConfig::default()
        };
        let engine = TipEngine::new(config);

        // RideMomentum base = 2M, but max = 1M → clamp down.
        let tip = engine.compute_tip(0, TipContext::RideMomentum);
        assert_eq!(tip, 1_000_000, "Tip should be clamped to max_tip");

        // Emergency base = 5M → also clamped to 1M.
        let tip2 = engine.compute_tip(0, TipContext::RideEmergency);
        assert_eq!(tip2, 1_000_000, "Emergency tip should also be clamped to max_tip");
    }

    #[test]
    fn test_clamp_to_min() {
        // Config with very low base tips.
        let config = TipConfig {
            scalp_tip: 100_000, // Below min_tip of 200k
            min_tip: 200_000,
            ..TipConfig::default()
        };
        let engine = TipEngine::new(config);

        let tip = engine.compute_tip(0, TipContext::Scalp);
        assert_eq!(tip, 200_000, "Tip below min should be raised to min_tip");
    }

    #[test]
    fn test_negative_profit_uses_base() {
        let engine = default_engine();
        // Negative profit → profit_tip = 0, falls back to base.
        let tip = engine.compute_tip(-100_000_000, TipContext::RideEarly);
        assert_eq!(tip, 1_000_000, "Negative profit should use base tip");
    }

    #[test]
    fn test_landing_rate_cold_start() {
        // Fresh engine with no results should assume 100% (optimistic).
        let engine = default_engine();
        assert_eq!(engine.landing_rate_pct(), 100);
    }

    #[test]
    fn test_landing_rate_circular_buffer() {
        let mut engine = default_engine();
        // Fill buffer with all successes.
        for _ in 0..32 {
            engine.record_result(true);
        }
        assert_eq!(engine.landing_rate_pct(), 100);

        // Now record 8 failures — overwrites 8 successes.
        for _ in 0..8 {
            engine.record_result(false);
        }
        // 24 landed out of 32 = 75%.
        assert_eq!(engine.landing_rate_pct(), 75);
    }

    #[test]
    fn test_record_result_saturates_count() {
        let mut engine = default_engine();
        for _ in 0..100 {
            engine.record_result(true);
        }
        assert_eq!(engine.results_count, 32, "Count should saturate at 32");
    }
}
