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

/// The attribution bucket an excursion statistic is computed *over*: a lane plus the
/// setup archetype within it.
///
/// Excursion behaviour is archetype-conditional — a graduation play and an active-market
/// scalp have different excursion shapes — so pooling them produces a distribution that
/// describes no actual setup. Carrying the [`Lane`] inside the key keeps §48's
/// no-cross-lane-blending law true of these statistics as well.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArchetypeKey {
    lane: Lane,
    archetype_id: u16,
}

impl ArchetypeKey {
    /// The archetype `archetype_id` within `lane`.
    pub const fn new(lane: Lane, archetype_id: u16) -> Self {
        Self { lane, archetype_id }
    }

    /// A fixed key for tests and golden vectors.
    pub const fn test() -> Self {
        Self::new(Lane::Scalp, 0)
    }
}

/// One position's excursion record: how far the trade went in each direction while it was
/// open, and how much of that the exit ladder actually kept.
///
/// `mfe_bps`/`mae_bps` are unsigned magnitudes (maximum favourable / adverse excursion) —
/// the direction is carried by which field it is. `realized_bps` is signed because an exit
/// can give back more than it took. `authenticity_screened` records whether the price path
/// survived the wash/phantom-volume screen (criterion 107): an unscreened row is not
/// evidence about the market, it is evidence about someone printing to themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExcursionRow {
    key: ArchetypeKey,
    mfe_bps: u32,
    mae_bps: u32,
    realized_bps: i32,
    authenticity_screened: bool,
}

impl ExcursionRow {
    /// Construct an excursion row from already-measured components.
    pub const fn test(
        key: ArchetypeKey,
        mfe_bps: u32,
        mae_bps: u32,
        realized_bps: i32,
        authenticity_screened: bool,
    ) -> Self {
        Self {
            key,
            mfe_bps,
            mae_bps,
            realized_bps,
            authenticity_screened,
        }
    }
}

/// An order-statistic summary of a bps distribution, by nearest-rank selection on a sorted
/// copy — no interpolation, therefore no floats and no tie-break ambiguity (§22).
///
/// Quartiles rather than a mean because excursion distributions are heavy-tailed: a mean
/// MFE is a statement about the one trade that ran, not about the archetype.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quartiles {
    pub n: u32,
    pub q1_bps: u32,
    pub median_bps: u32,
    pub q3_bps: u32,
}

impl Quartiles {
    /// The absence of a distribution — no samples, so no order statistics exist.
    pub const fn missing() -> Self {
        Self {
            n: 0,
            q1_bps: 0,
            median_bps: 0,
            q3_bps: 0,
        }
    }

    /// True when no sample was included; the zeroed fields are absence, not measurement.
    pub const fn is_missing(&self) -> bool {
        self.n == 0
    }
}

/// The capture ratio, which genuinely does not exist when there was no favourable
/// excursion to capture.
///
/// Modelled as a distinct `Missing` state rather than a zero: "kept 0% of the move" and
/// "there was no move" are different findings about an archetype, and collapsing them
/// would let a lane with no opportunity read as a lane with a broken exit ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureBps {
    /// `sum(mfe_bps) == 0` — the ratio's denominator is zero and no value is reported.
    Missing,
    /// Realized fraction of the total favourable excursion, in bps.
    Bps(u32),
}

impl PartialEq<u32> for CaptureBps {
    fn eq(&self, other: &u32) -> bool {
        matches!(self, Self::Bps(v) if v == other)
    }
}

/// Per-archetype excursion report: the distributions, the capture ratio, and — kept in the
/// same struct on purpose — the count of rows that were thrown away for failing the
/// authenticity screen.
///
/// `excluded_unscreened` travels with the statistic because an archetype whose evidence is
/// mostly wash-printed is not described by the surviving rows; the exclusion count is how a
/// reader learns to distrust the numbers next to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MfeReport {
    pub n: u32,
    pub excluded_unscreened: u32,
    pub mfe_bps: Quartiles,
    pub mae_bps: Quartiles,
    pub capture_bps_of_mfe: CaptureBps,
}

impl MfeReport {
    /// True when no screened row was included for the requested archetype.
    pub const fn is_missing(&self) -> bool {
        self.n == 0
    }
}

/// Nearest-rank order statistic `num/den` of a non-empty ascending slice: index
/// `ceil(num * n / den) - 1`, all integer, so the same slice always selects the same
/// element on every machine.
fn nearest_rank(sorted: &[u32], num: usize, den: usize) -> u32 {
    let rank = (num * sorted.len()).div_ceil(den).max(1);
    sorted[rank - 1]
}

/// Quartiles of an ascending slice; empty input is [`Quartiles::missing()`].
fn quartiles(sorted: &[u32]) -> Quartiles {
    if sorted.is_empty() {
        return Quartiles::missing();
    }
    Quartiles {
        n: sorted.len() as u32,
        q1_bps: nearest_rank(sorted, 1, 4),
        median_bps: nearest_rank(sorted, 1, 2),
        q3_bps: nearest_rank(sorted, 3, 4),
    }
}

/// ev_mfe_capture — MFE/MAE distributions and capture efficiency for one archetype,
/// computed over authenticity-screened rows only (criterion 107).
///
/// Unscreened rows are excluded from every number here and counted in
/// `excluded_unscreened`: a wash-printed phantom excursion is arbitrarily large and free to
/// manufacture, so admitting one would let an attacker (or a self-dealing market) inflate
/// this archetype's apparent opportunity without ever offering a fill.
///
/// `capture = sum(max(realized_bps, 0)) * 10_000 / sum(mfe_bps)`, integer division. The
/// numerator clamps losses to zero rather than netting them: giveback is what `mae_bps`
/// reports, and letting it subtract here would conflate "exited late" with "never had the
/// move", which are different ladder defects. A zero denominator yields
/// [`CaptureBps::Missing`], never a fabricated 0.
pub fn mfe_capture(rows: &[ExcursionRow], key: ArchetypeKey) -> MfeReport {
    let mut excluded_unscreened = 0u32;
    let (mut mfe, mut mae) = (Vec::new(), Vec::new());
    let (mut mfe_sum, mut realized_sum) = (0u128, 0u128);
    for r in rows.iter().filter(|r| r.key == key) {
        if !r.authenticity_screened {
            excluded_unscreened += 1;
            continue;
        }
        mfe.push(r.mfe_bps);
        mae.push(r.mae_bps);
        mfe_sum += r.mfe_bps as u128;
        realized_sum += r.realized_bps.max(0) as u128;
    }
    let capture_bps_of_mfe = if mfe_sum == 0 {
        CaptureBps::Missing
    } else {
        CaptureBps::Bps(u32::try_from(realized_sum * 10_000 / mfe_sum).unwrap_or(u32::MAX))
    };
    let n = mfe.len() as u32;
    mfe.sort_unstable();
    mae.sort_unstable();
    MfeReport {
        n,
        excluded_unscreened,
        mfe_bps: quartiles(&mfe),
        mae_bps: quartiles(&mae),
        capture_bps_of_mfe,
    }
}
