#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_core::fixedpoint::*;
#[test]
fn prop_amount_out_golden() {
    let out = amount_out(1_000_000_000, 2_000_000_000, 100_000_000, 1, 100).unwrap();
    assert!(out > 0 && out < 2_000_000_000);
    assert_eq!(amount_out(0, 10, 5, 1, 100), None);
    assert_eq!(amount_out(10, 10, 0, 1, 100), None);
}
