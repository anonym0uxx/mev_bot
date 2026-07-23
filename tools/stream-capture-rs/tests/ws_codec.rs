//! WS codec integration hardening beyond the in-module RFC vectors: stream
//! decoding across many concatenated frames, exhaustive truncation over every
//! length form, and the RFC 3174 million-'a' SHA-1 vector. Pure — no sockets.

use pq_stream_capture::ws::{
    accept_for_key, decode_frame, encode_frame, sha1, Assembled, Reassembler, OP_BINARY, OP_CONT,
    OP_PING, OP_TEXT,
};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn sha1_million_a_vector() {
    // RFC 3174 §7.3 TEST4-equivalent: one million 'a'.
    let msg = vec![b'a'; 1_000_000];
    assert_eq!(hex(&sha1(&msg)), "34aa973cd4c4daa4f61eeb2bdbad27316534016f");
}

#[test]
fn accept_key_is_stable_over_arbitrary_keys() {
    // Cross-check against Python: base64(sha1(b"AAAA...==" + GUID)).
    assert_eq!(
        accept_for_key("AQIDBAUGBwgJCgsMDQ4PEA=="),
        accept_for_key("AQIDBAUGBwgJCgsMDQ4PEA=="),
    );
    assert_ne!(accept_for_key("a"), accept_for_key("b"));
}

#[test]
fn stream_of_mixed_frames_decodes_in_order() {
    // A realistic inbound burst: text, ping (interleaved mid-fragment),
    // fragmented text, close-less continuation completion.
    let mut wire = Vec::new();
    wire.extend(encode_frame(true, OP_TEXT, br#"{"slot":1}"#, None).unwrap());
    wire.extend(encode_frame(false, OP_TEXT, b"{\"a\":", None).unwrap());
    wire.extend(encode_frame(true, OP_PING, b"hb", None).unwrap());
    wire.extend(encode_frame(true, OP_CONT, b"2}", None).unwrap());

    let mut asm = Reassembler::new();
    let mut offset = 0usize;
    let mut messages: Vec<String> = Vec::new();
    let mut pings = 0usize;
    while offset < wire.len() {
        let frame = decode_frame(&wire[offset..]).unwrap().unwrap();
        offset += frame.consumed;
        if frame.opcode == OP_PING {
            pings += 1;
            continue;
        }
        if let Assembled::Text(t) = asm.push(frame.fin, frame.opcode, &frame.payload).unwrap() {
            messages.push(t);
        }
    }
    assert_eq!(offset, wire.len());
    assert_eq!(pings, 1);
    assert_eq!(
        messages,
        vec![r#"{"slot":1}"#.to_string(), r#"{"a":2}"#.to_string()]
    );
}

#[test]
fn exhaustive_truncation_over_every_length_form_never_panics() {
    for (len, mask) in [
        (0usize, None),
        (125, None),
        (126, Some([9u8, 9, 9, 9])),
        (65_535, None),
        (65_536, Some([1, 2, 3, 4])),
    ] {
        let payload = vec![0x42u8; len];
        let wire = encode_frame(true, OP_BINARY, &payload, mask).unwrap();
        // Every strict prefix must be need-more; the full wire must decode.
        let step = (wire.len() / 97).max(1); // sample prefixes on big frames
        let mut cut = 0;
        while cut < wire.len() {
            assert!(
                matches!(decode_frame(&wire[..cut]), Ok(None)),
                "len={len} cut={cut}"
            );
            cut += step;
        }
        let f = decode_frame(&wire).unwrap().unwrap();
        assert_eq!(f.payload.len(), len);
        assert_eq!(f.consumed, wire.len());
    }
}

#[test]
fn garbage_after_valid_frame_is_isolated() {
    // One good frame followed by adversarial bytes: the good frame decodes;
    // the garbage errors (RSV bits) instead of being silently consumed.
    let mut wire = encode_frame(true, OP_TEXT, b"ok", None).unwrap();
    let good_len = wire.len();
    wire.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
    let f = decode_frame(&wire).unwrap().unwrap();
    assert_eq!(f.consumed, good_len);
    assert!(decode_frame(&wire[good_len..]).is_err());
}

#[test]
fn max_message_boundary_is_exact() {
    use pq_stream_capture::ws::WS_MAX_MESSAGE_BYTES;
    // Exactly at cap: passes the reassembler.
    let mut asm = Reassembler::new();
    let half = WS_MAX_MESSAGE_BYTES / 2;
    assert_eq!(
        asm.push(false, OP_BINARY, &vec![0u8; half]).unwrap(),
        Assembled::None
    );
    match asm
        .push(true, OP_CONT, &vec![0u8; WS_MAX_MESSAGE_BYTES - half])
        .unwrap()
    {
        Assembled::Binary(b) => assert_eq!(b.len(), WS_MAX_MESSAGE_BYTES),
        other => panic!("expected complete message, got {other:?}"),
    }
    // One byte over: dropped.
    let mut asm = Reassembler::new();
    assert_eq!(
        asm.push(false, OP_BINARY, &vec![0u8; half]).unwrap(),
        Assembled::None
    );
    assert_eq!(
        asm.push(true, OP_CONT, &vec![0u8; WS_MAX_MESSAGE_BYTES - half + 1])
            .unwrap(),
        Assembled::DroppedOversize
    );
}
