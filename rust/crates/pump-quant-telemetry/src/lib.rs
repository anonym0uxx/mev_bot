//! `pump-quant-telemetry` — the live book's integer telemetry and its alert surface.
//!
//! WHY THIS EXISTS. The daemon books lamports into two account buckets
//! (`bankroll_realized`, `bankroll_committed`) and holds a set of open positions. Three
//! questions get asked of that state while the bot runs — how far has equity fallen from
//! its peak, is the book still above the operator's floor, and how many positions are
//! actually deployed — and every one of them has to be answered by the SAME integer
//! definition the sizing law already uses, or the pager and the engine disagree about the
//! same number.
//!
//! ONE INTEGER DEFINITION PER NUMBER. Nothing here is computed in floating point. A
//! drawdown rendered as a percentage and a drawdown rendered as basis points would
//! eventually disagree at the third decimal, and telemetry that disagrees with itself is
//! worse than no telemetry at all.
//!
//! THE ALERT RULE (operator's standing order). A page is for a REAL FAULT only. An idle
//! bot — nothing deployed, nothing happening — is not a fault, and a page on a quiet
//! system trains the operator to ignore the pager, which is how a real breach gets missed.
//! [`alerts`] therefore returns an EMPTY vector for a healthy book and equally for a
//! merely idle one; idleness can never produce an alert on its own. Only a floor breach or
//! a measured drawdown band does.
//!
//! Every threshold here is a named, documented constant. There are no magic numbers in a
//! pager: an unexplained `1_500` in an alert path is an alert path nobody can audit.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// Basis-point denominator: one basis point is one ten-thousandth.
///
/// Pinned as a named constant so every ratio in this crate divides by the same number.
/// Two literal `10_000`s in two places are two chances to scale one of them differently
/// and silently report a drawdown that is off by a decimal point.
pub const BPS_DENOMINATOR: u32 = 10_000;

/// Drawdown (bps from the equity peak) at which the band becomes
/// [`BandHealth::Degraded`].
///
/// Rationale: this is deliberately NOT a new number — it is the same 1_500 bp tier the
/// engine's drawdown ratchet already acts on (`dd_tier1_bp`: −15% halves the per-position
/// fraction). When the ratchet is throttling the book, telemetry must call the book
/// Degraded, because by the operator's standing rules a throttled book is the first
/// visible sign that capital is being defended. One tier, two readers, no divergence.
///
/// The boundary is INCLUSIVE: a drawdown of exactly 1_500 bps is Degraded, not Healthy.
pub const DEGRADED_DRAWDOWN_BPS: u32 = 1_500;

/// Drawdown (bps from the equity peak) at which the band becomes
/// [`BandHealth::Critical`] and [`alerts`] pages.
///
/// Rationale: mirrored from the engine's second ratchet tier (`dd_tier2_bp`: −30% quarter
/// the per-position fraction). A drawdown this deep is a survival-mode event and is the
/// first point at which the operator's drawdown-floor rule expects the pager to have
/// already spoken. Below this and at or above [`DEGRADED_DRAWDOWN_BPS`] the book is
/// Degraded and reported without paging; below [`DEGRADED_DRAWDOWN_BPS`] it is Healthy.
///
/// The boundary is INCLUSIVE: exactly 3_000 bps is Critical.
pub const CRITICAL_DRAWDOWN_BPS: u32 = 3_000;

/// The live book's account state, exactly as the daemon holds it.
///
/// `equity` is defined as `bankroll_realized + bankroll_committed`, floored at 0: the two
/// buckets are signed (a losing book is negative in either), but a book cannot hold a
/// negative quantity of lamports, so the equity floored at zero is the reported number.
/// Committed lamports are still capital — they are deployed, not lost — which is why
/// equity, not realized alone, drives the drawdown band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountState {
    /// Realized lamports banked. Signed: a losing book is negative.
    pub bankroll_realized_lamports: i64,
    /// Lamports currently committed to open positions. Signed.
    pub bankroll_committed_lamports: i64,
    /// Positions with `size_lamports` deployed. A position counts as open only while it
    /// has size in it, so a book that has closed everything reports zero.
    pub open_positions: u32,
    /// High-water mark of equity seen so far, lamports. Zero means no peak is known yet
    /// (a fresh book, or one whose peak has not been established) — see
    /// [`TelemetrySnapshot::observe`].
    pub peak_equity_lamports: u64,
    /// The operator's drawdown floor, lamports. Equity strictly below this is a breach.
    pub floor_lamports: u64,
}

impl AccountState {
    /// Equity: `bankroll_realized + bankroll_committed`, floored at zero.
    ///
    /// Both additions and the floor are saturating, so a book driven to the extreme
    /// negative end of `i64` (two deeply negative buckets) reports 0 rather than wrapping
    /// into a positive equity — the one way this number must never be wrong.
    pub fn equity_lamports(&self) -> u64 {
        let sum = self
            .bankroll_realized_lamports
            .saturating_add(self.bankroll_committed_lamports);
        if sum <= 0 {
            0
        } else {
            sum as u64
        }
    }
}

/// One observation of the book, with every number the operator asks for already derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelemetrySnapshot {
    /// Equity in lamports: realized + committed, floored at zero.
    pub equity_lamports: u64,
    /// Realized lamports carried through from the account state (signed).
    pub realized_lamports: i64,
    /// Committed lamports carried through from the account state (signed).
    pub committed_lamports: i64,
    /// Open positions carried through: positions with size deployed.
    pub open_positions: u32,
    /// Peak-to-current drawdown in basis points: `(peak - equity) * 10_000 / peak`,
    /// integer division, saturating, and exactly 0 when the peak is zero or when equity
    /// is at or above the peak.
    pub drawdown_bps: u32,
    /// True when equity is strictly below [`TelemetrySnapshot::floor_lamports`].
    pub floor_breached: bool,
    /// The operator's drawdown floor, lamports, carried through from the account state.
    ///
    /// It is carried rather than re-read because [`alerts`] takes a snapshot and NOTHING
    /// else: a floor-breach alert has to be self-describing, and a pager that went back to
    /// the account state for the floor it is reporting could pair a new floor with an old
    /// equity and page a breach that never happened.
    pub floor_lamports: u64,
    /// The band implied by [`TelemetrySnapshot::drawdown_bps`]; see
    /// [`BandHealth::from_drawdown_bps`].
    pub band_health: BandHealth,
    /// Signed change in equity since the previous observation, lamports. Zero when there
    /// is no previous observation — see [`TelemetrySnapshot::observe`].
    pub pnl_delta_lamports: i64,
}

impl TelemetrySnapshot {
    /// Observe the book, deriving every number from the account state alone.
    ///
    /// `prev` is the previous observation, if any, and is used for ONE thing: the signed
    /// PnL delta. With `prev == None` the delta is 0 — the first observation of a book has
    /// no predecessor, and inventing one (a zero baseline, say) would report the entire
    /// bankroll as a gain and page nobody's fault.
    ///
    /// Purely integer. `drawdown_bps` is `(peak - equity) * 10_000 / peak` in integer
    /// arithmetic, saturating: it is 0 when `peak == 0` (no peak is known, so no drawdown
    /// is claimed — a fresh book is not in drawdown), it is 0 when equity is at or above
    /// the peak, and it saturates at `u32::MAX` rather than wrapping if a pathological
    /// peak/value pair ever scales past the representable range. The subtraction is done
    /// in a wide unsigned type first, so no intermediate can overflow.
    pub fn observe(prev: Option<&TelemetrySnapshot>, state: &AccountState) -> Self {
        let equity = state.equity_lamports();
        let drawdown = drawdown_bps(state.peak_equity_lamports, equity);
        let pnl_delta_lamports = match prev {
            None => 0,
            Some(p) => {
                as_i64_saturating(equity).saturating_sub(as_i64_saturating(p.equity_lamports))
            }
        };
        Self {
            equity_lamports: equity,
            realized_lamports: state.bankroll_realized_lamports,
            committed_lamports: state.bankroll_committed_lamports,
            open_positions: state.open_positions,
            drawdown_bps: drawdown,
            floor_breached: equity < state.floor_lamports,
            floor_lamports: state.floor_lamports,
            band_health: BandHealth::from_drawdown_bps(drawdown),
            pnl_delta_lamports,
        }
    }

    /// True when the book is merely idle: nothing deployed, no drawdown, floor intact.
    ///
    /// This is a REPORTING predicate for dashboards and logs, never a gate on
    /// [`alerts`]. The alert surface must stay silent while idle, and it does so because
    /// idleness produces no fault condition — not because a special case skips the check.
    /// A short-circuit here would be one `if` away from suppressing a real breach.
    pub fn is_idle(&self) -> bool {
        self.open_positions == 0 && self.drawdown_bps == 0 && !self.floor_breached
    }
}

/// Where the book sits relative to the operator's drawdown bands.
///
/// The rule, in one place: a drawdown below [`DEGRADED_DRAWDOWN_BPS`] is
/// [`BandHealth::Healthy`]; at or above it (and below the critical tier) the book is
/// [`BandHealth::Degraded`]; at or above [`CRITICAL_DRAWDOWN_BPS`] it is
/// [`BandHealth::Critical`]. Both boundaries are inclusive.
///
/// The band is a pure function of the measured drawdown and of nothing else. It is
/// deliberately NOT forced to Critical by a floor breach: the floor is an independent
/// tripwire that fires in [`alerts`] on its own evidence, and folding it into the band
/// would make one number mean two things.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandHealth {
    /// Drawdown below [`DEGRADED_DRAWDOWN_BPS`]: the book is inside its bands.
    Healthy,
    /// Drawdown at or above [`DEGRADED_DRAWDOWN_BPS`] and below
    /// [`CRITICAL_DRAWDOWN_BPS`]: capital is being defended, report without paging.
    Degraded,
    /// Drawdown at or above [`CRITICAL_DRAWDOWN_BPS`]: survival-mode depth, page.
    Critical,
}

impl BandHealth {
    /// The band for a measured drawdown, by the documented inclusive boundaries.
    pub const fn from_drawdown_bps(drawdown_bps: u32) -> Self {
        if drawdown_bps >= CRITICAL_DRAWDOWN_BPS {
            BandHealth::Critical
        } else if drawdown_bps >= DEGRADED_DRAWDOWN_BPS {
            BandHealth::Degraded
        } else {
            BandHealth::Healthy
        }
    }
}

/// A real fault worth waking the operator for.
///
/// Every variant here is a measured fault. There is no variant for idleness, no variant
/// for "no positions", and no variant for a quiet system — by construction, silence is
/// the absence of an alert rather than a message about the absence of activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alert {
    /// Equity has fallen strictly below the operator's floor: the rule that must speak
    /// before capital is destroyed has spoken.
    DrawdownFloorBreached {
        /// Equity at the moment of the breach, lamports.
        equity_lamports: u64,
        /// The floor that was crossed, lamports.
        floor_lamports: u64,
    },
    /// The drawdown band reached [`BandHealth::Critical`].
    DrawdownCritical {
        /// The measured drawdown, bps.
        drawdown_bps: u32,
    },
    /// The drawdown band reached [`BandHealth::Degraded`]: reported, not paged as a
    /// survival event.
    BandDegraded {
        /// The band this alert reports, always [`BandHealth::Degraded`] on this variant.
        band_health: BandHealth,
    },
}

/// The alerts owed for one snapshot: real faults only, floor breach first.
///
/// Ordering is contractual. A floor breach is the operator's first-read item, so it is
/// always element 0 when present; the drawdown band follows. At most two alerts can be
/// returned, because a book has one floor and one band.
///
/// SILENCE IS THE DEFAULT. A healthy book returns an empty vector, and so does a book
/// that is merely idle — zero open positions with no drawdown produces NOTHING to page
/// about. That is asserted by the crate's tests, because a pager that fires on a quiet
/// system is a false alarm and is forbidden by the operator's standing rules.
pub fn alerts(snapshot: &TelemetrySnapshot) -> Vec<Alert> {
    let mut out = Vec::new();
    if snapshot.floor_breached {
        out.push(Alert::DrawdownFloorBreached {
            equity_lamports: snapshot.equity_lamports,
            floor_lamports: snapshot.floor_lamports,
        });
    }
    match snapshot.band_health {
        BandHealth::Critical => out.push(Alert::DrawdownCritical {
            drawdown_bps: snapshot.drawdown_bps,
        }),
        BandHealth::Degraded => out.push(Alert::BandDegraded {
            band_health: BandHealth::Degraded,
        }),
        BandHealth::Healthy => {}
    }
    out
}

/// Peak-to-current drawdown in basis points, saturating.
///
/// 0 when no peak is known (`peak == 0`) and 0 when equity is at or above the peak. The
/// product is taken in `u128` so no intermediate can wrap, and the quotient is clamped to
/// `u32::MAX` rather than truncated.
fn drawdown_bps(peak: u64, equity: u64) -> u32 {
    if peak == 0 || equity >= peak {
        return 0;
    }
    let drop = (peak - equity) as u128;
    let scaled = drop * (BPS_DENOMINATOR as u128) / (peak as u128);
    if scaled > u32::MAX as u128 {
        u32::MAX
    } else {
        scaled as u32
    }
}

/// Widen a lamport total to `i64` for display, saturating rather than wrapping.
fn as_i64_saturating(v: u64) -> i64 {
    if v > i64::MAX as u64 {
        i64::MAX
    } else {
        v as i64
    }
}