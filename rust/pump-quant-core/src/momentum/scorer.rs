//! Integer-only graduation scorer for momentum engine.
//!
//! Scores graduation events on four dimensions (0-25 each, sum 0-100):
//! - **Speed**: how fast the token graduated (faster = stronger momentum)
//! - **Volume**: total bonding curve volume in SOL
//! - **Velocity**: buy transaction rate in the last 5 seconds
//! - **Recovery**: price recovery from opening dump (checked at entry time)
//!
//! ## Design Constraints
//!
//! - All integer arithmetic — no f64 anywhere
//! - `#[inline(always)]` on scoring functions (called from hot path)
//! - Inputs use centisol (volume × 100) and bps to avoid floating point

/// Score components (all 0-25, sum 0-100).
#[derive(Debug, Clone, Copy, Default)]
pub struct GraduationScore {
    /// Speed score: 0-25. Fast graduation = strong buy momentum.
    pub speed: u8,
    /// Volume score: 0-25. Total bonding curve volume in SOL.
    pub volume: u8,
    /// Velocity score: 0-25. Pre-trigger buy rate (buys in last 5s).
    pub velocity: u8,
    /// Recovery score: 0-25. Price recovery from opening dump (checked at entry).
    pub recovery: u8,
}

impl GraduationScore {
    /// Total score (0-100). Saturating add prevents overflow.
    #[inline(always)]
    pub fn total(&self) -> u8 {
        self.speed
            .saturating_add(self.volume)
            .saturating_add(self.velocity)
            .saturating_add(self.recovery)
    }

    /// Total score excluding recovery (used for pre-entry gate).
    /// Recovery is deferred to entry time since it requires live price.
    #[inline(always)]
    pub fn total_excluding_recovery(&self) -> u8 {
        self.speed
            .saturating_add(self.volume)
            .saturating_add(self.velocity)
    }
}

/// Score a graduation event. All integer arithmetic, no f64.
///
/// # Parameters
///
/// - `grad_speed_s`: seconds from token creation to graduation (0..=86400)
/// - `volume_sol_x100`: total bonding curve volume in centisol (sol × 100, avoids f64)
/// - `buys_last_5s`: number of buy transactions in the last 5 seconds of bonding curve
/// - `price_recovery_bps`: price recovery from opening low in bps (0..=10000)
///
/// # Scoring Logic
///
/// | Component | Formula | Max |
/// |-----------|---------|-----|
/// | Speed | `(300 - min(speed, 300)) / 12` | 25 |
/// | Volume | `centisol / 2000` | 25 (500 SOL) |
/// | Velocity | `min(buys_last_5s, 25)` | 25 |
/// | Recovery | `bps / 40` | 25 (1000 bps = 10%) |
#[inline(always)]
pub fn score_graduation(
    grad_speed_s: u32,
    volume_sol_x100: u32,
    buys_last_5s: u32,
    price_recovery_bps: u32,
) -> GraduationScore {
    // Speed: faster = better. 300s max useful window.
    // 0s → 25, 60s → 20, 120s → 15, 180s → 10, 240s → 5, 300s+ → 0
    let speed_penalty = grad_speed_s.min(300);
    let speed = ((300u32.saturating_sub(speed_penalty)) / 12).min(25) as u8;

    // Volume: 500 SOL (50000 centisol) = max score.
    // 50000 / 2000 = 25. 250 SOL → 12. 100 SOL → 5.
    let volume = (volume_sol_x100 / 2000).min(25) as u8;

    // Velocity: raw buy count in 5s window, capped at 25.
    // 25 buys in 5s = 5 buys/sec = maximum momentum signal.
    let velocity = buys_last_5s.min(25) as u8;

    // Recovery: bps / 40. 1000 bps (10% recovery) = 25 points.
    // 500 bps (5%) = 12 points. 0 bps = 0 points.
    let recovery = (price_recovery_bps / 40).min(25) as u8;

    GraduationScore {
        speed,
        volume,
        velocity,
        recovery,
    }
}

/// Compute recovery score from current price vs bonding curve terminal price.
///
/// Called at entry time (T+delay) when we have a live price. Uses fixed-point
/// prices (lamports per 1M token atoms) for integer-only arithmetic.
///
/// # Parameters
///
/// - `current_price_fp`: current market price (fixed-point)
/// - `bc_terminal_price_fp`: bonding curve terminal price (fixed-point, ~411)
///
/// # Returns
///
/// Recovery score 0-25.
#[inline(always)]
pub fn recovery_score_from_prices(current_price_fp: u64, bc_terminal_price_fp: u64) -> u8 {
    if bc_terminal_price_fp == 0 || current_price_fp == 0 {
        return 0;
    }

    if current_price_fp >= bc_terminal_price_fp {
        // Price is at or above BC terminal → full recovery, max score
        return 25;
    }

    // Discount in bps: ((terminal - current) * 10000) / terminal
    let discount_bps = (bc_terminal_price_fp - current_price_fp)
        .saturating_mul(10_000)
        / bc_terminal_price_fp;

    // Invert: recovery_bps = 10000 - discount_bps
    let recovery_bps = 10_000u64.saturating_sub(discount_bps);

    // Score: recovery_bps / 40, capped at 25
    (recovery_bps as u32 / 40).min(25) as u8
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_score_fast_graduation() {
        // 60 seconds → speed = (300 - 60) / 12 = 240 / 12 = 20
        let score = score_graduation(60, 0, 0, 0);
        assert_eq!(score.speed, 20);
    }

    #[test]
    fn test_score_very_fast_graduation() {
        // 0 seconds → speed = 300 / 12 = 25
        let score = score_graduation(0, 0, 0, 0);
        assert_eq!(score.speed, 25);
    }

    #[test]
    fn test_score_slow_graduation() {
        // 3600 seconds → speed_penalty = min(3600, 300) = 300
        // speed = (300 - 300) / 12 = 0
        let score = score_graduation(3600, 0, 0, 0);
        assert_eq!(score.speed, 0);
    }

    #[test]
    fn test_score_volume() {
        // 500 SOL = 50000 centisol → volume = 50000 / 2000 = 25
        let score = score_graduation(3600, 50_000, 0, 0);
        assert_eq!(score.volume, 25);

        // 250 SOL = 25000 centisol → volume = 25000 / 2000 = 12
        let score = score_graduation(3600, 25_000, 0, 0);
        assert_eq!(score.volume, 12);

        // 1000 SOL = 100000 centisol → volume = 100000 / 2000 = 50 → capped at 25
        let score = score_graduation(3600, 100_000, 0, 0);
        assert_eq!(score.volume, 25);
    }

    #[test]
    fn test_score_velocity() {
        // 10 buys in 5s → velocity = min(10, 25) = 10
        let score = score_graduation(3600, 0, 10, 0);
        assert_eq!(score.velocity, 10);

        // 30 buys → capped at 25
        let score = score_graduation(3600, 0, 30, 0);
        assert_eq!(score.velocity, 25);

        // 0 buys → 0
        let score = score_graduation(3600, 0, 0, 0);
        assert_eq!(score.velocity, 0);
    }

    #[test]
    fn test_score_recovery() {
        // 1000 bps (10% recovery) → recovery = 1000 / 40 = 25
        let score = score_graduation(3600, 0, 0, 1000);
        assert_eq!(score.recovery, 25);

        // 500 bps → recovery = 500 / 40 = 12
        let score = score_graduation(3600, 0, 0, 500);
        assert_eq!(score.recovery, 12);
    }

    #[test]
    fn test_score_total_caps_at_100() {
        // Max everything: speed=25, volume=25, velocity=25, recovery=25
        let score = score_graduation(0, 50_000, 25, 1000);
        assert_eq!(score.speed, 25);
        assert_eq!(score.volume, 25);
        assert_eq!(score.velocity, 25);
        assert_eq!(score.recovery, 25);
        assert_eq!(score.total(), 100);

        // Even with extreme values, total should not exceed 100
        let score = score_graduation(0, 1_000_000, 1000, 100_000);
        assert!(score.total() <= 100);
    }

    #[test]
    fn test_score_total_excluding_recovery() {
        let score = score_graduation(60, 25_000, 10, 500);
        let expected = score.speed + score.volume + score.velocity;
        assert_eq!(score.total_excluding_recovery(), expected);
    }

    #[test]
    fn test_recovery_score_from_prices_full_recovery() {
        // Price at or above terminal → max score
        let score = recovery_score_from_prices(500, 411);
        assert_eq!(score, 25);
    }

    #[test]
    fn test_recovery_score_from_prices_zero() {
        // Zero prices → 0
        assert_eq!(recovery_score_from_prices(0, 411), 0);
        assert_eq!(recovery_score_from_prices(411, 0), 0);
    }

    #[test]
    fn test_recovery_score_from_prices_partial() {
        // 90% of terminal price → 10% discount → 9000 bps recovery → 9000/40 = 225 → capped 25
        // Wait: recovery_bps = 10000 - discount_bps
        // discount_bps = (411 - 370) * 10000 / 411 = 410000 / 411 = 997
        // recovery_bps = 10000 - 997 = 9003
        // score = 9003 / 40 = 225 → capped at 25
        let score = recovery_score_from_prices(370, 411);
        assert_eq!(score, 25); // still very high recovery

        // 50% of terminal → discount_bps = 5000, recovery_bps = 5000, score = 125 → 25
        let score = recovery_score_from_prices(205, 411);
        // discount = (411-205)*10000/411 = 2060000/411 = 5012
        // recovery = 10000 - 5012 = 4988, score = 4988/40 = 124 → 25
        assert_eq!(score, 25);

        // Very low price: 10% of terminal → big discount
        let score = recovery_score_from_prices(41, 411);
        // discount = (411-41)*10000/411 = 3700000/411 = 9002
        // recovery = 10000 - 9002 = 998, score = 998/40 = 24
        assert_eq!(score, 24);
    }
}
