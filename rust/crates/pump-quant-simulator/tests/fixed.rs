//! Leaf tests for `fixed`: integer / fixed-point primitives (§22).
//! Expectations are computed independently by hand below each assertion.

use pump_quant_simulator::fixed::{
    i128_to_u64_saturating, mul_bps, reduce_bps, scale_signed_bps, BPS_ONE,
};

#[test]
fn mul_bps_basic_and_floor() {
    // 1_000_000 * 250 / 10_000 = 25_000 exactly.
    assert_eq!(mul_bps(1_000_000, 250), 25_000);
    // Flooring: 7 * 1 / 10000 = 0.0007 -> 0.
    assert_eq!(mul_bps(7, 1), 0);
    // 12_345 * 3333 / 10000 = 41_146_  ... = floor(4_114_618.5)= 4_114 ...
    // 12_345 * 3333 = 41_145_  -> 41,145,  12345*3333=41,146, compute: 12345*3333=41,146, ...
    // 12345*3333 = 41,146,  12345*3000=37,035,000; *333=4,110,885; sum=41,145,885; /10000=4114.
    assert_eq!(mul_bps(12_345, 3333), 4_114);
}

#[test]
fn mul_bps_saturates_on_amplification() {
    // bps == BPS_ONE returns the amount unchanged.
    assert_eq!(mul_bps(u64::MAX, BPS_ONE), u64::MAX);
    // 2x amplification of u64::MAX overflows u64 -> saturates by contract.
    assert_eq!(mul_bps(u64::MAX, 2 * BPS_ONE), u64::MAX);
}

#[test]
fn reduce_bps_clamps_and_floors() {
    // 1_000_000 - 25_000 = 975_000.
    assert_eq!(reduce_bps(1_000_000, 250), 975_000);
    // bps beyond 100% clamps to full reduction -> 0.
    assert_eq!(reduce_bps(100, 10_001), 0);
    assert_eq!(reduce_bps(100, 50_000), 0);
    // Zero reduction is identity.
    assert_eq!(reduce_bps(999, 0), 999);
}

#[test]
fn scale_signed_bps_up_down_and_wipeout() {
    // +50%: 1_000_000 * 15000/10000 = 1_500_000.
    assert_eq!(scale_signed_bps(1_000_000, 5_000), 1_500_000);
    // -30%: 1_000_000 * 7000/10000 = 700_000.
    assert_eq!(scale_signed_bps(1_000_000, -3_000), 700_000);
    // -100% clamps to 0.
    assert_eq!(scale_signed_bps(1_000_000, -10_000), 0);
    // Worse than -100% still clamps to 0 (never negative from price).
    assert_eq!(scale_signed_bps(1_000_000, -20_000), 0);
    // Large positive move exceeds u64 range conceptually but i128 holds it:
    // 1_000_000_000 * (10000+500000)/10000 = 1_000_000_000 * 51 = 51_000_000_000.
    assert_eq!(scale_signed_bps(1_000_000_000, 500_000), 51_000_000_000);
}

#[test]
fn i128_to_u64_saturating_edges() {
    assert_eq!(i128_to_u64_saturating(-5), 0);
    assert_eq!(i128_to_u64_saturating(0), 0);
    assert_eq!(i128_to_u64_saturating(123), 123);
    assert_eq!(i128_to_u64_saturating(u64::MAX as i128), u64::MAX);
    assert_eq!(i128_to_u64_saturating(u64::MAX as i128 + 10), u64::MAX);
}
