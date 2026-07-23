//! Webhook-listener integration: LOOPBACK sockets only (the one sanctioned
//! network surface in the test suite — 127.0.0.1:0, no egress), plus pure
//! fixture-driven normalization of the enhanced-transaction payload.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use pq_stream_capture::dedupe::DedupeRing;
use pq_stream_capture::json::{self, Value};
use pq_stream_capture::webhook_listener::{process_payload, serve, whale_line, WEBHOOK_DEDUPE_CAP};

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("fixture {path}: {e}"))
}

fn test_now() -> u64 {
    1_753_142_500_000
}

/// Shared stdout stand-in for the serve thread.
#[derive(Clone)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// One raw HTTP exchange against the listener; returns the full response.
fn exchange(addr: std::net::SocketAddr, request: &str) -> String {
    let mut sock = TcpStream::connect(addr).expect("loopback connect");
    sock.write_all(request.as_bytes()).expect("write");
    let mut resp = String::new();
    sock.read_to_string(&mut resp).expect("read");
    resp
}

fn post(addr: std::net::SocketAddr, auth: Option<&str>, body: &str) -> String {
    let auth_header = auth.map_or(String::new(), |a| format!("Authorization: {a}\r\n"));
    exchange(
        addr,
        &format!(
            "POST /hook HTTP/1.1\r\nHost: t\r\n{auth_header}Content-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    )
}

// ------------------------------------------------------- loopback serving

#[test]
fn loopback_full_flow_auth_dedupe_and_emission() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().unwrap();
    let buf = SharedBuf(Arc::new(Mutex::new(Vec::new())));
    let out = buf.clone();
    let server = thread::spawn(move || {
        let mut out = out;
        serve(&listener, "s3cret", test_now, &mut out, Some(5));
    });

    // 1. GET is refused.
    let resp = exchange(addr, "GET /hook HTTP/1.1\r\nHost: t\r\n\r\n");
    assert!(resp.starts_with("HTTP/1.1 405 "), "{resp}");

    // 2. Missing auth is a 401.
    let resp = post(addr, None, "[]");
    assert!(resp.starts_with("HTTP/1.1 401 "), "{resp}");

    // 3. Wrong auth is a 401.
    let resp = post(addr, Some("wrong"), "[]");
    assert!(resp.starts_with("HTTP/1.1 401 "), "{resp}");

    // 4. Good auth + fixture payload is a 200 ok.
    let body = fixture("webhook_enhanced.json");
    let resp = post(addr, Some("s3cret"), &body);
    assert!(resp.starts_with("HTTP/1.1 200 "), "{resp}");
    assert!(resp.ends_with("ok"), "{resp}");

    // 5. Redelivery of the same payload dedupes (200 again, nothing new).
    let resp = post(addr, Some("s3cret"), &body);
    assert!(resp.starts_with("HTTP/1.1 200 "), "{resp}");

    server.join().expect("server thread");

    let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    // 2 tx objects × (raw + whale), second delivery fully deduped.
    assert_eq!(lines.len(), 4, "unexpected lines:\n{text}");
    for line in &lines {
        assert!(json::parse(line).is_ok(), "invalid NDJSON: {line}");
    }
    assert!(lines[0].starts_with("{\"lane\":\"helius_webhook\",\"recv_unix_ms\":1753142500000,"));
    assert!(lines[1].starts_with("{\"lane\":\"whale\","));
    assert!(lines[1].contains("\"kind\":\"SWAP\""));
    assert!(lines[3].contains("\"kind\":\"TRANSFER\""));
}

#[test]
fn loopback_oversize_body_is_413() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().unwrap();
    let buf = SharedBuf(Arc::new(Mutex::new(Vec::new())));
    let out = buf.clone();
    let server = thread::spawn(move || {
        let mut out = out;
        serve(&listener, "s3cret", test_now, &mut out, Some(1));
    });
    // Declared over-cap: rejected on the header alone, no body sent.
    let resp = exchange(
        addr,
        &format!(
            "POST / HTTP/1.1\r\nHost: t\r\nAuthorization: s3cret\r\n\
             Content-Length: {}\r\n\r\n",
            pq_stream_capture::webhook_listener::WEBHOOK_MAX_BODY_BYTES + 1
        ),
    );
    assert!(resp.starts_with("HTTP/1.1 413 "), "{resp}");
    server.join().expect("server thread");
    assert!(buf.0.lock().unwrap().is_empty(), "nothing may be emitted");
}

// ------------------------------------------------ fixture normalization

fn fixture_txs() -> Vec<Value> {
    match json::parse(&fixture("webhook_enhanced.json")).expect("fixture parses") {
        Value::Array(items) => items,
        other => panic!("fixture is not an array: {other:?}"),
    }
}

#[test]
fn whale_line_for_fixture_swap_is_exact() {
    let txs = fixture_txs();
    let line = whale_line(&txs[0], 7);
    let expected = concat!(
        "{\"lane\":\"whale\",\"recv_unix_ms\":7,",
        "\"sig\":\"5whaleSwapSig1111111111111111111111111111111111111111111111111111111111111111111111111\",",
        "\"slot\":347650001,\"ts\":1753142401,\"kind\":\"SWAP\",",
        "\"wallets\":[\"WhaLe1FeePayer1111111111111111111111111111\",",
        "\"PoolVau1tSo1Acct11111111111111111111111111\",",
        "\"PoolAuth111111111111111111111111111111111\",",
        "\"ProtoFeeOwner1111111111111111111111111111\"],",
        "\"mints\":[\"MintWif2pumpXXXXXXXXXXXXXXXXXXXXXXXXXXXpump\"],",
        "\"native_moved_lamports\":2000000000,",
        "\"largest_token_move\":{\"mint\":\"MintWif2pumpXXXXXXXXXXXXXXXXXXXXXXXXXXXpump\",",
        "\"amount\":68420913.371}}"
    );
    assert_eq!(line, expected);
}

#[test]
fn whale_line_for_fixture_transfer_is_exact() {
    let txs = fixture_txs();
    let line = whale_line(&txs[1], 8);
    assert!(line.contains("\"kind\":\"TRANSFER\""));
    assert!(line.contains("\"native_moved_lamports\":150000000000"));
    assert!(line.contains("\"mints\":[]"));
    assert!(line.contains("\"largest_token_move\":null"));
    assert!(line.contains("\"wallets\":[\"WhaLe2Sender111111111111111111111111111111\",\"ExchDeposit1111111111111111111111111111111\"]"));
}

#[test]
fn raw_line_preserves_events_swap_untouched() {
    // §6.3: the raw emission must survive the lossless JSON round trip —
    // including events.swap's stringified amounts and float token amounts.
    let body = fixture("webhook_enhanced.json");
    let mut ring = DedupeRing::new(WEBHOOK_DEDUPE_CAP);
    let mut out = Vec::new();
    let stats = process_payload(&body, 1, &mut ring, &mut out).unwrap();
    assert_eq!(stats.emitted, 2);
    let text = String::from_utf8(out).unwrap();
    let raw_line = text.lines().next().unwrap();
    assert!(raw_line.contains("\"nativeInput\":{\"account\":\"WhaLe1FeePayer1111111111111111111111111111\",\"amount\":\"2000000000\"}"));
    assert!(
        raw_line.contains("\"rawTokenAmount\":{\"tokenAmount\":\"68420913371000\",\"decimals\":6}")
    );
    assert!(
        raw_line.contains("\"tokenAmount\":68420913.371"),
        "float text preserved"
    );
}

#[test]
fn process_payload_dedupe_ring_spans_deliveries() {
    let body = fixture("webhook_enhanced.json");
    let mut ring = DedupeRing::new(WEBHOOK_DEDUPE_CAP);
    let mut first = Vec::new();
    process_payload(&body, 1, &mut ring, &mut first).unwrap();
    let mut second = Vec::new();
    let stats = process_payload(&body, 2, &mut ring, &mut second).unwrap();
    assert_eq!(stats.emitted, 0);
    assert_eq!(stats.deduped, 2);
    assert!(second.is_empty());
}
