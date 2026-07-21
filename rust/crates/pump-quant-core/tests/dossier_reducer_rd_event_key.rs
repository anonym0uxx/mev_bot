#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_core::reducer::*;
#[test]
fn prop_key_source_independent() {
    let a = CanonEvent::test(100, 5, 0, /*source_seq*/ 1);
    let b = CanonEvent::test(100, 5, 0, /*source_seq*/ 999);
    assert_eq!(event_key(&a), event_key(&b));
    let c = CanonEvent::test(100, 6, 0, 1);
    assert!(event_key(&a) < event_key(&c));
}
