//! `edge_attribution` — edge decomposition for trading strategies (Level 3, §56.12).
//!
//! Decomposes a strategy's total P&L into constituent "edge sources":
//! - **Entry edge**: profit attributable to entry timing (buying below fair value).
//! - **Exit edge**: profit attributable to exit timing (selling above fair value).
//! - **Sizing edge**: profit attributable to position sizing (larger size on higher-confidence setups).
//! - **Selection edge**: profit attributable to token selection (picking the right mints).
//! - **Residual**: unexplained P&L (noise, slippage, fees, market impact).
//!
//! This tells us *why* a strategy makes money, not just *that* it does.
//! If the edge is concentrated in one source (e.g., entry timing), we know
//! what to protect if market conditions change. If it's spread across all
//! sources, the strategy is more robust.
//!
//! ## Method
//!
//! We use a Shapley-value-style decomposition (no floats, integer-only):
//! - Each trade is decomposed by comparing actual entry/exit/sizing vs a
//!   "neutral baseline" (e.g., TWAP entry, midpoint exit, equal-weight sizing).
//! - The decomposition is additive: entry_edge + exit_edge + sizing_edge +
//!   selection_edge + residual = total_pnl.
//!
//! ## Constitution compliance
//! - §56.12: Edge decomposition for attribution
//! - §22: Integer-only, no floats
//! - §16: No look-ahead — only uses past trade data

// ============================================================================
// Edge Attribution Types
// ============================================================================

/// One decomposed trade. Each field is the P&L contribution in lamports.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct EdgeDecomposition {
    /// Entry timing edge: actual_entry_pnl - twap_entry_pnl (lamports).
    /// Positive = bought better than TWAP. Negative = bought worse.
    pub entry_edge_lamports: i64,
    /// Exit timing edge: actual_exit_pnl - midpoint_exit_pnl (lamports).
    /// Positive = sold better than midpoint. Negative = sold worse.
    pub exit_edge_lamports: i64,
    /// Sizing edge: (actual_size - equal_weight_size) * per_unit_pnl (lamports).
    /// Positive = larger size on profitable trades, smaller on unprofitable.
    pub sizing_edge_lamports: i64,
    /// Selection edge: token selection vs random baseline (lamports).
    /// Positive = picked better-than-random mints. Negative = worse.
    pub selection_edge_lamports: i64,
    /// Residual (unexplained): total_pnl - (entry + exit + sizing + selection).
    /// Should be small; large residual means we're missing something.
    pub residual_lamports: i64,
    /// Total P&L for this trade (lamports). Sum of all edges.
    pub total_pnl_lamports: i64,
}

impl EdgeDecomposition {
    /// Verify that the decomposition is additive (edges sum to total P&L).
    /// This is a self-consistency check.
    #[must_use]
    pub fn is_additive(&self) -> bool {
        self.entry_edge_lamports
            .saturating_add(self.exit_edge_lamports)
            .saturating_add(self.sizing_edge_lamports)
            .saturating_add(self.selection_edge_lamports)
            .saturating_add(self.residual_lamports)
            == self.total_pnl_lamports
    }
}

/// Aggregated edge attribution across many trades for a strategy type.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct EdgeAttribution {
    /// Sum of entry edges across all trades (lamports).
    pub total_entry_edge: i64,
    /// Sum of exit edges across all trades (lamports).
    pub total_exit_edge: i64,
    /// Sum of sizing edges across all trades (lamports).
    pub total_sizing_edge: i64,
    /// Sum of selection edges across all trades (lamports).
    pub total_selection_edge: i64,
    /// Sum of residuals across all trades (lamports).
    pub total_residual: i64,
    /// Total P&L across all trades (lamports).
    pub total_pnl: i64,
    /// Number of trades decomposed.
    pub n_trades: u64,
    /// Strategy type id this attribution applies to.
    pub strategy_type_id: u64,
}

impl EdgeAttribution {
    /// Create an empty attribution for a strategy type.
    #[must_use]
    pub fn new(strategy_type_id: u64) -> Self {
        Self {
            strategy_type_id,
            ..Default::default()
        }
    }

    /// Add a single trade's decomposition to the aggregation.
    pub fn add(&mut self, d: &EdgeDecomposition) {
        self.total_entry_edge = self.total_entry_edge.saturating_add(d.entry_edge_lamports);
        self.total_exit_edge = self.total_exit_edge.saturating_add(d.exit_edge_lamports);
        self.total_sizing_edge = self.total_sizing_edge.saturating_add(d.sizing_edge_lamports);
        self.total_selection_edge =
            self.total_selection_edge.saturating_add(d.selection_edge_lamports);
        self.total_residual = self.total_residual.saturating_add(d.residual_lamports);
        self.total_pnl = self.total_pnl.saturating_add(d.total_pnl_lamports);
        self.n_trades += 1;
    }

    /// Decompose a trade and add it to the aggregation in one step.
    /// Convenience method that calls `decompose_trade` then `add`.
    pub fn add_trade(
        &mut self,
        actual_entry_lamports: i64,
        twap_entry_lamports: i64,
        actual_exit_lamports: i64,
        midpoint_exit_lamports: i64,
        actual_size_units: i64,
        equal_weight_size_units: i64,
        per_unit_pnl_lamports: i64,
        selection_pnl_lamports: i64,
    ) {
        let d = decompose_trade(
            actual_entry_lamports,
            twap_entry_lamports,
            actual_exit_lamports,
            midpoint_exit_lamports,
            actual_size_units,
            equal_weight_size_units,
            per_unit_pnl_lamports,
            selection_pnl_lamports,
        );
        self.add(&d);
    }

    /// Edge concentration: what fraction of total edge comes from the
    /// dominant source. Returns (source_tag, bps) where bps is the
    /// fraction in basis points (10000 = 100%).
    ///
    /// High concentration (>8000 bps from one source) = fragile edge.
    /// Low concentration (<4000 bps from any source) = robust, diversified edge.
    #[must_use]
    pub fn dominant_source(&self) -> (EdgeSource, u32) {
        let abs_entry = self.total_entry_edge.unsigned_abs();
        let abs_exit = self.total_exit_edge.unsigned_abs();
        let abs_sizing = self.total_sizing_edge.unsigned_abs();
        let abs_selection = self.total_selection_edge.unsigned_abs();

        let total_abs = abs_entry
            .saturating_add(abs_exit)
            .saturating_add(abs_sizing)
            .saturating_add(abs_selection);

        if total_abs == 0 {
            return (EdgeSource::Residual, 0);
        }

        let mut best = (EdgeSource::Entry, abs_entry);
        if abs_exit > best.1 {
            best = (EdgeSource::Exit, abs_exit);
        }
        if abs_sizing > best.1 {
            best = (EdgeSource::Sizing, abs_sizing);
        }
        if abs_selection > best.1 {
            best = (EdgeSource::Selection, abs_selection);
        }

        let concentration_bps = ((best.1 as u128 * 10_000) / total_abs as u128) as u32;
        (best.0, concentration_bps)
    }

    /// Edge robustness score in bps. Higher = more diversified (robust).
    /// 0 = all edge from one source (fragile). 10000 = perfectly spread.
    /// Computed as: 10000 - dominant_concentration_bps.
    #[must_use]
    pub fn robustness_bps(&self) -> u32 {
        10_000_u32.saturating_sub(self.dominant_source().1)
    }

    /// Edge fragility: true if >80% of edge comes from one source.
    #[must_use]
    pub fn is_fragile(&self) -> bool {
        self.dominant_source().1 >= 8000
    }

    /// Per-trade average P&L in lamports (integer division).
    #[must_use]
    pub fn avg_pnl_per_trade(&self) -> i64 {
        if self.n_trades == 0 {
            return 0;
        }
        self.total_pnl / self.n_trades as i64
    }
}

/// Which edge source dominates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeSource {
    Entry,
    Exit,
    Sizing,
    Selection,
    Residual,
}

impl EdgeSource {
    #[must_use]
    pub fn tag(&self) -> &'static str {
        match self {
            EdgeSource::Entry => "entry",
            EdgeSource::Exit => "exit",
            EdgeSource::Sizing => "sizing",
            EdgeSource::Selection => "selection",
            EdgeSource::Residual => "residual",
        }
    }
}

// ============================================================================
// Decomposition Functions
// ============================================================================

/// Decompose a single trade into edge components.
///
/// # Arguments
/// - `actual_entry_lamports`: the actual buy price paid (in lamports).
/// - `twap_entry_lamports`: the TWAP (time-weighted average price) at entry.
/// - `actual_exit_lamports`: the actual sell price received (in lamports).
/// - `midpoint_exit_lamports`: the midpoint price at exit time.
/// - `actual_size_units`: the actual position size in units (tokens).
/// - `equal_weight_size_units`: the equal-weight (neutral) position size.
/// - `per_unit_pnl_lamports`: P&L per unit (token) for this trade.
/// - `selection_pnl_lamports`: P&L attributable to token selection vs random.
///
/// # Returns
/// An `EdgeDecomposition` where all edges sum to the total P&L.
///
/// The decomposition is:
/// - entry_edge = (twap_entry - actual_entry) * actual_size
///   (buying below TWAP = positive edge)
/// - exit_edge = (actual_exit - midpoint_exit) * actual_size
///   (selling above midpoint = positive edge)
/// - sizing_edge = (actual_size - equal_weight_size) * per_unit_pnl
///   (larger size on profitable trades = positive edge)
/// - selection_edge = selection_pnl (passed in, from token-picking analysis)
/// - residual = total_pnl - (entry + exit + sizing + selection)
/// - total_pnl = (actual_exit - actual_entry) * actual_size
#[must_use]
pub fn decompose_trade(
    actual_entry_lamports: i64,
    twap_entry_lamports: i64,
    actual_exit_lamports: i64,
    midpoint_exit_lamports: i64,
    actual_size_units: i64,
    equal_weight_size_units: i64,
    per_unit_pnl_lamports: i64,
    selection_pnl_lamports: i64,
) -> EdgeDecomposition {
    // Entry edge: buying below TWAP is good. If actual < twap, edge is positive.
    // entry_edge = (twap - actual) * size
    let entry_edge = twap_entry_lamports
        .saturating_sub(actual_entry_lamports)
        .saturating_mul(actual_size_units);

    // Exit edge: selling above midpoint is good. If actual > midpoint, edge is positive.
    // exit_edge = (actual_exit - midpoint) * size
    let exit_edge = actual_exit_lamports
        .saturating_sub(midpoint_exit_lamports)
        .saturating_mul(actual_size_units);

    // Sizing edge: larger size on profitable trades = positive edge.
    // sizing_edge = (actual_size - equal_weight) * per_unit_pnl
    let sizing_edge = actual_size_units
        .saturating_sub(equal_weight_size_units)
        .saturating_mul(per_unit_pnl_lamports);

    // Selection edge: passed in directly.
    let selection_edge = selection_pnl_lamports;

    // Total P&L: (exit - entry) * size
    let total_pnl = actual_exit_lamports
        .saturating_sub(actual_entry_lamports)
        .saturating_mul(actual_size_units);

    // Residual: total - (entry + exit + sizing + selection)
    let explained = entry_edge
        .saturating_add(exit_edge)
        .saturating_add(sizing_edge)
        .saturating_add(selection_edge);
    let residual = total_pnl.saturating_sub(explained);

    EdgeDecomposition {
        entry_edge_lamports: entry_edge,
        exit_edge_lamports: exit_edge,
        sizing_edge_lamports: sizing_edge,
        selection_edge_lamports: selection_edge,
        residual_lamports: residual,
        total_pnl_lamports: total_pnl,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decomposition_is_additive() {
        let d = decompose_trade(
            90_000, // actual entry (below TWAP = good entry)
            100_000, // TWAP
            110_000, // actual exit (above midpoint = good exit)
            105_000, // midpoint
            100,    // actual size (larger than equal weight = good sizing on profitable trade)
            50,     // equal weight size
            200,    // per unit pnl (profitable)
            500,    // selection pnl
        );
        assert!(d.is_additive());
        assert!(d.entry_edge_lamports > 0); // bought below TWAP
        assert!(d.exit_edge_lamports > 0);  // sold above midpoint
        assert!(d.sizing_edge_lamports > 0); // larger size on profitable trade
        assert!(d.selection_edge_lamports > 0);
        assert!(d.total_pnl_lamports > 0);
    }

    #[test]
    fn bad_entry_produces_negative_entry_edge() {
        let d = decompose_trade(
            110_000, // actual entry (above TWAP = bad entry)
            100_000, // TWAP
            110_000, // actual exit
            110_000, // midpoint (same as exit = neutral exit)
            100,    // size
            100,    // equal weight (same = neutral sizing)
            0,      // per unit pnl (neutral)
            0,      // no selection edge
        );
        assert!(d.entry_edge_lamports < 0); // bought above TWAP
        assert_eq!(d.exit_edge_lamports, 0); // sold at midpoint
        assert_eq!(d.sizing_edge_lamports, 0); // neutral sizing
        assert!(d.is_additive());
    }

    #[test]
    fn aggregation_sums_correctly() {
        let mut attr = EdgeAttribution::new(42);
        attr.add_trade(
            90_000, 100_000, 110_000, 105_000,
            100, 50, 200, 500,
        );
        attr.add_trade(
            80_000, 100_000, 120_000, 110_000,
            100, 50, 400, 800,
        );
        assert_eq!(attr.n_trades, 2);
        assert!(attr.total_pnl > 0);
        assert!(attr.total_entry_edge > 0);
        assert!(attr.total_exit_edge > 0);
    }

    #[test]
    fn dominant_source_identifies_entry() {
        let mut attr = EdgeAttribution::new(1);
        // Only entry edge is positive, everything else is zero.
        attr.add_trade(
            50_000, 100_000, 100_000, 100_000,
            100, 100, 0, 0,
        );
        let (source, bps) = attr.dominant_source();
        assert_eq!(source, EdgeSource::Entry);
        assert!(bps > 8000); // highly concentrated
        assert!(attr.is_fragile());
    }

    #[test]
    fn robust_when_edges_spread() {
        let mut attr = EdgeAttribution::new(1);
        // All four edge sources contribute equally.
        attr.add_trade(
            90, 100, 110, 100,
            10, 5, 20, 50,
        );
        // entry = (100-90)*10 = 100
        // exit = (110-100)*10 = 100
        // sizing = (10-5)*20 = 100
        // selection = 50
        // Total abs = 350. Entry = 100/350 = 28.6% → ~2857 bps
        let (_, bps) = attr.dominant_source();
        assert!(bps < 5000); // not concentrated → robust
        assert!(!attr.is_fragile());
        assert!(attr.robustness_bps() > 5000);
    }

    #[test]
    fn avg_pnl_per_trade() {
        let mut attr = EdgeAttribution::new(1);
        attr.add_trade(0, 0, 1000, 0, 1, 1, 0, 0); // total pnl = 1000
        attr.add_trade(0, 0, 3000, 0, 1, 1, 0, 0); // total pnl = 3000
        assert_eq!(attr.avg_pnl_per_trade(), 2000);
    }

    #[test]
    fn empty_attribution_has_zero_pnl() {
        let attr = EdgeAttribution::new(1);
        assert_eq!(attr.total_pnl, 0);
        assert_eq!(attr.n_trades, 0);
        assert_eq!(attr.avg_pnl_per_trade(), 0);
    }

    #[test]
    fn losing_trade_decomposition() {
        let d = decompose_trade(
            100_000, // actual entry (at TWAP = neutral entry)
            100_000, // TWAP
            80_000,  // actual exit (below midpoint = bad exit)
            90_000,  // midpoint
            100,    // size
            100,    // equal weight (neutral sizing)
            -200,   // per unit pnl (losing trade)
            0,      // no selection edge
        );
        assert_eq!(d.entry_edge_lamports, 0); // neutral entry
        assert!(d.exit_edge_lamports < 0);   // sold below midpoint
        assert!(d.total_pnl_lamports < 0);   // losing trade
        assert!(d.is_additive());
    }

    #[test]
    fn residual_captures_unexplained() {
        // Create a decomposition where edges don't naturally sum to total.
        // The residual should capture the difference.
        let d = decompose_trade(
            100, 100, 200, 100,
            10, 10,
            5,   // per unit pnl
            300, // selection pnl (deliberately large)
        );
        // entry = (100-100)*10 = 0
        // exit = (200-100)*10 = 1000
        // sizing = (10-10)*5 = 0
        // selection = 300
        // total = (200-100)*10 = 1000
        // residual = 1000 - (0 + 1000 + 0 + 300) = -300
        assert_eq!(d.entry_edge_lamports, 0);
        assert_eq!(d.exit_edge_lamports, 1000);
        assert_eq!(d.sizing_edge_lamports, 0);
        assert_eq!(d.selection_edge_lamports, 300);
        assert_eq!(d.residual_lamports, -300);
        assert!(d.is_additive());
    }
}
