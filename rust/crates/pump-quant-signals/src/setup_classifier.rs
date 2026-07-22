//! Setup-archetype / scalp setup-family classifier (constitution §24, §22).
//!
//! `archetype` is an externally-supplied `u16` discriminator everywhere it
//! appears in the wider workspace (StrategyConfig, thesis identity, evaluator
//! excursion grouping) — nothing *derives* it. This module derives it: it maps
//! a reconstructed, integer-only market state to one of the §24 named scalp
//! setup families by composing the §21.6 bar-structure and §21.7 order-flow
//! primitives already in this crate ([`crate::microstructure`]).
//!
//! The families are the confirmed intra-scalp patterns that live *within* the
//! `ActiveMarketScalp` lane (the orthogonal higher-level entry-mode lane is not
//! a setup family):
//!
//! - **BreakoutRetest** — broke a prior resistance, pulled back to it, holding
//!   with order flow not collapsing.
//! - **FailedBreakdownReversal** — breached prior support intrabar then
//!   reclaimed it with net buying (a failed breakdown reversing up).
//! - **Reclaim** — reclaimed the anchored VWAP reference from below with net
//!   buying.
//! - **CompressionExpansion** — a compressed prior range followed by a range
//!   expansion.
//! - **ShortHorizonMeanReversion** — price stretched far from VWAP and now
//!   snapping back toward it.
//! - **OrderFlowDislocation** — CVD and price disagree (exhaustion divergence)
//!   beyond a magnitude threshold.
//! - **None** — no recognized family.
//!
//! Classification is a deterministic, first-match decision over the state, so
//! the precedence order (as listed above) is part of the contract. The result
//! also carries the stable `u16` archetype id the rest of the system consumes.
//!
//! # Constitution constraints (§22)
//!
//! Pure, deterministic, integer/fixed-point only. Prices are fixed-point,
//! ranges/margins are basis points, CVD is signed `i128`. Reuses
//! [`crate::microstructure::price_change_bps`] and
//! [`crate::microstructure::cvd_price_divergence`]. No floats, no wall-clock,
//! bounded state (§99). Live reconstruction is server-side; callers feed
//! fixtures.

use crate::microstructure::{cvd_price_divergence, price_change_bps, Divergence};

/// Reconstructed market state fed to the classifier (§24).
///
/// Responsibility: the integer feature bundle a caller reconstructs from
/// decoded flow — window endpoints, intrabar extremes, prior structural levels,
/// the anchored VWAP, net CVD, and range measures. Constitution §22: all
/// fixed-point / integer, `Copy` for cheap threading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketState {
    /// Window-open price (fixed-point).
    pub price_open_fp: u64,
    /// Current (window-end) price (fixed-point).
    pub price_fp: u64,
    /// Maximum price touched within the window (fixed-point).
    pub high_extreme_fp: u64,
    /// Minimum price touched within the window (fixed-point).
    pub low_extreme_fp: u64,
    /// Prior structural resistance (swing high before the window), fixed-point.
    pub prior_high_fp: u64,
    /// Prior structural support (swing low before the window), fixed-point.
    pub prior_low_fp: u64,
    /// Anchored VWAP reference (fixed-point).
    pub vwap_fp: u64,
    /// Net cumulative volume delta over the window (lamports, signed).
    pub cvd_delta: i128,
    /// Current window price range as basis points of price.
    pub range_bps: u32,
    /// Prior window price range as basis points of price (compression proxy).
    pub prior_range_bps: u32,
}

/// Tunable thresholds for the setup-family classifier (§24).
///
/// Responsibility: the recorded-prior margins separating each family from
/// noise. Constitution §22: integer/bps, `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupThresholds {
    /// Minimum depth (bps below prior support) an intrabar breach must reach to
    /// count as a *failed breakdown* rather than noise.
    pub breach_depth_bps: i64,
    /// Minimum height (bps above prior resistance) an intrabar high must reach
    /// to count as a breakout.
    pub breakout_margin_bps: i64,
    /// Maximum distance (bps above prior resistance) the current price may sit
    /// and still count as a *retest* (a deeper pullback is not a retest).
    pub retest_tolerance_bps: i64,
    /// Prior range at or below this (bps) counts as *compressed*.
    pub compression_range_bps: u32,
    /// Current range must be at least `expansion_multiple` times the prior
    /// range to count as an *expansion*.
    pub expansion_multiple: u32,
    /// `|price - vwap|` (bps) beyond which price is *stretched* (mean-reversion
    /// candidate).
    pub stretch_bps: i64,
    /// Minimum window price-move magnitude (bps) for an order-flow divergence to
    /// count as a *dislocation*.
    pub dislocation_bps: i64,
}

impl SetupThresholds {
    /// A neutral default parameterization.
    ///
    /// Responsibility: portable default prior (§24). Constitution §22: pure.
    pub const fn neutral() -> Self {
        SetupThresholds {
            breach_depth_bps: 50,
            breakout_margin_bps: 50,
            retest_tolerance_bps: 100,
            compression_range_bps: 200,
            expansion_multiple: 3,
            stretch_bps: 500,
            dislocation_bps: 100,
        }
    }
}

impl Default for SetupThresholds {
    fn default() -> Self {
        Self::neutral()
    }
}

/// The §24 named scalp setup family (the derived archetype).
///
/// Responsibility: the classifier's output — a named family plus, via
/// [`SetupFamily::archetype_id`], the stable `u16` discriminator the wider
/// system carries. Constitution §22: data only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupFamily {
    /// Broke prior resistance, retested it, holding.
    BreakoutRetest,
    /// Breached prior support then reclaimed it with net buying.
    FailedBreakdownReversal,
    /// Reclaimed the anchored VWAP from below with net buying.
    Reclaim,
    /// Compressed range followed by a range expansion.
    CompressionExpansion,
    /// Stretched far from VWAP and snapping back.
    ShortHorizonMeanReversion,
    /// CVD and price disagree (exhaustion divergence) beyond threshold.
    OrderFlowDislocation,
    /// No recognized setup family.
    None,
}

impl SetupFamily {
    /// Stable `u16` archetype id for this family — the discriminator the rest of
    /// the workspace (StrategyConfig, thesis identity, evaluator excursion
    /// grouping) carries. `None` maps to `0`.
    ///
    /// Responsibility: bridge the derived family to the externally-consumed
    /// `archetype:u16` (§24). Constitution §22: pure, total.
    #[inline]
    pub const fn archetype_id(self) -> u16 {
        match self {
            SetupFamily::None => 0,
            SetupFamily::BreakoutRetest => 1,
            SetupFamily::FailedBreakdownReversal => 2,
            SetupFamily::Reclaim => 3,
            SetupFamily::CompressionExpansion => 4,
            SetupFamily::ShortHorizonMeanReversion => 5,
            SetupFamily::OrderFlowDislocation => 6,
        }
    }
}

/// Classify a reconstructed market state into its §24 scalp setup family.
///
/// Deterministic first-match precedence (the documented order): failed
/// breakdown reversal, breakout retest, VWAP reclaim, compression→expansion,
/// short-horizon mean reversion, order-flow dislocation, else `None`.
///
/// Responsibility: the single mapping from reconstructed state to a named
/// family (§24), composing the §21.6/§21.7 primitives. Constitution §22:
/// integer comparisons, division guards inside the reused bps helpers.
pub fn classify_setup(state: &MarketState, t: &SetupThresholds) -> SetupFamily {
    // 1. Failed-breakdown reversal: breached prior support intrabar (beyond the
    //    depth margin), reclaimed it, with net buying.
    if state.prior_low_fp > 0
        && state.low_extreme_fp < state.prior_low_fp
        && price_change_bps(state.prior_low_fp, state.low_extreme_fp) <= -t.breach_depth_bps
        && state.price_fp >= state.prior_low_fp
        && state.cvd_delta > 0
    {
        return SetupFamily::FailedBreakdownReversal;
    }

    // 2. Breakout retest: broke prior resistance intrabar (beyond the margin),
    //    price is back at/above the level within the retest tolerance, and order
    //    flow is not net-negative.
    if state.prior_high_fp > 0
        && price_change_bps(state.prior_high_fp, state.high_extreme_fp) >= t.breakout_margin_bps
        && state.price_fp >= state.prior_high_fp
        && price_change_bps(state.prior_high_fp, state.price_fp) <= t.retest_tolerance_bps
        && state.cvd_delta >= 0
    {
        return SetupFamily::BreakoutRetest;
    }

    // 3. VWAP reclaim: was below VWAP intrabar, now above it, net buying.
    if state.vwap_fp > 0
        && state.low_extreme_fp < state.vwap_fp
        && state.price_fp > state.vwap_fp
        && state.cvd_delta > 0
    {
        return SetupFamily::Reclaim;
    }

    // 4. Compression -> expansion: prior range compressed, current range at
    //    least `expansion_multiple` times larger.
    if state.prior_range_bps > 0
        && state.prior_range_bps <= t.compression_range_bps
        && state.range_bps as u128
            >= state.prior_range_bps as u128 * t.expansion_multiple.max(1) as u128
    {
        return SetupFamily::CompressionExpansion;
    }

    // 5. Short-horizon mean reversion: stretched beyond `stretch_bps` from VWAP
    //    and the window move points back toward the VWAP.
    let dist_vwap = price_change_bps(state.vwap_fp, state.price_fp);
    let window_move = price_change_bps(state.price_open_fp, state.price_fp);
    if state.vwap_fp > 0
        && dist_vwap.abs() >= t.stretch_bps
        && window_move != 0
        && dist_vwap.signum() != window_move.signum()
    {
        return SetupFamily::ShortHorizonMeanReversion;
    }

    // 6. Order-flow dislocation: CVD and price disagree (exhaustion divergence)
    //    with a meaningful price move.
    let divergence = cvd_price_divergence(state.price_open_fp, state.price_fp, 0, state.cvd_delta);
    if window_move.abs() >= t.dislocation_bps
        && matches!(
            divergence,
            Divergence::BearishExhaustion | Divergence::BullishExhaustion
        )
    {
        return SetupFamily::OrderFlowDislocation;
    }

    SetupFamily::None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> MarketState {
        // A neutral, unremarkable state that matches nothing.
        MarketState {
            price_open_fp: 1_000,
            price_fp: 1_000,
            high_extreme_fp: 1_000,
            low_extreme_fp: 1_000,
            prior_high_fp: 2_000,
            prior_low_fp: 500,
            vwap_fp: 1_000,
            cvd_delta: 0,
            range_bps: 10,
            prior_range_bps: 10,
        }
    }

    #[test]
    fn neutral_state_is_none() {
        assert_eq!(
            classify_setup(&base(), &SetupThresholds::neutral()),
            SetupFamily::None
        );
    }

    #[test]
    fn archetype_ids_are_stable_and_distinct() {
        use SetupFamily::*;
        let all = [
            None,
            BreakoutRetest,
            FailedBreakdownReversal,
            Reclaim,
            CompressionExpansion,
            ShortHorizonMeanReversion,
            OrderFlowDislocation,
        ];
        let ids: Vec<u16> = all.iter().map(|f| f.archetype_id()).collect();
        assert_eq!(ids, vec![0, 1, 2, 3, 4, 5, 6]);
        assert_eq!(None.archetype_id(), 0);
    }

    #[test]
    fn failed_breakdown_reversal() {
        // prior_low 1_000. low_extreme 940 => breach -600 bps (<= -50).
        // price back to 1_010 (>= prior_low). cvd positive.
        let mut s = base();
        s.prior_low_fp = 1_000;
        s.low_extreme_fp = 940;
        s.price_fp = 1_010;
        s.price_open_fp = 1_000;
        s.high_extreme_fp = 1_010;
        s.cvd_delta = 5_000;
        // keep vwap out of the way so reclaim/mean-reversion don't fire first
        s.vwap_fp = 1_005;
        assert_eq!(
            classify_setup(&s, &SetupThresholds::neutral()),
            SetupFamily::FailedBreakdownReversal
        );
    }

    #[test]
    fn breakdown_without_reclaim_is_not_reversal() {
        // Breached but did NOT reclaim (price stays below support).
        let mut s = base();
        s.prior_low_fp = 1_000;
        s.low_extreme_fp = 900;
        s.price_fp = 950; // still below support
        s.cvd_delta = 5_000;
        assert_ne!(
            classify_setup(&s, &SetupThresholds::neutral()),
            SetupFamily::FailedBreakdownReversal
        );
    }

    #[test]
    fn breakout_retest() {
        // prior_high 1_000. high_extreme 1_080 => +800 bps breakout.
        // price 1_005 => +50 bps above level, within 100 tolerance. cvd >= 0.
        let mut s = base();
        s.prior_high_fp = 1_000;
        s.high_extreme_fp = 1_080;
        s.price_fp = 1_005;
        s.price_open_fp = 1_000;
        s.low_extreme_fp = 1_000;
        s.prior_low_fp = 400;
        s.vwap_fp = 1_000; // not below vwap so reclaim won't preempt
        s.cvd_delta = 100;
        assert_eq!(
            classify_setup(&s, &SetupThresholds::neutral()),
            SetupFamily::BreakoutRetest
        );
    }

    #[test]
    fn breakout_extended_too_far_is_not_retest() {
        // Broke out but price is 300 bps above the level: beyond retest tolerance.
        let mut s = base();
        s.prior_high_fp = 1_000;
        s.high_extreme_fp = 1_080;
        s.price_fp = 1_030; // +300 bps, > 100 tolerance
        s.low_extreme_fp = 1_000;
        s.cvd_delta = 100;
        assert_ne!(
            classify_setup(&s, &SetupThresholds::neutral()),
            SetupFamily::BreakoutRetest
        );
    }

    #[test]
    fn vwap_reclaim() {
        // Below vwap intrabar (low 950 < vwap 1_000), now above (1_010), cvd +.
        // No support breach, no breakout.
        let mut s = base();
        s.vwap_fp = 1_000;
        s.low_extreme_fp = 950;
        s.price_fp = 1_010;
        s.price_open_fp = 990;
        s.high_extreme_fp = 1_010;
        s.prior_low_fp = 100; // no support breach
        s.prior_high_fp = 5_000; // no breakout
        s.cvd_delta = 3_000;
        assert_eq!(
            classify_setup(&s, &SetupThresholds::neutral()),
            SetupFamily::Reclaim
        );
    }

    #[test]
    fn compression_expansion() {
        // prior_range 100 (<= 200 compressed), current 400 (>= 3x100=300).
        let mut s = base();
        s.prior_range_bps = 100;
        s.range_bps = 400;
        // avoid earlier matches
        s.cvd_delta = 0;
        s.prior_low_fp = 100;
        s.prior_high_fp = 5_000;
        s.vwap_fp = 1_000;
        s.low_extreme_fp = 1_000;
        assert_eq!(
            classify_setup(&s, &SetupThresholds::neutral()),
            SetupFamily::CompressionExpansion
        );
    }

    #[test]
    fn expansion_without_prior_compression_is_not_it() {
        // prior_range 300 > compression threshold 200.
        let mut s = base();
        s.prior_range_bps = 300;
        s.range_bps = 2_000;
        s.cvd_delta = 0;
        s.prior_low_fp = 100;
        s.prior_high_fp = 5_000;
        s.low_extreme_fp = 1_000;
        assert_ne!(
            classify_setup(&s, &SetupThresholds::neutral()),
            SetupFamily::CompressionExpansion
        );
    }

    #[test]
    fn short_horizon_mean_reversion() {
        // Stretched +700 bps above vwap (price 1_070 vs vwap 1_000), window move
        // negative (open 1_090 -> 1_070) => snapping back.
        let mut s = base();
        s.vwap_fp = 1_000;
        s.price_fp = 1_070; // dist +700 bps >= 500
        s.price_open_fp = 1_090; // window move negative
        s.high_extreme_fp = 1_090;
        s.low_extreme_fp = 1_050; // not below vwap -> no reclaim
        s.prior_low_fp = 100; // no breach
        s.prior_high_fp = 5_000; // no breakout
        s.cvd_delta = 0;
        s.prior_range_bps = 300; // no compression
        s.range_bps = 300;
        assert_eq!(
            classify_setup(&s, &SetupThresholds::neutral()),
            SetupFamily::ShortHorizonMeanReversion
        );
    }

    #[test]
    fn order_flow_dislocation() {
        // Price up +200 bps (open 1_000 -> 1_020) but CVD negative =>
        // bearish exhaustion divergence, move >= 100 bps dislocation.
        let mut s = base();
        s.price_open_fp = 1_000;
        s.price_fp = 1_020;
        s.high_extreme_fp = 1_020;
        s.low_extreme_fp = 1_000;
        s.vwap_fp = 1_015; // dist from vwap only +49 bps, not stretched
        s.prior_low_fp = 100; // no breach
        s.prior_high_fp = 5_000; // no breakout
        s.cvd_delta = -8_000; // disagrees with the up-move
        s.prior_range_bps = 300;
        s.range_bps = 300;
        assert_eq!(
            classify_setup(&s, &SetupThresholds::neutral()),
            SetupFamily::OrderFlowDislocation
        );
    }

    #[test]
    fn precedence_failed_breakdown_beats_reclaim() {
        // A state that satisfies BOTH failed-breakdown and reclaim; the
        // documented precedence returns FailedBreakdownReversal.
        let mut s = base();
        s.prior_low_fp = 1_000;
        s.vwap_fp = 1_000;
        s.low_extreme_fp = 940; // below support AND below vwap
        s.price_fp = 1_010; // reclaimed support AND above vwap
        s.price_open_fp = 1_000;
        s.high_extreme_fp = 1_010;
        s.cvd_delta = 5_000;
        assert_eq!(
            classify_setup(&s, &SetupThresholds::neutral()),
            SetupFamily::FailedBreakdownReversal
        );
    }
}
