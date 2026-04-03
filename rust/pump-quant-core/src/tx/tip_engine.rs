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
    /// Entry (buy) — NEW: used for buy-path tip computation
    Entry,
}

/// Request to compute a tip — used by both buy and sell paths.
pub struct TipRequest {
    pub context: TipContext,
    pub size_lamports: u64,
    pub gain_bps: i64,
    pub grad_score: f64,
    /// Observed price velocity in bps/s from the observation window (Entry only).
    /// Higher velocity → higher tip to land faster in competitive slots.
    pub obs_velocity_bps_per_s: i64,
}

impl TipRequest {
    pub fn is_emergency(&self) -> bool {
        matches!(self.context, TipContext::RideEmergency)
    }
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
    /// Base tip for entry (buy) transactions (lamports)
    pub entry_tip: u64,
    /// Size-proportional rate for emergency exits (bps of position)
    pub rate_emergency_bps: u64,
    /// Size-proportional rate for ride exits (bps of position)
    pub rate_ride_bps: u64,
    /// Size-proportional rate for scalp exits (bps of position)
    pub rate_scalp_bps: u64,
    /// Size-proportional rate for entry (buy) transactions (bps of position)
    pub rate_entry_bps: u64,
    /// Normal ceiling (lamports)
    pub ceiling_normal: u64,
    /// Emergency ceiling (lamports) — higher to ensure landing
    pub ceiling_emergency: u64,
    /// Congestion multiplier applied when landing rate < 80% (basis points, e.g., 15000 = 1.5x)
    pub congestion_multiplier_bp: u32,
    /// Absolute minimum tip (lamports) — Jito floor
    pub min_tip: u64,
    /// Extra tip per bps/s of observed price velocity (Entry only, lamports)
    pub velocity_tip_per_bps: u64,
    /// Cap on the velocity-based tip adder (lamports)
    pub velocity_tip_cap: u64,
}

impl Default for TipConfig {
    fn default() -> Self {
        Self {
            scalp_tip: 500_000,              // 500 μSOL
            ride_early_tip: 1_000_000,       // 1 mSOL
            ride_momentum_tip: 2_000_000,    // 2 mSOL
            ride_tighten_tip: 3_000_000,     // 3 mSOL
            ride_emergency_tip: 5_000_000,   // 5 mSOL
            entry_tip: 500_000,              // 500 μSOL — competitive entry floor
            rate_emergency_bps: 25,          // 0.25% — pay to escape
            rate_ride_bps: 10,               // 0.10% — share ride profits
            rate_scalp_bps: 8,               // 0.08% — moderate scalp share
            rate_entry_bps: 10,              // 0.10% — aggressive entry
            ceiling_normal: 3_000_000,       // 3 mSOL — tighter cap for micro probes
            ceiling_emergency: 10_000_000,   // 10 mSOL — generous for emergencies
            congestion_multiplier_bp: 20_000, // 2.0x — doubles tip under congestion
            min_tip: 500_000,                // 500 μSOL — competitive minimum
            velocity_tip_per_bps: 50,        // 50 lamports per bps/s of observed velocity
            velocity_tip_cap: 150_000,       // 150k lamports max velocity adder
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

    /// Base tip for the given context tier.
    fn base_tip(&self, ctx: TipContext) -> u64 {
        match ctx {
            TipContext::Scalp => self.config.scalp_tip,
            TipContext::RideEarly => self.config.ride_early_tip,
            TipContext::RideMomentum => self.config.ride_momentum_tip,
            TipContext::RideTighten => self.config.ride_tighten_tip,
            TipContext::RideEmergency => self.config.ride_emergency_tip,
            TipContext::Entry => self.config.entry_tip,
        }
    }

    /// Size-proportional rate (bps) for the given context.
    fn rate_bps(&self, ctx: TipContext) -> u64 {
        match ctx {
            TipContext::RideEmergency => self.config.rate_emergency_bps,
            TipContext::Entry => self.config.rate_entry_bps,
            TipContext::Scalp => self.config.rate_scalp_bps,
            _ => self.config.rate_ride_bps,
        }
    }

    /// Compute optimal tip for this exit/entry.
    ///
    /// Merged logic from TipEngine + compute_exit_tip:
    /// 1. Base tier floor
    /// 2. Size-proportional component
    /// 3. Congestion multiplier
    /// 4. PnL cap (winning non-emergency exits only)
    /// 5. Clamp to [min, ceiling]
    ///
    /// All integer arithmetic — zero f64 on the hot path.
    #[inline(always)]
    pub fn compute_tip(&self, req: &TipRequest) -> u64 {
        let base = self.base_tip(req.context);
        let proportional = req.size_lamports * self.rate_bps(req.context) / 10_000;
        let tip = base.max(proportional);

        // Velocity-scaled adder: boost tip for fast-moving tokens (Entry only)
        let tip = if matches!(req.context, TipContext::Entry) && req.obs_velocity_bps_per_s > 0 {
            let adder = (req.obs_velocity_bps_per_s as u64)
                .saturating_mul(self.config.velocity_tip_per_bps)
                .min(self.config.velocity_tip_cap);
            tip.saturating_add(adder)
        } else {
            tip
        };

        // Congestion multiplier if landing rate < 80%
        let tip = if self.landing_rate_pct() < 80 {
            tip * self.config.congestion_multiplier_bp as u64 / 10_000
        } else {
            tip
        };

        // PnL cap — only on winning, non-emergency exits
        let tip = if req.gain_bps > 0 && !req.is_emergency() {
            let gross = (req.size_lamports as u128 * req.gain_bps.unsigned_abs() as u128 / 10_000) as u64;
            let cap_pct: u64 = if req.gain_bps < 2_000 {
                15
            } else if req.gain_bps < 10_000 {
                10
            } else {
                5
            };
            let pnl_cap = gross * cap_pct / 100;
            tip.min(pnl_cap.max(base))
        } else {
            tip
        };

        let ceiling = if req.is_emergency() {
            self.config.ceiling_emergency
        } else {
            self.config.ceiling_normal
        };

        tip.max(self.config.min_tip).min(ceiling)
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
    pub fn landing_rate_pct(&self) -> u8 {
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

    /// Helper: create a TipRequest with common defaults.
    fn make_req(context: TipContext, size_lamports: u64, gain_bps: i64) -> TipRequest {
        TipRequest {
            context,
            size_lamports,
            gain_bps,
            grad_score: 0.0,
            obs_velocity_bps_per_s: 0,
        }
    }

    #[test]
    fn test_scalp_tip_minimum() {
        // Scalp with zero size should return base scalp tip (500k),
        // which is above min_tip (200k). No congestion (fresh engine = 100% rate).
        let engine = default_engine();
        let tip = engine.compute_tip(&make_req(TipContext::Scalp, 0, 0));
        assert_eq!(tip, 500_000, "Scalp with 0 size should use base scalp tip");
    }

    #[test]
    fn test_ride_tighten_size_proportional() {
        let engine = default_engine();
        // 10 SOL position: proportional = 10_000_000_000 * 10 / 10_000 = 10_000_000
        // base = 3_000_000. max(3M, 10M) = 10M. ceiling_normal = 3M. Result = 3M.
        let tip = engine.compute_tip(&make_req(TipContext::RideTighten, 10_000_000_000, 0));
        assert_eq!(tip, 3_000_000, "Large position tip should be clamped to ceiling_normal");
    }

    #[test]
    fn test_emergency_tip_ceiling() {
        let engine = default_engine();
        // Emergency with negative profit (stop-loss), large position.
        // base = 5M, proportional = 10B * 25 / 10000 = 25M.
        // max(5M, 25M) = 25M. ceiling_emergency = 10M. Result = 10M.
        let tip = engine.compute_tip(&make_req(TipContext::RideEmergency, 10_000_000_000, -5000));
        assert_eq!(tip, 10_000_000, "Emergency should use emergency ceiling");
    }

    #[test]
    fn test_congestion_increases_tip() {
        let mut engine = default_engine();
        // Record 16 landed, 16 failed → 50% rate.
        for i in 0..32 {
            engine.record_result(i < 16);
        }
        assert!(engine.landing_rate_pct() < 80, "Landing rate should be below 80%");

        // Scalp with 0 size: base = 500k.
        // Congestion multiplier: 500k * 20000 / 10000 = 1_000_000.
        let tip = engine.compute_tip(&make_req(TipContext::Scalp, 0, 0));
        assert_eq!(tip, 1_000_000, "Congestion should apply 2.0x multiplier");
    }

    #[test]
    fn test_pnl_cap_limits_tip() {
        let engine = default_engine();
        // RideMomentum, 1 SOL position, 500 bps gain (5%)
        // base = 2M, proportional = 1B * 6 / 10000 = 600k. max(2M, 600k) = 2M.
        // PnL cap: gross = 1B * 500 / 10000 = 50M lamports. cap_pct=15%.
        // pnl_cap = 50M * 15 / 100 = 7.5M. min(2M, max(7.5M, 2M)) = 2M.
        let tip = engine.compute_tip(&make_req(TipContext::RideMomentum, 1_000_000_000, 500));
        assert_eq!(tip, 2_000_000, "Small gain: base tip wins");

        // Large gain: 5000 bps (50%), 1 SOL position.
        // base = 2M, proportional = 1B * 6 / 10000 = 600k. max(2M, 600k) = 2M.
        // gross = 1B * 5000 / 10000 = 500M lamports. cap_pct=10%.
        // pnl_cap = 500M * 10 / 100 = 50M. min(2M, max(50M, 2M)) = 2M.
        let tip2 = engine.compute_tip(&make_req(TipContext::RideMomentum, 1_000_000_000, 5000));
        assert_eq!(tip2, 2_000_000, "Base tip wins when PnL cap is generous");
    }

    #[test]
    fn test_entry_tip() {
        let engine = default_engine();
        // Entry, 0.5 SOL position: proportional = 500M * 10 / 10000 = 500k.
        // base = 500k. max(500k, 500k) = 500k. min_tip = 500k. ceiling_normal = 3M.
        let tip = engine.compute_tip(&make_req(TipContext::Entry, 500_000_000, 0));
        assert_eq!(tip, 500_000, "Entry tip should use entry_tip base");

        // Small position: 0.01 SOL
        let tip2 = engine.compute_tip(&make_req(TipContext::Entry, 10_000_000, 0));
        // proportional = 10M * 10 / 10000 = 10000. base = 500k. max(500k, 10k) = 500k.
        // = min_tip 500k.
        assert_eq!(tip2, 500_000, "Small entry should use min_tip");
    }

    #[test]
    fn test_clamp_to_min() {
        let config = TipConfig {
            scalp_tip: 100_000, // Below min_tip of 500k
            min_tip: 500_000,
            ..TipConfig::default()
        };
        let engine = TipEngine::new(config);
        let tip = engine.compute_tip(&make_req(TipContext::Scalp, 0, 0));
        assert_eq!(tip, 500_000, "Tip below min should be raised to min_tip");
    }

    #[test]
    fn test_negative_gain_uses_base() {
        let engine = default_engine();
        let tip = engine.compute_tip(&make_req(TipContext::RideEarly, 1_000_000_000, -500));
        assert_eq!(tip, 1_000_000, "Negative gain should use base tip");
    }

    #[test]
    fn test_landing_rate_cold_start() {
        let engine = default_engine();
        assert_eq!(engine.landing_rate_pct(), 100);
    }

    #[test]
    fn test_landing_rate_circular_buffer() {
        let mut engine = default_engine();
        for _ in 0..32 {
            engine.record_result(true);
        }
        assert_eq!(engine.landing_rate_pct(), 100);

        for _ in 0..8 {
            engine.record_result(false);
        }
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
