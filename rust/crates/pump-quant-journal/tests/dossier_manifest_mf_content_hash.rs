// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'manifest' component (leaf 'mf_content_hash').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_journal::manifest::*;

fn entry(id: u64, first: u64, last: u64, frames: u32, bytes: u64, hash: u64) -> SealedSegment {
    SealedSegment {
        segment_id: id,
        schema_version: 1,
        connection_epoch: 0,
        first_sequence: first,
        last_sequence: last,
        frame_count: frames,
        byte_len: bytes,
        content_hash: hash,
    }
}

fn build() -> Manifest {
    let mut m = Manifest::new();
    m.add(entry(0, 0, 9, 10, 100, 0xAA)).unwrap();
    m.add(entry(1, 10, 19, 10, 200, 0xBB)).unwrap();
    m.add(entry(2, 20, 24, 5, 50, 0xCC)).unwrap();
    m
}

#[test]
fn mf_content_hash_property() {
    // Determinism: identical entry sequences fold to the identical hash.
    let base = build().content_hash();
    assert_eq!(build().content_hash(), base);

    // Empty manifest has a distinct, well-defined hash (domain-separated by count).
    let empty = Manifest::new().content_hash();
    assert_ne!(empty, base);

    // Field sensitivity: mutating exactly one field (content_hash of last entry)
    // changes the fold.
    let mut mh = Manifest::new();
    mh.add(entry(0, 0, 9, 10, 100, 0xAA)).unwrap();
    mh.add(entry(1, 10, 19, 10, 200, 0xBB)).unwrap();
    mh.add(entry(2, 20, 24, 5, 50, 0xCD)).unwrap();
    assert_ne!(mh.content_hash(), base);

    // byte_len sensitivity on the middle entry also moves the hash.
    let mut mb = Manifest::new();
    mb.add(entry(0, 0, 9, 10, 100, 0xAA)).unwrap();
    mb.add(entry(1, 10, 19, 10, 201, 0xBB)).unwrap();
    mb.add(entry(2, 20, 24, 5, 50, 0xCC)).unwrap();
    assert_ne!(mb.content_hash(), base);

    // Count separation: a two-entry prefix does not collide with the three-entry hash.
    let mut prefix = Manifest::new();
    prefix.add(entry(0, 0, 9, 10, 100, 0xAA)).unwrap();
    prefix.add(entry(1, 10, 19, 10, 200, 0xBB)).unwrap();
    assert_ne!(prefix.content_hash(), base);
}
