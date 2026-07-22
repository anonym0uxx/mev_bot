#![allow(unused_imports)]
use pump_quant_execution::ex_reconcile_fill::*;

fn mk_expected(log: i128, tol: i128) -> ExpectedFill {
    ExpectedFill {
        log_net_lamports: log,
        buy_recorded: true,
        sell_recorded: true,
        tolerance_lamports: tol,
    }
}

fn base_onchain() -> OnchainFill {
    OnchainFill {
        buy_confirmed: false,
        sell_confirmed: false,
        buy_spent_lamports: 0,
        sell_received_lamports: 0,
        buy_failed: false,
        sell_failed: false,
        stale: false,
    }
}

#[test]
fn both_confirmed_within_tolerance_reconciles() {
    // spent 50_000_000, received 52_000_000 -> realized +2_000_000
    // log says +2_050_000 -> discrepancy -50_000, tolerance 100_000 -> reconciled.
    let exp = mk_expected(2_050_000, 100_000);
    let oc = OnchainFill {
        buy_confirmed: true,
        sell_confirmed: true,
        buy_spent_lamports: 50_000_000,
        sell_received_lamports: 52_000_000,
        ..base_onchain()
    };
    let r = reconcile_fill(exp, oc);
    let realized: i128 = 52_000_000 - 50_000_000;
    let disc: i128 = realized - 2_050_000;
    assert_eq!(r.realized_net_lamports, realized);
    assert_eq!(r.discrepancy_lamports, disc);
    assert_eq!(r.status, ReconStatus::Reconciled);
    assert!(disc.abs() <= 100_000);
}

#[test]
fn both_confirmed_beyond_tolerance_flags_discrepancy() {
    // realized -2_000_000, log +2_000_000 -> disc -4_000_000 > tol 100_000.
    let exp = mk_expected(2_000_000, 100_000);
    let oc = OnchainFill {
        buy_confirmed: true,
        sell_confirmed: true,
        buy_spent_lamports: 50_000_000,
        sell_received_lamports: 48_000_000,
        ..base_onchain()
    };
    let r = reconcile_fill(exp, oc);
    assert_eq!(r.realized_net_lamports, 48_000_000i128 - 50_000_000);
    assert_eq!(
        r.discrepancy_lamports,
        (48_000_000i128 - 50_000_000) - 2_000_000
    );
    assert_eq!(r.status, ReconStatus::Discrepancy);
}

#[test]
fn buy_failed_is_phantom() {
    let exp = mk_expected(1_000, 100_000);
    let oc = OnchainFill {
        buy_failed: true,
        ..base_onchain()
    };
    let r = reconcile_fill(exp, oc);
    assert_eq!(r.status, ReconStatus::BuyNotConfirmed);
    assert_eq!(r.realized_net_lamports, 0);
}

#[test]
fn sell_failed_is_stuck() {
    let exp = mk_expected(1_000, 100_000);
    let oc = OnchainFill {
        buy_confirmed: true,
        sell_failed: true,
        buy_spent_lamports: 10_000,
        ..base_onchain()
    };
    let r = reconcile_fill(exp, oc);
    assert_eq!(r.status, ReconStatus::SellNotConfirmed);
}

#[test]
fn stale_unconfirmed_buy_is_phantom() {
    let exp = mk_expected(0, 100_000);
    let oc = OnchainFill {
        stale: true,
        ..base_onchain()
    };
    let r = reconcile_fill(exp, oc);
    assert_eq!(r.status, ReconStatus::BuyNotConfirmed);
}

#[test]
fn stale_confirmed_buy_unconfirmed_sell_is_stuck() {
    let exp = mk_expected(0, 100_000);
    let oc = OnchainFill {
        buy_confirmed: true,
        stale: true,
        buy_spent_lamports: 5_000,
        ..base_onchain()
    };
    let r = reconcile_fill(exp, oc);
    assert_eq!(r.status, ReconStatus::SellNotConfirmed);
}

#[test]
fn not_yet_confirmed_is_pending() {
    let exp = mk_expected(0, 100_000);
    let oc = OnchainFill {
        buy_confirmed: true,
        ..base_onchain()
    };
    let r = reconcile_fill(exp, oc);
    assert_eq!(r.status, ReconStatus::Pending);
    assert_eq!(r.realized_net_lamports, 0);
}

#[test]
fn exact_tolerance_boundary_reconciles() {
    // discrepancy exactly == tolerance is NOT a discrepancy (strict >).
    let realized_spent = 100u64;
    let realized_recv = 200u64;
    let realized = realized_recv as i128 - realized_spent as i128; // 100
    let tol = 40i128;
    let log = realized - 40; // disc = +40 == tol
    let exp = mk_expected(log, tol);
    let oc = OnchainFill {
        buy_confirmed: true,
        sell_confirmed: true,
        buy_spent_lamports: realized_spent,
        sell_received_lamports: realized_recv,
        ..base_onchain()
    };
    let r = reconcile_fill(exp, oc);
    assert_eq!(r.discrepancy_lamports, 40);
    assert_eq!(r.status, ReconStatus::Reconciled);
}

#[test]
fn large_lamport_values_do_not_overflow() {
    let big = u64::MAX;
    let exp = mk_expected(0, 0);
    let oc = OnchainFill {
        buy_confirmed: true,
        sell_confirmed: true,
        buy_spent_lamports: big,
        sell_received_lamports: 0,
        ..base_onchain()
    };
    let r = reconcile_fill(exp, oc);
    assert_eq!(r.realized_net_lamports, -(big as i128));
    assert_eq!(r.status, ReconStatus::Discrepancy);
}
