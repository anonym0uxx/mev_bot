#![allow(unused_imports)]
use pump_quant_core::fixedpoint::*;
#[test]
fn prop_mul_div_matches_reference() {
    assert_eq!(mul_div_u128(6, 7, 3), Some(14));
    assert_eq!(mul_div_u128(10, 10, 0), None);
    assert_eq!(mul_div_u128(u128::MAX, 2, 1), None); // overflow -> None
    assert_eq!(mul_div_u128(100, 0, 5), Some(0));
}
