#![allow(unused_imports)]
//! Integration tests for leaf `sg_buy_sell_ratio_gate`.
//!
//! Expectations recomputed independently from the ratio + gate definition,
//! covering the gate boundary, capping, zero cases, and sell-heavy prints.

use pump_quant_signals::scorer::*;

/// Independent reference: min(buys/max(sells,1)*2, 10), halved when buys < min.
fn expected(buys: u32, sells: u32, min_for_full: u32) -> u32 {
    let sells = if sells == 0 { 1 } else { sells };
    let raw = (buys / sells).saturating_mul(2).min(10);
    if buys < min_for_full {
        raw / 2
    } else {
        raw
    }
}

#[test]
fn zero_buys_is_zero() {
    assert_eq!(buy_sell_ratio_score(0, 5, 5), 0);
    assert_eq!(buy_sell_ratio_score(0, 0, 5), 0);
}

#[test]
fn equal_pressure_at_gate() {
    // 5/5=1, *2=2, buys=5 >= min 5 -> full 2.
    assert_eq!(buy_sell_ratio_score(5, 5, 5), 2);
    assert_eq!(buy_sell_ratio_score(5, 5, 5), expected(5, 5, 5));
}

#[test]
fn strong_buy_pressure_caps_at_10() {
    // 10/2=5, *2=10 (buys>=min) -> 10.
    assert_eq!(buy_sell_ratio_score(10, 2, 5), 10);
    // 100/1=100, *2=200 cap 10.
    assert_eq!(buy_sell_ratio_score(100, 1, 5), 10);
    // zero sells treated as 1: 10/1*2=20 cap 10.
    assert_eq!(buy_sell_ratio_score(10, 0, 5), 10);
}

#[test]
fn gate_halves_thin_prints() {
    // 3 buys < min 5: raw=3/1*2=6 -> halved 3.
    assert_eq!(buy_sell_ratio_score(3, 0, 5), 3);
    // 4 buys < 5: raw=4/1*2=8 -> halved 4.
    assert_eq!(buy_sell_ratio_score(4, 0, 5), 4);
    // 5 buys == 5: raw=5/1*2=10 full -> 10.
    assert_eq!(buy_sell_ratio_score(5, 0, 5), 10);
}

#[test]
fn sell_heavy_is_zero() {
    // 2/10=0 -> 0.
    assert_eq!(buy_sell_ratio_score(2, 10, 5), 0);
}

#[test]
fn matches_independent_reference_over_grid() {
    for buys in 0u32..30 {
        for sells in 0u32..12 {
            for &min_for_full in &[0u32, 1, 5, 8, 20] {
                assert_eq!(
                    buy_sell_ratio_score(buys, sells, min_for_full),
                    expected(buys, sells, min_for_full),
                    "mismatch buys={buys} sells={sells} min={min_for_full}"
                );
            }
        }
    }
}

#[test]
fn range_is_bounded_0_to_10() {
    for buys in 0u32..1000 {
        let s = buy_sell_ratio_score(buys, 0, 5);
        assert!(s <= 10, "score {s} out of range for buys={buys}");
    }
}
