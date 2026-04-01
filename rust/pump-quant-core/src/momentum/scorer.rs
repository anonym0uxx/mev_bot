//! Integer-only graduation scorer for momentum engine (v4).
//!
//! Scores graduation events on seven dimensions (sum 0-100):
//! - **Speed** (0-20): how slow the token graduated (slower = stronger post-grad momentum)
//! - **Volume tier** (0-25): total bonding curve volume in SOL, moderate = best
//! - **Velocity** (0-15): buy rate normalized by volume (organic demand signal)
//! - **Buy/sell ratio** (0-10): unidirectional buy pressure vs distribution
//! - **Entry discount** (0-15): structural edge from buying below BC terminal price
//! - **LP reserve size** (0-10): fresh pump.fun graduates land with 85-120 SOL sweet spot
//! - **Cold miss bonus**: omitted from struct (applied externally when enrichment is cold)
//!
//! ## Design Constraints
//!
//! - All integer arithmetic — no f64 anywhere
//! - `#[inline(always)]` on scoring functions (called from hot path)
//! - Inputs use centisol (volume x 100) and bps to avoid floating point
//!
//! ## v4 Changelog (weight redistribution + LP reserve)
//!
//! Weight redistribution based on backtesting signal quality:
//! - Volume tier upweighted 15→25: strongest discriminator (50-100 SOL sweet spot = 39.6% WR)
//! - Speed downweighted 25→20: still important but less than volume
//! - Velocity downweighted 20→15: good signal but noisy at extremes
//! - Buy/sell ratio downweighted 25→10: high false positive rate from single-buyer tokens
//! - Entry discount unchanged at 15: reliable structural edge
//! - LP reserve size NEW at 10: fresh graduates (85-120 SOL) = momentum-tradeable pools
//!
//! | Component      | v3  | v4  | Rationale                                       |
//! |----------------|-----|-----|-------------------------------------------------|
//! | Speed          | 25  | 20  | Slightly reduced — still rewards organic grads  |
//! | Volume tier    | 15  | 25  | Strongest signal — moderate vol = organic        |
//! | Velocity       | 20  | 15  | Reduced — noisy at volume extremes               |
//! | Buy/sell ratio | 25  | 10  | Heavily reduced — single-buyer false positives   |
//! | Entry discount | 15  | 15  | Unchanged — reliable structural edge             |
//! | LP reserve     |  0  | 10  | NEW: small pool = high momentum tradeability     |
//!
//! ## Call-site signature (v4: added reserve_sol_lamports)
//!
//! ```rust,ignore
//! let score = score_graduation(
//!     grad_speed_s,           // u32: seconds from creation to graduation
//!     volume_sol_x100,        // u32: total BC volume in centisol (sol x 100)
//!     buys_last_5s,           // u32: buy txns in last 5s of BC
//!     sells_last_5s,          // u32: sell txns in last 5s of BC
//!     entry_price_fp,         // u64: entry price in fixed-point lamports/1M atoms
//!     bc_terminal_price_fp,   // u64: BC terminal price in fixed-point
//!     reserve_sol_lamports,   // u64: LP pool SOL reserve in lamports (NEW in v4)
//! );
//! ```

/// Score components (v4: 6 scored components + cold miss bonus, sum 0-100).
#[derive(Debug, Clone, Copy, Default)]
pub struct GraduationScore {
    /// Speed score: 0-20. Slow graduation = organic momentum (inverted from v2).
    pub speed: u8,
    /// Volume tier score: 0-25. Moderate volume = organic sweet spot.
    pub volume_tier: u8,
    /// Velocity score: 0-15. Buy rate normalized by volume (organic demand).
    pub velocity: u8,
    /// Buy/sell ratio score: 0-10. Unidirectional pressure signal (reduced from v3).
    pub buy_sell_ratio: u8,
    /// Entry discount score: 0-15. Buying below BC terminal = structural edge.
    pub entry_discount: u8,
    /// LP reserve size score: 0-10. Fresh pump.fun graduates (85-120 SOL) = sweet spot.
    pub lp_reserve: u8,
    /// Cold miss bonus: 0-5. Applied externally when enrichment data was unavailable.
    /// Information asymmetry edge — we're faster than enrichment-dependent bots.
    pub cold_miss_bonus: u8,
}

impl GraduationScore {
    /// Total score (0-100). Saturating add prevents overflow.
    #[inline(always)]
    pub fn total(&self) -> u8 {
        self.speed
            .saturating_add(self.volume_tier)
            .saturating_add(self.velocity)
            .saturating_add(self.buy_sell_ratio)
            .saturating_add(self.entry_discount)
            .saturating_add(self.lp_reserve)
            .saturating_add(self.cold_miss_bonus)
    }

    /// Total score excluding entry_discount (used for pre-entry gate when
    /// entry price is not yet known). Sum of speed + volume_tier + velocity
    /// + buy_sell_ratio + lp_reserve + cold_miss_bonus = max 85.
    #[inline(always)]
    pub fn total_excluding_discount(&self) -> u8 {
        self.speed
            .saturating_add(self.volume_tier)
            .saturating_add(self.velocity)
            .saturating_add(self.buy_sell_ratio)
            .saturating_add(self.lp_reserve)
            .saturating_add(self.cold_miss_bonus)
    }
}

// -- Component scorers --------------------------------------------------------

/// Speed score (0-20). Inverted from v2: SLOWER graduation = HIGHER score.
/// Fast grads (<=60s) = bot/whale fills = 7.3% WR. Slow grads (>=120s) = 41.1% WR.
/// v4: rescaled from 0-25 to 0-20.
#[inline(always)]
fn score_speed(grad_speed_s: u32) -> u8 {
    if grad_speed_s <= 60 {
        0
    } else if grad_speed_s <= 90 {
        ((grad_speed_s - 60) * 4 / 30).min(4) as u8
    } else if grad_speed_s <= 120 {
        (4 + (grad_speed_s - 90) * 8 / 30).min(12) as u8
    } else if grad_speed_s <= 180 {
        (12 + (grad_speed_s - 120) * 4 / 60).min(16) as u8
    } else if grad_speed_s <= 300 {
        (16 + (grad_speed_s - 180) * 4 / 120).min(20) as u8
    } else {
        20u8.saturating_sub(((grad_speed_s - 300) * 4 / 300).min(4) as u8)
    }
}

/// Volume tier score (0-25). Inverted from v2: MODERATE volume = HIGH score.
/// 50-100 SOL = sweet spot (39.6% WR). >=655 SOL saturated = 5.9% WR.
/// v4: rescaled from 0-15 to 0-25.
#[inline(always)]
fn score_volume_tier(volume_sol_x100: u32) -> u8 {
    if volume_sol_x100 >= 65_500 {
        0  // >=655 SOL: confirmed bot
    } else if volume_sol_x100 >= 40_000 {
        3  // 400-655 SOL: likely bot
    } else if volume_sol_x100 >= 20_000 {
        8  // 200-400 SOL: institutional
    } else if volume_sol_x100 >= 10_000 {
        20 // 100-200 SOL: good organic
    } else if volume_sol_x100 >= 5_000 {
        25 // 50-100 SOL: sweet spot ← MAX
    } else if volume_sol_x100 >= 3_000 {
        8  // 30-50 SOL: light
    } else {
        0  // <30 SOL: insufficient
    }
}

/// Velocity score: 0-15 (v4: reduced from 0-20).
///
/// Normalized buy rate: `buys_5s * 10_000 / max(volume_sol_x100, 1)`.
/// High velocity relative to volume = organic demand (many small buys),
/// not just a single whale deposit. Capped at 15.
#[inline(always)]
fn score_velocity(buys_last_5s: u32, volume_sol_x100: u32) -> u8 {
    let vol = volume_sol_x100.max(1);
    let normalized = buys_last_5s.saturating_mul(10_000) / vol;
    normalized.min(15) as u8
}

/// Buy/sell ratio score: 0-10 (v4: reduced from 0-25).
///
/// `ratio = buys_5s / max(sells_5s, 1)`.
/// Linear: `min(ratio * 2, 10)`.
#[inline(always)]
fn score_buy_sell_ratio(buys_last_5s: u32, sells_last_5s: u32) -> u8 {
    let sells = sells_last_5s.max(1);
    let ratio = buys_last_5s / sells;
    (ratio.saturating_mul(2)).min(10) as u8
}

/// Entry discount score: 0-15 (unchanged from v3).
///
/// `discount_bps = (bc_terminal - entry) * 10_000 / bc_terminal`
/// Scoring: discount_bps / 66, capped at 15.
/// - 0 bps (at terminal) -> 0
/// - ~330 bps (3.3%) -> 5
/// - ~660 bps (6.6%) -> 10
/// - ~1000 bps (10%) -> 15 (max)
///
/// If entry >= terminal (premium), score = 0.
#[inline(always)]
fn score_entry_discount(entry_price_fp: u64, bc_terminal_price_fp: u64) -> u8 {
    if bc_terminal_price_fp == 0 || entry_price_fp == 0 {
        return 0;
    }
    if entry_price_fp >= bc_terminal_price_fp {
        return 0;
    }
    let discount_bps = (bc_terminal_price_fp - entry_price_fp)
        .saturating_mul(10_000)
        / bc_terminal_price_fp;

    (discount_bps as u32 / 66).min(15) as u8
}

/// LP reserve size score (0-10). Fresh pump.fun graduates land with 85-120 SOL.
/// Smaller pools are more volatile and momentum-tradeable.
/// Very large pools (Raydium majors) dampen momentum — skip.
#[inline(always)]
fn score_lp_reserve(reserve_sol_lamports: u64) -> u8 {
    let sol = reserve_sol_lamports / 1_000_000_000;
    if sol < 50 {
        0  // too thin
    } else if sol < 100 {
        10 // 50-100 SOL ← sweet spot
    } else if sol < 200 {
        8  // 100-200 SOL: good
    } else if sol < 500 {
        4  // 200-500 SOL: dampened
    } else if sol < 2000 {
        2  // 500-2000 SOL: institutional
    } else {
        0  // >2000 SOL: market making, skip
    }
}

/// Score a graduation event (v4). All integer arithmetic, no f64.
///
/// # Parameters
///
/// - `grad_speed_s`: seconds from token creation to graduation (0..=86400)
/// - `volume_sol_x100`: total bonding curve volume in centisol (sol x 100)
/// - `buys_last_5s`: buy transactions in the last 5 seconds of bonding curve
/// - `sells_last_5s`: sell transactions in the last 5 seconds of bonding curve
/// - `entry_price_fp`: entry price in fixed-point (lamports per 1M token atoms)
/// - `bc_terminal_price_fp`: bonding curve terminal price in fixed-point (~411)
/// - `reserve_sol_lamports`: LP pool SOL reserve in lamports (NEW in v4)
///
/// # Returns
///
/// `GraduationScore` with 6 components summing to 0-100.
#[inline(always)]
pub fn score_graduation(
    grad_speed_s: u32,
    volume_sol_x100: u32,
    buys_last_5s: u32,
    sells_last_5s: u32,
    entry_price_fp: u64,
    bc_terminal_price_fp: u64,
    reserve_sol_lamports: u64,
) -> GraduationScore {
    GraduationScore {
        speed: score_speed(grad_speed_s),
        volume_tier: score_volume_tier(volume_sol_x100),
        velocity: score_velocity(buys_last_5s, volume_sol_x100),
        buy_sell_ratio: score_buy_sell_ratio(buys_last_5s, sells_last_5s),
        entry_discount: score_entry_discount(entry_price_fp, bc_terminal_price_fp),
        lp_reserve: score_lp_reserve(reserve_sol_lamports),
        cold_miss_bonus: 0, // Applied post-scoring in mod.rs when enrichment data unavailable
    }
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Helpers ---------------------------------------------------------------

    /// Default reserve: 85 SOL (typical pump.fun graduation) = 85_000_000_000 lamports
    const DEFAULT_RESERVE: u64 = 85_000_000_000;

    fn score_with_speed(s: u32) -> GraduationScore {
        score_graduation(s, 0, 0, 0, 0, 0, DEFAULT_RESERVE)
    }

    fn score_with_volume(centisol: u32) -> GraduationScore {
        score_graduation(3600, centisol, 0, 0, 0, 0, DEFAULT_RESERVE)
    }

    fn score_with_velocity(buys: u32, volume_centisol: u32) -> GraduationScore {
        score_graduation(3600, volume_centisol, buys, 0, 0, 0, DEFAULT_RESERVE)
    }

    fn score_with_ratio(buys: u32, sells: u32) -> GraduationScore {
        score_graduation(3600, 0, buys, sells, 0, 0, DEFAULT_RESERVE)
    }

    fn score_with_discount(entry: u64, terminal: u64) -> GraduationScore {
        score_graduation(3600, 0, 0, 0, entry, terminal, DEFAULT_RESERVE)
    }

    fn score_with_reserve(lamports: u64) -> GraduationScore {
        score_graduation(3600, 0, 0, 0, 0, 0, lamports)
    }

    // -- Speed component (0-20, inverted) -------------------------------------

    #[test]
    fn test_speed_instant() {
        assert_eq!(score_with_speed(0).speed, 0);
    }

    #[test]
    fn test_speed_60s() {
        assert_eq!(score_with_speed(60).speed, 0);
    }

    #[test]
    fn test_speed_90s() {
        // (90-60)*4/30 = 4
        assert_eq!(score_with_speed(90).speed, 4);
    }

    #[test]
    fn test_speed_120s() {
        // 4 + (120-90)*8/30 = 4 + 8 = 12
        assert_eq!(score_with_speed(120).speed, 12);
    }

    #[test]
    fn test_speed_150s() {
        // 12 + (150-120)*4/60 = 12 + 2 = 14
        assert_eq!(score_with_speed(150).speed, 14);
    }

    #[test]
    fn test_speed_180s() {
        // 12 + (180-120)*4/60 = 12 + 4 = 16
        assert_eq!(score_with_speed(180).speed, 16);
    }

    #[test]
    fn test_speed_240s() {
        // 16 + (240-180)*4/120 = 16 + 2 = 18
        assert_eq!(score_with_speed(240).speed, 18);
    }

    #[test]
    fn test_speed_300s() {
        // 16 + (300-180)*4/120 = 16 + 4 = 20
        assert_eq!(score_with_speed(300).speed, 20);
    }

    #[test]
    fn test_speed_slow() {
        // 3600s -> 20 - min((3600-300)*4/300, 4) = 20 - 4 = 16
        assert_eq!(score_with_speed(3600).speed, 16);
    }

    // -- Volume tier component (0-25) -----------------------------------------

    #[test]
    fn test_volume_tier_sweet_spot() {
        // 50 SOL (5_000 centisol) -> sweet spot -> 25
        assert_eq!(score_with_volume(5_000).volume_tier, 25);
        // 99.99 SOL (9_999 centisol) -> still sweet spot -> 25
        assert_eq!(score_with_volume(9_999).volume_tier, 25);
    }

    #[test]
    fn test_volume_tier_insufficient() {
        assert_eq!(score_with_volume(0).volume_tier, 0);
        assert_eq!(score_with_volume(2_999).volume_tier, 0);
    }

    #[test]
    fn test_volume_tier_light() {
        // 30-50 SOL -> 8
        assert_eq!(score_with_volume(3_000).volume_tier, 8);
        assert_eq!(score_with_volume(4_999).volume_tier, 8);
    }

    #[test]
    fn test_volume_tier_good_organic() {
        // 100 SOL -> 20
        assert_eq!(score_with_volume(10_000).volume_tier, 20);
        assert_eq!(score_with_volume(19_999).volume_tier, 20);
    }

    #[test]
    fn test_volume_tier_institutional() {
        // 200-400 SOL -> 8
        assert_eq!(score_with_volume(20_000).volume_tier, 8);
        assert_eq!(score_with_volume(30_000).volume_tier, 8);
    }

    #[test]
    fn test_volume_tier_likely_bot() {
        // 400-655 SOL -> 3
        assert_eq!(score_with_volume(40_000).volume_tier, 3);
        assert_eq!(score_with_volume(60_000).volume_tier, 3);
    }

    #[test]
    fn test_volume_tier_confirmed_bot() {
        // >=655 SOL -> 0
        assert_eq!(score_with_volume(65_500).volume_tier, 0);
        assert_eq!(score_with_volume(100_000).volume_tier, 0);
    }

    // -- Velocity component (0-15) --------------------------------------------

    #[test]
    fn test_velocity_zero_buys() {
        assert_eq!(score_with_velocity(0, 50_000).velocity, 0);
    }

    #[test]
    fn test_velocity_normalized() {
        // 3 buys, 500 SOL (50_000 centisol)
        // normalized = 3 * 10_000 / 50_000 = 0
        assert_eq!(score_with_velocity(3, 50_000).velocity, 0);

        // 10 buys, 100 SOL (10_000 centisol) = 10
        assert_eq!(score_with_velocity(10, 10_000).velocity, 10);

        // 15 buys, 100 SOL = 15 (capped)
        assert_eq!(score_with_velocity(15, 10_000).velocity, 15);
    }

    #[test]
    fn test_velocity_capped_at_15() {
        assert_eq!(score_with_velocity(50, 10_000).velocity, 15);
    }

    #[test]
    fn test_velocity_zero_volume() {
        // buys=1, vol=0 -> 1*10_000/1 = 10_000 -> capped at 15
        assert_eq!(score_with_velocity(1, 0).velocity, 15);
    }

    #[test]
    fn test_velocity_high_organic() {
        // 25 buys, 50 SOL -> 25*10_000/5_000 = 50 -> capped at 15
        assert_eq!(score_with_velocity(25, 5_000).velocity, 15);
        // 25 buys, 2000 SOL -> 25*10_000/200_000 = 1
        assert_eq!(score_with_velocity(25, 200_000).velocity, 1);
    }

    // -- Buy/sell ratio component (0-10) --------------------------------------

    #[test]
    fn test_ratio_zero_buys() {
        assert_eq!(score_with_ratio(0, 5).buy_sell_ratio, 0);
    }

    #[test]
    fn test_ratio_equal_pressure() {
        // 5/5=1, 1*2=2
        assert_eq!(score_with_ratio(5, 5).buy_sell_ratio, 2);
    }

    #[test]
    fn test_ratio_strong_buy() {
        // 10/2=5, 5*2=10 (capped)
        assert_eq!(score_with_ratio(10, 2).buy_sell_ratio, 10);
    }

    #[test]
    fn test_ratio_capped_at_10() {
        // 100/1=100, 100*2=200, capped at 10
        assert_eq!(score_with_ratio(100, 1).buy_sell_ratio, 10);
    }

    #[test]
    fn test_ratio_zero_sells() {
        // 10/1=10, 10*2=20, capped at 10
        assert_eq!(score_with_ratio(10, 0).buy_sell_ratio, 10);
    }

    #[test]
    fn test_ratio_heavy_selling() {
        // 2/10=0, 0*2=0
        assert_eq!(score_with_ratio(2, 10).buy_sell_ratio, 0);
    }

    // -- Entry discount component (0-15, unchanged from v3) -------------------

    #[test]
    fn test_discount_at_terminal() {
        assert_eq!(score_with_discount(411, 411).entry_discount, 0);
    }

    #[test]
    fn test_discount_above_terminal() {
        assert_eq!(score_with_discount(500, 411).entry_discount, 0);
    }

    #[test]
    fn test_discount_zero_prices() {
        assert_eq!(score_with_discount(0, 411).entry_discount, 0);
        assert_eq!(score_with_discount(411, 0).entry_discount, 0);
        assert_eq!(score_with_discount(0, 0).entry_discount, 0);
    }

    #[test]
    fn test_discount_10_percent() {
        // entry=370, terminal=411 -> discount_bps=997 -> 997/66=15
        assert_eq!(score_with_discount(370, 411).entry_discount, 15);
    }

    #[test]
    fn test_discount_5_percent() {
        // entry=390, terminal=411 -> discount_bps=510 -> 510/66=7
        assert_eq!(score_with_discount(390, 411).entry_discount, 7);
    }

    #[test]
    fn test_discount_3_percent() {
        // entry=399, terminal=411 -> discount_bps=291 -> 291/66=4
        assert_eq!(score_with_discount(399, 411).entry_discount, 4);
    }

    #[test]
    fn test_discount_1_percent() {
        // entry=407, terminal=411 -> discount_bps=97 -> 97/66=1
        assert_eq!(score_with_discount(407, 411).entry_discount, 1);
    }

    #[test]
    fn test_discount_large_values() {
        assert_eq!(score_with_discount(370_000_000, 411_000_000).entry_discount, 15);
    }

    // -- LP reserve component (0-10, NEW in v4) -------------------------------

    #[test]
    fn test_lp_reserve_too_thin() {
        // <50 SOL -> 0
        assert_eq!(score_with_reserve(0).lp_reserve, 0);
        assert_eq!(score_with_reserve(49_999_999_999).lp_reserve, 0);
    }

    #[test]
    fn test_lp_reserve_sweet_spot() {
        // 50-100 SOL -> 10
        assert_eq!(score_with_reserve(50_000_000_000).lp_reserve, 10);
        assert_eq!(score_with_reserve(85_000_000_000).lp_reserve, 10);
        assert_eq!(score_with_reserve(99_999_999_999).lp_reserve, 10);
    }

    #[test]
    fn test_lp_reserve_good() {
        // 100-200 SOL -> 8
        assert_eq!(score_with_reserve(100_000_000_000).lp_reserve, 8);
        assert_eq!(score_with_reserve(199_999_999_999).lp_reserve, 8);
    }

    #[test]
    fn test_lp_reserve_dampened() {
        // 200-500 SOL -> 4
        assert_eq!(score_with_reserve(200_000_000_000).lp_reserve, 4);
        assert_eq!(score_with_reserve(499_999_999_999).lp_reserve, 4);
    }

    #[test]
    fn test_lp_reserve_institutional() {
        // 500-2000 SOL -> 2
        assert_eq!(score_with_reserve(500_000_000_000).lp_reserve, 2);
        assert_eq!(score_with_reserve(1_999_999_999_999).lp_reserve, 2);
    }

    #[test]
    fn test_lp_reserve_market_making() {
        // >2000 SOL -> 0
        assert_eq!(score_with_reserve(2_000_000_000_000).lp_reserve, 0);
        assert_eq!(score_with_reserve(10_000_000_000_000).lp_reserve, 0);
    }

    // -- Integration / total score tests --------------------------------------

    #[test]
    fn test_total_theoretical_max_100() {
        // speed: 300s -> 20 (peak of inverted curve)
        // volume: 5_000 centisol (50 SOL) -> 25 (sweet spot)
        // velocity: 10*10_000/5_000 = 20 -> capped at 15
        // ratio: 10/2 = 5 -> 5*2 = 10
        // discount: 10% below -> 15
        // lp_reserve: 85 SOL -> 10
        let score = score_graduation(300, 5_000, 10, 2, 370, 411, DEFAULT_RESERVE);
        assert_eq!(score.speed, 20);
        assert_eq!(score.volume_tier, 25);
        assert_eq!(score.velocity, 15);
        assert_eq!(score.buy_sell_ratio, 10);
        assert_eq!(score.entry_discount, 15);
        assert_eq!(score.lp_reserve, 10);
        // Verify: velocity is capped at 15 (10*10000/5000=20 -> 15)
        assert_eq!(score.total(), 95);
    }

    #[test]
    fn test_total_true_max_100() {
        // To reach exactly 100:
        // speed=300 -> 20
        // volume=5_000 -> 25
        // velocity: need buys*10000/vol >= 15 but we also need ratio buys/sells >= 5
        //   buys=8, vol=5_000: 8*10000/5000 = 16 -> 15 ✓
        //   sells=1: 8/1=8, 8*2=16 -> 10 ✓
        // discount: 370/411 -> 15
        // lp_reserve: 85 SOL -> 10
        let score = score_graduation(300, 5_000, 8, 1, 370, 411, DEFAULT_RESERVE);
        assert_eq!(score.speed, 20);
        assert_eq!(score.volume_tier, 25);
        assert_eq!(score.velocity, 15);
        assert_eq!(score.buy_sell_ratio, 10);
        assert_eq!(score.entry_discount, 15);
        assert_eq!(score.lp_reserve, 10);
        // Theoretical max = 20 + 25 + 15 + 10 + 15 + 10 = 95
        // ... actually the theoretical max is 95 with these weights
        // because velocity caps at 15 and ratio caps at 10.
        // The spec says "max 100 pts total" but the sum of individual maxes is:
        // 20 + 25 + 15 + 10 + 15 + 10 = 95 from these 6 components.
        // Cold miss bonus (5 pts, applied externally) brings total to 100.
        assert_eq!(score.total(), 95);
    }

    #[test]
    fn test_total_zero_score() {
        let score = score_graduation(0, 0, 0, 0, 0, 0, 0);
        assert_eq!(score.total(), 0);
    }

    #[test]
    fn test_total_no_enrichment() {
        // speed=0s (bot), volume=0, buys=0, sells=0, but has discount + reserve
        let score = score_graduation(0, 0, 0, 0, 370, 411, DEFAULT_RESERVE);
        assert_eq!(score.speed, 0);
        assert_eq!(score.volume_tier, 0);
        assert_eq!(score.velocity, 0);
        assert_eq!(score.buy_sell_ratio, 0);
        assert_eq!(score.entry_discount, 15);
        assert_eq!(score.lp_reserve, 10);
        assert_eq!(score.total(), 25);
    }

    #[test]
    fn test_total_excluding_discount() {
        let score = score_graduation(60, 30_000, 10, 2, 390, 411, DEFAULT_RESERVE);
        let expected = score.speed + score.volume_tier + score.velocity + score.buy_sell_ratio + score.lp_reserve;
        assert_eq!(score.total_excluding_discount(), expected);
        assert!(score.total() > score.total_excluding_discount());
    }

    #[test]
    fn test_total_saturating_no_overflow() {
        let score = score_graduation(0, 1_000_000, 10_000, 0, 1, 1_000_000, 10_000_000_000_000);
        assert!(score.total() <= 100);
    }

    // -- Realistic scenario tests ---------------------------------------------

    #[test]
    fn test_scenario_hot_organic_graduation() {
        // Fast (30s) + medium volume (400 SOL) + 15 buys + 2 sells + 5% discount
        let score = score_graduation(30, 40_000, 15, 2, 390, 411, DEFAULT_RESERVE);
        assert_eq!(score.speed, 0);          // <=60s -> 0
        assert_eq!(score.volume_tier, 3);    // 400-655 SOL -> 3
        assert_eq!(score.velocity, 3);       // 15*10_000/40_000 = 3
        assert_eq!(score.buy_sell_ratio, 10); // 15/2=7, 7*2=14, cap 10
        assert_eq!(score.entry_discount, 7);
        assert_eq!(score.lp_reserve, 10);    // 85 SOL -> sweet spot
        assert_eq!(score.total(), 33);
    }

    #[test]
    fn test_scenario_whale_pump() {
        // Very fast (10s), huge volume (1000 SOL), few buys (3), no sells, at terminal
        let score = score_graduation(10, 100_000, 3, 0, 411, 411, DEFAULT_RESERVE);
        assert_eq!(score.speed, 0);          // <=60s -> 0
        assert_eq!(score.volume_tier, 0);    // >=655 SOL -> 0
        assert_eq!(score.velocity, 0);       // 3*10_000/100_000 = 0
        assert_eq!(score.buy_sell_ratio, 6); // 3/1=3, 3*2=6
        assert_eq!(score.entry_discount, 0);
        assert_eq!(score.lp_reserve, 10);
        assert_eq!(score.total(), 16);
    }

    #[test]
    fn test_scenario_distribution_dump() {
        // Medium speed (200s), medium volume (300 SOL), 5 buys, 10 sells, above terminal
        let score = score_graduation(200, 30_000, 5, 10, 450, 411, DEFAULT_RESERVE);
        // speed: 200s -> 16 + (200-180)*4/120 = 16 + 0 = 16
        assert_eq!(score.speed, 16);
        assert_eq!(score.volume_tier, 8);    // 200-400 SOL -> 8
        assert_eq!(score.velocity, 1);       // 5*10_000/30_000 = 1
        assert_eq!(score.buy_sell_ratio, 0); // 5/10=0
        assert_eq!(score.entry_discount, 0); // above terminal
        assert_eq!(score.lp_reserve, 10);
        assert_eq!(score.total(), 35);
    }

    // -- v4 Validation tests --------------------------------------------------

    #[test]
    fn test_scorer_v4_whale_pump_scores_low() {
        let score = score_graduation(60, 65_535, 3, 0, 411, 411, DEFAULT_RESERVE);
        assert_eq!(score.speed, 0);           // <=60s -> 0
        assert_eq!(score.volume_tier, 0);     // >=65_500 -> 0
        assert_eq!(score.velocity, 0);        // 3*10_000/65_535 = 0
        assert_eq!(score.buy_sell_ratio, 6);  // 3/1=3, 3*2=6
        assert_eq!(score.entry_discount, 0);  // at terminal
        assert_eq!(score.lp_reserve, 10);
        assert_eq!(score.total(), 16);
    }

    #[test]
    fn test_scorer_v4_organic_scores_high() {
        // speed=180, vol=7500 (75 SOL) -> speed=16, vol=25
        let score = score_graduation(180, 7_500, 15, 2, 390, 411, DEFAULT_RESERVE);
        assert_eq!(score.speed, 16);          // 180s -> 16
        assert_eq!(score.volume_tier, 25);    // 50-100 SOL -> 25 (sweet spot)
        assert_eq!(score.velocity, 15);       // 15*10_000/7_500 = 20 -> capped 15
        assert_eq!(score.buy_sell_ratio, 10); // 15/2=7, 7*2=14, cap 10
        assert_eq!(score.entry_discount, 7);  // 510bps/66 = 7
        assert_eq!(score.lp_reserve, 10);     // 85 SOL -> sweet spot
        assert_eq!(score.total(), 83);
        assert!(score.total() > 50);
    }

    #[test]
    fn test_scorer_v4_component_maxes() {
        // Verify individual component maxes
        assert_eq!(score_speed(300), 20);
        assert_eq!(score_volume_tier(5_000), 25);
        assert_eq!(score_lp_reserve(85_000_000_000), 10);
        // 20 + 25 + 15 + 10 + 15 + 10 = 95 (cold miss bonus adds 5 externally for 100)
        let theoretical_max: u8 = 20 + 25 + 15 + 10 + 15 + 10;
        assert_eq!(theoretical_max, 95);
    }
}