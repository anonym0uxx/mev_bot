//! Leaf pl_wallet_floor: wallet-survival-floor veto (criterion 27).

use pump_quant_strategy::probe_ladder::{wallet_floor_guard, FloorVerdict};

#[test]
fn allows_within_deployable() {
    // deployable = balance - floor = 10 - 4 = 6.
    assert_eq!(wallet_floor_guard(5, 10, 4), FloorVerdict::Allowed);
    // exactly consuming deployable is allowed (balance lands on the floor).
    assert_eq!(wallet_floor_guard(6, 10, 4), FloorVerdict::Allowed);
}

#[test]
fn refuses_above_deployable() {
    // 7 > 6 deployable.
    assert_eq!(
        wallet_floor_guard(7, 10, 4),
        FloorVerdict::RefusedBelowFloor
    );
    // any nonzero size when balance <= floor.
    assert_eq!(wallet_floor_guard(1, 4, 4), FloorVerdict::RefusedBelowFloor);
    assert_eq!(wallet_floor_guard(1, 3, 4), FloorVerdict::RefusedBelowFloor);
}

#[test]
fn zero_size_always_allowed() {
    assert_eq!(wallet_floor_guard(0, 4, 4), FloorVerdict::Allowed);
    assert_eq!(wallet_floor_guard(0, 0, 100), FloorVerdict::Allowed);
}

#[test]
fn boundary_sweep_matches_independent_formula() {
    let balance = 1_000u64;
    let floor = 300u64;
    let deployable = balance - floor; // 700
    for size in [0u64, 699, 700, 701, 1_000, 2_000] {
        let expected = if size > deployable {
            FloorVerdict::RefusedBelowFloor
        } else {
            FloorVerdict::Allowed
        };
        assert_eq!(
            wallet_floor_guard(size, balance, floor),
            expected,
            "size={size}"
        );
    }
}
