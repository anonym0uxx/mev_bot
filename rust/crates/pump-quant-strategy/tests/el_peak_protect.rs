#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_strategy::exit_ladder::*;
#[test]
fn prop_protection_whole_life_monotone() {
    let p0 = protection_level_fp(1_000, 1_000, 500, 800);
    assert!(p0 > 0); // armed at entry, not after TP2
    let p1 = protection_level_fp(2_000, 1_000, 500, 800);
    assert!(p1 >= p0); // peak up -> protection never down
    assert!(
        protection_level_fp(2_000, 1_000, 500, 800) >= protection_level_fp(1_000, 1_000, 500, 800)
    );
}
