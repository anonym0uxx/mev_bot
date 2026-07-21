#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_core::reducer::*;
#[test]
fn prop_dedup_and_gaps() {
    let mut s = SeqState::new();
    let k = |slot| EventKey::test(slot, 0, 0);
    assert!(matches!(admit(&mut s, k(1)), Admit::Apply));
    assert!(matches!(admit(&mut s, k(1)), Admit::Duplicate));
    assert!(matches!(admit(&mut s, k(5)), Admit::GapThenApply(3)));
    assert!(matches!(admit(&mut s, k(0)), Admit::Regression));
}
