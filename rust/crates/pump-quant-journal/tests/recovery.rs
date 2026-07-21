//! Leaf: crash-recovery scan.
//!
//! Buffers are assembled from real encoded frames; expected truncation offsets are
//! computed from the frame-size formula, so every expectation is derived
//! independently of the scanner under test.

use pump_quant_journal::frame::{Frame, FRAME_OVERHEAD};
use pump_quant_journal::recovery::{recovery_scan, StopReason};

const MAX: u32 = 1 << 20;

/// Build `n` contiguous frames (sequence `start..start+n`) with fixed payloads,
/// returning the concatenated bytes and the per-frame lengths.
fn build(start: u64, payloads: &[&[u8]]) -> (Vec<u8>, Vec<usize>) {
    let mut buf = Vec::new();
    let mut lens = Vec::new();
    for (i, p) in payloads.iter().enumerate() {
        let seq = start + i as u64;
        let f = Frame::new(1, 0, seq, p.to_vec());
        let len = f.encode_into(&mut buf).unwrap();
        lens.push(len);
    }
    (buf, lens)
}

#[test]
fn clean_buffer_recovers_all() {
    let (buf, lens) = build(100, &[b"aa", b"bbb", b"c", b"dddd"]);
    let report = recovery_scan(&buf, Some(100), MAX);
    assert_eq!(report.frames_recovered, 4);
    assert_eq!(report.valid_len, buf.len());
    assert_eq!(report.valid_len, lens.iter().sum::<usize>());
    assert_eq!(report.last_sequence, Some(103));
    assert_eq!(report.stop_reason, StopReason::CleanEnd);
}

#[test]
fn empty_buffer_is_clean_end() {
    let report = recovery_scan(&[], None, MAX);
    assert_eq!(report.frames_recovered, 0);
    assert_eq!(report.valid_len, 0);
    assert_eq!(report.last_sequence, None);
    assert_eq!(report.stop_reason, StopReason::CleanEnd);
}

#[test]
fn truncated_tail_stops_at_last_complete_frame() {
    let (mut buf, lens) = build(0, &[b"aaa", b"bbb", b"ccc"]);
    // Drop the last 4 bytes: the third frame is now incomplete.
    let full = buf.len();
    buf.truncate(full - 4);
    let report = recovery_scan(&buf, Some(0), MAX);
    assert_eq!(report.frames_recovered, 2);
    // Safe truncation point == end of the first two frames.
    assert_eq!(report.valid_len, lens[0] + lens[1]);
    assert_eq!(report.last_sequence, Some(1));
    assert_eq!(report.stop_reason, StopReason::Truncated);
}

#[test]
fn corruption_in_second_frame_stops_after_first() {
    let (mut buf, lens) = build(0, &[b"first", b"second", b"third"]);
    // Corrupt a payload byte inside the second frame (past the first frame's bytes,
    // past the second frame's header).
    let corrupt_at = lens[0] + FRAME_OVERHEAD; // first payload byte of frame 2
    buf[corrupt_at] ^= 0x01;
    let report = recovery_scan(&buf, Some(0), MAX);
    assert_eq!(report.frames_recovered, 1);
    assert_eq!(report.valid_len, lens[0]);
    assert_eq!(report.last_sequence, Some(0));
    assert_eq!(report.stop_reason, StopReason::BadChecksum);
}

#[test]
fn bad_magic_midstream_stops() {
    let (mut buf, lens) = build(0, &[b"one", b"two"]);
    // Clobber the magic of the second frame.
    let magic_at = lens[0];
    buf[magic_at] ^= 0xFF;
    let report = recovery_scan(&buf, None, MAX);
    assert_eq!(report.frames_recovered, 1);
    assert_eq!(report.valid_len, lens[0]);
    assert_eq!(report.stop_reason, StopReason::BadMagic);
}

#[test]
fn sequence_gap_detected_without_consuming_offending_frame() {
    // Frames encoded with sequences 0,1,3 (a gap where 2 should be).
    let (buf, lens) = {
        let mut buf = Vec::new();
        let mut lens = Vec::new();
        for seq in [0u64, 1, 3] {
            let f = Frame::new(1, 0, seq, vec![seq as u8]);
            lens.push(f.encode_into(&mut buf).unwrap());
        }
        (buf, lens)
    };
    let report = recovery_scan(&buf, Some(0), MAX);
    assert_eq!(report.frames_recovered, 2);
    assert_eq!(report.valid_len, lens[0] + lens[1]);
    assert_eq!(report.last_sequence, Some(1));
    match report.stop_reason {
        StopReason::SequenceGap { expected, found } => {
            assert_eq!(expected, 2);
            assert_eq!(found, 3);
        }
        other => panic!("expected SequenceGap, got {other:?}"),
    }
}

#[test]
fn sequence_not_checked_when_expected_is_none() {
    // Same gapped buffer, but no expectation -> all three frames recovered.
    let mut buf = Vec::new();
    for seq in [0u64, 1, 3] {
        Frame::new(1, 0, seq, vec![seq as u8])
            .encode_into(&mut buf)
            .unwrap();
    }
    let report = recovery_scan(&buf, None, MAX);
    assert_eq!(report.frames_recovered, 3);
    assert_eq!(report.valid_len, buf.len());
    assert_eq!(report.stop_reason, StopReason::CleanEnd);
}

#[test]
fn payload_bound_enforced_during_scan() {
    // A valid frame with a 100-byte payload, scanned with a 10-byte cap.
    let (buf, _) = build(0, &[&[0u8; 100]]);
    let report = recovery_scan(&buf, Some(0), 10);
    assert_eq!(report.frames_recovered, 0);
    assert_eq!(report.valid_len, 0);
    assert_eq!(report.stop_reason, StopReason::PayloadTooLarge);
}
