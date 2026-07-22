//! `edge_decomposition` — per-trade edge attribution and aggregate decomposition
//! (constitution §50).
//!
//! §50 requires the frozen evaluator to decompose reconciled PnL into its edge
//! sources — selection edge, EntryMode contribution, latency decay, price
//! impact, fees/tips/route cost, failed-attempt/retry cost, slippage,
//! exit-timing, sellability loss — plus an *unattributed residual*, each labelled
//! `measured` / `estimated` / `assumed` / `unknown` with an uncertainty band.
//! Reconciliation elsewhere yields only a scalar realized net and a single
//! discrepancy; this module is the post-hoc attribution the spec calls for.
//!
//! §22: integer-only lamports in `i128`; uncertainty half-widths in `u128`. The
//! residual is defined so the decomposition is exact: `residual = realized_net −
//! Σ attributed`. No floats, no wall-clock, no RNG; deterministic in the inputs.

/// Attribution quality of one edge component — how it was known.
///
/// Ordered by *confidence*: `Measured` is the most trustworthy and `Unknown` the
/// least. [`Attribution::confidence_rank`] exposes that order so an aggregate can
/// report the least-confident contributor to any component.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Attribution {
    /// Directly measured from reconciled fills / receipts.
    Measured,
    /// Estimated from a calibrated model.
    Estimated,
    /// Assumed from a fixed policy constant.
    Assumed,
    /// Not attributable — folded into the residual's meaning.
    Unknown,
}

impl Attribution {
    /// Confidence rank: `0` = most confident (`Measured`), `3` = least
    /// (`Unknown`). Used to compute the worst (least-confident) contributor.
    pub fn confidence_rank(self) -> u8 {
        match self {
            Attribution::Measured => 0,
            Attribution::Estimated => 1,
            Attribution::Assumed => 2,
            Attribution::Unknown => 3,
        }
    }

    /// Index into a `[_; 4]` quality-count array.
    fn index(self) -> usize {
        self.confidence_rank() as usize
    }
}

/// The nine attributable edge components of §50 (the residual is separate).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EdgeComponent {
    /// Alpha from picking this token/timing vs a neutral baseline.
    SelectionEdge,
    /// Contribution of the chosen EntryMode (sniper/scale/etc.).
    EntryMode,
    /// Value lost to latency between decision and fill.
    LatencyDecay,
    /// Price impact of the order against the book.
    PriceImpact,
    /// Fees + tips + route cost.
    FeesTipsRoute,
    /// Cost of failed attempts / retries attributable to this trade.
    FailedRetry,
    /// Slippage vs the quoted price.
    Slippage,
    /// Value gained/lost from exit timing vs a reference exit.
    ExitTiming,
    /// Loss from impaired sellability (illiquid / taxed exit).
    SellabilityLoss,
}

impl EdgeComponent {
    /// All nine components in stable order (matches the per-trade array index).
    pub const ALL: [EdgeComponent; 9] = [
        EdgeComponent::SelectionEdge,
        EdgeComponent::EntryMode,
        EdgeComponent::LatencyDecay,
        EdgeComponent::PriceImpact,
        EdgeComponent::FeesTipsRoute,
        EdgeComponent::FailedRetry,
        EdgeComponent::Slippage,
        EdgeComponent::ExitTiming,
        EdgeComponent::SellabilityLoss,
    ];
}

/// One attributed component of a single trade's PnL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComponentValue {
    /// Signed lamports this component contributed (costs are negative).
    pub lamports: i128,
    /// How this value was known.
    pub quality: Attribution,
    /// Symmetric uncertainty half-width, lamports (`0` for exact/measured).
    pub uncertainty_lamports: u128,
}

impl ComponentValue {
    /// A measured, exact component.
    pub fn measured(lamports: i128) -> Self {
        ComponentValue {
            lamports,
            quality: Attribution::Measured,
            uncertainty_lamports: 0,
        }
    }

    /// An estimated component with an uncertainty half-width.
    pub fn estimated(lamports: i128, uncertainty: u128) -> Self {
        ComponentValue {
            lamports,
            quality: Attribution::Estimated,
            uncertainty_lamports: uncertainty,
        }
    }

    /// A zero, unknown component (no attribution).
    pub fn unknown() -> Self {
        ComponentValue {
            lamports: 0,
            quality: Attribution::Unknown,
            uncertainty_lamports: 0,
        }
    }
}

/// One reconciled trade with its per-component attribution (§50 input).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PerTradeEdge {
    /// The reconciled realized net PnL, lamports (ground truth).
    pub realized_net_lamports: i128,
    /// Attributed components, indexed to match [`EdgeComponent::ALL`].
    pub components: [ComponentValue; 9],
}

impl PerTradeEdge {
    /// Build from a realized net and an array of components.
    pub fn new(realized_net_lamports: i128, components: [ComponentValue; 9]) -> Self {
        PerTradeEdge {
            realized_net_lamports,
            components,
        }
    }
}

/// The exact per-trade decomposition, including the balancing residual.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TradeDecomposition {
    /// Sum of the nine attributed components, lamports.
    pub attributed_lamports: i128,
    /// Unattributed residual = `realized_net − attributed`, lamports.
    pub residual_lamports: i128,
    /// Total propagated uncertainty half-width across components, lamports.
    pub total_uncertainty_lamports: u128,
}

/// Decompose a single trade into attributed sum + exact residual (§50).
///
/// The residual is defined to make the identity exact:
/// `attributed + residual == realized_net`. Uncertainty half-widths add
/// linearly (a conservative, correlation-agnostic band). Deterministic.
pub fn decompose_trade(trade: &PerTradeEdge) -> TradeDecomposition {
    let mut attributed: i128 = 0;
    let mut uncertainty: u128 = 0;
    for c in &trade.components {
        attributed = attributed
            .checked_add(c.lamports)
            .expect("decompose_trade: attributed overflow");
        uncertainty = uncertainty
            .checked_add(c.uncertainty_lamports)
            .expect("decompose_trade: uncertainty overflow");
    }
    let residual = trade
        .realized_net_lamports
        .checked_sub(attributed)
        .expect("decompose_trade: residual overflow");
    TradeDecomposition {
        attributed_lamports: attributed,
        residual_lamports: residual,
        total_uncertainty_lamports: uncertainty,
    }
}

/// Aggregate attribution for one component across many trades.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComponentAgg {
    /// Which component.
    pub component: EdgeComponent,
    /// Summed lamports across trades.
    pub sum_lamports: i128,
    /// Summed uncertainty half-width across trades, lamports.
    pub uncertainty_lamports: u128,
    /// Least-confident quality that contributed a non-zero value to this
    /// component (the aggregate is only as trustworthy as its weakest input).
    pub worst_quality: Attribution,
    /// Count of contributing values by quality, indexed by
    /// [`Attribution::confidence_rank`] (`[measured, estimated, assumed, unknown]`).
    pub quality_counts: [u32; 4],
}

/// Aggregate edge attribution across a book of reconciled trades (§50).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EdgeAttribution {
    /// Number of trades aggregated.
    pub n: u32,
    /// Per-component aggregates, in [`EdgeComponent::ALL`] order.
    pub components: [ComponentAgg; 9],
    /// Total unattributed residual across all trades, lamports.
    pub residual_lamports: i128,
    /// Sum of |residual| across trades — the gross unexplained magnitude, a
    /// sanity signal separate from the net residual (which can cancel).
    pub residual_abs_sum_lamports: i128,
    /// Total realized net across all trades, lamports (== attributed + residual).
    pub realized_net_lamports: i128,
}

/// Aggregate per-trade decompositions into a book-level edge attribution (§50).
///
/// Sums each component across trades, propagates uncertainty linearly, records
/// the least-confident contributing quality per component, and carries the total
/// (and gross-absolute) unattributed residual. The identity
/// `Σ component.sum + residual == realized_net` holds exactly. Deterministic;
/// an empty book yields a fully-zeroed attribution with `worst_quality` unknown.
pub fn aggregate_edge(trades: &[PerTradeEdge]) -> EdgeAttribution {
    let mut sums = [0i128; 9];
    let mut uncerts = [0u128; 9];
    let mut worst = [Attribution::Measured; 9];
    let mut counts = [[0u32; 4]; 9];
    let mut any = [false; 9];
    let mut residual: i128 = 0;
    let mut residual_abs: i128 = 0;
    let mut realized: i128 = 0;

    for t in trades {
        let d = decompose_trade(t);
        residual = residual
            .checked_add(d.residual_lamports)
            .expect("aggregate_edge: residual overflow");
        residual_abs = residual_abs
            .checked_add(d.residual_lamports.abs())
            .expect("aggregate_edge: residual_abs overflow");
        realized = realized
            .checked_add(t.realized_net_lamports)
            .expect("aggregate_edge: realized overflow");

        for (i, c) in t.components.iter().enumerate() {
            sums[i] = sums[i]
                .checked_add(c.lamports)
                .expect("aggregate_edge: component sum overflow");
            uncerts[i] = uncerts[i]
                .checked_add(c.uncertainty_lamports)
                .expect("aggregate_edge: component uncertainty overflow");
            counts[i][c.quality.index()] += 1;
            // Track worst (least-confident) quality among non-zero contributors.
            if c.lamports != 0 || c.quality == Attribution::Unknown {
                if !any[i] || c.quality.confidence_rank() > worst[i].confidence_rank() {
                    worst[i] = c.quality;
                }
                any[i] = true;
            }
        }
    }

    let components: [ComponentAgg; 9] = std::array::from_fn(|i| ComponentAgg {
        component: EdgeComponent::ALL[i],
        sum_lamports: sums[i],
        uncertainty_lamports: uncerts[i],
        worst_quality: if any[i] {
            worst[i]
        } else {
            Attribution::Unknown
        },
        quality_counts: counts[i],
    });

    EdgeAttribution {
        n: trades.len() as u32,
        components,
        residual_lamports: residual,
        residual_abs_sum_lamports: residual_abs,
        realized_net_lamports: realized,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_trade() -> PerTradeEdge {
        // Attributed sum = 100 + 50 - 30 - 20 - 40 - 5 - 10 + 15 - 25 = 35.
        // realized_net = 50 -> residual = 15.
        let comps = [
            ComponentValue::measured(100),     // SelectionEdge
            ComponentValue::estimated(50, 8),  // EntryMode
            ComponentValue::estimated(-30, 5), // LatencyDecay
            ComponentValue::measured(-20),     // PriceImpact
            ComponentValue::measured(-40),     // FeesTipsRoute
            ComponentValue::measured(-5),      // FailedRetry
            ComponentValue::estimated(-10, 3), // Slippage
            ComponentValue::estimated(15, 4),  // ExitTiming
            ComponentValue::measured(-25),     // SellabilityLoss
        ];
        PerTradeEdge::new(50, comps)
    }

    #[test]
    fn decompose_is_exact() {
        let t = sample_trade();
        let d = decompose_trade(&t);
        assert_eq!(d.attributed_lamports, 35);
        assert_eq!(d.residual_lamports, 15);
        assert_eq!(
            d.attributed_lamports + d.residual_lamports,
            t.realized_net_lamports
        );
        assert_eq!(d.total_uncertainty_lamports, 8 + 5 + 3 + 4);
    }

    #[test]
    fn aggregate_identity_holds() {
        let trades = vec![sample_trade(), sample_trade()];
        let a = aggregate_edge(&trades);
        assert_eq!(a.n, 2);
        let comp_sum: i128 = a.components.iter().map(|c| c.sum_lamports).sum();
        assert_eq!(comp_sum + a.residual_lamports, a.realized_net_lamports);
        assert_eq!(a.realized_net_lamports, 100);
        assert_eq!(a.residual_lamports, 30); // 15 * 2
        assert_eq!(a.residual_abs_sum_lamports, 30);
    }

    #[test]
    fn selection_edge_is_measured() {
        let a = aggregate_edge(&[sample_trade()]);
        let sel = a.components[0];
        assert_eq!(sel.component, EdgeComponent::SelectionEdge);
        assert_eq!(sel.sum_lamports, 100);
        assert_eq!(sel.worst_quality, Attribution::Measured);
        assert_eq!(sel.quality_counts, [1, 0, 0, 0]);
    }

    #[test]
    fn worst_quality_tracks_least_confident() {
        // One trade measured, one estimated for the same component -> Estimated.
        let mut t1 = sample_trade();
        t1.components[0] = ComponentValue::measured(10);
        let mut t2 = sample_trade();
        t2.components[0] = ComponentValue::estimated(20, 2);
        let a = aggregate_edge(&[t1, t2]);
        assert_eq!(a.components[0].worst_quality, Attribution::Estimated);
        assert_eq!(a.components[0].quality_counts, [1, 1, 0, 0]);
        assert_eq!(a.components[0].uncertainty_lamports, 2);
    }

    #[test]
    fn empty_book_is_zeroed() {
        let a = aggregate_edge(&[]);
        assert_eq!(a.n, 0);
        assert_eq!(a.residual_lamports, 0);
        assert_eq!(a.realized_net_lamports, 0);
        for c in &a.components {
            assert_eq!(c.sum_lamports, 0);
            assert_eq!(c.worst_quality, Attribution::Unknown);
            assert_eq!(c.quality_counts, [0, 0, 0, 0]);
        }
    }

    #[test]
    fn unknown_component_counted_even_when_zero() {
        let mut t = sample_trade();
        t.components[3] = ComponentValue::unknown();
        let a = aggregate_edge(&[t]);
        // PriceImpact now unknown -> counted as unknown, worst quality Unknown.
        assert_eq!(a.components[3].worst_quality, Attribution::Unknown);
        assert_eq!(a.components[3].quality_counts, [0, 0, 0, 1]);
    }
}
