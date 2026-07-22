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
//! §43 (calibration budget) / Section on ExecutionCalibrationBudget: lifetime,
//! per-trade, daily caps + minimum-information-gain; §22 integer fixed-point;
//! deterministic (the day bucket is supplied by the caller, never a syscall clock).

/// The calibration ledger: caps plus running spend, all in lamports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalibrationLedger {
    /// Lifetime calibration cap.
    pub lifetime_cap: u64,
    /// Per-trade calibration cap.
    pub per_trade_cap: u64,
    /// Daily calibration cap.
    pub daily_cap: u64,
    /// Lifetime spend so far.
    pub spent_lifetime: u64,
    /// Spend accrued within `current_day`.
    pub spent_today: u64,
    /// The day bucket `spent_today` is accounted against.
    pub current_day: u64,
}

impl CalibrationLedger {
    /// A fresh ledger with the given caps and zero spend, anchored at `day`.
    pub fn new(lifetime_cap: u64, per_trade_cap: u64, daily_cap: u64, day: u64) -> Self {
        CalibrationLedger {
            lifetime_cap,
            per_trade_cap,
            daily_cap,
            spent_lifetime: 0,
            spent_today: 0,
            current_day: day,
        }
    }

    /// Remaining lifetime budget (saturating).
    #[inline]
    pub fn remaining_lifetime(&self) -> u64 {
        self.lifetime_cap.saturating_sub(self.spent_lifetime)
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
}

/// Admit a calibration trade against the budget (leaf **cb_ledger**).
///
/// Pure: returns the updated ledger and a research-expenditure label on success,
/// or a typed refusal, never mutating in place. Checks in a fixed order so the
/// reject reason is stable: measurement → per-trade → daily → lifetime. When the
/// request's `day` differs from `current_day`, the daily counter is rolled over
/// (a new day resets `spent_today` before accounting this trade).
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

    let updated = CalibrationLedger {
        spent_lifetime: new_lifetime,
        spent_today: new_today,
        current_day: day,
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
