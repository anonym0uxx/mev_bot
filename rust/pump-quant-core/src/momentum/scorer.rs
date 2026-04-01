//! Integer-only graduation scorer for momentum engine (v3).
//!
//! Scores graduation events on five dimensions (sum 0-100):
//! - **Speed** (0-25): how slow the token graduated (slower = stronger post-grad momentum)
//! - **Volume tier** (0-15): total bonding curve volume in SOL, moderate = best
//! - **Velocity** (0-20): buy rate normalized by volume (organic demand signal)
//! - **Buy/sell ratio** (0-25): unidirectional buy pressure vs distribution
//! - **Entry discount** (0-15): structural edge from buying below BC terminal price
//!
//! ## Design Constraints
//!
//! - All integer arithmetic — no f64 anywhere
//! - `#[inline(always)]` on scoring functions (called from hot path)
//! - Inputs use centisol (volume x 100) and bps to avoid floating point
//!
//! ## v3 Changelog (speed/volume inversion)
//!
//! Backtesting proved v2 rewarded the wrong tokens:
//! - Speed<=60s: 7.3% WR (bot/whale fills). Speed>=120s: 41.1% WR (organic).
//! - Volume>=655 SOL: 5.9% WR (saturated). Volume 50-100 SOL: 39.6% WR (sweet spot).
//!
//! | Component      | v2  | v3  | Rationale                                       |
//! |----------------|-----|-----|-------------------------------------------------|
//! | Speed          | 15  | 25  | INVERTED: slow grad = organic, fast = bot/whale |
//! | Volume tier    | 10  | 15  | INVERTED: moderate vol = organic sweet spot      |
//! | Velocity       | 20  | 20  | Unchanged                                       |
//! | Buy/sell ratio | 25  | 25  | Unchanged                                       |
//! | Entry discount | 30  | 15  | Halved: was over-weighted vs other signals       |
//!
//! ## Call-site signature (unchanged from v2)
//!
//! ```rust,ignore
//! let score = score_graduation(
//!     grad_speed_s,           // u32: seconds from creation to graduation
//!     volume_sol_x100,        // u32: total BC volume in centisol (sol x 100)
//!     buys_last_5s,           // u32: buy txns in last 5s of BC
//!     sells_last_5s,          // u32: sell txns in last 5s of BC
//!     entry_price_fp,         // u64: entry price in fixed-point lamports/1M atoms
//!     bc_terminal_price_fp,   // u64: BC terminal price in fixed-point
//! );
//! ```

/// Score components (v3: 5 components, sum 0-100).
#[derive(Debug, Clone, Copy, Default)]
pub struct GraduationScore {
    /// Speed score: 0-25. Slow graduation = organic momentum (inverted from v2).
    pub speed: u8,
    /// Volume tier score: 0-15. Moderate volume = organic sweet spot (inverted from v2).
    pub volume_tier: u8,
    /// Velocity score: 0-20. Buy rate normalized by volume (organic demand).
    pub velocity: u8,
    /// Buy/sell ratio score: 0-25. Unidirectional pressure signal.
    pub buy_sell_ratio: u8,
    /// Entry discount score: 0-15. Buying below BC terminal = structural edge (halved from v2).
    pub entry_discount: u8,
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
    }

    /// Total score excluding entry_discount (used for pre-entry gate when
    /// entry price is not yet known). Sum of speed + volume_tier + velocity
    /// + buy_sell_ratio = max 85.
    #[inline(always)]
    pub fn total_excluding_discount(&self) -> u8 {
        self.speed
            .saturating_add(self.volume_tier)
            .saturating_add(self.velocity)
            .saturating_add(self.buy_sell_ratio)
    }
}

// -- Component scorers --------------------------------------------------------

/// Speed score (0-25). Inverted from v2: SLOWER graduation = HIGHER score.
/// Fast grads (<=60s) = bot/whale fills = 7.3% WR. Slow grads (>=120s) = 41.1% WR.
#[inline(always)]
fn score_speed(grad_speed_s: u32) -> u8 {
    if grad_speed_s <= 60 {
        0 // Bot/whale fill -- no post-grad momentum
    } else if grad_speed_s <= 90 {
        ((grad_speed_s.saturating_sub(60)) * 5 / 30).min(5) as u8
    } else if grad_speed_s <= 120 {
        (5 + (grad_speed_s.saturating_sub(90)) * 10 / 30).min(15) as u8
    } else if grad_speed_s <= 180 {
        (15 + (grad_speed_s.saturating_sub(120)) * 5 / 60).min(20) as u8
    } else if grad_speed_s <= 300 {
        (20 + (grad_speed_s.saturating_sub(180)) * 5 / 120).min(25) as u8
    } else {
        // Very slow (>300s): slight decline -- may lack discovery
        25u8.saturating_sub(((grad_speed_s.saturating_sub(300)) * 5 / 300).min(5) as u8)
    }
}

/// Volume tier score (0-15). Inverted from v2: MODERATE volume = HIGH score.
/// 50-100 SOL = sweet spot (39.6% WR). >=655 SOL saturated = 5.9% WR.
#[inline(always)]
fn score_volume_tier(volume_sol_x100: u32) -> u8 {
    if volume_sol_x100 >= 65_500 {
        0  // >=655 SOL saturated -- confirmed bot/whale
    } else if volume_sol_x100 >= 40_000 {
        2  // 400-655 SOL -- likely bot/whale
    } else if volume_sol_x100 >= 20_000 {
        5  // 200-400 SOL -- institutional, lower WR
    } else if volume_sol_x100 >= 10_000 {
        12 // 100-200 SOL -- good organic range
    } else if volume_sol_x100 >= 5_000 {
        15 // 50-100 SOL -- sweet spot
    } else if volume_sol_x100 >= 3_000 {
        5  // 30-50 SOL -- light activity
    } else {
        0  // <30 SOL -- insufficient
    }
}

/// Velocity score: 0-20.
///
/// Normalized buy rate: `buys_5s * 10_000 / max(volume_sol_x100, 1)`.
/// High velocity relative to volume = organic demand (many small buys),
/// not just a single whale deposit. Capped at 20.
#[inline(always)]
fn score_velocity(buys_last_5s: u32, volume_sol_x100: u32) -> u8 {
    let vol = volume_sol_x100.max(1);
    let normalized = buys_last_5s.saturating_mul(10_000) / vol;
    normalized.min(20) as u8
}

/// Buy/sell ratio score: 0-25.
///
/// `ratio = buys_5s / max(sells_5s, 1)`.
/// Linear: `min(ratio * 5, 25)`.
#[inline(always)]
fn score_buy_sell_ratio(buys_last_5s: u32, sells_last_5s: u32) -> u8 {
    let sells = sells_last_5s.max(1);
    let ratio = buys_last_5s / sells;
    (ratio.saturating_mul(5)).min(25) as u8
}

/// Entry discount score: 0-15 (halved from v2's 0-30).
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

/// Score a graduation event (v3). All integer arithmetic, no f64.
///
/// # Parameters
///
/// - `grad_speed_s`: seconds from token creation to graduation (0..=86400)
/// - `volume_sol_x100`: total bonding curve volume in centisol (sol x 100)
/// - `buys_last_5s`: buy transactions in the last 5 seconds of bonding curve
/// - `sells_last_5s`: sell transactions in the last 5 seconds of bonding curve
/// - `entry_price_fp`: entry price in fixed-point (lamports per 1M token atoms)
/// - `bc_terminal_price_fp`: bonding curve terminal price in fixed-point (~411)
///
/// # Returns
///
/// `GraduationScore` with 5 components summing to 0-100.
#[inline(always)]
pub fn score_graduation(
    grad_speed_s: u32,
    volume_sol_x100: u32,
    buys_last_5s: u32,
    sells_last_5s: u32,
    entry_price_fp: u64,
    bc_terminal_price_fp: u64,
) -> GraduationScore {
    GraduationScore {
        speed: score_speed(grad_speed_s),
        volume_tier: score_volume_tier(volume_sol_x100),
        velocity: score_velocity(buys_last_5s, volume_sol_x100),
        buy_sell_ratio: score_buy_sell_ratio(buys_last_5s, sells_last_5s),
        entry_discount: score_entry_discount(entry_price_fp, bc_terminal_price_fp),
    }
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Helpers ---------------------------------------------------------------

    fn score_with_speed(s: u32) -> GraduationScore {
        score_graduation(s, 0, 0, 0, 0, 0)
    }

    fn score_with_volume(centisol: u32) -> GraduationScore {
        score_graduation(3600, centisol, 0, 0, 0, 0)
    }

    fn score_with_velocity(buys: u32, volume_centisol: u32) -> GraduationScore {
        score_graduation(3600, volume_centisol, buys, 0, 0, 0)
    }

    fn score_with_ratio(buys: u32, sells: u32) -> GraduationScore {
        score_graduation(3600, 0, buys, sells, 0, 0)
    }

    fn score_with_discount(entry: u64, terminal: u64) -> GraduationScore {
        score_graduation(3600, 0, 0, 0, entry, terminal)
    }

    // -- Speed component (0-25, inverted) -------------------------------------

    #[test]
    fn test_speed_instant() {
        // 0s -> 0 (bot/whale fill)
        assert_eq!(score_with_speed(0).speed, 0);
    }

    #[test]
    fn test_speed_60s() {
        // 60s -> 0 (still too fast)
        assert_eq!(score_with_speed(60).speed, 0);
    }

    #[test]
    fn test_speed_90s() {
        // 90s -> (90-60)*5/30 = 5
        assert_eq!(score_with_speed(90).speed, 5);
    }

    #[test]
    fn test_speed_120s() {
        // 120s -> 5 + (120-90)*10/30 = 5 + 10 = 15
        assert_eq!(score_with_speed(120).speed, 15);
    }

    #[test]
    fn test_speed_150s() {
        // 150s -> 15 + (150-120)*5/60 = 15 + 2 = 17
        assert_eq!(score_with_speed(150).speed, 17);
    }

    #[test]
    fn test_speed_180s() {
        // 180s -> 15 + (180-120)*5/60 = 15 + 5 = 20
        assert_eq!(score_with_speed(180).speed, 20);
    }

    #[test]
    fn test_speed_240s() {
        // 240s -> 20 + (240-180)*5/120 = 20 + 2 = 22
        assert_eq!(score_with_speed(240).speed, 22);
    }

    #[test]
    fn test_speed_300s() {
        // 300s -> 20 + (300-180)*5/120 = 20 + 5 = 25
        assert_eq!(score_with_speed(300).speed, 25);
    }

    #[test]
    fn test_speed_slow() {
        // 3600s -> 25 - min((3600-300)*5/300, 5) = 25 - 5 = 20
        assert_eq!(score_with_speed(3600).speed, 20);
    }

    // -- Volume tier component (0-15, inverted) -------------------------------

    #[test]
    fn test_volume_tier_low() {
        // 50 SOL (5_000 centisol) -> sweet spot -> 15
        assert_eq!(score_with_volume(5_000).volume_tier, 15);
        // 0 -> insufficient -> 0
        assert_eq!(score_with_volume(0).volume_tier, 0);
        // 99.99 SOL (9_999 centisol) -> still sweet spot -> 15
        assert_eq!(score_with_volume(9_999).volume_tier, 15);
    }

    #[test]
    fn test_volume_tier_mid_low() {
        // 100 SOL -> good organic -> 12
        assert_eq!(score_with_volume(10_000).volume_tier, 12);
        // 200 SOL -> institutional -> 5
        assert_eq!(score_with_volume(20_000).volume_tier, 5);
        // 299.99 SOL -> institutional -> 5
        assert_eq!(score_with_volume(29_999).volume_tier, 5);
    }

    #[test]
    fn test_volume_tier_mid_high() {
        // 300 SOL -> institutional (200-400) -> 5
        assert_eq!(score_with_volume(30_000).volume_tier, 5);
        // 450 SOL -> likely bot/whale (400-655) -> 2
        assert_eq!(score_with_volume(45_000).volume_tier, 2);
        // 599.99 SOL -> likely bot/whale -> 2
        assert_eq!(score_with_volume(59_999).volume_tier, 2);
    }

    #[test]
    fn test_volume_tier_high() {
        // 600 SOL -> likely bot/whale (400-655) -> 2
        assert_eq!(score_with_volume(60_000).volume_tier, 2);
        // 1000 SOL -> saturated (>=655) -> 0
        assert_eq!(score_with_volume(100_000).volume_tier, 0);
    }

    // -- Velocity component (0-20) --------------------------------------------

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

        // 20 buys, 100 SOL = 20
        assert_eq!(score_with_velocity(20, 10_000).velocity, 20);
    }

    #[test]
    fn test_velocity_capped_at_20() {
        assert_eq!(score_with_velocity(50, 10_000).velocity, 20);
    }

    #[test]
    fn test_velocity_zero_volume() {
        assert_eq!(score_with_velocity(1, 0).velocity, 20);
    }

    #[test]
    fn test_velocity_high_organic() {
        assert_eq!(score_with_velocity(25, 5_000).velocity, 20);
        assert_eq!(score_with_velocity(25, 200_000).velocity, 1);
    }

    // -- Buy/sell ratio component (0-25) --------------------------------------

    #[test]
    fn test_ratio_zero_buys() {
        assert_eq!(score_with_ratio(0, 5).buy_sell_ratio, 0);
    }

    #[test]
    fn test_ratio_equal_pressure() {
        assert_eq!(score_with_ratio(5, 5).buy_sell_ratio, 5);
    }

    #[test]
    fn test_ratio_strong_buy() {
        assert_eq!(score_with_ratio(10, 2).buy_sell_ratio, 25);
    }

    #[test]
    fn test_ratio_capped_at_25() {
        assert_eq!(score_with_ratio(100, 1).buy_sell_ratio, 25);
    }

    #[test]
    fn test_ratio_zero_sells() {
        assert_eq!(score_with_ratio(10, 0).buy_sell_ratio, 25);
    }

    #[test]
    fn test_ratio_heavy_selling() {
        assert_eq!(score_with_ratio(2, 10).buy_sell_ratio, 0);
    }

    // -- Entry discount component (0-15, halved from v2) ----------------------

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
        // terminal=411_000_000, entry=370_000_000 -> discount_bps=997 -> 997/66=15
        assert_eq!(score_with_discount(370_000_000, 411_000_000).entry_discount, 15);
    }

    // -- Integration / total score tests --------------------------------------

    #[test]
    fn test_total_max_score() {
        // v3: speed 0s -> 0, volume 10_000 -> 12, velocity 20, ratio 25, discount 15
        let score = score_graduation(0, 10_000, 20, 1, 370, 411);
        assert_eq!(score.speed, 0);
        assert_eq!(score.volume_tier, 12);
        assert_eq!(score.velocity, 20);
        assert_eq!(score.buy_sell_ratio, 25);
        assert_eq!(score.entry_discount, 15);
        // total = 0 + 12 + 20 + 25 + 15 = 72

        // Higher volume (whale-level) scores worse in v3:
        let max_score = score_graduation(0, 60_000, 20, 1, 370, 411);
        assert_eq!(max_score.speed, 0);
        assert_eq!(max_score.volume_tier, 2); // 400-655 SOL
        assert_eq!(max_score.velocity, 3);    // 20*10_000/60_000 = 3
        assert_eq!(max_score.buy_sell_ratio, 25);
        assert_eq!(max_score.entry_discount, 15);
        // total = 0 + 2 + 3 + 25 + 15 = 45
    }

    #[test]
    fn test_total_theoretical_max_100() {
        // speed: 300s -> 25 (peak of inverted curve)
        // volume: 5_000 centisol (50 SOL) -> 15 (sweet spot)
        // velocity: 10*10_000/5_000 = 20 -> 20
        // ratio: 10/2 = 5 -> 5*5 = 25
        // discount: 10% below -> 15
        let score = score_graduation(300, 5_000, 10, 2, 370, 411);
        assert_eq!(score.speed, 25);
        assert_eq!(score.volume_tier, 15);
        assert_eq!(score.velocity, 20);
        assert_eq!(score.buy_sell_ratio, 25);
        assert_eq!(score.entry_discount, 15);
        assert_eq!(score.total(), 100);
    }

    #[test]
    fn test_total_zero_score() {
        // Bot/whale speed (<=60s) + insufficient volume + no buys + no discount
        let score = score_graduation(0, 0, 0, 0, 0, 0);
        assert_eq!(score.total(), 0);
    }

    #[test]
    fn test_total_no_enrichment() {
        // speed=0s (bot), volume=0, buys=0, sells=0, but has discount
        let score = score_graduation(0, 0, 0, 0, 370, 411);
        assert_eq!(score.speed, 0);
        assert_eq!(score.volume_tier, 0);
        assert_eq!(score.velocity, 0);
        assert_eq!(score.buy_sell_ratio, 0);
        assert_eq!(score.entry_discount, 15);
        assert_eq!(score.total(), 15);
    }

    #[test]
    fn test_total_excluding_discount() {
        let score = score_graduation(60, 30_000, 10, 2, 390, 411);
        let expected = score.speed + score.volume_tier + score.velocity + score.buy_sell_ratio;
        assert_eq!(score.total_excluding_discount(), expected);
        assert!(score.total() > score.total_excluding_discount());
    }

    #[test]
    fn test_total_saturating_no_overflow() {
        let score = score_graduation(0, 1_000_000, 10_000, 0, 1, 1_000_000);
        assert!(score.total() <= 100);
    }

    // -- Realistic scenario tests ---------------------------------------------

    #[test]
    fn test_scenario_hot_organic_graduation() {
        // Fast (30s) + medium volume (400 SOL) + 15 buys + 2 sells + 5% discount
        let score = score_graduation(30, 40_000, 15, 2, 390, 411);
        assert_eq!(score.speed, 0);          // <=60s -> 0 in v3
        assert_eq!(score.volume_tier, 2);    // 400-655 SOL -> 2 in v3
        assert_eq!(score.velocity, 3);       // 15*10_000/40_000 = 3
        assert_eq!(score.buy_sell_ratio, 25); // 15/2=7, 7*5=35, cap 25
        assert_eq!(score.entry_discount, 7);
        assert_eq!(score.total(), 37);
    }

    #[test]
    fn test_scenario_whale_pump() {
        // Very fast (10s), huge volume (1000 SOL), few buys (3), no sells, at terminal
        let score = score_graduation(10, 100_000, 3, 0, 411, 411);
        assert_eq!(score.speed, 0);          // <=60s -> 0 in v3
        assert_eq!(score.volume_tier, 0);    // >=655 SOL -> 0 in v3
        assert_eq!(score.velocity, 0);       // 3*10_000/100_000 = 0
        assert_eq!(score.buy_sell_ratio, 15); // 3/1=3, 3*5=15
        assert_eq!(score.entry_discount, 0);
        assert_eq!(score.total(), 15);
    }

    #[test]
    fn test_scenario_distribution_dump() {
        // Medium speed (200s), medium volume (300 SOL), 5 buys, 10 sells, above terminal
        let score = score_graduation(200, 30_000, 5, 10, 450, 411);
        // speed: 200s -> 20 + (200-180)*5/120 = 20 + 0 = 20
        assert_eq!(score.speed, 20);
        assert_eq!(score.volume_tier, 5);    // 200-400 SOL -> 5 in v3
        assert_eq!(score.velocity, 1);       // 5*10_000/30_000 = 1
        assert_eq!(score.buy_sell_ratio, 0); // 5/10=0
        assert_eq!(score.entry_discount, 0); // above terminal
        assert_eq!(score.total(), 26);
    }

    // -- v3 Validation tests --------------------------------------------------

    #[test]
    fn test_scorer_v3_whale_pump_scores_low() {
        // speed=60, vol=65535 (saturated) -> both = 0
        let score = score_graduation(60, 65_535, 3, 0, 411, 411);
        assert_eq!(score.speed, 0);           // <=60s -> 0
        assert_eq!(score.volume_tier, 0);     // >=65_500 -> 0
        assert_eq!(score.velocity, 0);        // 3*10_000/65_535 = 0
        assert_eq!(score.buy_sell_ratio, 15); // 3/1=3, 3*5=15
        assert_eq!(score.entry_discount, 0);  // at terminal
        assert_eq!(score.total(), 15);        // Only ratio contributes
    }

    #[test]
    fn test_scorer_v3_organic_scores_high() {
        // speed=180, vol=7500 (75 SOL) -> speed=20, vol=15
        let score = score_graduation(180, 7_500, 15, 2, 390, 411);
        assert_eq!(score.speed, 20);          // 180s -> 20
        assert_eq!(score.volume_tier, 15);    // 50-100 SOL -> 15 (sweet spot)
        assert_eq!(score.velocity, 20);       // 15*10_000/7_500 = 20
        assert_eq!(score.buy_sell_ratio, 25); // 15/2=7, 7*5=35, cap 25
        assert_eq!(score.entry_discount, 7);  // 510bps/66 = 7
        assert_eq!(score.total(), 87);
        assert!(score.total() > 50);
    }

    #[test]
    fn test_scorer_v3_total_max_is_100() {
        // Verify individual maxes
        assert_eq!(score_speed(300), 25);
        assert_eq!(score_volume_tier(5_000), 15);
        // 25 + 15 + 20 + 25 + 15 = 100
        let theoretical_max: u8 = 25 + 15 + 20 + 25 + 15;
        assert_eq!(theoretical_max, 100);

        // Also via actual call
        let score = score_graduation(300, 5_000, 10, 2, 370, 411);
        assert_eq!(score.total(), 100);
    }
}
