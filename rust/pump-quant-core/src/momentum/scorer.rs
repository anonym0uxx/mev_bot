//! Integer-only graduation scorer for momentum engine (v5).
//!
//! Scores graduation events on eight dimensions (sum 0-100):
//! - **Speed** (0-20): how slow the token graduated (slower = stronger post-grad momentum)
//! - **Volume tier** (0-20): total bonding curve volume in SOL, moderate = best
//! - **Velocity** (0-15): buy rate normalized by volume (organic demand signal)
//! - **Buy/sell ratio** (0-10): unidirectional buy pressure vs distribution (gated by min buys)
//! - **Entry discount** (0-10): structural edge from buying below BC terminal price
//! - **LP reserve size** (0-10): fresh pump.fun graduates land with 85-120 SOL sweet spot
//! - **Pre-entry momentum** (0-10): observed price velocity during observation window
//! - **Cold miss bonus**: omitted from struct (applied externally when enrichment is cold)
//!
//! ## Design Constraints
//!
//! - All integer arithmetic -- no f64 anywhere
//! - `#[inline(always)]` on scoring functions (called from hot path)
//! - Inputs use centisol (volume x 100) and bps to avoid floating point
//!
//! ## v5 Changelog (pre-entry momentum + buy/sell ratio gate)
//!
//! - Pre-entry momentum NEW at 10: observed price velocity before entry confirms organic demand
//! - Volume tier reduced 25->20: steal 5pts for new component, still strongest discriminator
//! - Entry discount reduced 15->10: AMM price dominates post-graduation anyway
//! - Buy/sell ratio gated: halved if buys_5s < min_buys_for_full_ratio_score (whale pump penalty)
//!
//! | Component          | v4  | v5  | Rationale                                       |
//! |--------------------|-----|-----|-------------------------------------------------|
//! | Speed              | 20  | 20  | Unchanged                                       |
//! | Volume tier        | 25  | 20  | Reduced -- 5pts moved to pre_entry_momentum     |
//! | Velocity           | 15  | 15  | Unchanged                                       |
//! | Buy/sell ratio     | 10  | 10  | Unchanged max, but gated by min buys            |
//! | Entry discount     | 15  | 10  | Reduced -- 5pts moved to pre_entry_momentum     |
//! | LP reserve         | 10  | 10  | Unchanged                                       |
//! | Pre-entry momentum |  0  | 10  | NEW: observed velocity before entry              |
//!
//! ## Call-site signature (v5: added velocity_bps_per_s + min_buys_for_full_ratio_score)
//!
//! ```rust,ignore
//! let score = score_graduation(
//!     grad_speed_s,                    // u32: seconds from creation to graduation
//!     volume_sol_x100,                 // u32: total BC volume in centisol (sol x 100)
//!     buys_last_5s,                    // u32: buy txns in last 5s of BC
//!     sells_last_5s,                   // u32: sell txns in last 5s of BC
//!     entry_price_fp,                  // u64: entry price in fixed-point lamports/1M atoms
//!     bc_terminal_price_fp,            // u64: BC terminal price in fixed-point
//!     reserve_sol_lamports,            // u64: LP pool SOL reserve in lamports
//!     velocity_bps_per_s,              // i64: observed price velocity (0 if not yet observed)
//!     min_buys_for_full_ratio_score,   // u32: min buys for full ratio score
//! );
//! ```

/// Score components (v5: 7 scored components + cold miss bonus, sum 0-100).
#[derive(Debug, Clone, Copy, Default)]
pub struct GraduationScore {
    /// Speed score: 0-20. Slow graduation = organic momentum (inverted from v2).
    pub speed: u8,
    /// Volume tier score: 0-20. Moderate volume = organic sweet spot.
    pub volume_tier: u8,
    /// Velocity score: 0-15. Buy rate normalized by volume (organic demand).
    pub velocity: u8,
    /// Buy/sell ratio score: 0-10. Unidirectional pressure signal (gated by min buys in v5).
    pub buy_sell_ratio: u8,
    /// Entry discount score: 0-10. Buying below BC terminal = structural edge.
    pub entry_discount: u8,
    /// LP reserve size score: 0-10. Fresh pump.fun graduates (85-120 SOL) = sweet spot.
    pub lp_reserve: u8,
    /// Cold miss bonus: 0-5. Applied externally when enrichment data was unavailable.
    /// Information asymmetry edge -- we're faster than enrichment-dependent bots.
    pub cold_miss_bonus: u8,
    /// Pre-entry momentum score: 0-10. Based on observed price velocity during
    /// observation window. 0 if not yet observed (scored at entry time, not graduation time).
    pub pre_entry_momentum: u8,
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
            .saturating_add(self.pre_entry_momentum)
    }

    /// Total score excluding entry_discount (used for pre-entry gate when
    /// entry price is not yet known). Sum of speed + volume_tier + velocity
    /// + buy_sell_ratio + lp_reserve + cold_miss_bonus + pre_entry_momentum = max 90.
    #[inline(always)]
    pub fn total_excluding_discount(&self) -> u8 {
        self.speed
            .saturating_add(self.volume_tier)
            .saturating_add(self.velocity)
            .saturating_add(self.buy_sell_ratio)
            .saturating_add(self.lp_reserve)
            .saturating_add(self.cold_miss_bonus)
            .saturating_add(self.pre_entry_momentum)
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

/// Volume tier score (0-20, v5: reduced from 0-25). Moderate volume = HIGH score.
/// 50-100 SOL = sweet spot.
#[inline(always)]
fn score_volume_tier(volume_sol_x100: u32) -> u8 {
    // 0-25 centisol (<0.25 SOL): 0
    if volume_sol_x100 < 25 { return 0; }
    // 25-1000 centisol (0.25-10 SOL): 2
    if volume_sol_x100 < 1_000 { return 2; }
    // 1000-3000 centisol (10-30 SOL): 5
    if volume_sol_x100 < 3_000 { return 5; }
    // 3000-5000 centisol (30-50 SOL): 10
    if volume_sol_x100 < 5_000 { return 10; }
    // 5000-10000 centisol (50-100 SOL): 20 <- SWEET SPOT (was 25)
    if volume_sol_x100 < 10_000 { return 20; }
    // 10000-20000 centisol (100-200 SOL): 12 (was 20)
    if volume_sol_x100 < 20_000 { return 12; }
    // 20000-40000 centisol (200-400 SOL): 6 (was 8)
    if volume_sol_x100 < 40_000 { return 6; }
    // 40000-65535 centisol (400-655 SOL): 2 (was 3)
    if volume_sol_x100 < 65_535 { return 2; }
    // >= 655 SOL: 0 (whale pump, no organic signal)
    0
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

/// Buy/sell ratio score: 0-10.
///
/// `ratio = buys_5s / max(sells_5s, 1)`.
/// Linear: `min(ratio * 2, 10)`.
#[inline(always)]
fn score_buy_sell_ratio(buys_last_5s: u32, sells_last_5s: u32) -> u8 {
    let sells = sells_last_5s.max(1);
    let ratio = buys_last_5s / sells;
    (ratio.saturating_mul(2)).min(10) as u8
}

/// Buy/sell ratio score with activity gate (v5).
/// If buys < min_for_full, ratio score is halved (whale pump penalty).
#[inline(always)]
fn score_buy_sell_ratio_gated(buys: u32, sells: u32, min_for_full: u32) -> u8 {
    let raw = score_buy_sell_ratio(buys, sells);
    if buys < min_for_full {
        raw / 2
    } else {
        raw
    }
}

/// Entry discount score: 0-10 (v5: reduced from 0-15).
///
/// `discount_bps = (bc_terminal - entry) * 10_000 / bc_terminal`
/// Linear scale: 0 bps=0, 1500+ bps=10.
///
/// If entry >= terminal (premium), score = 0.
#[inline(always)]
fn score_entry_discount(entry_price_fp: u64, bc_terminal_price_fp: u64) -> u8 {
    if bc_terminal_price_fp == 0 || entry_price_fp == 0 {
        return 0;
    }
    if entry_price_fp >= bc_terminal_price_fp {
        return 0; // At or above terminal -- no discount
    }
    // discount_bps = (terminal - entry) / terminal * 10000
    let discount_bps = ((bc_terminal_price_fp - entry_price_fp) as u128 * 10_000
        / bc_terminal_price_fp as u128) as u32;
    // Linear scale: 0 bps=0, 1500+ bps=10 (was 15)
    if discount_bps >= 1_500 { return 10; }
    (discount_bps * 10 / 1_500).min(10) as u8
}

/// LP reserve size score (0-10). Fresh pump.fun graduates land with 85-120 SOL.
/// Smaller pools are more volatile and momentum-tradeable.
/// Very large pools (Raydium majors) dampen momentum -- skip.
#[inline(always)]
fn score_lp_reserve(reserve_sol_lamports: u64) -> u8 {
    let sol = reserve_sol_lamports / 1_000_000_000;
    if sol < 50 {
        0  // too thin
    } else if sol < 100 {
        10 // 50-100 SOL <- sweet spot
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

/// Pre-entry momentum score (0-10). Based on observed price velocity (bps/s)
/// during observation window.
///
/// Sweet spot: 51-300 bps/s = strong organic demand visible before entry.
/// > 300 bps/s = possible spike top, score reduced to 5 (still enter but cautious).
/// <= 0 = flat/declining = 0 (no penalty, just no bonus).
#[inline(always)]
pub fn score_pre_entry_momentum(velocity_bps_per_s: i64) -> u8 {
    if velocity_bps_per_s <= 0 {
        0
    } else if velocity_bps_per_s <= 50 {
        2
    } else if velocity_bps_per_s <= 150 {
        7
    } else if velocity_bps_per_s <= 300 {
        10
    } else {
        5 // Too fast -- spike revert risk, partial credit
    }
}

/// Score a graduation event (v5). All integer arithmetic, no f64.
///
/// # Parameters
///
/// - `grad_speed_s`: seconds from token creation to graduation (0..=86400)
/// - `volume_sol_x100`: total bonding curve volume in centisol (sol x 100)
/// - `buys_last_5s`: buy transactions in the last 5 seconds of bonding curve
/// - `sells_last_5s`: sell transactions in the last 5 seconds of bonding curve
/// - `entry_price_fp`: entry price in fixed-point (lamports per 1M token atoms)
/// - `bc_terminal_price_fp`: bonding curve terminal price in fixed-point (~411)
/// - `reserve_sol_lamports`: LP pool SOL reserve in lamports
/// - `velocity_bps_per_s`: observed price velocity in bps/s (0 if not yet observed)
/// - `min_buys_for_full_ratio_score`: min buys_5s for full buy/sell ratio score
///
/// # Returns
///
/// `GraduationScore` with 7 components summing to 0-100.
#[inline(always)]
pub fn score_graduation(
    grad_speed_s: u32,
    volume_sol_x100: u32,
    buys_last_5s: u32,
    sells_last_5s: u32,
    entry_price_fp: u64,
    bc_terminal_price_fp: u64,
    reserve_sol_lamports: u64,
    velocity_bps_per_s: i64,
    min_buys_for_full_ratio_score: u32,
) -> GraduationScore {
    GraduationScore {
        speed: score_speed(grad_speed_s),
        volume_tier: score_volume_tier(volume_sol_x100),
        velocity: score_velocity(buys_last_5s, volume_sol_x100),
        buy_sell_ratio: score_buy_sell_ratio_gated(buys_last_5s, sells_last_5s, min_buys_for_full_ratio_score),
        entry_discount: score_entry_discount(entry_price_fp, bc_terminal_price_fp),
        lp_reserve: score_lp_reserve(reserve_sol_lamports),
        cold_miss_bonus: 0, // Applied post-scoring in mod.rs when enrichment data unavailable
        pre_entry_momentum: score_pre_entry_momentum(velocity_bps_per_s),
    }
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Helpers ---------------------------------------------------------------

    /// Default reserve: 85 SOL (typical pump.fun graduation) = 85_000_000_000 lamports
    const DEFAULT_RESERVE: u64 = 85_000_000_000;
    /// Default min buys for full ratio score
    const DEFAULT_MIN_BUYS: u32 = 5;

    fn score_with_speed(s: u32) -> GraduationScore {
        score_graduation(s, 0, 0, 0, 0, 0, DEFAULT_RESERVE, 0, DEFAULT_MIN_BUYS)
    }

    fn score_with_volume(centisol: u32) -> GraduationScore {
        score_graduation(3600, centisol, 0, 0, 0, 0, DEFAULT_RESERVE, 0, DEFAULT_MIN_BUYS)
    }

    fn score_with_velocity(buys: u32, volume_centisol: u32) -> GraduationScore {
        score_graduation(3600, volume_centisol, buys, 0, 0, 0, DEFAULT_RESERVE, 0, DEFAULT_MIN_BUYS)
    }

    fn score_with_ratio(buys: u32, sells: u32) -> GraduationScore {
        score_graduation(3600, 0, buys, sells, 0, 0, DEFAULT_RESERVE, 0, DEFAULT_MIN_BUYS)
    }

    fn score_with_discount(entry: u64, terminal: u64) -> GraduationScore {
        score_graduation(3600, 0, 0, 0, entry, terminal, DEFAULT_RESERVE, 0, DEFAULT_MIN_BUYS)
    }

    fn score_with_reserve(lamports: u64) -> GraduationScore {
        score_graduation(3600, 0, 0, 0, 0, 0, lamports, 0, DEFAULT_MIN_BUYS)
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

    // -- Volume tier component (0-20, v5) -------------------------------------

    #[test]
    fn test_volume_tier_sweet_spot() {
        // 50 SOL (5_000 centisol) -> sweet spot -> 20
        assert_eq!(score_with_volume(5_000).volume_tier, 20);
        // 99.99 SOL (9_999 centisol) -> still sweet spot -> 20
        assert_eq!(score_with_volume(9_999).volume_tier, 20);
    }

    #[test]
    fn test_volume_tier_insufficient() {
        assert_eq!(score_with_volume(0).volume_tier, 0);
        assert_eq!(score_with_volume(24).volume_tier, 0);
    }

    #[test]
    fn test_volume_tier_tiny() {
        // 0.25-10 SOL -> 2
        assert_eq!(score_with_volume(25).volume_tier, 2);
        assert_eq!(score_with_volume(999).volume_tier, 2);
    }

    #[test]
    fn test_volume_tier_small() {
        // 10-30 SOL -> 5
        assert_eq!(score_with_volume(1_000).volume_tier, 5);
        assert_eq!(score_with_volume(2_999).volume_tier, 5);
    }

    #[test]
    fn test_volume_tier_light() {
        // 30-50 SOL -> 10
        assert_eq!(score_with_volume(3_000).volume_tier, 10);
        assert_eq!(score_with_volume(4_999).volume_tier, 10);
    }

    #[test]
    fn test_volume_tier_good_organic() {
        // 100-200 SOL -> 12
        assert_eq!(score_with_volume(10_000).volume_tier, 12);
        assert_eq!(score_with_volume(19_999).volume_tier, 12);
    }

    #[test]
    fn test_volume_tier_institutional() {
        // 200-400 SOL -> 6
        assert_eq!(score_with_volume(20_000).volume_tier, 6);
        assert_eq!(score_with_volume(30_000).volume_tier, 6);
    }

    #[test]
    fn test_volume_tier_likely_bot() {
        // 400-655 SOL -> 2
        assert_eq!(score_with_volume(40_000).volume_tier, 2);
        assert_eq!(score_with_volume(60_000).volume_tier, 2);
    }

    #[test]
    fn test_volume_tier_confirmed_bot() {
        // >=655 SOL -> 0
        assert_eq!(score_with_volume(65_535).volume_tier, 0);
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

    // -- Buy/sell ratio component (0-10, gated in v5) -------------------------

    #[test]
    fn test_ratio_zero_buys() {
        assert_eq!(score_with_ratio(0, 5).buy_sell_ratio, 0);
    }

    #[test]
    fn test_ratio_equal_pressure() {
        // 5/5=1, 1*2=2, buys=5 >= min_buys=5 -> full score
        assert_eq!(score_with_ratio(5, 5).buy_sell_ratio, 2);
    }

    #[test]
    fn test_ratio_strong_buy() {
        // 10/2=5, 5*2=10 (capped), buys=10 >= 5 -> full
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

    #[test]
    fn test_ratio_gated_low_buys() {
        // 3 buys (< min_buys=5), 0 sells: raw = 3/1*2 = 6, gated = 6/2 = 3
        assert_eq!(score_with_ratio(3, 0).buy_sell_ratio, 3);
    }

    #[test]
    fn test_ratio_gated_boundary() {
        // 4 buys (< 5): raw = 4/1*2 = 8, gated = 8/2 = 4
        assert_eq!(score_with_ratio(4, 0).buy_sell_ratio, 4);
        // 5 buys (= 5): raw = 5/1*2 = 10, full score
        assert_eq!(score_with_ratio(5, 0).buy_sell_ratio, 10);
    }

    // -- Entry discount component (0-10, v5: reduced from 0-15) ---------------

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
    fn test_discount_large_discount() {
        // entry=370, terminal=411 -> discount_bps = (411-370)*10000/411 = 997
        // 997 * 10 / 1500 = 6
        assert_eq!(score_with_discount(370, 411).entry_discount, 6);
    }

    #[test]
    fn test_discount_5_percent() {
        // entry=390, terminal=411 -> discount_bps = (411-390)*10000/411 = 510
        // 510 * 10 / 1500 = 3
        assert_eq!(score_with_discount(390, 411).entry_discount, 3);
    }

    #[test]
    fn test_discount_3_percent() {
        // entry=399, terminal=411 -> discount_bps = (411-399)*10000/411 = 291
        // 291 * 10 / 1500 = 1
        assert_eq!(score_with_discount(399, 411).entry_discount, 1);
    }

    #[test]
    fn test_discount_1_percent() {
        // entry=407, terminal=411 -> discount_bps = (411-407)*10000/411 = 97
        // 97 * 10 / 1500 = 0
        assert_eq!(score_with_discount(407, 411).entry_discount, 0);
    }

    #[test]
    fn test_discount_max_10() {
        // entry=200, terminal=411 -> discount_bps = (411-200)*10000/411 = 5133
        // >= 1500 -> 10
        assert_eq!(score_with_discount(200, 411).entry_discount, 10);
    }

    #[test]
    fn test_discount_large_values() {
        // Large fixed-point values: same ratio
        // entry=370_000_000, terminal=411_000_000 -> discount_bps=997 -> 997*10/1500=6
        assert_eq!(score_with_discount(370_000_000, 411_000_000).entry_discount, 6);
    }

    // -- LP reserve component (0-10) ------------------------------------------

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

    // -- Pre-entry momentum component (0-10, NEW in v5) -----------------------

    #[test]
    fn test_pre_entry_momentum_negative() {
        assert_eq!(score_pre_entry_momentum(-100), 0);
        assert_eq!(score_pre_entry_momentum(-1), 0);
    }

    #[test]
    fn test_pre_entry_momentum_zero() {
        assert_eq!(score_pre_entry_momentum(0), 0);
    }

    #[test]
    fn test_pre_entry_momentum_slow() {
        // 1-50 bps/s -> 2
        assert_eq!(score_pre_entry_momentum(1), 2);
        assert_eq!(score_pre_entry_momentum(50), 2);
    }

    #[test]
    fn test_pre_entry_momentum_moderate() {
        // 51-150 bps/s -> 7
        assert_eq!(score_pre_entry_momentum(51), 7);
        assert_eq!(score_pre_entry_momentum(150), 7);
    }

    #[test]
    fn test_pre_entry_momentum_strong() {
        // 151-300 bps/s -> 10
        assert_eq!(score_pre_entry_momentum(151), 10);
        assert_eq!(score_pre_entry_momentum(300), 10);
    }

    #[test]
    fn test_pre_entry_momentum_spike() {
        // > 300 bps/s -> 5 (spike risk, partial credit)
        assert_eq!(score_pre_entry_momentum(301), 5);
        assert_eq!(score_pre_entry_momentum(1000), 5);
    }

    // -- Integration / total score tests --------------------------------------

    #[test]
    fn test_total_theoretical_max_100() {
        // speed: 300s -> 20
        // volume: 5_000 centisol (50 SOL) -> 20 (was 25)
        // velocity: 10*10_000/5_000 = 20 -> capped at 15
        // ratio: 10/2=5, 5*2=10 (buys=10 >= 5 -> full)
        // discount: entry=200, terminal=411 -> >=1500bps -> 10
        // lp_reserve: 85 SOL -> 10
        // pre_entry_momentum: 200 bps/s -> 10
        let score = score_graduation(300, 5_000, 10, 2, 200, 411, DEFAULT_RESERVE, 200, DEFAULT_MIN_BUYS);
        assert_eq!(score.speed, 20);
        assert_eq!(score.volume_tier, 20);
        assert_eq!(score.velocity, 15);
        assert_eq!(score.buy_sell_ratio, 10);
        assert_eq!(score.entry_discount, 10);
        assert_eq!(score.lp_reserve, 10);
        assert_eq!(score.pre_entry_momentum, 10);
        // 20 + 20 + 15 + 10 + 10 + 10 + 10 = 95
        assert_eq!(score.total(), 95);
    }

    #[test]
    fn test_total_true_max_100() {
        // Same as above but with cold_miss_bonus = 5 applied externally
        // 20 + 20 + 15 + 10 + 10 + 10 + 10 = 95, + 5 cold_miss = 100
        let mut score = score_graduation(300, 5_000, 8, 1, 200, 411, DEFAULT_RESERVE, 200, DEFAULT_MIN_BUYS);
        assert_eq!(score.speed, 20);
        assert_eq!(score.volume_tier, 20);
        assert_eq!(score.velocity, 15);
        assert_eq!(score.buy_sell_ratio, 10);
        assert_eq!(score.entry_discount, 10);
        assert_eq!(score.lp_reserve, 10);
        assert_eq!(score.pre_entry_momentum, 10);
        assert_eq!(score.total(), 95);
        score.cold_miss_bonus = 5;
        assert_eq!(score.total(), 100);
    }

    #[test]
    fn test_total_zero_score() {
        let score = score_graduation(0, 0, 0, 0, 0, 0, 0, 0, DEFAULT_MIN_BUYS);
        assert_eq!(score.total(), 0);
    }

    #[test]
    fn test_total_no_enrichment() {
        // speed=0s (bot), volume=0, buys=0, sells=0, but has discount + reserve
        // entry=200, terminal=411 -> discount >= 1500bps -> 10
        let score = score_graduation(0, 0, 0, 0, 200, 411, DEFAULT_RESERVE, 0, DEFAULT_MIN_BUYS);
        assert_eq!(score.speed, 0);
        assert_eq!(score.volume_tier, 0);
        assert_eq!(score.velocity, 0);
        assert_eq!(score.buy_sell_ratio, 0);
        assert_eq!(score.entry_discount, 10);
        assert_eq!(score.lp_reserve, 10);
        assert_eq!(score.pre_entry_momentum, 0);
        assert_eq!(score.total(), 20);
    }

    #[test]
    fn test_total_excluding_discount() {
        let score = score_graduation(60, 30_000, 10, 2, 390, 411, DEFAULT_RESERVE, 0, DEFAULT_MIN_BUYS);
        let expected = score.speed + score.volume_tier + score.velocity + score.buy_sell_ratio + score.lp_reserve + score.pre_entry_momentum;
        assert_eq!(score.total_excluding_discount(), expected);
        assert!(score.total() > score.total_excluding_discount());
    }

    #[test]
    fn test_total_saturating_no_overflow() {
        let score = score_graduation(0, 1_000_000, 10_000, 0, 1, 1_000_000, 10_000_000_000_000, 500, DEFAULT_MIN_BUYS);
        assert!(score.total() <= 100);
    }

    // -- Realistic scenario tests ---------------------------------------------

    #[test]
    fn test_scenario_hot_organic_graduation() {
        // Fast (30s) + medium-high volume (400 SOL) + 15 buys + 2 sells + 5% discount
        let score = score_graduation(30, 40_000, 15, 2, 390, 411, DEFAULT_RESERVE, 0, DEFAULT_MIN_BUYS);
        assert_eq!(score.speed, 0);          // <=60s -> 0
        assert_eq!(score.volume_tier, 2);    // 40_000 centisol = 400 SOL, NOT < 40_000 -> falls to 400-655 SOL bucket -> 2
        assert_eq!(score.velocity, 3);       // 15*10_000/40_000 = 3
        assert_eq!(score.buy_sell_ratio, 10); // 15/2=7, 7*2=14, cap 10 (buys=15 >= 5)
        assert_eq!(score.entry_discount, 3); // 510bps * 10/1500 = 3
        assert_eq!(score.lp_reserve, 10);    // 85 SOL -> sweet spot
        assert_eq!(score.pre_entry_momentum, 0); // velocity=0
        assert_eq!(score.total(), 28);       // 0+2+3+10+3+10+0 = 28
    }

    #[test]
    fn test_scenario_whale_pump() {
        // Very fast (10s), huge volume (1000 SOL), few buys (3), no sells, at terminal
        let score = score_graduation(10, 100_000, 3, 0, 411, 411, DEFAULT_RESERVE, 0, DEFAULT_MIN_BUYS);
        assert_eq!(score.speed, 0);          // <=60s -> 0
        assert_eq!(score.volume_tier, 0);    // >=655 SOL -> 0
        assert_eq!(score.velocity, 0);       // 3*10_000/100_000 = 0
        // 3 buys < 5 min_buys: raw=3/1*2=6, gated=6/2=3
        assert_eq!(score.buy_sell_ratio, 3);
        assert_eq!(score.entry_discount, 0);
        assert_eq!(score.lp_reserve, 10);
        assert_eq!(score.pre_entry_momentum, 0);
        assert_eq!(score.total(), 13);
    }

    #[test]
    fn test_scenario_distribution_dump() {
        // Medium speed (200s), medium volume (300 SOL), 5 buys, 10 sells, above terminal
        let score = score_graduation(200, 30_000, 5, 10, 450, 411, DEFAULT_RESERVE, 0, DEFAULT_MIN_BUYS);
        // speed: 200s -> 16 + (200-180)*4/120 = 16 + 0 = 16
        assert_eq!(score.speed, 16);
        assert_eq!(score.volume_tier, 6);    // 200-400 SOL -> 6
        assert_eq!(score.velocity, 1);       // 5*10_000/30_000 = 1
        assert_eq!(score.buy_sell_ratio, 0); // 5/10=0
        assert_eq!(score.entry_discount, 0); // above terminal
        assert_eq!(score.lp_reserve, 10);
        assert_eq!(score.pre_entry_momentum, 0);
        assert_eq!(score.total(), 33);
    }

    // -- v5 Validation tests --------------------------------------------------

    #[test]
    fn test_scorer_v5_whale_pump_scores_low() {
        let score = score_graduation(60, 65_535, 3, 0, 411, 411, DEFAULT_RESERVE, 0, DEFAULT_MIN_BUYS);
        assert_eq!(score.speed, 0);           // <=60s -> 0
        assert_eq!(score.volume_tier, 0);     // >=65535 -> 0
        assert_eq!(score.velocity, 0);        // 3*10_000/65_535 = 0
        // 3 buys < 5: raw=6, gated=3
        assert_eq!(score.buy_sell_ratio, 3);
        assert_eq!(score.entry_discount, 0);  // at terminal
        assert_eq!(score.lp_reserve, 10);
        assert_eq!(score.pre_entry_momentum, 0);
        assert_eq!(score.total(), 13);
    }

    #[test]
    fn test_scorer_v5_organic_scores_high() {
        // speed=180, vol=7500 (75 SOL), velocity=200bps/s
        let score = score_graduation(180, 7_500, 15, 2, 390, 411, DEFAULT_RESERVE, 200, DEFAULT_MIN_BUYS);
        assert_eq!(score.speed, 16);          // 180s -> 16
        assert_eq!(score.volume_tier, 20);    // 50-100 SOL -> 20 (sweet spot)
        assert_eq!(score.velocity, 15);       // 15*10_000/7_500 = 20 -> capped 15
        assert_eq!(score.buy_sell_ratio, 10); // 15/2=7, 7*2=14, cap 10 (buys=15 >= 5)
        assert_eq!(score.entry_discount, 3);  // 510bps * 10/1500 = 3
        assert_eq!(score.lp_reserve, 10);     // 85 SOL -> sweet spot
        assert_eq!(score.pre_entry_momentum, 10); // 200 bps/s -> 10
        assert_eq!(score.total(), 84);
        assert!(score.total() > 50);
    }

    #[test]
    fn test_scorer_v5_component_maxes() {
        // Verify individual component maxes
        assert_eq!(score_speed(300), 20);
        assert_eq!(score_volume_tier(5_000), 20);
        assert_eq!(score_lp_reserve(85_000_000_000), 10);
        assert_eq!(score_pre_entry_momentum(200), 10);
        // 20 + 20 + 15 + 10 + 10 + 10 + 10 = 95 (cold miss bonus adds 5 externally for 100)
        let theoretical_max: u8 = 20 + 20 + 15 + 10 + 10 + 10 + 10;
        assert_eq!(theoretical_max, 95);
    }

    #[test]
    fn test_pre_entry_momentum_in_score() {
        // Test that velocity flows through score_graduation correctly
        let score_no_vel = score_graduation(180, 7_500, 15, 2, 0, 0, DEFAULT_RESERVE, 0, DEFAULT_MIN_BUYS);
        let score_with_vel = score_graduation(180, 7_500, 15, 2, 0, 0, DEFAULT_RESERVE, 200, DEFAULT_MIN_BUYS);
        assert_eq!(score_no_vel.pre_entry_momentum, 0);
        assert_eq!(score_with_vel.pre_entry_momentum, 10);
        assert_eq!(score_with_vel.total() - score_no_vel.total(), 10);
    }
}
