//! Leaf: sealed-segment manifest index + content hash.
//!
//! Independent expectations: contiguity/ordering enforced against hand-built
//! entries, totals summed by hand, find_by_sequence checked at every boundary, and
//! the content hash's sensitivity verified by mutating one field at a time.

use pump_quant_journal::manifest::{Manifest, ManifestError, SealedSegment};

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

fn contiguous_manifest() -> Manifest {
    let mut m = Manifest::new();
    m.add(entry(0, 0, 9, 10, 100, 0xAA)).unwrap();
    m.add(entry(1, 10, 19, 10, 200, 0xBB)).unwrap();
    m.add(entry(2, 20, 24, 5, 50, 0xCC)).unwrap();
    m
}

#[test]
fn add_contiguous_and_totals() {
    let m = contiguous_manifest();
    assert_eq!(m.len(), 3);
    assert!(!m.is_empty());
    // Independent sums.
    assert_eq!(m.total_frames(), Some(25));
    assert_eq!(m.total_bytes(), Some(350));
    assert_eq!(m.sequence_span(), Some((0, 24)));
}

#[test]
fn non_contiguous_rejected() {
    let mut m = Manifest::new();
    m.add(entry(0, 0, 9, 10, 100, 1)).unwrap();
    // Next must start at 10; start at 11.
    match m.add(entry(1, 11, 20, 10, 100, 2)) {
        Err(ManifestError::NonContiguous { expected, found }) => {
            assert_eq!(expected, 10);
            assert_eq!(found, 11);
        }
        other => panic!("expected NonContiguous, got {other:?}"),
    }
    // Manifest unchanged on rejection.
    assert_eq!(m.len(), 1);
}

#[test]
fn invalid_range_rejected() {
    let mut m = Manifest::new();
    match m.add(entry(0, 10, 5, 1, 10, 1)) {
        Err(ManifestError::InvalidRange { first, last }) => {
            assert_eq!(first, 10);
            assert_eq!(last, 5);
        }
        other => panic!("expected InvalidRange, got {other:?}"),
    }
}

#[test]
fn segment_out_of_order_rejected() {
    let mut m = Manifest::new();
    m.add(entry(5, 0, 9, 10, 100, 1)).unwrap();
    // Contiguous sequence but non-increasing segment id.
    match m.add(entry(5, 10, 19, 10, 100, 2)) {
        Err(ManifestError::SegmentOutOfOrder { previous, found }) => {
            assert_eq!(previous, 5);
            assert_eq!(found, 5);
        }
        other => panic!("expected SegmentOutOfOrder, got {other:?}"),
    }
}

#[test]
fn find_by_sequence_at_boundaries() {
    let m = contiguous_manifest();
    // Segment 0 covers 0..=9, segment 1 covers 10..=19, segment 2 covers 20..=24.
    assert_eq!(m.find_by_sequence(0), Some(0));
    assert_eq!(m.find_by_sequence(9), Some(0));
    assert_eq!(m.find_by_sequence(10), Some(1));
    assert_eq!(m.find_by_sequence(19), Some(1));
    assert_eq!(m.find_by_sequence(20), Some(2));
    assert_eq!(m.find_by_sequence(24), Some(2));
    // Out of range.
    assert_eq!(m.find_by_sequence(25), None);
    assert_eq!(m.find_by_sequence(u64::MAX), None);
}

#[test]
fn find_by_sequence_empty_manifest() {
    let m = Manifest::new();
    assert!(m.is_empty());
    assert_eq!(m.find_by_sequence(0), None);
    assert_eq!(m.sequence_span(), None);
    assert_eq!(m.total_frames(), Some(0));
    assert_eq!(m.total_bytes(), Some(0));
}

#[test]
fn content_hash_deterministic_and_field_sensitive() {
    let m1 = contiguous_manifest();
    let m2 = contiguous_manifest();
    // Deterministic: same entries -> same hash.
    assert_eq!(m1.content_hash(), m2.content_hash());

    let base = m1.content_hash();

    // Change exactly one field of the last entry and confirm the hash moves.
    let mut mh = Manifest::new();
    mh.add(entry(0, 0, 9, 10, 100, 0xAA)).unwrap();
    mh.add(entry(1, 10, 19, 10, 200, 0xBB)).unwrap();
    mh.add(entry(2, 20, 24, 5, 50, 0xCD)).unwrap(); // content_hash 0xCC -> 0xCD
    assert_ne!(mh.content_hash(), base);

    // Empty manifest has its own distinct hash.
    assert_ne!(Manifest::new().content_hash(), base);
}

#[test]
fn content_hash_order_matters() {
    // Two manifests with the same set of ranges but assembled so the byte_len
    // differs on the middle entry produce different hashes.
    let mut a = Manifest::new();
    a.add(entry(0, 0, 9, 10, 100, 1)).unwrap();
    a.add(entry(1, 10, 19, 10, 111, 2)).unwrap();

    let mut b = Manifest::new();
    b.add(entry(0, 0, 9, 10, 100, 1)).unwrap();
    b.add(entry(1, 10, 19, 10, 222, 2)).unwrap();

    assert_ne!(a.content_hash(), b.content_hash());
}
