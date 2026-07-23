#![allow(unused_imports)]
//! G10 / criterion 102 defect #10: reconciliation granularity must be
//! bankroll-derived, not the fixed legacy 100_000 lamports.

use pump_quant_execution::ex_reconcile_fill::*;

fn expected(log: i128) -> ExpectedFill {
    ExpectedFill {
        log_net_lamports: log,
        buy_recorded: true,
        sell_recorded: true,
        // Deliberately absurd so we can prove the bankroll path ignores it.
        tolerance_lamports: 0,
    }
}

fn both_confirmed(spent: u64, recv: u64) -> OnchainFill {
    OnchainFill {
        buy_confirmed: true,
        sell_confirmed: true,
        buy_spent_lamports: spent,
        sell_received_lamports: recv,
        buy_failed: false,
        sell_failed: false,
        stale: false,
    }
}

#[test]
fn granularity_is_monotonic_non_decreasing_in_bankroll() {
    let mut prev = recon_granularity_lamports(0);
    let mut b: u64 = 0;
    while b < 100_000_000_000 {
        let g = recon_granularity_lamports(b);
        assert!(
            g >= prev,
            "granularity dropped at bankroll {b}: {g} < {prev}"
        );
        prev = g;
        b += 123_456_789;
    }
}

#[test]
fn granularity_floor_applies_for_small_bankrolls() {
    // Below the point where bps*bankroll exceeds the floor, result == floor.
    assert_eq!(
        recon_granularity_lamports(0),
        RECON_GRANULARITY_FLOOR_LAMPORTS
    );
    assert_eq!(
        recon_granularity_lamports(1),
        RECON_GRANULARITY_FLOOR_LAMPORTS
    );
    // floor / bps-fraction boundary: floor = bankroll*bps/10_000 => bankroll
    // = floor*10_000/bps. Just below that, still floored.
    let boundary = RECON_GRANULARITY_FLOOR_LAMPORTS * 10_000 / RECON_GRANULARITY_BPS;
    assert_eq!(
        recon_granularity_lamports(boundary - 1),
        RECON_GRANULARITY_FLOOR_LAMPORTS
    );
    // Exactly at the boundary the raw value equals the floor.
    assert_eq!(
        recon_granularity_lamports(boundary),
        RECON_GRANULARITY_FLOOR_LAMPORTS
    );
}

#[test]
fn granularity_ceiling_applies_for_large_bankrolls() {
    // Above the ceiling boundary the result saturates to the ceiling.
    let boundary = RECON_GRANULARITY_CEIL_LAMPORTS * 10_000 / RECON_GRANULARITY_BPS;
    assert_eq!(
        recon_granularity_lamports(boundary),
        RECON_GRANULARITY_CEIL_LAMPORTS
    );
    assert_eq!(
        recon_granularity_lamports(boundary + 1),
        RECON_GRANULARITY_CEIL_LAMPORTS
    );
    assert_eq!(
        recon_granularity_lamports(u64::MAX),
        RECON_GRANULARITY_CEIL_LAMPORTS
    );
    const _: () = assert!(RECON_GRANULARITY_CEIL_LAMPORTS > RECON_GRANULARITY_FLOOR_LAMPORTS);
}

#[test]
fn granularity_scales_between_floor_and_ceiling() {
    // A mid-range bankroll gives bankroll*bps/10_000, strictly inside the band.
    let bankroll = 2_000_000_000u64; // 2 SOL
    let expected_raw = bankroll * RECON_GRANULARITY_BPS / 10_000; // 2_000_000
    assert!(expected_raw > RECON_GRANULARITY_FLOOR_LAMPORTS);
    assert!(expected_raw < RECON_GRANULARITY_CEIL_LAMPORTS);
    assert_eq!(recon_granularity_lamports(bankroll), expected_raw);
}

#[test]
fn reconcile_uses_bankroll_tolerance_not_the_passthrough() {
    // realized = +1_500_000; log = 0 => discrepancy 1_500_000.
    let oc = both_confirmed(50_000_000, 51_500_000);
    let exp = expected(0);

    // Small bankroll -> floor tolerance 100_000 < 1_500_000 => Discrepancy.
    let small = reconcile_fill_with_bankroll(exp, oc, 0);
    assert_eq!(small.realized_net_lamports, 1_500_000);
    assert_eq!(small.status, ReconStatus::Discrepancy);

    // Large bankroll -> wide tolerance (>= 1_500_000) => Reconciled, even though
    // expected.tolerance_lamports is 0 (proving the passthrough is bypassed).
    let big_bankroll = 3_000_000_000u64; // 3 SOL -> 3_000_000 granularity
    assert_eq!(recon_granularity_lamports(big_bankroll), 3_000_000);
    let big = reconcile_fill_with_bankroll(exp, oc, big_bankroll);
    assert_eq!(big.status, ReconStatus::Reconciled);
}
