//! Leaf: bounded segment append + atomic seal.
//!
//! Independent expectations: byte totals computed from the frame-size formula,
//! sequence spans computed from the start sequence, and the seal content hash
//! recomputed independently with FNV-1a-64 over the segment bytes.

use pump_quant_journal::checksum::fnv1a64;
use pump_quant_journal::frame::{encoded_len, Frame, FRAME_OVERHEAD};
use pump_quant_journal::recovery::{recovery_scan, StopReason};
use pump_quant_journal::segment::{Segment, SegmentError, SegmentLimits};

fn seg() -> Segment {
    Segment::new(7, 2, 99, 1000, SegmentLimits::new(1024, 1 << 20))
}

#[test]
fn append_assigns_contiguous_sequences_from_start() {
    let mut s = seg();
    assert_eq!(s.append(b"a").unwrap(), 1000);
    assert_eq!(s.append(b"bb").unwrap(), 1001);
    assert_eq!(s.append(b"ccc").unwrap(), 1002);
    assert_eq!(s.frame_count(), 3);
    assert_eq!(s.first_sequence(), Some(1000));
    assert_eq!(s.last_sequence(), Some(1002));
    assert!(!s.is_empty());
    assert!(!s.is_sealed());
}

#[test]
fn byte_len_matches_frame_size_formula() {
    let mut s = seg();
    let payloads: &[&[u8]] = &[b"", b"x", b"1234567890"];
    let mut expected = 0u64;
    for p in payloads {
        s.append(p).unwrap();
        expected += (p.len() + FRAME_OVERHEAD) as u64;
    }
    assert_eq!(s.byte_len(), expected);
    // Also equals sum of encoded_len for each payload.
    let via_encoded: u64 = payloads
        .iter()
        .map(|p| encoded_len(p.len()).unwrap() as u64)
        .sum();
    assert_eq!(s.byte_len(), via_encoded);
}

#[test]
fn full_on_max_frames() {
    let mut s = Segment::new(1, 1, 1, 0, SegmentLimits::new(2, 1 << 20));
    assert!(s.would_fit(1));
    s.append(b"a").unwrap();
    s.append(b"b").unwrap();
    assert!(!s.would_fit(1));
    assert_eq!(s.append(b"c"), Err(SegmentError::Full));
    assert_eq!(s.frame_count(), 2);
}

#[test]
fn full_on_max_bytes() {
    // One 1-byte frame is FRAME_OVERHEAD+1 bytes. Cap bytes so exactly one fits.
    let one = (FRAME_OVERHEAD + 1) as u64;
    let mut s = Segment::new(1, 1, 1, 0, SegmentLimits::new(1000, one));
    assert!(s.would_fit(1));
    s.append(b"a").unwrap();
    assert!(!s.would_fit(1));
    assert_eq!(s.append(b"b"), Err(SegmentError::Full));
    assert_eq!(s.byte_len(), one);
}

#[test]
fn seal_produces_correct_entry_and_independent_content_hash() {
    let mut s = seg();
    s.append(b"hello").unwrap();
    s.append(b"world!!").unwrap();

    // Independently recompute the expected content hash over the raw bytes.
    let expected_hash = fnv1a64(s.bytes());
    let expected_bytes = s.byte_len();

    let sealed = s.seal().unwrap();
    assert_eq!(sealed.segment_id, 7);
    assert_eq!(sealed.schema_version, 2);
    assert_eq!(sealed.connection_epoch, 99);
    assert_eq!(sealed.first_sequence, 1000);
    assert_eq!(sealed.last_sequence, 1001);
    assert_eq!(sealed.frame_count, 2);
    assert_eq!(sealed.byte_len, expected_bytes);
    assert_eq!(sealed.content_hash, expected_hash);
    assert!(s.is_sealed());
}

#[test]
fn append_after_seal_is_rejected() {
    let mut s = seg();
    s.append(b"a").unwrap();
    s.seal().unwrap();
    assert_eq!(s.append(b"b"), Err(SegmentError::Sealed));
    assert_eq!(s.seal(), Err(SegmentError::Sealed));
}

#[test]
fn sealing_empty_segment_is_rejected() {
    let mut s = seg();
    assert_eq!(s.seal(), Err(SegmentError::EmptySegment));
}

#[test]
fn segment_bytes_recover_cleanly_via_scan() {
    let mut s = seg();
    for i in 0..5u8 {
        s.append(&[i, i, i]).unwrap();
    }
    // Cross-check: the raw segment bytes decode back into exactly 5 contiguous
    // frames starting at the segment's first sequence.
    let report = recovery_scan(s.bytes(), s.first_sequence(), 1 << 20);
    assert_eq!(report.frames_recovered, 5);
    assert_eq!(report.valid_len, s.bytes().len());
    assert_eq!(report.last_sequence, s.last_sequence());
    assert_eq!(report.stop_reason, StopReason::CleanEnd);
}

#[test]
fn payload_too_large_is_rejected() {
    let mut s = Segment::new(1, 1, 1, 0, SegmentLimits::new(10, u64::MAX));
    // Build a payload one byte over the codec bound.
    let big = vec![0u8; pump_quant_journal::frame::DEFAULT_MAX_PAYLOAD_LEN as usize + 1];
    assert_eq!(s.append(&big), Err(SegmentError::PayloadTooLarge));
}

#[test]
fn first_frame_decodes_from_segment_buffer() {
    let mut s = seg();
    s.append(b"probe").unwrap();
    let decoded = Frame::decode(s.bytes(), 1 << 20).unwrap();
    assert_eq!(decoded.frame.sequence, 1000);
    assert_eq!(decoded.frame.schema_version, 2);
    assert_eq!(decoded.frame.connection_epoch, 99);
    assert_eq!(decoded.frame.payload, b"probe");
}
