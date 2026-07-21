#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_core::reducer::*;
#[test]
fn prop_apply_pure_and_deterministic() {
    let s0 = MarketState::test();
    let ev = CanonEvent::test(10, 0, 0, 1);
    let a = apply(&s0, &ev);
    let b = apply(&s0, &ev);
    assert_eq!(a, b);
    assert_eq!(s0, MarketState::test()); // input untouched
}
