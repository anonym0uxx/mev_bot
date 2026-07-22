#![allow(unused_imports)]
//! Integration tests for leaf `sg_graduation_score`.
//!
//! Expectations are computed independently from the ported piecewise formulas
//! (not memorized answers), covering edge cases and realistic scenarios.

use pump_quant_signals::scorer::*;

/// Independent reference for the speed dimension (mirrors the documented
/// piecewise-linear breakpoints, recomputed here from scratch).
fn expected_speed(s: u32) -> u8 {
    if s <= 60 {
        0
    } else if s <= 90 {
        ((s - 60) * 4 / 30).min(4) as u8
    } else if s <= 120 {
        (4 + (s - 90) * 8 / 30).min(12) as u8
    } else if s <= 180 {
        (12 + (s - 120) * 4 / 60).min(16) as u8
    } else if s <= 300 {
        (16 + (s - 180) * 4 / 120).min(20) as u8
    } else {
        20u8.saturating_sub(((s - 300) * 4 / 300).min(4) as u8)
    }
}

const RESERVE_85_SOL: u64 = 85_000_000_000;
const MIN_BUYS: u32 = 5;

#[test]
fn speed_dimension_matches_independent_formula() {
    for &s in &[
        0u32, 60, 61, 75, 90, 105, 120, 150, 180, 240, 300, 600, 3600, 100_000,
    ] {
        let sc = score_graduation(s, 0, 0, 0, 0, 0, RESERVE_85_SOL, 0, MIN_BUYS);
        assert_eq!(sc.speed, expected_speed(s), "speed mismatch at s={s}");
    }
}

#[test]
fn organic_high_scoring_graduation() {
    // speed=180 -> 16, vol=7500 centisol (75 SOL) -> 20, velocity 15*10_000/7500=20 cap 15,
    // ratio 15/2=7 *2=14 cap 10 (buys>=5), discount entry=390 term=411:
    //   (411-390)*10000/411 = 510 bps -> 510*10/1500 = 3,
    // lp 85 SOL -> 10, momentum 200 -> 10. cold_miss=0.
    let sc = score_graduation(180, 7_500, 15, 2, 390, 411, RESERVE_85_SOL, 200, MIN_BUYS);
    assert_eq!(sc.speed, 16);
    assert_eq!(sc.volume_tier, 20);
    assert_eq!(sc.velocity, 15);
    assert_eq!(sc.buy_sell_ratio, 10);
    assert_eq!(sc.entry_discount, 3);
    assert_eq!(sc.lp_reserve, 10);
    assert_eq!(sc.pre_entry_momentum, 10);
    assert_eq!(sc.cold_miss_bonus, 0);
    // 16+20+15+10+3+10+0+10 = 84
    assert_eq!(sc.total(), 84);
}

#[test]
fn whale_pump_scores_low_via_gate() {
    // speed 60 -> 0, vol 65_535 -> 0, velocity 3*10000/65535=0,
    // ratio: buys=3 < min 5 -> raw 3/1*2=6 halved -> 3, discount at terminal -> 0,
    // lp 85 -> 10, momentum 0 -> 0.
    let sc = score_graduation(60, 65_535, 3, 0, 411, 411, RESERVE_85_SOL, 0, MIN_BUYS);
    assert_eq!(sc.speed, 0);
    assert_eq!(sc.volume_tier, 0);
    assert_eq!(sc.velocity, 0);
    assert_eq!(sc.buy_sell_ratio, 3);
    assert_eq!(sc.entry_discount, 0);
    assert_eq!(sc.lp_reserve, 10);
    assert_eq!(sc.pre_entry_momentum, 0);
    assert_eq!(sc.total(), 13);
}

#[test]
fn all_zero_inputs_score_zero() {
    let sc = score_graduation(0, 0, 0, 0, 0, 0, 0, 0, MIN_BUYS);
    assert_eq!(sc.total(), 0);
    assert_eq!(sc, GraduationScore::default());
}

#[test]
fn total_never_overflows_and_excludes_discount() {
    // Extreme inputs must saturate at <= 100 and stay integer.
    let sc = score_graduation(
        0,
        1_000_000,
        10_000,
        0,
        1,
        1_000_000,
        u64::MAX,
        500,
        MIN_BUYS,
    );
    assert!(sc.total() <= 100);
    // total_excluding_discount omits exactly the entry_discount component.
    let expected_excl = sc.total().saturating_sub(sc.entry_discount);
    assert_eq!(sc.total_excluding_discount(), expected_excl);
}

#[test]
fn velocity_flows_into_pre_entry_momentum() {
    let no_vel = score_graduation(180, 7_500, 15, 2, 0, 0, RESERVE_85_SOL, 0, MIN_BUYS);
    let with_vel = score_graduation(180, 7_500, 15, 2, 0, 0, RESERVE_85_SOL, 200, MIN_BUYS);
    assert_eq!(no_vel.pre_entry_momentum, 0);
    assert_eq!(with_vel.pre_entry_momentum, 10);
    assert_eq!(with_vel.total() - no_vel.total(), 10);
}
