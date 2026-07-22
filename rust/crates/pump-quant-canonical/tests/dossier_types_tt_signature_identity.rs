// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'types' component (leaf 'tt_signature_identity').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_canonical::types::*;

#[test]
fn signature_preserves_bytes_and_orders_lexicographically() {
    let a = Signature::new([7u8; 64]);
    // Raw bytes round-trip exactly.
    assert_eq!(a.bytes(), &[7u8; 64]);

    // Equal bytes => equal identity (the merge key).
    let a2 = Signature::new([7u8; 64]);
    assert_eq!(a, a2);

    // Distinct bytes => distinct identity.
    let b = Signature::new([8u8; 64]);
    assert_ne!(a, b);

    // Byte-lexicographic ordering for deterministic iteration / eviction tie-break.
    assert!(a < b);
    let mut lo = [0u8; 64];
    lo[0] = 1;
    let mut hi = [0u8; 64];
    hi[0] = 2;
    assert!(Signature::new(lo) < Signature::new(hi));
    // First differing byte dominates: [1,9,...] < [2,0,...].
    let mut x = [0u8; 64];
    x[0] = 1;
    x[1] = 9;
    let mut y = [0u8; 64];
    y[0] = 2;
    assert!(Signature::new(x) < Signature::new(y));
}
