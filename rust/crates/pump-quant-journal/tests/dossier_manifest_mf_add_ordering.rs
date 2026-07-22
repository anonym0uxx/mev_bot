// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'manifest' component (leaf 'mf_add_ordering').
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
fn mf_add_ordering_property() {
    // Contiguous, strictly-increasing-id run is accepted in order.
    let mut m = Manifest::new();
    assert!(m.is_empty());
    m.add(entry(0, 0, 9, 10, 100, 0xAA)).unwrap();
    m.add(entry(1, 10, 19, 10, 200, 0xBB)).unwrap();
    m.add(entry(2, 20, 24, 5, 50, 0xCC)).unwrap();
    assert_eq!(m.len(), 3);
    assert!(!m.is_empty());
    assert_eq!(m.get(1).map(|e| e.segment_id), Some(1));

    // NonContiguous: next must start at 25; start at 21. Rejected, unchanged.
    match m.add(entry(3, 21, 30, 10, 100, 1)) {
        Err(ManifestError::NonContiguous { expected, found }) => {
            assert_eq!(expected, 25);
            assert_eq!(found, 21);
        }
        other => panic!("expected NonContiguous, got {other:?}"),
    }
    assert_eq!(m.len(), 3);

    // SegmentOutOfOrder: contiguous seq but non-increasing id. Rejected, unchanged.
    match m.add(entry(2, 25, 30, 6, 60, 2)) {
        Err(ManifestError::SegmentOutOfOrder { previous, found }) => {
            assert_eq!(previous, 2);
            assert_eq!(found, 2);
        }
        other => panic!("expected SegmentOutOfOrder, got {other:?}"),
    }
    assert_eq!(m.len(), 3);

    // A valid continuation is still accepted after rejections (state intact).
    m.add(entry(3, 25, 30, 6, 60, 3)).unwrap();
    assert_eq!(m.len(), 4);

    // InvalidRange: first > last on the very first entry.
    let mut e = Manifest::new();
    match e.add(entry(0, 10, 5, 1, 10, 1)) {
        Err(ManifestError::InvalidRange { first, last }) => {
            assert_eq!(first, 10);
            assert_eq!(last, 5);
        }
        other => panic!("expected InvalidRange, got {other:?}"),
    }
    assert_eq!(e.len(), 0);
}
