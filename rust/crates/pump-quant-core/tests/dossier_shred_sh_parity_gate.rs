#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_core::shred::*;
#[test]
fn prop_parity_counts_and_verdict() {
    let s = [TxSig::test(1), TxSig::test(2), TxSig::test(2)]; // dup collapses
    let c = [TxSig::test(2), TxSig::test(3)];
    let p = slot_parity(&s, &c, &ArrivalMap::test());
    assert_eq!((p.matched, p.shred_only, p.canon_only), (1, 1, 1));
    assert!(matches!(p.verdict, ParityVerdict::Fail));
    let q = slot_parity(&[TxSig::test(7)], &[TxSig::test(7)], &ArrivalMap::test());
    assert!(matches!(q.verdict, ParityVerdict::Pass));
}
