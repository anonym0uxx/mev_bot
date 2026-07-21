// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'replay' component (leaf 'rp_recovery_scan').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports)]
use pump_quant_core::replay::*;

#[test]
fn prop_recovery_prefix_exact() {
    let mut buf = SegBuf::new();
    for i in 0..10u64 {
        encode_frame(&mut buf, 1, 1, i, &i.to_le_bytes()).unwrap();
    }
    let full = recover(buf.bytes());
    assert_eq!(full.frames.len(), 10);
    for cut in 1..buf.bytes().len() {
        let r = recover(&buf.bytes()[..cut]);
        assert!(r.frames.len() <= 10);
        assert!(r.valid_len <= cut);
        // frames reported are exactly those whose full extent fits in the cut prefix
        assert!(r.frames.iter().all(|m| m.end <= cut));
    }
}
