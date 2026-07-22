#![allow(unused_imports)]
//! Integration tests for leaf `sg_velocity`.
//!
//! Expectations are recomputed independently via `i128` from the price pair and
//! elapsed time, covering rising/falling/flat, sub-second and multi-second
//! windows, and saturation edge cases.

use pump_quant_signals::velocity::*;

/// Independent reference computation using i128 (no floats).
fn expected(prev: u64, cur: u64, dt_ms: u64) -> i64 {
    if prev == 0 || dt_ms == 0 {
        return 0;
    }
    let v = (cur as i128 - prev as i128) * 10_000 * 1_000 / (prev as i128 * dt_ms as i128);
    if v > i64::MAX as i128 {
        i64::MAX
    } else if v < i64::MIN as i128 {
        i64::MIN
    } else {
        v as i64
    }
}

#[test]
fn rising_price_one_second() {
    // 1000 -> 1100 over 1000ms = +10% = +1000 bps over 1s => +1000 bps/s.
    assert_eq!(velocity_bps_per_s(1000, 1100, 1000), 1000);
    assert_eq!(
        velocity_bps_per_s(1000, 1100, 1000),
        expected(1000, 1100, 1000)
    );
}

#[test]
fn rising_price_half_second_doubles_rate() {
    // Same +10% move in 500ms => +2000 bps/s.
    assert_eq!(velocity_bps_per_s(1000, 1100, 500), 2000);
    assert_eq!(
        velocity_bps_per_s(1000, 1100, 500),
        expected(1000, 1100, 500)
    );
}

#[test]
fn falling_price_is_negative() {
    // 1000 -> 900 over 1000ms = -1000 bps/s.
    assert_eq!(velocity_bps_per_s(1000, 900, 1000), -1000);
    assert_eq!(
        velocity_bps_per_s(1000, 900, 1000),
        expected(1000, 900, 1000)
    );
}

#[test]
fn flat_price_is_zero() {
    assert_eq!(velocity_bps_per_s(1000, 1000, 1000), 0);
    assert_eq!(velocity_bps_per_s(u64::MAX, u64::MAX, 250), 0);
}

#[test]
fn zero_baseline_or_zero_dt_returns_zero() {
    assert_eq!(velocity_bps_per_s(0, 1000, 1000), 0);
    assert_eq!(velocity_bps_per_s(1000, 1100, 0), 0);
    assert_eq!(velocity_bps_per_s(0, 0, 0), 0);
}

#[test]
fn matches_independent_reference_over_grid() {
    let prices = [1u64, 411, 1000, 500_000, 1_000_000_000, u64::MAX / 2];
    let dts = [1u64, 50, 250, 1000, 5000, 60_000];
    for &prev in &prices {
        for &cur in &prices {
            for &dt in &dts {
                assert_eq!(
                    velocity_bps_per_s(prev, cur, dt),
                    expected(prev, cur, dt),
                    "mismatch prev={prev} cur={cur} dt={dt}"
                );
            }
        }
    }
}

#[test]
fn saturates_instead_of_wrapping() {
    // prev=1, cur=u64::MAX, dt=1ms: astronomically large positive rate -> saturates to i64::MAX.
    assert_eq!(velocity_bps_per_s(1, u64::MAX, 1), i64::MAX);
}
