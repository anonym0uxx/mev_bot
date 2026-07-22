//! Capacity-curve harness over the §55 size grid.
//!
//! Responsibility: run a qualified strategy's fill model at each mandated position
//! size and report how price impact, landing probability, expectancy, drawdown, and
//! terminal-loss exposure vary with size (constitution §55: "Run qualified
//! strategies at 0.01, 0.025, 0.05, 0.10, 0.25, 0.50, and 1.00 SOL ... Scaling never
//! assumes linear PnL"). The impact model is size-dependent, so per-unit economics
//! degrade with size rather than scaling linearly.

use crate::fill::{simulate_fill, CostModel, ExitImpairment, FillMode, MarketState};
use crate::fixed::BPS_ONE;
use crate::terminal_loss::TerminalLossPolicy;

/// The §55 capacity size grid, in lamports (`1 SOL == 1_000_000_000` lamports):
/// `0.01, 0.025, 0.05, 0.10, 0.25, 0.50, 1.00` SOL. Strictly increasing.
pub const CAPACITY_GRID_LAMPORTS: [u64; 7] = [
    10_000_000,
    25_000_000,
    50_000_000,
    100_000_000,
    250_000_000,
    500_000_000,
    1_000_000_000,
];

/// Deterministic landing-probability model: probability decreases with size as
/// larger orders are harder to land. `landing_bps = base_bps - size * penalty_k / depth`,
/// floored at `0` and capped at `BPS_ONE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LandingModel {
    /// Landing probability (bps) at negligible size.
    pub base_bps: u32,
    /// Penalty scale: subtracts `size * penalty_k_bps / depth` bps.
    pub penalty_k_bps: u32,
}

impl LandingModel {
    /// Landing probability in bps for `size` against `depth`.
    #[must_use]
    pub fn landing_bps(&self, size_lamports: u64, depth_lamports: u64) -> u32 {
        if depth_lamports == 0 {
            return 0;
        }
        let penalty =
            (size_lamports as u128) * (self.penalty_k_bps as u128) / (depth_lamports as u128);
        let penalty = if penalty > BPS_ONE as u128 {
            BPS_ONE
        } else {
            penalty as u32
        };
        self.base_bps.min(BPS_ONE).saturating_sub(penalty)
    }
}

/// One point of the capacity curve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityPoint {
    /// Position size at this point, in lamports.
    pub size_lamports: u64,
    /// Entry-side price impact at this size, in bps.
    pub price_impact_bps: u32,
    /// Modeled landing probability at this size, in bps.
    pub landing_prob_bps: u32,
    /// Net-SOL expectancy for the deterministic fill at this size, in lamports.
    pub expectancy_lamports: i128,
    /// Downside proxy: `max(0, -expectancy)` in lamports.
    pub drawdown_lamports: i128,
    /// Terminal-loss exposure at this size (loss realized if unexitable, else `0`).
    pub terminal_loss_exposure_lamports: i128,
}

/// Build the capacity curve over [`CAPACITY_GRID_LAMPORTS`].
///
/// `base_market` supplies depth, move, and impact scale; the notional is overridden
/// per grid size. The returned vector is ordered by ascending size (the grid order).
/// Deterministic: no RNG, no clock — a fixed input grid yields a fixed curve.
#[must_use]
pub fn capacity_curve(
    base_market: &MarketState,
    costs: &CostModel,
    imp: &ExitImpairment,
    mode: FillMode,
    terminal: &TerminalLossPolicy,
    landing: &LandingModel,
) -> Vec<CapacityPoint> {
    CAPACITY_GRID_LAMPORTS
        .iter()
        .map(|&size| {
            let market = MarketState {
                notional_lamports: size,
                ..*base_market
            };
            let fill = simulate_fill(&market, costs, imp, mode, terminal);
            let expectancy = fill.net_pnl_lamports;
            let drawdown = if expectancy < 0 { -expectancy } else { 0 };
            let terminal_exposure = if fill.unexitable {
                fill.entry_cost_lamports as i128 - fill.exit_proceeds_lamports
            } else {
                0
            };
            CapacityPoint {
                size_lamports: size,
                price_impact_bps: fill.entry_impact_bps,
                landing_prob_bps: landing.landing_bps(size, market.depth_lamports),
                expectancy_lamports: expectancy,
                drawdown_lamports: drawdown,
                terminal_loss_exposure_lamports: terminal_exposure,
            }
        })
        .collect()
}
