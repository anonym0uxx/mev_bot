// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'shred' component (leaf 'sh_parity_gate').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports)]
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
