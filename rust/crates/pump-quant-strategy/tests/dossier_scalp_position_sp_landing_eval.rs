#![allow(unused_imports)]
use pump_quant_strategy::scalp_position::*;

#[test]
fn prop_landing_is_adverse() {
    let p = 1_000_000u64;
    let buy = expected_landing(p, Side::Buy, 50, 30).unwrap();
    let sell = expected_landing(p, Side::Sell, 50, 30).unwrap();
    assert!(buy >= p && sell <= p);
    assert_eq!(expected_landing(p, Side::Buy, 0, 0).unwrap(), p);
    assert!(expected_landing(u64::MAX, Side::Buy, 10_000, 10_000).is_none() ||
            expected_landing(u64::MAX, Side::Buy, 10_000, 10_000).unwrap() >= u64::MAX / 2);
}
