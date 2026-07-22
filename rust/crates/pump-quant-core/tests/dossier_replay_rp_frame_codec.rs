// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'replay' component (leaf 'rp_frame_codec').
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
use pump_quant_core::replay::*;

#[test]
fn prop_frame_roundtrip_and_corruption() {
    let mut buf = SegBuf::new();
    encode_frame(&mut buf, 1, 7, 42, b"hello-solana").unwrap();
    let (f, used) = decode_frame(buf.bytes()).unwrap();
    assert_eq!(
        (f.schema, f.epoch, f.seq, f.payload),
        (1, 7, 42, &b"hello-solana"[..])
    );
    let mut corrupt = buf.bytes().to_vec();
    corrupt[used / 2] ^= 0x01;
    assert!(matches!(decode_frame(&corrupt), Err(JErr::Crc)));
    assert!(matches!(
        decode_frame(&buf.bytes()[..used - 3]),
        Err(JErr::Truncated)
    ));
}
