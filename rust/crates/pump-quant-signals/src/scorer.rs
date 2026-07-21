//! Integer-only graduation entry scorer (ported from legacy `momentum::scorer`, v5).
//!
//! Scores a bonding-curve graduation event on eight integer dimensions whose
//! scored components sum to `0..=100`:
//!
//! | Component            | Max | Meaning                                             |
//! |----------------------|-----|-----------------------------------------------------|
//! | Speed                | 20  | Slower graduation = more organic post-grad momentum |
//! | Volume tier          | 20  | Total BC volume in SOL; 50-100 SOL is the sweet spot|
//! | Velocity             | 15  | Buy rate normalized by volume (organic demand)      |
//! | Buy/sell ratio       | 10  | Unidirectional buy pressure, gated by min buys      |
//! | Entry discount       | 10  | Structural edge from buying below BC terminal price |
//! | LP reserve size      | 10  | Fresh graduates land in an 85-120 SOL sweet spot    |
//! | Pre-entry momentum   | 10  | Observed price velocity during the observation window|
//! | Cold-miss bonus      |  5  | Applied externally when enrichment data is cold     |
//!
//! # Constitution constraints (§22)
//!
//! All arithmetic here is integer / fixed-point. Inputs use *centisol*
//! (SOL x 100) for volume and basis points (bps) for rates/discounts so that
//! no floating point is ever required. Overflow is handled explicitly with
//! `saturating_*` on accumulation and `u128` widening on multiplications that
//! could exceed `u64`.

/// Scored components of a graduation event (v5: 7 scored components +
/// cold-miss bonus). Each field is capped at its documented maximum; the
/// scored components sum to `0..=100` via [`GraduationScore::total`].
///
/// Responsibility: hold the per-dimension breakdown so callers can inspect
/// *why* an entry scored the way it did, not just the aggregate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GraduationScore {
    /// Speed score: `0..=20`. Slow graduation = organic momentum.
    pub speed: u8,
    /// Volume tier score: `0..=20`. Moderate volume = organic sweet spot.
    pub volume_tier: u8,
    /// Velocity score: `0..=15`. Buy rate normalized by volume (organic demand).
    pub velocity: u8,
    /// Buy/sell ratio score: `0..=10`. Unidirectional pressure, gated by min buys.
    pub buy_sell_ratio: u8,
    /// Entry discount score: `0..=10`. Buying below BC terminal = structural edge.
    pub entry_discount: u8,
    /// LP reserve size score: `0..=10`. Fresh graduates (50-100 SOL) = sweet spot.
    pub lp_reserve: u8,
    /// Cold-miss bonus: `0..=5`. Applied externally when enrichment was unavailable
    /// (information-asymmetry edge). Always `0` from [`score_graduation`].
    pub cold_miss_bonus: u8,
    /// Pre-entry momentum score: `0..=10`. Based on observed price velocity
    /// during the observation window. `0` if velocity has not yet been observed.
    pub pre_entry_momentum: u8,
}

impl GraduationScore {
    /// Total score (`0..=100`). Uses saturating addition so accumulation can
    /// never overflow `u8` regardless of (mis)configured component maxima.
    ///
    /// Responsibility: aggregate all eight dimensions into a single entry score.
    #[inline]
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

    /// Total score excluding [`entry_discount`](Self::entry_discount), used for a
    /// pre-entry gate when the entry price is not yet known. Sum of the remaining
    /// components has a maximum of 90.
    ///
    /// Responsibility: provide a discount-independent aggregate for early gating.
    #[inline]
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

/// Speed score (`0..=20`). SLOWER graduation scores HIGHER: fast fills
/// (<=60s) are typically bot/whale driven, slow fills (>=120s) show organic
/// demand. Piecewise-linear in integer arithmetic.
///
/// Responsibility: map creation->graduation seconds to a speed dimension score.
/// Constitution §22: integer-only, no floats.
#[inline]
pub fn score_speed(grad_speed_s: u32) -> u8 {
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

/// Volume tier score (`0..=20`). Moderate total bonding-curve volume scores
/// highest; 50-100 SOL (5_000-9_999 centisol) is the organic sweet spot, while
/// both dust and whale-sized volume score low.
///
/// Responsibility: map total BC volume (centisol) to a volume-tier score.
/// Constitution §22: integer-only, centisol input avoids fractional SOL.
#[inline]
pub fn score_volume_tier(volume_sol_x100: u32) -> u8 {
    if volume_sol_x100 < 25 {
        0 // < 0.25 SOL: dust
    } else if volume_sol_x100 < 1_000 {
        2 // 0.25-10 SOL
    } else if volume_sol_x100 < 3_000 {
        5 // 10-30 SOL
    } else if volume_sol_x100 < 5_000 {
        10 // 30-50 SOL
    } else if volume_sol_x100 < 10_000 {
        20 // 50-100 SOL <- SWEET SPOT
    } else if volume_sol_x100 < 20_000 {
        12 // 100-200 SOL
    } else if volume_sol_x100 < 40_000 {
        6 // 200-400 SOL
    } else if volume_sol_x100 < 65_535 {
        2 // 400-655 SOL
    } else {
        0 // >= 655 SOL: whale pump, no organic signal
    }
}

/// Velocity score (`0..=15`). Normalized buy rate:
/// `buys_5s * 10_000 / max(volume_sol_x100, 1)`. Many small buys relative to
/// volume indicate organic demand rather than a single whale deposit.
///
/// Responsibility: map buy count + volume to a normalized-demand score.
/// Constitution §22: integer-only; `saturating_mul` guards the widening.
#[inline]
pub fn score_velocity(buys_5s: u32, volume_sol_x100: u32) -> u8 {
    let vol = volume_sol_x100.max(1);
    let normalized = buys_5s.saturating_mul(10_000) / vol;
    normalized.min(15) as u8
}

/// Buy/sell ratio score with activity gate (`0..=10`).
///
/// Base score is `min(buys/max(sells,1) * 2, 10)` -- linear in the buy/sell
/// ratio. When `buys_5s < min_buys_for_full` the score is halved (integer
/// division), penalizing thin "whale pump" prints that lack broad participation.
///
/// Ported leaf `sg_buy_sell_ratio_gate`. Returns `u32` in `0..=10`.
///
/// Responsibility: quantify unidirectional buy pressure, discounted when the
/// sample is too thin to be trustworthy. Constitution §22: integer-only.
#[inline]
pub fn buy_sell_ratio_score(buys_5s: u32, sells_5s: u32, min_buys_for_full: u32) -> u32 {
    let sells = sells_5s.max(1);
    let ratio = buys_5s / sells;
    let raw = ratio.saturating_mul(2).min(10);
    if buys_5s < min_buys_for_full {
        raw / 2
    } else {
        raw
    }
}

/// Entry discount score (`0..=10`).
///
/// `discount_bps = (bc_terminal - entry) * 10_000 / bc_terminal`, then scaled
/// linearly so that 0 bps -> 0 and >= 1_500 bps -> 10. Any premium (entry at or
/// above terminal) or a zero price yields 0 -- there is no structural edge.
///
/// Ported leaf `sg_entry_discount`. Returns `u32` in `0..=10`.
///
/// Responsibility: reward buying strictly below the bonding-curve terminal
/// price. Constitution §22: integer-only; `u128` widening on the bps multiply
/// prevents overflow for large fixed-point prices.
#[inline]
pub fn entry_discount_score(entry_price_fp: u64, bc_terminal_price_fp: u64) -> u32 {
    if bc_terminal_price_fp == 0 || entry_price_fp == 0 {
        return 0;
    }
    if entry_price_fp >= bc_terminal_price_fp {
        return 0; // at or above terminal -- no discount
    }
    let discount_bps = ((bc_terminal_price_fp - entry_price_fp) as u128 * 10_000
        / bc_terminal_price_fp as u128) as u32;
    if discount_bps >= 1_500 {
        return 10;
    }
    (discount_bps * 10 / 1_500).min(10)
}

/// LP reserve size score (`0..=10`). Fresh pump.fun graduates land with roughly
/// 85-120 SOL of pooled liquidity; smaller pools are more momentum-tradeable
/// while very large pools (Raydium majors / market makers) dampen momentum.
///
/// Responsibility: map pooled SOL reserve (lamports) to a tradeability score.
/// Constitution §22: integer-only; lamports->SOL is integer division.
#[inline]
pub fn score_lp_reserve(reserve_lamports: u64) -> u8 {
    let sol = reserve_lamports / 1_000_000_000;
    if sol < 50 {
        0 // too thin
    } else if sol < 100 {
        10 // 50-100 SOL <- sweet spot
    } else if sol < 200 {
        8 // 100-200 SOL: good
    } else if sol < 500 {
        4 // 200-500 SOL: dampened
    } else if sol < 2000 {
        2 // 500-2000 SOL: institutional
    } else {
        0 // > 2000 SOL: market making, skip
    }
}

/// Pre-entry momentum score (`0..=10`). Buckets the observed price velocity
/// (bps/sec) seen during the observation window:
///
/// - `<= 0` bps/s: flat/declining -> 0 (no bonus, no penalty)
/// - `1..=50`: slow rise -> 2
/// - `51..=150`: moderate organic demand -> 7
/// - `151..=300`: strong organic demand -> 10
/// - `> 300`: possible spike top -> 5 (partial credit, revert risk)
///
/// Responsibility: reward organic upward momentum visible *before* entry.
/// Constitution §22: integer-only, signed velocity input.
#[inline]
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
        5
    }
}

/// Score a graduation entry (v5). All integer arithmetic, no floats.
///
/// Ported leaf `sg_graduation_score`. Combines the eight integer dimensions
/// into a [`GraduationScore`]. The `cold_miss_bonus` field is always `0` here;
/// it is applied externally when enrichment data was unavailable.
///
/// # Parameters
///
/// - `grad_speed_s`: seconds from token creation to graduation.
/// - `volume_sol_x100`: total bonding-curve volume in centisol (SOL x 100).
/// - `buys_5s`: buy transactions in the last 5 seconds of the bonding curve.
/// - `sells_5s`: sell transactions in the last 5 seconds of the bonding curve.
/// - `entry_price_fp`: entry price in fixed-point units.
/// - `bc_terminal_price_fp`: bonding-curve terminal price in fixed-point units.
/// - `reserve_lamports`: pooled SOL reserve in lamports.
/// - `velocity_bps_per_s`: observed price velocity in bps/sec (0 if unobserved).
/// - `min_buys_for_full_ratio`: minimum `buys_5s` for a full buy/sell ratio score.
///
/// Responsibility: single entry point producing the full per-dimension entry
/// signal breakdown. Constitution §22: integer-only throughout.
#[inline]
#[allow(clippy::too_many_arguments)]
pub fn score_graduation(
    grad_speed_s: u32,
    volume_sol_x100: u32,
    buys_5s: u32,
    sells_5s: u32,
    entry_price_fp: u64,
    bc_terminal_price_fp: u64,
    reserve_lamports: u64,
    velocity_bps_per_s: i64,
    min_buys_for_full_ratio: u32,
) -> GraduationScore {
    GraduationScore {
        speed: score_speed(grad_speed_s),
        volume_tier: score_volume_tier(volume_sol_x100),
        velocity: score_velocity(buys_5s, volume_sol_x100),
        buy_sell_ratio: buy_sell_ratio_score(buys_5s, sells_5s, min_buys_for_full_ratio) as u8,
        entry_discount: entry_discount_score(entry_price_fp, bc_terminal_price_fp) as u8,
        lp_reserve: score_lp_reserve(reserve_lamports),
        cold_miss_bonus: 0,
        pre_entry_momentum: score_pre_entry_momentum(velocity_bps_per_s),
    }
}
