// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'manifest' component (leaf 'mf_find_by_sequence').
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

#[test]
fn mf_find_by_sequence_property() {
    let mut m = Manifest::new();
    m.add(entry(0, 0, 9, 10, 100, 0xAA)).unwrap();
    m.add(entry(1, 10, 19, 10, 200, 0xBB)).unwrap();
    m.add(entry(2, 20, 24, 5, 50, 0xCC)).unwrap();

    // Every in-range sequence resolves to the segment whose [first,last] contains it.
    assert_eq!(m.find_by_sequence(0), Some(0));
    assert_eq!(m.find_by_sequence(9), Some(0));
    assert_eq!(m.find_by_sequence(10), Some(1));
    assert_eq!(m.find_by_sequence(15), Some(1));
    assert_eq!(m.find_by_sequence(19), Some(1));
    assert_eq!(m.find_by_sequence(20), Some(2));
    assert_eq!(m.find_by_sequence(24), Some(2));

    // Consistency: for every covered sequence, the found entry's range contains it.
    for s in 0u64..=24 {
        let idx = m.find_by_sequence(s).expect("covered");
        let e = m.get(idx).unwrap();
        assert!(e.first_sequence <= s && s <= e.last_sequence);
    }

    // Out-of-range past the end resolves to None.
    assert_eq!(m.find_by_sequence(25), None);
    assert_eq!(m.find_by_sequence(u64::MAX), None);

    // Empty manifest: nothing is found, span/totals reflect emptiness.
    let empty = Manifest::new();
    assert_eq!(empty.find_by_sequence(0), None);
    assert_eq!(empty.sequence_span(), None);
    assert_eq!(m.sequence_span(), Some((0, 24)));
}
