#![allow(unused_imports)]
//! Integration tests for leaf `sg_entry_discount`.
//!
//! Expectations recomputed independently via `u128` from the discount-bps
//! definition, covering premium/at-terminal, zero prices, small and large
//! fixed-point magnitudes, and the 1_500 bps saturation point.

use pump_quant_signals::scorer::*;

/// Independent reference computation (u128, no floats).
fn expected(entry: u64, terminal: u64) -> u32 {
    if terminal == 0 || entry == 0 || entry >= terminal {
        return 0;
    }
    let bps = ((terminal - entry) as u128 * 10_000 / terminal as u128) as u32;
    if bps >= 1_500 {
        10
    } else {
        (bps * 10 / 1_500).min(10)
    }
}

#[test]
fn at_or_above_terminal_is_zero() {
    assert_eq!(entry_discount_score(411, 411), 0);
    assert_eq!(entry_discount_score(500, 411), 0);
}

#[test]
fn zero_prices_are_zero() {
    assert_eq!(entry_discount_score(0, 411), 0);
    assert_eq!(entry_discount_score(411, 0), 0);
    assert_eq!(entry_discount_score(0, 0), 0);
}

#[test]
fn moderate_discount_scales_linearly() {
    // entry=390 term=411: (411-390)*10000/411 = 510 bps -> 510*10/1500 = 3.
    assert_eq!(entry_discount_score(390, 411), 3);
    assert_eq!(entry_discount_score(390, 411), expected(390, 411));
    // entry=370 term=411: (411-370)*10000/411 = 997 bps -> 997*10/1500 = 6.
    assert_eq!(entry_discount_score(370, 411), 6);
    // entry=399 term=411: (411-399)*10000/411 = 291 bps -> 291*10/1500 = 1.
    assert_eq!(entry_discount_score(399, 411), 1);
    // entry=407 term=411: 97 bps -> 97*10/1500 = 0.
    assert_eq!(entry_discount_score(407, 411), 0);
}

#[test]
fn deep_discount_saturates_at_10() {
    // entry=200 term=411: (211)*10000/411 = 5133 bps >= 1500 -> 10.
    assert_eq!(entry_discount_score(200, 411), 10);
}

#[test]
fn large_fixed_point_preserves_ratio() {
    // Same 997 bps ratio scaled up by 1e6 -> still 6, no overflow (u128 widening).
    assert_eq!(entry_discount_score(370_000_000, 411_000_000), 6);
    assert_eq!(
        entry_discount_score(370_000_000, 411_000_000),
        entry_discount_score(370, 411)
    );
    // Near-u64::MAX magnitudes must not overflow.
    let term = u64::MAX;
    let entry = term / 2; // 5000 bps discount -> >= 1500 -> 10.
    assert_eq!(entry_discount_score(entry, term), 10);
    assert_eq!(entry_discount_score(entry, term), expected(entry, term));
}

#[test]
fn matches_independent_reference_over_grid() {
    let terminals = [1u64, 411, 10_000, 1_000_000_000];
    for &term in &terminals {
        for num in 0u64..=20 {
            let entry = term * num / 20; // sweep 0..=terminal
            assert_eq!(
                entry_discount_score(entry, term),
                expected(entry, term),
                "mismatch entry={entry} term={term}"
            );
        }
    }
}
