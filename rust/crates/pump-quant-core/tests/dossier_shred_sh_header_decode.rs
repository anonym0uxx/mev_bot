// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'shred' component (leaf 'sh_header_decode').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(
    unused_imports,
    dead_code,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_core::shred::*;

#[test]
fn prop_header_bounds() {
    let good = ShredHeader::test_bytes(100, 3, 0);
    assert!(decode_header(&good).is_ok());
    for cut in 0..good.len().min(16) {
        assert!(decode_header(&good[..cut]).is_err()); // never panics, always Err
    }
    let mut bad_len = good.clone();
    bad_len[BAD_PAYLOAD_LEN_OFF] = 0xFF; // claims payload beyond buffer
    assert!(decode_header(&bad_len).is_err());
}
