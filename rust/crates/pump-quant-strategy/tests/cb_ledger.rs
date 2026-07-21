//! Leaf cb_ledger: calibration-budget accountant (criterion 53).

use pump_quant_strategy::calibration_budget::{
    admit_calibration, BudgetReject, CalibrationLedger, CalibrationRequest,
};

fn ledger() -> CalibrationLedger {
    // lifetime 1_000, per-trade 100, daily 300, day 0.
    CalibrationLedger::new(1_000, 100, 300, 0)
}

fn req(cost: u64, day: u64, m: Option<u32>) -> CalibrationRequest {
    CalibrationRequest {
        cost_lamports: cost,
        day,
        measurement_id: m,
    }
}

#[test]
fn admits_and_accounts() {
    let l = ledger();
    let (l2, label) = admit_calibration(&l, &req(50, 0, Some(9))).unwrap();
    assert_eq!(l2.spent_lifetime, 50);
    assert_eq!(l2.spent_today, 50);
    assert_eq!(label.research_cost_lamports, 50);
    assert_eq!(label.measurement_id, 9);
    assert_eq!(l2.remaining_lifetime(), 950);
}

#[test]
fn requires_measurement() {
    assert_eq!(
        admit_calibration(&ledger(), &req(50, 0, None)),
        Err(BudgetReject::NoMeasurement)
    );
}

#[test]
fn refuses_over_per_trade_cap() {
    assert_eq!(
        admit_calibration(&ledger(), &req(101, 0, Some(1))),
        Err(BudgetReject::ExceedsPerTrade)
    );
    // Exactly at cap is allowed.
    assert!(admit_calibration(&ledger(), &req(100, 0, Some(1))).is_ok());
}

#[test]
fn refuses_over_daily_cap() {
    // Three trades of 100 fill the 300 daily cap; the 4th (same day) refused.
    let mut l = ledger();
    for _ in 0..3 {
        l = admit_calibration(&l, &req(100, 0, Some(1))).unwrap().0;
    }
    assert_eq!(l.spent_today, 300);
    assert_eq!(
        admit_calibration(&l, &req(1, 0, Some(1))),
        Err(BudgetReject::ExceedsDaily)
    );
}

#[test]
fn daily_counter_rolls_over_new_day() {
    let mut l = ledger();
    for _ in 0..3 {
        l = admit_calibration(&l, &req(100, 0, Some(1))).unwrap().0;
    }
    // New day resets spent_today; lifetime keeps accumulating.
    let (l2, _) = admit_calibration(&l, &req(100, 1, Some(1))).unwrap();
    assert_eq!(l2.spent_today, 100);
    assert_eq!(l2.current_day, 1);
    assert_eq!(l2.spent_lifetime, 400);
}

#[test]
fn refuses_over_lifetime_cap() {
    // Lifetime cap 250 with generous daily to isolate the lifetime path.
    let mut l = CalibrationLedger::new(250, 100, 10_000, 0);
    l = admit_calibration(&l, &req(100, 0, Some(1))).unwrap().0;
    l = admit_calibration(&l, &req(100, 1, Some(1))).unwrap().0; // lifetime 200
    assert_eq!(
        admit_calibration(&l, &req(100, 2, Some(1))),
        Err(BudgetReject::ExceedsLifetime) // 300 > 250
    );
    // 50 still fits (200 + 50 = 250).
    assert!(admit_calibration(&l, &req(50, 2, Some(1))).is_ok());
}
