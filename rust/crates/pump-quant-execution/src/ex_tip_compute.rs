//! Leaf `ex_tip_compute`: priority / Jito tip sizing from congestion inputs.
//!
//! Distilled from two legacy sources:
//! - `mev/jito-bundle-builder.ts` `computeTip`, which floored a base tip and
//!   scaled it up.
//! - `momentum/sell_engine.rs`'s escalation ladder, where higher urgency levels
//!   added progressively larger priority premiums.
//!
//! The legacy TS used floats; here the whole computation is integer basis-point
//! math with a widened `u128` intermediate (operator §22).
//!
//! ## Responsibility
//! Turn a base tip plus two live signals — network congestion (basis points)
//! and caller urgency (a small level) — into a single integer tip in lamports.
//!
//! ## Formula
//! ```text
//! congestion_factor_bps = 10_000 + congestion_bps          // 1.00 + congestion
//! urgency_factor_bps    = 10_000 + urgency * URGENCY_STEP_BPS
//! tip = base_tip * congestion_factor_bps / 10_000
//!               * urgency_factor_bps    / 10_000
//! ```
//! Both factors are `>= 10_000` (`1.0x`), so the result is always `>= base_tip`
//! — the configured base is a hard floor, matching the legacy
//! `max(cfg.jito_tip_lamports, ...)` behavior.
//!
//! ## Operator refs
//! - §22: integer basis-point math only.
//! - Overflow: intermediates are `u128`; the result is saturated back to `u64`.

/// Basis-point premium added per urgency level. Urgency `u` multiplies the tip
/// by `1 + u * 0.5` (each level adds 50%), mirroring the steep priority-fee
/// growth of the legacy escalation ladder.
pub const URGENCY_STEP_BPS: u64 = 5_000;

/// One whole unit expressed in basis points (`1.0 == 10_000 bps`).
pub const BPS_ONE: u64 = 10_000;

/// The observed inclusion market for one venue, in lamports: what recently **landed**
/// competitors actually paid. A tip is a bid against this, not a configured constant — a
/// constant is over-tipping in a quiet market and under-tipping in a busy one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TipMarket {
    /// Median landing price.
    pub p50: u64,
    /// 75th percentile landing price.
    pub p75: u64,
    /// 90th percentile landing price.
    pub p90: u64,
}

/// How far above the **observed median** the anchor may reach.
///
/// A self-consistency guard on the market READ, not an economic bound: percentiles more than 8× the
/// median mean the read is wrong (a corrupted percentile, or one landmark bundle dominating a thin
/// sample). Anchoring the cap to the floor instead would forbid bidding above 8 × 5,000 in a
/// genuinely hot market — exactly when bidding matters.
pub const MAX_ANCHOR_MULTIPLE: u64 = 8;

/// The market price to bid against, at the requested win rate.
///
/// `target_win_bps` is the share of the landed population we intend to outbid: `5_000` is the
/// median, `7_500` p75, `9_000` p90. Values between the reported percentiles are interpolated,
/// so the win rate does not have to be one of exactly three numbers.
#[must_use]
pub fn observed_anchor(market: &TipMarket, target_win_bps: u32) -> u64 {
    let t = target_win_bps.min(BPS_ONE as u32);
    if t <= 5_000 {
        return market.p50;
    }
    if t <= 7_500 {
        let span = market.p75.saturating_sub(market.p50);
        return market.p50 + (span * u64::from(t - 5_000) / 2_500);
    }
    let span = market.p90.saturating_sub(market.p75);
    market.p75 + (span * u64::from(t - 7_500) / 2_500)
}

/// The tip to bid for ONE send: the tier floor raised to the observed market, then shaped by
/// congestion and urgency, and bounded above by [`MAX_ANCHOR_MULTIPLE`] × the observed median.
///
/// - **Never below the floor** — a bid under the venue's minimum cannot land regardless of the
///   edge behind it, so a quiet market must not pull the tip down into a guaranteed miss.
/// - **Absent market data this is EXACTLY [`compute_tip`] on the floor** — the pre-existing
///   behaviour — so a deployment with no market signal pays what it paid before.
/// - **Never clamped to the budget here.** The budget check in the caller stays the economic
///   gate: if the market demands more than the edge can carry, the trade must DECLINE rather
///   than quietly pay less than the market and miss.
#[must_use]
pub fn bid_per_send(
    floor: u64,
    market: Option<&TipMarket>,
    target_win_bps: u32,
    congestion_bps: u32,
    urgency: u8,
) -> u64 {
    let base = match market {
        Some(m) => {
            // The read is self-consistency checked against its OWN median, so a hot market can be
            // chased and a corrupt one cannot. A zero median (empty/degenerate read) leaves the
            // floor in force.
            let ceiling = m.p50.saturating_mul(MAX_ANCHOR_MULTIPLE);
            observed_anchor(m, target_win_bps).min(ceiling).max(floor)
        }
        None => floor,
    };
    compute_tip(base, congestion_bps, urgency)
}

/// Compute the tip in lamports from a base tip, congestion, and urgency.
///
/// - `base_tip`: configured minimum tip in lamports (acts as a floor).
/// - `congestion_bps`: network congestion as basis points of extra tip
///   (`0` = idle, `10_000` = double the base for congestion alone).
/// - `urgency`: caller urgency level; each level adds [`URGENCY_STEP_BPS`]
///   (50%) on top of the congestion-scaled tip.
///
/// Returns the scaled tip, saturated into `u64`. Never returns less than
/// `base_tip`.
pub fn compute_tip(base_tip: u64, congestion_bps: u32, urgency: u8) -> u64 {
    let congestion_factor_bps = BPS_ONE.saturating_add(u64::from(congestion_bps));
    let urgency_factor_bps =
        BPS_ONE.saturating_add(u64::from(urgency).saturating_mul(URGENCY_STEP_BPS));

    // Widen to u128 so the two-factor multiply cannot overflow before we divide.
    let mut tip = u128::from(base_tip);
    tip = tip * u128::from(congestion_factor_bps) / u128::from(BPS_ONE);
    tip = tip * u128::from(urgency_factor_bps) / u128::from(BPS_ONE);

    if tip > u128::from(u64::MAX) {
        u64::MAX
    } else {
        tip as u64
    }
}
