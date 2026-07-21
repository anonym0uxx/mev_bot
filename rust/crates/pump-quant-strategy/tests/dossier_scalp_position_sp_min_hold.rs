#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_strategy::scalp_position::*;

#[test]
fn prop_exemptions_absolute() {
    for c in [
        ExitClass::Emergency,
        ExitClass::SellabilityFailure,
        ExitClass::RiskLimit,
        ExitClass::CircuitBreaker,
    ] {
        assert!(!min_hold_blocks_exit(Lane::Scalp, 0, u64::MAX, c));
    }
    assert!(min_hold_blocks_exit(Lane::Scalp, 10, 20, ExitClass::Normal));
    assert!(!min_hold_blocks_exit(
        Lane::Scalp,
        20,
        20,
        ExitClass::Normal
    ));
    assert!(!min_hold_blocks_exit(Lane::Scalp, 5, 0, ExitClass::Normal)); // zero min-hold legal
}
