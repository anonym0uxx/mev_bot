//! Leaf cb_ledger: calibration-budget accountant (criterion 53).

use pump_quant_strategy::calibration_budget::{
    admit_calibration, BudgetReject, CalibrationLedger, CalibrationRequest, RouteId,
    ROUTE_TABLE_CAP,
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
        route: None,
    }
}

fn routed_req(cost: u64, day: u64, m: Option<u32>, route: RouteId) -> CalibrationRequest {
    CalibrationRequest {
        cost_lamports: cost,
        day,
        measurement_id: m,
        route: Some(route),
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

#[test]
fn default_new_leaves_per_route_unlimited() {
    // A ledger from `new` accounts route spend but never refuses on it (cap is
    // u64::MAX), so behavior matches the pre-per-route ledger.
    let l = ledger();
    let (l2, _) = admit_calibration(&l, &routed_req(100, 0, Some(1), RouteId(5))).unwrap();
    assert_eq!(l2.spent_on_route(RouteId(5)), 100);
    assert_eq!(l2.tracked_routes(), 1);
    // Untracked route reads zero.
    assert_eq!(l2.spent_on_route(RouteId(6)), 0);
}

#[test]
fn per_route_cap_is_enforced_independently_of_global_caps() {
    // Per-route cap 150; generous lifetime/daily/per-trade so only the route
    // dimension can bind.
    let l = CalibrationLedger::new_with_route_cap(10_000, 1_000, 10_000, 150, 0);
    let r = RouteId(2);
    let l = admit_calibration(&l, &routed_req(100, 0, Some(1), r))
        .unwrap()
        .0;
    assert_eq!(l.spent_on_route(r), 100);
    assert_eq!(l.remaining_route(r), 50);
    // 100 more would make 200 > 150 on this route: refused, ledger unchanged.
    assert_eq!(
        admit_calibration(&l, &routed_req(100, 0, Some(1), r)),
        Err(BudgetReject::ExceedsPerRoute)
    );
    // A different route has its own independent budget.
    let l2 = admit_calibration(&l, &routed_req(100, 0, Some(1), RouteId(3)))
        .unwrap()
        .0;
    assert_eq!(l2.spent_on_route(RouteId(3)), 100);
    assert_eq!(l2.tracked_routes(), 2);
    // 50 still fits on the first route (100 + 50 = 150, exactly at cap).
    assert!(admit_calibration(&l, &routed_req(50, 0, Some(1), r)).is_ok());
}

#[test]
fn new_route_whose_first_spend_exceeds_cap_is_refused() {
    let l = CalibrationLedger::new_with_route_cap(10_000, 1_000, 10_000, 150, 0);
    assert_eq!(
        admit_calibration(&l, &routed_req(200, 0, Some(1), RouteId(9))),
        Err(BudgetReject::ExceedsPerRoute)
    );
    // Refusal did not register the route.
    assert_eq!(l.tracked_routes(), 0);
}

#[test]
fn route_table_is_bounded_and_refuses_new_route_when_full() {
    // Fill the bounded table with ROUTE_TABLE_CAP distinct routes.
    let mut l = CalibrationLedger::new_with_route_cap(1_000_000, 1_000, 1_000_000, 1_000, 0);
    for i in 0..ROUTE_TABLE_CAP {
        l = admit_calibration(&l, &routed_req(10, 0, Some(1), RouteId(i as u16)))
            .unwrap()
            .0;
    }
    assert_eq!(l.tracked_routes(), ROUTE_TABLE_CAP);
    // A brand-new route beyond capacity is refused (§99: no silent eviction).
    assert_eq!(
        admit_calibration(&l, &routed_req(10, 0, Some(1), RouteId(9_999))),
        Err(BudgetReject::RouteTableFull)
    );
    // But an already-tracked route can still be accounted (no new slot needed).
    assert!(admit_calibration(&l, &routed_req(10, 0, Some(1), RouteId(0))).is_ok());
}

#[test]
fn per_route_is_checked_after_global_caps() {
    // Lifetime cap 50 (tight) but per-route cap 10 (tighter). A 100-cost trade
    // trips per-trade first; a 40-cost trade on an empty ledger trips per-route
    // only if globals pass. Here lifetime 50 with a 40 spend passes globally,
    // then per-route (cap 10) binds.
    let l = CalibrationLedger::new_with_route_cap(50, 1_000, 1_000, 10, 0);
    assert_eq!(
        admit_calibration(&l, &routed_req(40, 0, Some(1), RouteId(1))),
        Err(BudgetReject::ExceedsPerRoute)
    );
    // A request exceeding the lifetime cap reports the global reason first,
    // even though a route is named.
    assert_eq!(
        admit_calibration(&l, &routed_req(60, 0, Some(1), RouteId(1))),
        Err(BudgetReject::ExceedsLifetime)
    );
}
