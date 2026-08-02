//! Tests for the Sender transport. No sockets: the `Transport` seam is mocked,
//! so URL assembly, body construction, parsing and error classification are all
//! exercised end to end.

use pq_stream_capture::rpc::{Reply, Transport};
use pq_stream_capture::sender::*;
use std::cell::RefCell;

/// Records what it was asked to send and replays a canned reply.
struct MockTransport {
    reply: Result<Reply, String>,
    seen: RefCell<Vec<(String, String)>>,
}

impl MockTransport {
    fn ok(body: &str, latency_us: u64) -> Self {
        Self {
            reply: Ok(Reply {
                body: body.to_string(),
                latency_us,
            }),
            seen: RefCell::new(Vec::new()),
        }
    }
    fn err(msg: &str) -> Self {
        Self {
            reply: Err(msg.to_string()),
            seen: RefCell::new(Vec::new()),
        }
    }
    fn last(&self) -> (String, String) {
        self.seen
            .borrow()
            .last()
            .cloned()
            .expect("no call recorded")
    }
}

impl Transport for MockTransport {
    fn post_json(&self, url: &str, body: &str) -> Result<Reply, String> {
        self.seen
            .borrow_mut()
            .push((url.to_string(), body.to_string()));
        match &self.reply {
            Ok(r) => Ok(Reply {
                body: r.body.clone(),
                latency_us: r.latency_us,
            }),
            Err(e) => Err(e.clone()),
        }
    }
}

const TX: &str = "AQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const SIG: &str =
    "5j7s6NiJS3JAkvgkoc18WVAsiSaci2pxB2A6ueCJP4tprA2TFg9wSyTLeYouxPBJEMzJinENTkpA52YStRW5Dia7";

fn https() -> SenderEndpoint {
    SenderEndpoint::new(GLOBAL_ENDPOINT, true, false).unwrap()
}

// ───────────────────────────── NEGATIVE CONTROLS ─────────────────────────────

#[test]
fn negative_control_plaintext_endpoint_is_refused() {
    // The regional endpoints really are http://. Submitting a signed tx in the
    // clear is a free front-run, so the default constructor must refuse.
    let e = SenderEndpoint::new("http://slc-sender.helius-rpc.com/fast", true, false);
    assert!(matches!(e, Err(SenderError::BadEndpoint(_))));
    // And the escape hatch must exist, explicitly named, for colocated callers.
    assert!(SenderEndpoint::new_allow_plaintext(
        "http://slc-sender.helius-rpc.com/fast",
        true,
        false
    )
    .is_ok());
}

#[test]
fn negative_control_endpoint_rejects_preexisting_query() {
    // Routing options are set through the constructor; a base that already has a
    // query string would silently produce two '?' segments.
    assert!(matches!(
        SenderEndpoint::new(
            "https://sender.helius-rpc.com/fast?swqos_only=true",
            true,
            false
        ),
        Err(SenderError::BadEndpoint(_))
    ));
    assert!(matches!(
        SenderEndpoint::new("", true, false),
        Err(SenderError::BadEndpoint(_))
    ));
    assert!(matches!(
        SenderEndpoint::new("sender.helius-rpc.com/fast", true, false),
        Err(SenderError::BadEndpoint(_))
    ));
}

#[test]
fn negative_control_non_base64_payload_cannot_reach_the_wire() {
    // The body is assembled by concatenation, so this is the injection guard.
    let quote = build_send_body("abc", "AAAA\",\"evil\":\"1");
    assert!(matches!(quote, Err(SenderError::BadPayload(_))));
    let spaced = build_send_body("abc", "AAAA AAAA");
    assert!(matches!(spaced, Err(SenderError::BadPayload(_))));
    assert!(matches!(
        build_send_body("abc", ""),
        Err(SenderError::BadPayload(_))
    ));
    let oversize = "A".repeat(MAX_TX_BASE64_LEN + 1);
    assert!(matches!(
        build_send_body("abc", &oversize),
        Err(SenderError::BadPayload(_))
    ));
}

#[test]
fn negative_control_request_id_is_constrained() {
    assert!(matches!(
        build_send_body("a\",\"x\":\"y", TX),
        Err(SenderError::BadPayload(_))
    ));
    assert!(matches!(
        build_send_body("", TX),
        Err(SenderError::BadPayload(_))
    ));
    assert!(build_send_body("send-1_A", TX).is_ok());
}

#[test]
fn negative_control_error_reply_is_never_read_as_success() {
    // An error message that embeds a result-looking substring must still be an
    // error. Checking result first would fail open on exactly this shape.
    let body = "{\"jsonrpc\":\"2.0\",\"error\":{\"code\":-32602,\"message\":\"bad param near result\"},\"id\":\"1\"}";
    match parse_send_reply(body) {
        Err(SenderError::Rpc { code, .. }) => assert_eq!(code, -32602),
        other => panic!("expected an Rpc error, got {other:?}"),
    }
}

#[test]
fn negative_control_empty_or_missing_result_is_unparseable() {
    assert!(matches!(
        parse_send_reply("{\"jsonrpc\":\"2.0\",\"id\":\"1\"}"),
        Err(SenderError::Unparseable(_))
    ));
    assert!(matches!(
        parse_send_reply("{\"jsonrpc\":\"2.0\",\"result\":\"\",\"id\":\"1\"}"),
        Err(SenderError::Unparseable(_))
    ));
}

#[test]
fn negative_control_bundle_cap_is_enforced() {
    let five = [TX, TX, TX, TX, TX];
    assert!(matches!(
        build_bundle_body("b", &five),
        Err(SenderError::BadPayload(_))
    ));
    assert!(matches!(
        build_bundle_body("b", &[]),
        Err(SenderError::BadPayload(_))
    ));
    assert!(build_bundle_body("b", &[TX, TX]).is_ok());
}

// ─────────────────────────────── URL assembly ────────────────────────────────

#[test]
fn url_matches_the_documented_routing_parameters() {
    let base = GLOBAL_ENDPOINT;
    assert_eq!(
        SenderEndpoint::new(base, true, false).unwrap().url(),
        "https://sender.helius-rpc.com/fast?swqos_only=true"
    );
    assert_eq!(
        SenderEndpoint::new(base, true, true).unwrap().url(),
        "https://sender.helius-rpc.com/fast?swqos_only=true&mev-protect=true"
    );
    assert_eq!(
        SenderEndpoint::new(base, false, false).unwrap().url(),
        "https://sender.helius-rpc.com/fast"
    );
    assert_eq!(
        SenderEndpoint::new(base, false, true).unwrap().url(),
        "https://sender.helius-rpc.com/fast?mev-protect=true"
    );
    // A trailing slash on the base must not produce a double slash.
    assert_eq!(
        SenderEndpoint::new("https://sender.helius-rpc.com/fast/", false, false)
            .unwrap()
            .url(),
        "https://sender.helius-rpc.com/fast"
    );
}

// ──────────────────────────────── body shape ─────────────────────────────────

#[test]
fn send_body_carries_the_required_params() {
    let body = build_send_body("send-1", TX).unwrap();
    assert!(body.contains("\"method\":\"sendTransaction\""));
    assert!(body.contains("\"encoding\":\"base64\""));
    // skipPreflight saves a round trip; maxRetries 0 leaves retry to Sender's
    // own routing rather than racing a duplicate transaction against ourselves.
    assert!(body.contains("\"skipPreflight\":true"));
    assert!(body.contains("\"maxRetries\":0"));
    assert!(body.contains(TX));
}

#[test]
fn bundle_body_carries_every_transaction_in_order() {
    let body = build_bundle_body("b1", &[TX, "QkJCQg=="]).unwrap();
    assert!(body.contains("\"method\":\"sendBundle\""));
    let first = body.find(TX).unwrap();
    let second = body.find("QkJCQg==").unwrap();
    assert!(first < second, "bundle order must be preserved");
}

// ───────────────────────────── client round trip ─────────────────────────────

#[test]
fn successful_send_returns_signature_and_millisecond_latency() {
    let reply = format!("{{\"jsonrpc\":\"2.0\",\"result\":\"{SIG}\",\"id\":\"send-1\"}}");
    let t = MockTransport::ok(&reply, 41_812);
    let c = SenderClient::new(&t, https());

    let accepted = c.send_transaction("send-1", TX).unwrap();
    assert_eq!(accepted.signature, SIG);
    // 41_812 us truncates to 41 ms - integer division, no float rounding.
    assert_eq!(accepted.submit_latency_ms, 41);

    let (url, body) = t.last();
    assert_eq!(url, "https://sender.helius-rpc.com/fast?swqos_only=true");
    assert!(body.contains(TX));
}

#[test]
fn transport_failure_is_reported_with_a_redacted_url() {
    let t = MockTransport::err("connection reset");
    let e = SenderEndpoint::new("https://sender.helius-rpc.com/fast", false, false).unwrap();
    let c = SenderClient::new(&t, e);
    match c.send_transaction("send-1", TX) {
        Err(SenderError::Transport(m)) => {
            assert!(m.contains("connection reset"));
            assert!(m.contains("https://sender.helius-rpc.com"));
            assert!(!m.contains("/fast"), "path and query must be redacted");
        }
        other => panic!("expected a Transport error, got {other:?}"),
    }
}

#[test]
fn rpc_error_reply_surfaces_code_and_message() {
    let reply = "{\"jsonrpc\":\"2.0\",\"error\":{\"code\":-32002,\"message\":\"transaction simulation failed\"},\"id\":\"1\"}";
    let t = MockTransport::ok(reply, 5_000);
    let c = SenderClient::new(&t, https());
    match c.send_transaction("send-1", TX) {
        Err(SenderError::Rpc { code, message }) => {
            assert_eq!(code, -32002);
            assert_eq!(message, "transaction simulation failed");
        }
        other => panic!("expected an Rpc error, got {other:?}"),
    }
}

#[test]
fn a_rejected_payload_never_reaches_the_transport() {
    let t = MockTransport::ok("{\"result\":\"x\"}", 1_000);
    let c = SenderClient::new(&t, https());
    assert!(c.send_transaction("id", "not base64!!").is_err());
    assert!(
        t.seen.borrow().is_empty(),
        "validation must run before the socket"
    );
}

#[test]
fn errors_render_without_leaking_the_payload() {
    let e = SenderError::BadPayload("transaction is not plain base64".to_string());
    let rendered = format!("{e}");
    assert!(rendered.starts_with("sender payload rejected"));
    assert!(!rendered.contains(TX));
}
