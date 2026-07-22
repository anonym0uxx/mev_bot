// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'shred' component (leaf 'sh_reassemble').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_core::shred::*;

#[test]
fn prop_reassembly_order_and_timing() {
    let set = CompleteSet::test(&[(2, b"cc", 30), (0, b"aa", 10), (1, b"bb", 5)]);
    let mut out = SegBuf::new();
    let meta = reassemble(&set, &mut out).unwrap();
    assert_eq!(out.bytes(), b"aabbcc"); // index order, not arrival order
    assert_eq!(meta.first_local_arrival_ns, 5); // earliest constituent
}
