//! evaluator_stats — implemented leaf-by-leaf against the dossier property tests.
//! Functions are added here by the build; this skeleton only establishes the module.

/// The attribution boundary every evaluator statistic is computed *within*.
///
/// §48's objective law forbids blending PnL across the independently attributed,
/// independently validated setup families of the lifecycle: the preserved early-entry
/// family (CreationSniper/EarlyConfirmation), graduation plays (GraduationTransition),
/// and active-market scalps (the ActiveMarketScalp lane). Making the lane a required
/// argument of every aggregate is how that law is enforced by construction rather than
/// by remembering to filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Lane {
    /// Extremely early low-cap entries — the preserved early-entry family.
    Early,
    /// Graduation/migration plays.
    Graduation,
    /// Active-market scalps.
    Scalp,
}

/// One reconciled trade: a closed round trip whose every cost class has been matched to
/// on-chain reality, not to the intent that was submitted.
///
/// `gross_lamports` is `proceeds - cost_basis` and is signed — a losing trade is a
/// negative gross, not a missing one. The three cost fields are unsigned because a cost
/// can never be a credit; `failed_attempt_lamports` is the fixed cost of the attempts
/// that landed nothing, which is charged to the trade that eventually landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconTrade {
    lane: Lane,
    gross_lamports: i128,
    fee_lamports: u128,
    tip_lamports: u128,
    failed_attempt_lamports: u128,
}

impl ReconTrade {
    /// Construct a reconciled trade from already-reconciled components.
    pub const fn test(
        lane: Lane,
        gross_lamports: i128,
        fee_lamports: u128,
        tip_lamports: u128,
        failed_attempt_lamports: u128,
    ) -> Self {
        Self {
            lane,
            gross_lamports,
            fee_lamports,
            tip_lamports,
            failed_attempt_lamports,
        }
    }
}

/// Reconciled net-SOL aggregate for one lane, with the cost classes kept separable so a
/// negative result can be attributed rather than merely reported.
///
/// Carries its own sample size: `n == 0` is [`NetSol::missing()`], which is a distinct
/// state from "zero net SOL over some trades". A statistic that cannot tell those apart
/// lets an empty slice masquerade as a break-even lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetSol {
    pub net_lamports: i128,
    pub gross_lamports: i128,
    pub fees: u128,
    pub tips: u128,
    pub failed_costs: u128,
    pub n: u32,
}

impl NetSol {
    /// The absence of data — no trades were included, so no statistic exists.
    pub const fn missing() -> Self {
        Self {
            net_lamports: 0,
            gross_lamports: 0,
            fees: 0,
            tips: 0,
            failed_costs: 0,
            n: 0,
        }
    }

    /// True when no trade was included; the zeroed fields are absence, not measurement.
    pub const fn is_missing(&self) -> bool {
        self.n == 0
    }
}

/// ev_net_sol — reconciled net-SOL aggregation for `lane`, every cost class included.
///
/// `net = gross - fees - tips - failed_attempt_costs`, exactly, in i128 lamports. The
/// i128 accumulators are the overflow strategy: the lamport supply is far inside u64, so
/// no honest trade set can approach the i128 range, and the debug assertion below plus
/// the crate's `overflow-checks = true` (kept on even in release for money math, §22)
/// turn any violation of that premise into a stop rather than a wrapped number.
///
/// Trades outside `lane` are not counted anywhere in the result — they are another
/// lane's evidence (§48), and `n` reports only what was included.
pub fn net_sol(trades: &[ReconTrade], lane: Lane) -> NetSol {
    let mut s = NetSol::missing();
    for t in trades.iter().filter(|t| t.lane == lane) {
        s.gross_lamports += t.gross_lamports;
        s.fees += t.fee_lamports;
        s.tips += t.tip_lamports;
        s.failed_costs += t.failed_attempt_lamports;
        s.n += 1;
    }
    if s.n == 0 {
        return NetSol::missing();
    }
    let costs = s.fees + s.tips + s.failed_costs;
    debug_assert!(
        costs <= i128::MAX as u128,
        "cost total outside i128 headroom"
    );
    s.net_lamports = s.gross_lamports - costs as i128;
    s
}
