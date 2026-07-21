#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_core::fixedpoint::*;
#[test]
fn robust_slow_path_exact() {
    // a*b overflows u128 but (a*b)/c fits: a checked-only impl would wrongly return None.
    assert_eq!(mul_div_u128(u128::MAX, 3, 3), Some(u128::MAX));
    assert_eq!(mul_div_u128(u128::MAX, 6, 3), None); // = MAX*2, exceeds u128 -> None
    assert_eq!(
        mul_div_u128(u128::MAX, u128::MAX, u128::MAX),
        Some(u128::MAX)
    );
    // large-reserve swap where reserve_out*dx overflows u128 (real AMM regime)
    let big = amount_out(u128::MAX / 2, u128::MAX / 2, 1_000_000, 30, 10_000);
    assert!(big.is_some());
}
