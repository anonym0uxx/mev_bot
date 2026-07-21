#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_core::reducer::*;
#[test]
fn prop_quantize_contract() {
    assert_eq!(quantize_feature(1.25, 100), Some(125));
    assert_eq!(quantize_feature(-1.255, 1000), Some(-1255));
    assert_eq!(quantize_feature(f64::NAN, 100), None);
    assert_eq!(quantize_feature(f64::INFINITY, 100), None);
    assert_eq!(quantize_feature(0.5, 1), Some(1)); // half away from zero
}
