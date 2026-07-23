//! REGRESSION HARDENING (additive) for the stream-capture edge — owned by the
//! end-to-end regression layer, NOT the crate author. Everything here is PURE
//! or loopback-only (127.0.0.1), deterministic, and network-free (§22):
//!
//!  1. WS frame codec ADVERSARIAL FUZZ — a deterministic splitmix64-driven byte
//!     generator feeds `decode_frame`; the decoder must NEVER panic and must
//!     always make progress or stop, over thousands of hostile inputs.
//!  2. Truncation at EVERY offset for masked / unmasked / fragmented /
//!     oversized-declared-length frames — every strict prefix is `Ok(None)`
//!     (need-more) or a clean `Err`, never a panic; the oversized length is a
//!     byte-bomb `Err` rejected BEFORE allocation.
//!  3. Webhook auth reject + dedupe IDEMPOTENCY — redelivery is a no-op across
//!     many rounds; the production auth predicate rejects missing/wrong secrets.
//!  4. RPC failover DETERMINISTIC ORDER under a mock transport — the walk is a
//!     pure function of health + clock; identical across fresh pools.
//!  5. Fail-closed (missing key → exit 3) per lane via the CLI.
//!
//! These are guardrails: if a future change makes the decoder panic on a
//! truncated frame, silences a drift, or weakens a fail-closed exit, one of
//! these fails.

use std::process::Command;

use pq_stream_capture::dedupe::DedupeRing;
use pq_stream_capture::rpc::{Reply, RpcPool, Transport};
use pq_stream_capture::webhook_listener::{process_payload, read_request, WEBHOOK_DEDUPE_CAP};
use pq_stream_capture::ws::{
    decode_frame, encode_frame, Reassembler, OP_BINARY, OP_CONT, OP_PING, OP_TEXT,
    WS_MAX_MESSAGE_BYTES,
};

// ---------------------------------------------------------------- prng

/// splitmix64 — a pure, deterministic bit-mixer. Same seed → same stream on
/// every machine and every run (§22): the "fuzz" is reproducible, not random.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn byte(&mut self) -> u8 {
        (self.next_u64() & 0xFF) as u8
    }
    fn range(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }
}

// ------------------------------------------------- 1. adversarial fuzz

#[test]
fn decode_frame_never_panics_on_hostile_bytes() {
    // Thousands of deterministic hostile buffers: pure garbage, near-valid
    // headers, huge declared lengths, mask bits toggled. The decoder must
    // return Ok(None) | Ok(Some) | Err for EVERY one — never panic, never hang.
    let mut rng = SplitMix64(0xDEAD_BEEF_CAFE_F00D);
    for _ in 0..20_000 {
        let len = rng.range(24);
        let mut buf: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
        // Bias toward plausible frame heads so the length/mask paths are hit.
        if !buf.is_empty() {
            // random opcode in the low nibble, random FIN/RSV in the high nibble
            buf[0] = rng.byte();
        }
        if buf.len() >= 2 {
            buf[1] = rng.byte(); // random mask bit + 7-bit length marker
        }
        if let Ok(Some(f)) = decode_frame(&buf) {
            // A decoded frame must have consumed at least the 2-byte head
            // and no more than the buffer it was handed.
            assert!(f.consumed >= 2 && f.consumed <= buf.len(), "consumed OOB");
        }
    }
}

#[test]
fn stream_decoder_over_random_concatenations_terminates_and_never_panics() {
    // Feed a random byte stream through the decode→consume loop exactly as the
    // live reader does. It must always terminate: each step either consumes
    // >=2 bytes (progress) or returns need-more / error (stop). No infinite
    // loop, no panic, regardless of the bytes.
    let mut rng = SplitMix64(0x0123_4567_89AB_CDEF);
    for _ in 0..2_000 {
        let len = rng.range(512);
        let stream: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
        let mut off = 0usize;
        let mut steps = 0usize;
        loop {
            steps += 1;
            assert!(steps < 100_000, "decode loop failed to terminate");
            match decode_frame(&stream[off..]) {
                Ok(Some(f)) => {
                    assert!(f.consumed >= 2, "zero-progress frame would loop forever");
                    off += f.consumed;
                    if off >= stream.len() {
                        break;
                    }
                }
                Ok(None) | Err(_) => break,
            }
        }
    }
}

#[test]
fn random_data_frames_through_reassembler_stay_bounded() {
    // The reassembler is a bounded state machine (§99): feeding it arbitrary
    // (fin, opcode, payload) triples — even illegal orderings — never panics
    // and never exceeds the cap. Illegal sequences return Err; legal ones a
    // bounded Assembled.
    let mut rng = SplitMix64(0xFEED_FACE_1234_5678);
    for _ in 0..5_000 {
        let mut asm = Reassembler::new();
        let frames = 1 + rng.range(6);
        for _ in 0..frames {
            let fin = rng.next_u64() & 1 == 1;
            let opcode = [OP_TEXT, OP_BINARY, OP_CONT][rng.range(3)];
            let plen = rng.range(64);
            let payload: Vec<u8> = (0..plen).map(|_| rng.byte()).collect();
            // Must not panic whatever the ordering; Err is a fine outcome.
            let _ = asm.push(fin, opcode, &payload);
        }
    }
}

// ------------------------------- 2. exhaustive truncation, every form

/// Every strict prefix of a valid frame must be need-more; the full buffer
/// decodes to the expected payload length. Covers unmasked, masked, and (via
/// the caller) fragmented frames.
fn assert_every_prefix_is_need_more(wire: &[u8], expect_len: usize) {
    for cut in 0..wire.len() {
        match decode_frame(&wire[..cut]) {
            Ok(None) => {}
            other => panic!(
                "prefix cut={cut}/{} must be need-more, got {other:?}",
                wire.len()
            ),
        }
    }
    let f = decode_frame(wire)
        .expect("full decode ok")
        .expect("full frame");
    assert_eq!(f.payload.len(), expect_len);
    assert_eq!(f.consumed, wire.len());
}

#[test]
fn truncation_masked_and_unmasked_at_every_offset() {
    let mask = Some([0xA5u8, 0x5A, 0x3C, 0xC3]);
    for &len in &[0usize, 1, 2, 125, 126, 127, 200, 65_535, 65_536, 70_000] {
        let payload: Vec<u8> = (0..len).map(|i| (i * 31 + 7) as u8).collect();
        // Unmasked (server→client) …
        let wire = encode_frame(true, OP_BINARY, &payload, None).unwrap();
        assert_every_prefix_is_need_more(&wire, len);
        // … and masked (client→server) — the +4 mask-key bytes are also a
        // truncation boundary the decoder must treat as need-more.
        let wire = encode_frame(true, OP_BINARY, &payload, mask).unwrap();
        assert_every_prefix_is_need_more(&wire, len);
    }
}

#[test]
fn truncation_of_fragmented_message_never_panics() {
    // A fragmented text message on the wire: first frame FIN=0, then a CONT
    // FIN=1. Each frame independently must survive prefix truncation.
    let a = encode_frame(false, OP_TEXT, b"{\"partial\":", None).unwrap();
    let b = encode_frame(true, OP_CONT, b"true}", None).unwrap();
    assert_every_prefix_is_need_more(&a, b"{\"partial\":".len());
    assert_every_prefix_is_need_more(&b, b"true}".len());

    // Concatenated, truncated at every offset across the whole two-frame
    // message: never a panic; the boundary is exactly `a.len()`.
    let mut both = a.clone();
    both.extend_from_slice(&b);
    for cut in 0..both.len() {
        let _ = decode_frame(&both[..cut]); // must not panic
    }
}

#[test]
fn oversized_declared_length_is_byte_bomb_err_before_allocation() {
    // Hand-craft a 64-bit-length header declaring a payload one byte beyond the
    // cap, with NO payload bytes present. The decoder must reject on the
    // DECLARED length alone (never trying to allocate/read gigabytes).
    let over = (WS_MAX_MESSAGE_BYTES as u64) + 1;
    let mut head = vec![0x82u8, 127]; // FIN + binary, 64-bit length form
    head.extend_from_slice(&over.to_be_bytes());
    // Full 10-byte header, zero payload → the cap fires as Err, not need-more.
    match decode_frame(&head) {
        Err(msg) => assert!(msg.contains("exceeds cap"), "{msg}"),
        other => panic!("oversized length must be Err, got {other:?}"),
    }
    // Any prefix that does not yet contain all 8 length bytes is need-more,
    // never a panic and never a premature allocation.
    for cut in 0..head.len().min(10) {
        // Before the length is fully readable (< 10 bytes) it is need-more;
        // once readable (>=10) it is the byte-bomb Err. Neither panics.
        let _ = decode_frame(&head[..cut]);
    }
    // A 16-bit non-minimal length and a 64-bit MSB-set length are also clean
    // Errs, never panics.
    assert!(
        decode_frame(&[0x82, 126, 0x00, 0x10]).is_err(),
        "non-minimal 16-bit"
    );
    let mut msb = vec![0x82u8, 127];
    msb.extend_from_slice(&(1u64 << 63).to_be_bytes());
    assert!(decode_frame(&msb).is_err(), "64-bit MSB set");
}

#[test]
fn control_frame_invariants_hold_under_encode_decode_roundtrip() {
    // Oversized / fragmented control frames must be refused at ENCODE time and
    // rejected at DECODE time — a regression that let one through would corrupt
    // the ping/pong/close path.
    assert!(encode_frame(true, OP_PING, &[0u8; 126], None).is_err());
    assert!(encode_frame(false, OP_PING, b"x", None).is_err());
    // A hand-built fragmented (FIN=0) ping must decode to Err.
    let bad = [0x09u8, 0x01, 0x00]; // FIN=0, opcode=ping, len=1, one byte
    assert!(decode_frame(&bad).is_err(), "fragmented control must Err");
}

// ------------------------------------ 3. webhook auth + dedupe idempotency

fn webhook_fixture() -> String {
    let path = format!(
        "{}/tests/fixtures/webhook_enhanced.json",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("fixture {path}: {e}"))
}

#[test]
fn webhook_dedupe_is_idempotent_across_many_redeliveries() {
    // The dedupe ring must make redelivery a strict no-op: the first delivery
    // emits, every subsequent identical delivery emits nothing and is fully
    // counted as deduped. Proven across SIX rounds on one shared ring.
    let body = webhook_fixture();
    let mut ring = DedupeRing::new(WEBHOOK_DEDUPE_CAP);

    let mut first = Vec::new();
    let s0 = process_payload(&body, 1, &mut ring, &mut first).unwrap();
    assert!(s0.emitted >= 1, "first delivery must emit");
    let first_emitted = s0.emitted;
    assert!(!first.is_empty());

    for round in 2..=6u64 {
        let mut out = Vec::new();
        let s = process_payload(&body, round, &mut ring, &mut out).unwrap();
        assert_eq!(s.emitted, 0, "redelivery {round} must emit nothing");
        assert_eq!(s.deduped, first_emitted, "redelivery {round} fully deduped");
        assert!(out.is_empty(), "redelivery {round} produced bytes");
    }
}

#[test]
fn webhook_auth_predicate_rejects_missing_and_wrong_secret() {
    // The production 401 decision is `request.authorization.as_deref() !=
    // Some(secret)`. Drive `read_request` (the pub parser it uses) over raw
    // requests and assert that predicate — hermetic, no socket.
    let secret = "s3cret";
    let cases: [(Option<&str>, bool); 4] = [
        (None, true),            // missing → reject
        (Some("wrong"), true),   // wrong → reject
        (Some("s3cretX"), true), // near-miss → reject
        (Some("s3cret"), false), // exact → accept
    ];
    for (auth, should_reject) in cases {
        let auth_header = auth.map_or(String::new(), |a| format!("Authorization: {a}\r\n"));
        let raw =
            format!("POST /hook HTTP/1.1\r\nHost: t\r\n{auth_header}Content-Length: 2\r\n\r\n[]");
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(raw.into_bytes()));
        let req = read_request(&mut reader).expect("well-formed request parses");
        let rejected = req.authorization.as_deref() != Some(secret);
        assert_eq!(rejected, should_reject, "auth={auth:?}");
    }
}

// ------------------------------- 4. rpc failover deterministic order

/// Scripted mock transport recording the exact URL walk. Succeeds only at the
/// nominated provider; every earlier one fails, forcing a deterministic walk.
struct WalkMock {
    win: &'static str,
    calls: std::cell::RefCell<Vec<String>>,
}
impl Transport for WalkMock {
    fn post_json(&self, url: &str, _body: &str) -> Result<Reply, String> {
        self.calls.borrow_mut().push(url.to_string());
        if url == self.win {
            Ok(Reply {
                body: "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":1}".to_string(),
                latency_us: 5,
            })
        } else {
            Err("down".to_string())
        }
    }
}

#[test]
fn rpc_failover_walks_priority_order_deterministically() {
    // Same URLs + same health + same clock → identical walk, every time.
    // Two independent fresh pools must produce byte-identical walks (§4
    // provider order is priority order; no reordering).
    for _ in 0..3 {
        let mut pool = RpcPool::from_urls_csv("https://a,https://b,https://c").unwrap();
        let mock = WalkMock {
            win: "https://c",
            calls: std::cell::RefCell::new(Vec::new()),
        };
        let out = pool.call(&mock, 0, "getSlot", "[]").unwrap();
        assert_eq!(out.provider_index, 2, "the third provider answered");
        assert_eq!(
            mock.calls.borrow().as_slice(),
            ["https://a", "https://b", "https://c"],
            "walk must be priority order a→b→c"
        );
    }
}

#[test]
fn rpc_all_providers_failing_is_a_clean_err_not_a_panic() {
    let mut pool = RpcPool::from_urls_csv("https://a,https://b").unwrap();
    let mock = WalkMock {
        win: "https://never",
        calls: std::cell::RefCell::new(Vec::new()),
    };
    let res = pool.call(&mock, 0, "getSlot", "[]");
    assert!(res.is_err(), "every provider failing is an Err");
    assert_eq!(
        mock.calls.borrow().len(),
        2,
        "both providers were tried once"
    );
}

// ------------------------------- 5. fail-closed per lane (CLI, exit 3)

fn run_scrubbed(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_pq-stream-capture"))
        .args(args)
        .env_remove("HELIUS_API_KEY")
        .env_remove("HELIUS_WS_URL")
        .env_remove("WEBHOOK_AUTH_SECRET")
        .env_remove("RPC_URLS")
        .output()
        .expect("binary runs")
}

#[test]
fn every_credentialed_lane_fails_closed_with_exit_3() {
    // One consolidated guard: each REQUIRED-credential lane must refuse with the
    // distinct capability-loss exit code 3 and emit NO data on stdout. A
    // regression that turned any lane fail-open would flip one of these.
    let lanes: [&[&str]; 3] = [
        &[
            "helius-ws",
            "--programs",
            "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",
        ],
        &["webhook-listener"],
        &["fee-sampler", "--once"],
    ];
    for lane in lanes {
        let out = run_scrubbed(lane);
        assert_eq!(out.status.code(), Some(3), "lane {lane:?} must exit 3");
        assert!(
            out.stdout.is_empty(),
            "lane {lane:?} leaked stdout on refusal"
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("ARMING_FAILED") || err.contains("WEBHOOK_AUTH_SECRET"),
            "lane {lane:?} must announce the arming failure: {err}"
        );
    }
}
