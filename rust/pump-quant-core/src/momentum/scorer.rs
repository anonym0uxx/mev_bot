//! Integer-only graduation scorer for momentum engine (v2).
//!
//! Scores graduation events on five dimensions (sum 0-100):
//! - **Speed** (0-15): how fast the token graduated (faster = stronger momentum)
//! - **Volume tier** (0-10): total bonding curve volume in SOL, tiered buckets
//! - **Velocity** (0-20): buy rate normalized by volume (organic demand signal)
//! - **Buy/sell ratio** (0-25): unidirectional buy pressure vs distribution
//! - **Entry discount** (0-30): structural edge from buying below BC terminal price
//!
//! ## Design Constraints
//!
//! - All integer arithmetic — no f64 anywhere
//! - `#[inline(always)]` on scoring functions (called from hot path)
//! - Inputs use centisol (volume × 100) and bps to avoid floating point
//!
//! ## v2 Changelog (scorer overhaul)
//!
//! Replaced the 4×25 model (speed/volume/velocity/recovery) with a 5-component
//! weighted model that better discriminates winners from losers:
//!
//! | Component      | Old | New | Rationale                                      |
//! |----------------|-----|-----|------------------------------------------------|
//! | Speed          | 25  | 15  | Still useful, but was over-weighted             |
//! | Volume tier    | 25  | 10  | Tiered instead of linear (was always maxed)     |
//! | Velocity       | 25  | 20  | Normalized per SOL (raw was near-zero variance) |
//! | Buy/sell ratio | —   | 25  | NEW: detects distribution vs accumulation       |
//! | Entry discount | —   | 30  | Replaces recovery; computable without WS data   |
//! | Recovery       | 25  | —   | REMOVED: required WS data that 89% lack         |
//!
//! ## New call-site signature
//!
//! ```rust,ignore
//! // In mod.rs on_graduation():
//! let score = score_graduation(
//!     grad_speed_s,           // u32: seconds from creation to graduation
//!     volume_sol_x100,        // u32: total BC volume in centisol (sol × 100)
//!     buys_last_5s,           // u32: buy txns in last 5s of BC
//!     sells_last_5s,          // u32: sell txns in last 5s of BC (NEW)
//!     entry_price_fp,         // u64: entry price in fixed-point lamports/1M atoms (NEW)
//!     bc_terminal_price_fp,   // u64: BC terminal price in fixed-point (NEW)
//! );
//! ```
//!
//! The `recovery_score_from_prices()` function is **removed** — entry discount
//! is now computed inline as part of `score_graduation()`.

/// Score components (v2: 5 components, sum 0-100).
#[derive(Debug, Clone, Copy, Default)]
pub struct GraduationScore {
    /// Speed score: 0-15. Fast graduation = strong buy momentum.
    pub speed: u8,
    /// Volume tier score: 0-10. Tiered bonding curve volume.
    pub volume_tier: u8,
    /// Velocity score: 0-20. Buy rate normalized by volume (organic demand).
    pub velocity: u8,
    /// Buy/sell ratio score: 0-25. Unidirectional pressure signal.
    pub buy_sell_ratio: u8,
    /// Entry discount score: 0-30. Buying below BC terminal = structural edge.
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
    /// + buy_sell_ratio = max 70.
    #[inline(always)]
    pub fn total_excluding_discount(&self) -> u8 {
        self.speed
            .saturating_add(self.volume_tier)
            .saturating_add(self.velocity)
            .saturating_add(self.buy_sell_ratio)
    }
}

// ── Component scorers ────────────────────────────────────────────────────────

/// Speed score: 0-15.
///
/// Piecewise linear with inflection points from the spec:
/// - 0s → 15  (instant graduation, max signal)
/// - 60s → 15 (anything under 60s gets full marks)
/// - 120s → 10
/// - 180s → 5
/// - 300s+ → 0
///
/// Between inflection points we interpolate linearly in integer math.
/// For the 60-300s range: score = (300 - clamped_speed) / 16
/// (300-60)/16=15, (300-120)/16=11≈10, (300-180)/16=7≈5, (300-300)/16=0
///
/// To hit the exact spec values we use a piecewise approach:
/// - [0, 60] → 15
/// - (60, 120] → 10 + (120 - s) * 5 / 60  (linear 10→15)
/// - (120, 180] → 5 + (180 - s) * 5 / 60   (linear 5→10)
/// - (180, 300] → (300 - s) * 5 / 120       (linear 0→5)
/// - (300, ∞) → 0
#[inline(always)]
fn score_speed(grad_speed_s: u32) -> u8 {
    if grad_speed_s <= 60 {
        15
    } else if grad_speed_s <= 120 {
        // Linear 15 → 10 over [60, 120]
        (10 + (120u32.saturating_sub(grad_speed_s)) * 5 / 60).min(15) as u8
    } else if grad_speed_s <= 180 {
        // Linear 10 → 5 over [120, 180]
        (5 + (180u32.saturating_sub(grad_speed_s)) * 5 / 60).min(10) as u8
    } else if grad_speed_s <= 300 {
        // Linear 5 → 0 over [180, 300]
        (300u32.saturating_sub(grad_speed_s) * 5 / 120).min(5) as u8
    } else {
        0
    }
}

/// Volume tier score: 0-10.
///
/// Tiered buckets instead of linear scaling (old linear was always maxed at
/// 25/25 since avg volume = 539 SOL):
///
/// | Volume (SOL) | centisol range | Score |
/// |-------------|----------------|-------|
/// | < 100       | < 10_000       | 0     |
/// | 100 – 300   | 10_000-30_000  | 4     |
/// | 300 – 600   | 30_000-60_000  | 7     |
/// | 600+        | ≥ 60_000       | 10    |
#[inline(always)]
fn score_volume_tier(volume_sol_x100: u32) -> u8 {
    if volume_sol_x100 >= 60_000 {
        10
    } else if volume_sol_x100 >= 30_000 {
        7
    } else if volume_sol_x100 >= 10_000 {
        4
    } else {
        0
    }
}

/// Velocity score: 0-20.
///
/// Normalized buy rate: `buys_5s * 100 / max(volume_sol, 1)`.
/// High velocity relative to volume = organic demand (many small buys),
/// not just a single whale deposit.
///
/// `volume_sol_x100` is centisol, so volume_sol = volume_sol_x100 / 100.
/// Formula: `buys_5s * 100 / max(volume_sol_x100 / 100, 1)` =
///          `buys_5s * 10_000 / max(volume_sol_x100, 1)`.
/// Capped at 20.
#[inline(always)]
fn score_velocity(buys_last_5s: u32, volume_sol_x100: u32) -> u8 {
    let vol = volume_sol_x100.max(1);
    let normalized = buys_last_5s.saturating_mul(10_000) / vol;
    normalized.min(20) as u8
}

/// Buy/sell ratio score: 0-25.
///
/// `ratio = buys_5s / max(sells_5s, 1)`.
/// - ratio 0 → 0 (no buys)
/// - ratio 1 → 5  (equal pressure)
/// - ratio 2 → 10
/// - ratio 3 → 15
/// - ratio 5+ → 25 (strong unidirectional buying)
///
/// Linear: `min(ratio * 5, 25)`.
#[inline(always)]
fn score_buy_sell_ratio(buys_last_5s: u32, sells_last_5s: u32) -> u8 {
    let sells = sells_last_5s.max(1);
    let ratio = buys_last_5s / sells;
    (ratio.saturating_mul(5)).min(25) as u8
}

/// Entry discount score: 0-30.
///
/// How far below BC terminal price we're buying. Buying below terminal =
/// structural edge since the market hasn't fully priced in the graduation.
///
/// `discount_bps = (bc_terminal - entry) * 10_000 / bc_terminal`
///
/// Scoring: discount_bps / 33, capped at 30.
/// - 0 bps discount (at terminal) → 0
/// - 330 bps (3.3%) discount → 10
/// - 660 bps (6.6%) discount → 20
/// - 1000 bps (10%) discount → 30 (max)
///
/// If entry >= terminal (premium), score = 0 (no structural edge from discount,
/// but not penalized — other components carry the signal).
#[inline(always)]
fn score_entry_discount(entry_price_fp: u64, bc_terminal_price_fp: u64) -> u8 {
    if bc_terminal_price_fp == 0 || entry_price_fp == 0 {
        return 0;
    }
    if entry_price_fp >= bc_terminal_price_fp {
        // Buying at or above terminal — no discount edge
        return 0;
    }
    // discount_bps = (terminal - entry) * 10_000 / terminal
    let discount_bps = (bc_terminal_price_fp - entry_price_fp)
        .saturating_mul(10_000)
        / bc_terminal_price_fp;

    // Score: discount_bps / 33, capped at 30
    // 1000 bps / 33 = 30 (max at 10% discount)
    (discount_bps as u32 / 33).min(30) as u8
}

/// Score a graduation event (v2). All integer arithmetic, no f64.
///
/// # Parameters
///
/// - `grad_speed_s`: seconds from token creation to graduation (0..=86400)
/// - `volume_sol_x100`: total bonding curve volume in centisol (sol × 100)
/// - `buys_last_5s`: number of buy transactions in the last 5 seconds of bonding curve
/// - `sells_last_5s`: number of sell transactions in the last 5 seconds of bonding curve
/// - `entry_price_fp`: entry price in fixed-point (lamports per 1M token atoms)
/// - `bc_terminal_price_fp`: bonding curve terminal price in fixed-point (~411)
///
/// # Returns
///
/// `GraduationScore` with 5 components summing to 0-100.
///
/// # Scoring Logic (v2)
///
/// | Component       | Weight | Formula                                                |
/// |-----------------|--------|--------------------------------------------------------|
/// | Speed           | 0-15   | Piecewise: 60s→15, 120s→10, 180s→5, 300s+→0           |
/// | Volume tier     | 0-10   | Tiered: <100→0, 100-300→4, 300-600→7, 600+→10          |
/// | Velocity        | 0-20   | `buys_5s * 10_000 / max(volume_centisol, 1)` cap 20    |
/// | Buy/sell ratio  | 0-25   | `min(buys_5s / max(sells_5s, 1) * 5, 25)`              |
/// | Entry discount  | 0-30   | `min((terminal - entry) * 10_000 / terminal / 33, 30)` |
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

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helper: call with defaults for untested components ────────────

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

    // ── Speed component (0-15) ───────────────────────────────────────

    #[test]
    fn test_speed_instant() {
        // 0s → 15
        assert_eq!(score_with_speed(0).speed, 15);
    }

    #[test]
    fn test_speed_60s() {
        // 60s → 15 (boundary of first tier)
        assert_eq!(score_with_speed(60).speed, 15);
    }

    #[test]
    fn test_speed_90s() {
        // 90s → between 10 and 15 → 10 + (120-90)*5/60 = 10 + 2 = 12
        assert_eq!(score_with_speed(90).speed, 12);
    }

    #[test]
    fn test_speed_120s() {
        // 120s → 10
        assert_eq!(score_with_speed(120).speed, 10);
    }

    #[test]
    fn test_speed_150s() {
        // 150s → 5 + (180-150)*5/60 = 5 + 2 = 7
        assert_eq!(score_with_speed(150).speed, 7);
    }

    #[test]
    fn test_speed_180s() {
        // 180s → 5
        assert_eq!(score_with_speed(180).speed, 5);
    }

    #[test]
    fn test_speed_240s() {
        // 240s → (300-240)*5/120 = 300/120 = 2
        assert_eq!(score_with_speed(240).speed, 2);
    }

    #[test]
    fn test_speed_300s() {
        // 300s → 0
        assert_eq!(score_with_speed(300).speed, 0);
    }

    #[test]
    fn test_speed_slow() {
        // 3600s → 0
        assert_eq!(score_with_speed(3600).speed, 0);
    }

    // ── Volume tier component (0-10) ─────────────────────────────────

    #[test]
    fn test_volume_tier_low() {
        // < 100 SOL (< 10_000 centisol) → 0
        assert_eq!(score_with_volume(5_000).volume_tier, 0);
        assert_eq!(score_with_volume(0).volume_tier, 0);
        assert_eq!(score_with_volume(9_999).volume_tier, 0);
    }

    #[test]
    fn test_volume_tier_mid_low() {
        // 100-300 SOL → 4
        assert_eq!(score_with_volume(10_000).volume_tier, 4);
        assert_eq!(score_with_volume(20_000).volume_tier, 4);
        assert_eq!(score_with_volume(29_999).volume_tier, 4);
    }

    #[test]
    fn test_volume_tier_mid_high() {
        // 300-600 SOL → 7
        assert_eq!(score_with_volume(30_000).volume_tier, 7);
        assert_eq!(score_with_volume(45_000).volume_tier, 7);
        assert_eq!(score_with_volume(59_999).volume_tier, 7);
    }

    #[test]
    fn test_volume_tier_high() {
        // 600+ SOL → 10
        assert_eq!(score_with_volume(60_000).volume_tier, 10);
        assert_eq!(score_with_volume(100_000).volume_tier, 10);
    }

    // ── Velocity component (0-20) ────────────────────────────────────

    #[test]
    fn test_velocity_zero_buys() {
        assert_eq!(score_with_velocity(0, 50_000).velocity, 0);
    }

    #[test]
    fn test_velocity_normalized() {
        // 3 buys, 500 SOL (50_000 centisol)
        // normalized = 3 * 10_000 / 50_000 = 0 (integer division)
        assert_eq!(score_with_velocity(3, 50_000).velocity, 0);

        // 10 buys, 100 SOL (10_000 centisol)
        // normalized = 10 * 10_000 / 10_000 = 10
        assert_eq!(score_with_velocity(10, 10_000).velocity, 10);

        // 20 buys, 100 SOL
        // normalized = 20 * 10_000 / 10_000 = 20
        assert_eq!(score_with_velocity(20, 10_000).velocity, 20);
    }

    #[test]
    fn test_velocity_capped_at_20() {
        // 50 buys, 100 SOL → 50 * 10_000 / 10_000 = 50 → cap 20
        assert_eq!(score_with_velocity(50, 10_000).velocity, 20);
    }

    #[test]
    fn test_velocity_zero_volume() {
        // Zero volume uses max(1) → buys * 10_000 / 1 = buys * 10_000, cap 20
        // Even 1 buy → 10_000, cap 20
        assert_eq!(score_with_velocity(1, 0).velocity, 20);
    }

    #[test]
    fn test_velocity_high_organic() {
        // 25 buys on a small-volume token (50 SOL = 5_000 centisol)
        // = 25 * 10_000 / 5_000 = 50 → cap 20
        assert_eq!(score_with_velocity(25, 5_000).velocity, 20);

        // Same 25 buys on a whale-heavy token (2000 SOL = 200_000 centisol)
        // = 25 * 10_000 / 200_000 = 1
        assert_eq!(score_with_velocity(25, 200_000).velocity, 1);
    }

    // ── Buy/sell ratio component (0-25) ──────────────────────────────

    #[test]
    fn test_ratio_zero_buys() {
        // No buys → 0
        assert_eq!(score_with_ratio(0, 5).buy_sell_ratio, 0);
    }

    #[test]
    fn test_ratio_equal_pressure() {
        // 5 buys, 5 sells → ratio 1 → 1*5 = 5
        assert_eq!(score_with_ratio(5, 5).buy_sell_ratio, 5);
    }

    #[test]
    fn test_ratio_strong_buy() {
        // 10 buys, 2 sells → ratio 5 → 5*5 = 25
        assert_eq!(score_with_ratio(10, 2).buy_sell_ratio, 25);
    }

    #[test]
    fn test_ratio_capped_at_25() {
        // 100 buys, 1 sell → ratio 100 → 100*5 = 500 → cap 25
        assert_eq!(score_with_ratio(100, 1).buy_sell_ratio, 25);
    }

    #[test]
    fn test_ratio_zero_sells() {
        // Zero sells → max(0, 1) = 1 → ratio = buys/1
        // 10 buys → 10 * 5 = 50 → cap 25
        assert_eq!(score_with_ratio(10, 0).buy_sell_ratio, 25);
    }

    #[test]
    fn test_ratio_heavy_selling() {
        // 2 buys, 10 sells → ratio 0 (integer) → 0*5 = 0
        assert_eq!(score_with_ratio(2, 10).buy_sell_ratio, 0);
    }

    // ── Entry discount component (0-30) ──────────────────────────────

    #[test]
    fn test_discount_at_terminal() {
        // Entry = terminal → no discount → 0
        assert_eq!(score_with_discount(411, 411).entry_discount, 0);
    }

    #[test]
    fn test_discount_above_terminal() {
        // Entry above terminal (premium) → 0
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
        // 10% below terminal: entry = 370, terminal = 411
        // discount_bps = (411 - 370) * 10_000 / 411 = 410_000 / 411 = 997
        // score = 997 / 33 = 30 → max
        assert_eq!(score_with_discount(370, 411).entry_discount, 30);
    }

    #[test]
    fn test_discount_5_percent() {
        // ~5% below terminal: entry = 390, terminal = 411
        // discount_bps = (411 - 390) * 10_000 / 411 = 210_000 / 411 = 510
        // score = 510 / 33 = 15
        assert_eq!(score_with_discount(390, 411).entry_discount, 15);
    }

    #[test]
    fn test_discount_3_percent() {
        // ~3% below terminal: entry = 399, terminal = 411
        // discount_bps = (411 - 399) * 10_000 / 411 = 120_000 / 411 = 291
        // score = 291 / 33 = 8
        assert_eq!(score_with_discount(399, 411).entry_discount, 8);
    }

    #[test]
    fn test_discount_1_percent() {
        // ~1% below terminal: entry = 407, terminal = 411
        // discount_bps = (411 - 407) * 10_000 / 411 = 40_000 / 411 = 97
        // score = 97 / 33 = 2
        assert_eq!(score_with_discount(407, 411).entry_discount, 2);
    }

    #[test]
    fn test_discount_large_values() {
        // Test with realistic large fixed-point prices (e.g. lamports per 1M atoms)
        // terminal = 411_000_000, entry = 370_000_000 (10% discount)
        // discount_bps = 41_000_000 * 10_000 / 411_000_000 = 997
        // score = 997 / 33 = 30
        assert_eq!(score_with_discount(370_000_000, 411_000_000).entry_discount, 30);
    }

    // ── Integration / total score tests ──────────────────────────────

    #[test]
    fn test_total_max_score() {
        // speed: 0s → 15
        // volume: 60_000 centisol (600 SOL) → 10
        // velocity: 20 buys / 10_000 centisol = 20 → 20
        // ratio: 20 buys / 1 sell = 20 → 20*5 = 100 → cap 25
        // discount: entry=370, terminal=411 → 30
        let score = score_graduation(0, 10_000, 20, 1, 370, 411);
        assert_eq!(score.speed, 15);
        assert_eq!(score.volume_tier, 4); // 10_000 centisol = 100 SOL tier
        assert_eq!(score.velocity, 20);
        assert_eq!(score.buy_sell_ratio, 25);  // 20/1 = 20, 20*5 = 100, cap 25
        assert_eq!(score.entry_discount, 30);
        // But volume_tier is only 4 here, not 10. Let me build a true max:

        let max_score = score_graduation(0, 60_000, 20, 1, 370, 411);
        assert_eq!(max_score.speed, 15);
        assert_eq!(max_score.volume_tier, 10);
        // velocity: 20 * 10_000 / 60_000 = 3
        assert_eq!(max_score.velocity, 3);
        assert_eq!(max_score.buy_sell_ratio, 25);
        assert_eq!(max_score.entry_discount, 30);
        // total = 15 + 10 + 3 + 25 + 30 = 83 — not quite 100 because
        // velocity and volume tier are in tension (high volume → low normalized velocity)
    }

    #[test]
    fn test_total_theoretical_max_100() {
        // Construct inputs that max every component:
        // speed: ≤60s → 15
        // volume_tier: ≥60_000 → 10
        // velocity: needs buys * 10_000 / vol ≥ 20 → buys ≥ vol * 20 / 10_000
        //   For vol=60_000: buys ≥ 120. Use buys=120.
        // ratio: buys/sells ≥ 5 → sells ≤ buys/5 = 24
        // discount: ≥10% below terminal → 30
        let score = score_graduation(0, 60_000, 120, 24, 370, 411);
        assert_eq!(score.speed, 15);
        assert_eq!(score.volume_tier, 10);
        assert_eq!(score.velocity, 20);
        assert_eq!(score.buy_sell_ratio, 25);
        assert_eq!(score.entry_discount, 30);
        assert_eq!(score.total(), 100);
    }

    #[test]
    fn test_total_zero_score() {
        // All zeroed inputs → everything 0
        let score = score_graduation(3600, 0, 0, 0, 0, 0);
        assert_eq!(score.total(), 0);
    }

    #[test]
    fn test_total_no_enrichment() {
        // Cold graduation: speed=0, volume=0, buys=0, sells=0
        // But has price data: entry below terminal
        let score = score_graduation(0, 0, 0, 0, 370, 411);
        // speed=15 (0s), volume_tier=0, velocity=0 (no buys), ratio=0, discount=30
        assert_eq!(score.speed, 15);
        assert_eq!(score.volume_tier, 0);
        assert_eq!(score.velocity, 0);
        assert_eq!(score.buy_sell_ratio, 0);
        assert_eq!(score.entry_discount, 30);
        assert_eq!(score.total(), 45);
    }

    #[test]
    fn test_total_excluding_discount() {
        let score = score_graduation(60, 30_000, 10, 2, 390, 411);
        let expected = score.speed + score.volume_tier + score.velocity + score.buy_sell_ratio;
        assert_eq!(score.total_excluding_discount(), expected);
        // Verify discount is excluded
        assert!(score.total() > score.total_excluding_discount());
    }

    #[test]
    fn test_total_saturating_no_overflow() {
        // Even with extreme inputs, u8 total should not overflow
        let score = score_graduation(0, 1_000_000, 10_000, 0, 1, 1_000_000);
        assert!(score.total() <= 100);
    }

    // ── Realistic scenario tests ─────────────────────────────────────

    #[test]
    fn test_scenario_hot_organic_graduation() {
        // Fast (30s), medium volume (400 SOL), high buy rate (15 buys/5s),
        // low sells (2), buying 5% below terminal
        let score = score_graduation(30, 40_000, 15, 2, 390, 411);
        assert_eq!(score.speed, 15);        // ≤60s
        assert_eq!(score.volume_tier, 7);   // 300-600 SOL
        // velocity: 15 * 10_000 / 40_000 = 3
        assert_eq!(score.velocity, 3);
        // ratio: 15/2 = 7, 7*5 = 35, cap 25
        assert_eq!(score.buy_sell_ratio, 25);
        assert_eq!(score.entry_discount, 15);
        assert_eq!(score.total(), 65);
    }

    #[test]
    fn test_scenario_whale_pump() {
        // Very fast (10s), huge volume (1000 SOL), few buys (3), no sells,
        // at terminal price (no discount)
        let score = score_graduation(10, 100_000, 3, 0, 411, 411);
        assert_eq!(score.speed, 15);        // ≤60s
        assert_eq!(score.volume_tier, 10);  // 600+ SOL
        // velocity: 3 * 10_000 / 100_000 = 0
        assert_eq!(score.velocity, 0);
        // ratio: 3/1 = 3, 3*5 = 15
        assert_eq!(score.buy_sell_ratio, 15);
        assert_eq!(score.entry_discount, 0);
        assert_eq!(score.total(), 40);
    }

    #[test]
    fn test_scenario_distribution_dump() {
        // Medium speed (200s), medium volume (300 SOL), some buys (5),
        // but lots of sells (10), buying above terminal
        let score = score_graduation(200, 30_000, 5, 10, 450, 411);
        assert_eq!(score.speed, 4);         // (300-200)*5/120 = 4
        assert_eq!(score.volume_tier, 7);   // 300-600 SOL
        // velocity: 5 * 10_000 / 30_000 = 1
        assert_eq!(score.velocity, 1);
        // ratio: 5/10 = 0, 0*5 = 0
        assert_eq!(score.buy_sell_ratio, 0);
        assert_eq!(score.entry_discount, 0); // above terminal
        assert_eq!(score.total(), 12);
    }
}