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

use std::collections::VecDeque;

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

/// Minimum samples before a read counts as a MARKET.
///
/// A one- or two-slot window is noise, and a bid anchored to noise is precisely the
/// over/under-tipping this policy exists to avoid. A thin read returns `None`, and the caller
/// keeps the venue floor — never a fabricated market.
pub const MIN_MARKET_SAMPLES: usize = 8;

impl TipMarket {
    /// Build the observed market from fee samples.
    ///
    /// Zero-fee samples are **ignored**: `getRecentPrioritizationFees` returns one entry per slot,
    /// and a slot with no fee is *no observation* (nothing needed to pay), not a price of zero.
    /// Counting them makes every read inert — measured live on mainnet: the global query returned
    /// 150 samples with **0** nonzero, and the pump.fun program **2 of 150** — and an inert anchor
    /// is a bid that never leaves the venue floor.
    ///
    /// `None` (fewer than [`MIN_MARKET_SAMPLES`] PAID samples) means "no market signal", which is
    /// the pre-existing behaviour: the venue floor governs.
    #[must_use]
    pub fn from_fee_samples(samples: &[u64]) -> Option<TipMarket> {
        let mut xs: Vec<u64> = samples.iter().copied().filter(|f| *f > 0).collect();
        if xs.len() < MIN_MARKET_SAMPLES {
            return None;
        }
        xs.sort_unstable();
        Some(TipMarket {
            p50: percentile_bps(&xs, 5_000),
            p75: percentile_bps(&xs, 7_500),
            p90: percentile_bps(&xs, 9_000),
        })
    }
}

/// A rolling window of **landed** fee samples — the producer that matters for this venue.
///
/// The per-slot RPC read is thin (measured: 2 paid samples in 150 slots for pump.fun), so the
/// market worth anchoring to is built from the transactions the stream actually delivers. This is
/// the bounded accumulator for that feed. It is pure and window-bounded by construction, so the
/// policy can be tested without a stream and the memory cost cannot grow with uptime.
#[derive(Debug, Clone)]
pub struct TipMarketWindow {
    samples: VecDeque<u64>,
    capacity: usize,
}

impl TipMarketWindow {
    /// Samples kept before the oldest is evicted. 512 covers >3 minutes of a busy venue at one
    /// sample per landed tx — long enough to be a market, short enough to be *current*.
    pub const DEFAULT_CAPACITY: usize = 512;

    /// New window. A zero capacity is promoted to 1 (a window that keeps nothing is not a window).
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            samples: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// Record one LANDED fee. Zero is dropped for the same reason [`TipMarket::from_fee_samples`]
    /// drops it: it is not an observation of price.
    pub fn record(&mut self, fee_lamports: u64) {
        if fee_lamports == 0 {
            return;
        }
        if self.samples.len() == self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(fee_lamports);
    }

    /// Paid samples currently held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Whether the window holds no paid samples at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// The market implied by the window, or `None` while it is still too thin to be one.
    #[must_use]
    pub fn market(&self) -> Option<TipMarket> {
        let samples: Vec<u64> = self.samples.iter().copied().collect();
        TipMarket::from_fee_samples(&samples)
    }
}

/// Nearest-rank percentile over a SORTED slice, expressed in basis points (`10_000` == p100).
fn percentile_bps(sorted: &[u64], bps: u32) -> u64 {
    debug_assert!(!sorted.is_empty());
    let n = sorted.len() as u64;
    // ceil(bps * n / 10_000) in the range 1..=n, computed with integers only (§22).
    let rank = (u64::from(bps.min(BPS_ONE as u32)) * n)
        .div_ceil(BPS_ONE)
        .max(1);
    sorted[(rank - 1) as usize]
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
