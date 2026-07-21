#![allow(unused_imports)]
use pump_quant_protocol::curve::*;

/// Independent reference for the constant-product-with-fee output.
fn expected(reserve_in: u128, reserve_out: u128, amount_in: u128, fee_bps: u32) -> Option<u128> {
    if fee_bps as u128 > 10_000 {
        return None;
    }
    let net = amount_in * (10_000 - fee_bps as u128) / 10_000;
    let denom = reserve_in + net;
    if denom == 0 {
        return None;
    }
    Some(reserve_out * net / denom)
}

#[test]
fn matches_independent_reference_across_inputs() {
    let cases = [
        (1_000_000_000u128, 2_000_000_000u128, 100_000_000u128, 30u32),
        (5_000_000_000, 5_000_000_000, 1_000_000_000, 25),
        (1_000_000_000_000, 4_000_000_000_000, 3_333_333, 100),
        (10, 10, 5, 0),
        (u64::MAX as u128, u64::MAX as u128, u64::MAX as u128, 30),
        (777_777_777, 123_456_789, 55_555, 250),
    ];
    for (r_in, r_out, amt, fee) in cases {
        assert_eq!(
            pumpswap_amount_out(r_in, r_out, amt, fee),
            expected(r_in, r_out, amt, fee),
            "mismatch for r_in={r_in} r_out={r_out} amt={amt} fee={fee}"
        );
    }
}

#[test]
fn zero_fee_is_pure_constant_product() {
    // fee_bps = 0 => net == amount_in exactly.
    let r_in = 1_000_000u128;
    let r_out = 3_000_000u128;
    let amt = 500_000u128;
    let want = r_out * amt / (r_in + amt);
    assert_eq!(pumpswap_amount_out(r_in, r_out, amt, 0), Some(want));
}

#[test]
fn rejects_impossible_fee() {
    assert_eq!(pumpswap_amount_out(1000, 1000, 100, 10_001), None);
    assert_eq!(pumpswap_amount_out(1000, 1000, 100, u32::MAX), None);
}

#[test]
fn full_fee_yields_zero_out() {
    // fee_bps == 10_000 keeps 0% of input, so net == 0 => output 0.
    assert_eq!(pumpswap_amount_out(1000, 1000, 100, 10_000), Some(0));
}

#[test]
fn empty_input_reserve_with_zero_amount_is_none() {
    // reserve_in + net == 0 => divide-by-zero guard returns None.
    assert_eq!(pumpswap_amount_out(0, 1000, 0, 0), None);
}

#[test]
fn no_overflow_at_full_width() {
    // Large but valid u128 magnitudes must not overflow the checked math:
    // reserve_out * net stays below 2^128 (~2^120 here), so a real value
    // is produced rather than the overflow guard tripping.
    let big = 1u128 << 60;
    let got = pumpswap_amount_out(big, big, big, 30);
    assert_eq!(got, expected(big, big, big, 30));
    assert!(got.is_some());
}

#[test]
fn overflow_returns_none_not_panic() {
    // reserve_out * net would exceed u128 (~2^180); the checked math must
    // surface None rather than wrapping or panicking.
    let huge = 1u128 << 90;
    assert_eq!(pumpswap_amount_out(huge, huge, huge, 30), None);
}
