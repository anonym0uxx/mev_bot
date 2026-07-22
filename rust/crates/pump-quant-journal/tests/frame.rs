//! Leaf: frame codec + checksum primitives.
//!
//! Independent expectations use published test vectors (CRC-32/ISO-HDLC check
//! value, FNV-1a-64 vectors) and hand-derived frame sizes, plus multi-input
//! round-trip and corruption properties — real algorithms, not hardcoded outputs.

use pump_quant_journal::checksum::{crc32, fnv1a64, Fnv1a64, FNV_OFFSET_BASIS_64};
use pump_quant_journal::frame::{
    encoded_len, Frame, FrameError, DEFAULT_MAX_PAYLOAD_LEN, FRAME_OVERHEAD, HEADER_LEN,
};

#[test]
fn crc32_known_check_vector() {
    // The canonical CRC-32/ISO-HDLC check value over ASCII "123456789".
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    // Empty input: init ^ finalxor of all-ones cancels to 0.
    assert_eq!(crc32(b""), 0x0000_0000);
}

#[test]
fn crc32_detects_single_bit_flip() {
    // Two different inputs give different CRCs.
    assert_ne!(crc32(b"hello world"), crc32(b"hello worle"));
    // Flip one bit in a fixed buffer and confirm the CRC changes.
    let mut base = *b"pump-quant";
    let c0 = crc32(&base);
    base[3] ^= 0b0000_0001;
    let c1 = crc32(&base);
    assert_ne!(c0, c1);
}

#[test]
fn fnv1a64_known_vectors() {
    // Empty input hashes to the offset basis.
    assert_eq!(fnv1a64(b""), FNV_OFFSET_BASIS_64);
    // Published FNV-1a-64 vectors.
    assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(fnv1a64(b"foobar"), 0x85944171f73967e8);
}

#[test]
fn fnv1a64_streaming_equals_oneshot_and_is_order_sensitive() {
    let mut h = Fnv1a64::new();
    h.update(b"foo");
    h.update(b"bar");
    assert_eq!(h.finish(), fnv1a64(b"foobar"));

    // Order sensitivity: "ab" != "ba".
    assert_ne!(fnv1a64(b"ab"), fnv1a64(b"ba"));
}

#[test]
fn encoded_len_matches_overhead_for_multiple_sizes() {
    for &n in &[0usize, 1, 7, 32, 1000] {
        // Independent expectation: payload + 22 header + 4 trailer.
        assert_eq!(encoded_len(n), Some(n + FRAME_OVERHEAD));
    }
    assert_eq!(HEADER_LEN, 22);
    assert_eq!(FRAME_OVERHEAD, 26);
}

#[test]
fn round_trip_multiple_frames() {
    let cases: &[(u16, u32, u64, &[u8])] = &[
        (1, 0, 0, b""),
        (2, 7, 42, b"x"),
        (0xFFFF, 0xDEAD_BEEF, u64::MAX, b"the quick brown fox"),
        (3, 1, 1, &[0u8; 256]),
    ];
    for &(schema, epoch, seq, payload) in cases {
        let frame = Frame::new(schema, epoch, seq, payload.to_vec());
        let bytes = frame.encode().unwrap();
        // Length matches the independent formula.
        assert_eq!(bytes.len(), payload.len() + FRAME_OVERHEAD);
        assert_eq!(frame.encoded_len(), Some(bytes.len()));

        let decoded = Frame::decode(&bytes, DEFAULT_MAX_PAYLOAD_LEN).unwrap();
        assert_eq!(decoded.consumed, bytes.len());
        assert_eq!(decoded.frame, frame);
        assert_eq!(decoded.frame.schema_version, schema);
        assert_eq!(decoded.frame.connection_epoch, epoch);
        assert_eq!(decoded.frame.sequence, seq);
        assert_eq!(decoded.frame.payload, payload);
    }
}

#[test]
fn decode_ignores_trailing_bytes_and_reports_consumed() {
    let frame = Frame::new(1, 2, 3, b"abc".to_vec());
    let mut bytes = frame.encode().unwrap();
    let real_len = bytes.len();
    bytes.extend_from_slice(b"TRAILING GARBAGE");
    let decoded = Frame::decode(&bytes, DEFAULT_MAX_PAYLOAD_LEN).unwrap();
    assert_eq!(decoded.consumed, real_len);
    assert_eq!(decoded.frame.payload, b"abc");
}

#[test]
fn decode_too_short_for_header() {
    let short = [0u8; HEADER_LEN - 1];
    match Frame::decode(&short, DEFAULT_MAX_PAYLOAD_LEN) {
        Err(FrameError::TooShortForHeader { have }) => assert_eq!(have, HEADER_LEN - 1),
        other => panic!("expected TooShortForHeader, got {other:?}"),
    }
}

#[test]
fn decode_bad_magic() {
    let frame = Frame::new(1, 2, 3, b"abc".to_vec());
    let mut bytes = frame.encode().unwrap();
    bytes[0] ^= 0xFF;
    match Frame::decode(&bytes, DEFAULT_MAX_PAYLOAD_LEN) {
        Err(FrameError::BadMagic { .. }) => {}
        other => panic!("expected BadMagic, got {other:?}"),
    }
}

#[test]
fn decode_truncated_reports_needed_length() {
    let frame = Frame::new(1, 2, 3, b"abcdef".to_vec());
    let bytes = frame.encode().unwrap();
    let full = bytes.len();
    let cut = &bytes[..full - 3];
    match Frame::decode(cut, DEFAULT_MAX_PAYLOAD_LEN) {
        Err(FrameError::Truncated { have, need }) => {
            assert_eq!(have, full - 3);
            assert_eq!(need, full);
        }
        other => panic!("expected Truncated, got {other:?}"),
    }
}

#[test]
fn decode_bad_checksum_on_payload_flip() {
    let frame = Frame::new(1, 2, 3, b"abcdef".to_vec());
    let mut bytes = frame.encode().unwrap();
    // Flip a payload byte (index HEADER_LEN is the first payload byte).
    bytes[HEADER_LEN] ^= 0x01;
    match Frame::decode(&bytes, DEFAULT_MAX_PAYLOAD_LEN) {
        Err(FrameError::BadChecksum { expected, found }) => assert_ne!(expected, found),
        other => panic!("expected BadChecksum, got {other:?}"),
    }
}

#[test]
fn decode_payload_too_large_against_bound() {
    let frame = Frame::new(1, 2, 3, vec![0u8; 100]);
    let bytes = frame.encode().unwrap();
    // Declared payload is 100; cap it at 50.
    match Frame::decode(&bytes, 50) {
        Err(FrameError::PayloadTooLarge { len, max }) => {
            assert_eq!(len, 100);
            assert_eq!(max, 50);
        }
        other => panic!("expected PayloadTooLarge, got {other:?}"),
    }
}
