#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_strategy::economic_gate::*;

#[test]
fn prop_effective_fixed_multiplier() {
    assert_eq!(effective_fixed_lamports(100, 0), Some(100));
    // 26.8% failure -> multiplier ~1.366 -> ~136
    let e = effective_fixed_lamports(100, 2680).unwrap();
    assert!(e >= 136 && e <= 137);
    // monotone: higher failure rate -> higher effective fixed
    assert!(
        effective_fixed_lamports(100, 5000).unwrap() > effective_fixed_lamports(100, 2680).unwrap()
    );
    assert_eq!(effective_fixed_lamports(100, 10_000), None);
}
