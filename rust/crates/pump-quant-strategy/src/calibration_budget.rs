//! # calibration_budget — execution-calibration budget accountant (criterion 53)
//!
//! A pure integer ledger ([`CalibrationLedger`]) enforcing the
//! ExecutionCalibrationBudget: it accounts calibration spend against per-trade,
//! daily, and lifetime caps, requires every calibration trade to name a
//! measurement it improves (minimum-information-gain), tags admitted spend as
//! research expenditure ([`CalibrationLabel`]), and **refuses** admission once any
//! cap is exhausted. Calibration losses are research data-acquisition costs, never
//! claims of profitable deployment.
//!
//! ## Constitution
//! §43 (calibration budget) / §39 ExecutionCalibrationBudget: lifetime,
//! per-trade, daily, **and per-route** caps + minimum-information-gain; §22
//! integer fixed-point; deterministic (the day bucket is supplied by the caller,
//! never a syscall clock). The per-route spend table is a bounded fixed-capacity
//! map (§99: no unbounded accumulator).

/// Maximum number of distinct submission routes the per-route spend table tracks
/// (§99 bound). A fixed capacity keeps [`CalibrationLedger`] `Copy` and the
/// accounting statically bounded; the live route set (Jito, Nozomi, direct RPC,
/// verified alternates) is small, so this is generous headroom. When the table
/// is full a *new* route is refused ([`BudgetReject::RouteTableFull`]) rather
/// than evicting an existing route's accounted spend — money accounting is never
/// silently dropped to satisfy the bound.
pub const ROUTE_TABLE_CAP: usize = 16;

/// A submission-route discriminant (§39 per-route dimension). An opaque integer
/// id assigned by the route registry; the ledger only compares ids for equality.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RouteId(pub u16);

/// The calibration ledger: caps plus running spend, all in lamports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalibrationLedger {
    /// Lifetime calibration cap.
    pub lifetime_cap: u64,
    /// Per-trade calibration cap.
    pub per_trade_cap: u64,
    /// Daily calibration cap.
    pub daily_cap: u64,
    /// Per-route lifetime calibration cap (§39). Applies to each distinct
    /// [`RouteId`]; `u64::MAX` = effectively unlimited (the [`CalibrationLedger::new`]
    /// default, which reproduces pre-per-route behavior).
    pub per_route_cap: u64,
    /// Lifetime spend so far.
    pub spent_lifetime: u64,
    /// Spend accrued within `current_day`.
    pub spent_today: u64,
    /// The day bucket `spent_today` is accounted against.
    pub current_day: u64,
    /// Bounded `(route_id, lifetime_spend)` table (§39/§99). Only the first
    /// `route_count` slots are occupied; private so the bound and the
    /// no-silent-eviction invariant are enforced through [`admit_calibration`].
    route_spend: [(u16, u64); ROUTE_TABLE_CAP],
    /// Number of occupied slots in `route_spend`.
    route_count: usize,
}

impl CalibrationLedger {
    /// A fresh ledger with the given caps and zero spend, anchored at `day`.
    ///
    /// The per-route cap defaults to `u64::MAX` (unlimited), so a ledger built
    /// with `new` behaves exactly as it did before the per-route dimension
    /// existed. Use [`CalibrationLedger::new_with_route_cap`] to enforce a
    /// finite §39 per-route cap.
    pub fn new(lifetime_cap: u64, per_trade_cap: u64, daily_cap: u64, day: u64) -> Self {
        Self::new_with_route_cap(lifetime_cap, per_trade_cap, daily_cap, u64::MAX, day)
    }

    /// A fresh ledger that additionally enforces a finite §39 per-route cap.
    pub fn new_with_route_cap(
        lifetime_cap: u64,
        per_trade_cap: u64,
        daily_cap: u64,
        per_route_cap: u64,
        day: u64,
    ) -> Self {
        CalibrationLedger {
            lifetime_cap,
            per_trade_cap,
            daily_cap,
            per_route_cap,
            spent_lifetime: 0,
            spent_today: 0,
            current_day: day,
            route_spend: [(0, 0); ROUTE_TABLE_CAP],
            route_count: 0,
        }
    }

    /// Remaining lifetime budget (saturating).
    #[inline]
    pub fn remaining_lifetime(&self) -> u64 {
        self.lifetime_cap.saturating_sub(self.spent_lifetime)
    }

    /// Lifetime calibration spend accounted against `route` (0 if the route has
    /// never been calibrated on).
    pub fn spent_on_route(&self, route: RouteId) -> u64 {
        for slot in &self.route_spend[..self.route_count] {
            if slot.0 == route.0 {
                return slot.1;
            }
        }
        0
    }

    /// Remaining per-route calibration budget for `route` (saturating, §39).
    #[inline]
    pub fn remaining_route(&self, route: RouteId) -> u64 {
        self.per_route_cap
            .saturating_sub(self.spent_on_route(route))
    }

    /// Number of distinct routes currently tracked (`<= ROUTE_TABLE_CAP`).
    #[inline]
    pub fn tracked_routes(&self) -> usize {
        self.route_count
    }

    /// Compute the route table that would result from accounting `cost` against
    /// `route`, enforcing the §39 per-route cap and the §99 table bound. Returns
    /// the updated `(table, count)` or the typed refusal; never mutates.
    fn route_spend_after(
        &self,
        route: RouteId,
        cost: u64,
    ) -> Result<([(u16, u64); ROUTE_TABLE_CAP], usize), BudgetReject> {
        let mut table = self.route_spend;
        // Existing route: accumulate against its lifetime-per-route spend.
        for i in 0..self.route_count {
            if table[i].0 == route.0 {
                let new_spend = table[i]
                    .1
                    .checked_add(cost)
                    .ok_or(BudgetReject::ExceedsPerRoute)?;
                if new_spend > self.per_route_cap {
                    return Err(BudgetReject::ExceedsPerRoute);
                }
                table[i].1 = new_spend;
                return Ok((table, self.route_count));
            }
        }
        // New route: its first spend must itself fit the per-route cap.
        if cost > self.per_route_cap {
            return Err(BudgetReject::ExceedsPerRoute);
        }
        // §99: the table is bounded; a new route beyond capacity is refused, not
        // silently evicted (evicting would drop accounted spend).
        if self.route_count >= ROUTE_TABLE_CAP {
            return Err(BudgetReject::RouteTableFull);
        }
        table[self.route_count] = (route.0, cost);
        Ok((table, self.route_count + 1))
    }
}

/// A single calibration-trade admission request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalibrationRequest {
    /// Expected economic cost of the calibration trade, in lamports.
    pub cost_lamports: u64,
    /// The day bucket this trade belongs to.
    pub day: u64,
    /// The measurement id this trade improves, or `None` (which is refused).
    pub measurement_id: Option<u32>,
    /// The submission route this calibration trade probes (§39 per-route cap),
    /// or `None` to skip per-route accounting entirely (preserving the original
    /// lifetime/per-trade/daily-only behavior).
    pub route: Option<RouteId>,
}

/// The research-expenditure label attached to an admitted calibration trade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalibrationLabel {
    /// The accounted research cost, in lamports.
    pub research_cost_lamports: u64,
    /// The measurement this expenditure funds.
    pub measurement_id: u32,
}

/// Why a calibration trade was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetReject {
    /// No measurement specified (minimum-information-gain violated).
    NoMeasurement,
    /// The trade cost exceeds the per-trade cap.
    ExceedsPerTrade,
    /// The trade would exceed the daily cap.
    ExceedsDaily,
    /// The trade would exceed the lifetime cap.
    ExceedsLifetime,
    /// The trade would exceed the §39 per-route cap for its route.
    ExceedsPerRoute,
    /// A new route was requested but the bounded per-route table is full
    /// ([`ROUTE_TABLE_CAP`], §99). Refused rather than evicting accounted spend.
    RouteTableFull,
}

/// Admit a calibration trade against the budget (leaf **cb_ledger**).
///
/// Pure: returns the updated ledger and a research-expenditure label on success,
/// or a typed refusal, never mutating in place. Checks in a fixed order so the
/// reject reason is stable: measurement → per-trade → daily → lifetime →
/// per-route. The §39 per-route dimension is checked *last* and only when the
/// request names a route: this is an orthogonal, additive dimension, so its
/// presence never changes the reject reason for a request that names no route,
/// and for a routed request the survival-relevant global caps (daily, lifetime)
/// are proven before the local per-route cap. When the request's `day` differs
/// from `current_day`, the daily counter is rolled over (a new day resets
/// `spent_today` before accounting this trade).
pub fn admit_calibration(
    ledger: &CalibrationLedger,
    req: &CalibrationRequest,
) -> Result<(CalibrationLedger, CalibrationLabel), BudgetReject> {
    let measurement_id = req.measurement_id.ok_or(BudgetReject::NoMeasurement)?;

    if req.cost_lamports > ledger.per_trade_cap {
        return Err(BudgetReject::ExceedsPerTrade);
    }

    // Roll the daily counter over on a new day.
    let (day, spent_today_base) = if req.day == ledger.current_day {
        (ledger.current_day, ledger.spent_today)
    } else {
        (req.day, 0)
    };

    let new_today = spent_today_base
        .checked_add(req.cost_lamports)
        .ok_or(BudgetReject::ExceedsDaily)?;
    if new_today > ledger.daily_cap {
        return Err(BudgetReject::ExceedsDaily);
    }

    let new_lifetime = ledger
        .spent_lifetime
        .checked_add(req.cost_lamports)
        .ok_or(BudgetReject::ExceedsLifetime)?;
    if new_lifetime > ledger.lifetime_cap {
        return Err(BudgetReject::ExceedsLifetime);
    }

    // §39 per-route cap (last; skipped when no route is named).
    let (route_spend, route_count) = match req.route {
        Some(route) => ledger.route_spend_after(route, req.cost_lamports)?,
        None => (ledger.route_spend, ledger.route_count),
    };

    let updated = CalibrationLedger {
        spent_lifetime: new_lifetime,
        spent_today: new_today,
        current_day: day,
        route_spend,
        route_count,
        ..*ledger
    };
    Ok((
        updated,
        CalibrationLabel {
            research_cost_lamports: req.cost_lamports,
            measurement_id,
        },
    ))
}
