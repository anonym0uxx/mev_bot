//! `webhook-listener` subcommand — pure-std HTTP/1.1 listener for Helius
//! ENHANCED webhooks (whale / address-activity lane).
//!
//! Deployment shape: binds 127.0.0.1 (default [`DEFAULT_BIND`]) and speaks
//! plain HTTP — Helius requires an httpS webhook URL, so this sits BEHIND a
//! TLS-terminating reverse proxy (caddy/nginx) that forwards to loopback.
//! POST only. The `Authorization` header must equal env
//! `WEBHOOK_AUTH_SECRET` verbatim (set the same value in the Helius webhook
//! config): missing env is fail-closed arming (exit [`EXIT_ARMING`]), a wrong
//! header is a counted 401.
//!
//! Latency contract: Helius retries a delivery only 3× and then DROPS it, so
//! the listener reads the body, responds `200 ok` IMMEDIATELY, and only then
//! parses/emits — acknowledgment never waits on processing (well inside the
//! 1 s ceiling; processing is pure in-memory work but the ordering is the
//! guarantee). Body is capped at [`WEBHOOK_MAX_BODY_BYTES`] (§99). Deliveries
//! are deduped by transaction signature in a bounded ring
//! ([`WEBHOOK_DEDUPE_CAP`]) because Helius redelivers on slow/failed acks.
//!
//! Emission per transaction object, BOTH (§6.3 raw first, derived second):
//! * `{"lane":"helius_webhook","recv_unix_ms":...,"raw":<object>}` — the
//!   untouched enhanced-transaction object (lossless JSON round trip);
//! * a normalized whale line ([`whale_line`]) — a pure, fixture-tested
//!   projection for the corroboration engine.
//!
//! TIER DISCIPLINE (§6.6/§28): Helius's enhanced parse is third-party
//! interpretation — this lane is DISCOVERY/CORROBORATION tier ONLY, never
//! canonical truth. Canonical facts come from raw transactions on the gRPC/WS
//! lanes; a whale line is a pointer telling the engine where to look.

use std::cmp::Ordering;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use crate::dedupe::DedupeRing;
use crate::emit;
use crate::json::{self, Value};

/// Body cap (2 MiB, §99): the largest real enhanced-webhook batches are far
/// smaller; beyond this is a 413, not an allocation.
pub const WEBHOOK_MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

/// Dedupe ring capacity (transaction signatures remembered).
pub const WEBHOOK_DEDUPE_CAP: usize = 8192;

/// Default bind address — loopback ONLY by design (TLS terminates upstream).
pub const DEFAULT_BIND: &str = "127.0.0.1:8787";

/// Fail-closed arming exit code (§18.8), same convention as the other lanes.
pub const EXIT_ARMING: u8 = 3;

/// Per-connection socket read timeout (seconds).
pub const CONN_READ_TIMEOUT_SECS: u64 = 5;

/// Max header line length / header count (§99 bounding).
const MAX_HEADER_LINE_BYTES: usize = 8 * 1024;
const MAX_HEADER_COUNT: usize = 100;

// ----------------------------------------------------------- HTTP parsing

/// One parsed inbound request (the subset this listener needs).
#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    /// HTTP method verbatim.
    pub method: String,
    /// Request target verbatim.
    pub path: String,
    /// `Authorization` header value, if present.
    pub authorization: Option<String>,
    /// Body bytes (length-delimited, capped).
    pub body: Vec<u8>,
}

/// Parse failure → the HTTP status the listener answers with.
#[derive(Debug, PartialEq, Eq)]
pub struct HttpReject {
    /// Response status code.
    pub status: u16,
    /// Response reason/body text.
    pub reason: &'static str,
}

const fn reject(status: u16, reason: &'static str) -> HttpReject {
    HttpReject { status, reason }
}

fn read_crlf_line(r: &mut impl BufRead) -> Result<String, HttpReject> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match r.read(&mut byte) {
            Ok(0) => return Err(reject(400, "truncated request")),
            Ok(_) => {
                if byte[0] == b'\n' {
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    return String::from_utf8(line).map_err(|_| reject(400, "non-UTF-8 header"));
                }
                line.push(byte[0]);
                if line.len() > MAX_HEADER_LINE_BYTES {
                    return Err(reject(431, "header line too large"));
                }
            }
            Err(_) => return Err(reject(408, "request read timeout")),
        }
    }
}

/// Read one HTTP/1.1 request (request line, headers, length-delimited body)
/// from `r`. Pure over the reader: loopback tests drive it with real sockets,
/// unit tests with in-memory readers. Chunked bodies are refused (Helius
/// sends Content-Length), oversize is 413 BEFORE allocation.
pub fn read_request(r: &mut impl BufRead) -> Result<Request, HttpReject> {
    let request_line = read_crlf_line(r)?;
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    if method.is_empty() || path.is_empty() {
        return Err(reject(400, "malformed request line"));
    }
    let mut authorization: Option<String> = None;
    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    for _ in 0..MAX_HEADER_COUNT {
        let line = read_crlf_line(r)?;
        if line.is_empty() {
            if chunked {
                return Err(reject(411, "chunked bodies not supported"));
            }
            let body = match content_length {
                None => Vec::new(),
                Some(n) => {
                    if n > WEBHOOK_MAX_BODY_BYTES {
                        return Err(reject(413, "body exceeds cap"));
                    }
                    let mut body = vec![0u8; n];
                    r.read_exact(&mut body)
                        .map_err(|_| reject(400, "truncated body"))?;
                    body
                }
            };
            return Ok(Request {
                method,
                path,
                authorization,
                body,
            });
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(reject(400, "malformed header"));
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "authorization" => authorization = Some(value.to_string()),
            "content-length" => {
                content_length = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| reject(400, "bad content-length"))?,
                );
            }
            "transfer-encoding" => chunked = value.to_ascii_lowercase().contains("chunked"),
            _ => {}
        }
    }
    Err(reject(431, "too many headers"))
}

/// Render a minimal HTTP/1.1 response. Pure.
#[must_use]
pub fn response_bytes(status: u16, reason: &str, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

// -------------------------------------------------- whale normalization

/// Compare two JSON decimal number texts by numeric value WITHOUT floating
/// point (integer/lexicographic only): sign, then integer-digit magnitude,
/// then fraction digits. Pure; malformed text compares as smallest.
#[must_use]
pub fn cmp_decimal(a: &str, b: &str) -> Ordering {
    fn split(s: &str) -> Option<(bool, &str, &str)> {
        let (neg, rest) = match s.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, s),
        };
        if rest.is_empty() || rest.contains(['e', 'E']) {
            // Exponent forms don't appear in enhanced payload amounts; treat
            // as unparsed rather than mis-ranked.
            return None;
        }
        let (int, frac) = match rest.split_once('.') {
            Some((i, f)) => (i, f),
            None => (rest, ""),
        };
        if int.is_empty() || !int.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some((neg, int.trim_start_matches('0'), frac.trim_end_matches('0')))
    }
    match (split(a), split(b)) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some((an, ai, af)), Some((bn, bi, bf))) => {
            let a_zero = ai.is_empty() && af.is_empty();
            let b_zero = bi.is_empty() && bf.is_empty();
            if a_zero && b_zero {
                return Ordering::Equal;
            }
            match (an && !a_zero, bn && !b_zero) {
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                (neg, _) => {
                    let mag = ai
                        .len()
                        .cmp(&bi.len())
                        .then_with(|| ai.cmp(bi))
                        .then_with(|| af.cmp(bf));
                    if neg {
                        mag.reverse()
                    } else {
                        mag
                    }
                }
            }
        }
    }
}

fn push_wallet(list: &mut Vec<String>, v: Option<&Value>) {
    if let Some(s) = v.and_then(Value::as_str) {
        if !s.is_empty() && !list.iter().any(|w| w == s) {
            list.push(s.to_string());
        }
    }
}

/// Build the normalized whale line for one enhanced-transaction object.
/// Pure, integer-only arithmetic (lamports saturate at u64::MAX; token
/// amounts are RANKED by [`cmp_decimal`] and re-emitted as their raw number
/// text — never through a float). Absent fields degrade to `""`/`0`/`null`
/// (fail-open-as-absence), never error.
#[must_use]
pub fn whale_line(tx: &Value, recv_unix_ms: u64) -> String {
    let sig = tx.get("signature").and_then(Value::as_str).unwrap_or("");
    let slot = tx.get("slot").and_then(Value::as_u64).unwrap_or(0);
    let ts = tx.get("timestamp").and_then(Value::as_u64).unwrap_or(0);
    let kind = tx.get("type").and_then(Value::as_str).unwrap_or("UNKNOWN");

    let mut wallets: Vec<String> = Vec::new();
    push_wallet(&mut wallets, tx.get("feePayer"));
    let mut mints: Vec<String> = Vec::new();
    let mut native_sum: u64 = 0;
    if let Some(transfers) = tx.get("nativeTransfers").and_then(Value::as_array) {
        for t in transfers {
            push_wallet(&mut wallets, t.get("fromUserAccount"));
            push_wallet(&mut wallets, t.get("toUserAccount"));
            native_sum =
                native_sum.saturating_add(t.get("amount").and_then(Value::as_u64).unwrap_or(0));
        }
    }
    let mut largest: Option<(&str, &str)> = None; // (mint, raw amount text)
    if let Some(transfers) = tx.get("tokenTransfers").and_then(Value::as_array) {
        for t in transfers {
            push_wallet(&mut wallets, t.get("fromUserAccount"));
            push_wallet(&mut wallets, t.get("toUserAccount"));
            let mint = t.get("mint").and_then(Value::as_str).unwrap_or("");
            if !mint.is_empty() && !mints.iter().any(|m| m == mint) {
                mints.push(mint.to_string());
            }
            if let Some(Value::Number(raw)) = t.get("tokenAmount") {
                let bigger = match largest {
                    None => true,
                    Some((_, cur)) => cmp_decimal(raw, cur) == Ordering::Greater,
                };
                if bigger && !mint.is_empty() {
                    largest = Some((mint, raw));
                }
            }
        }
    }

    let mut out = String::with_capacity(256);
    out.push_str("{\"lane\":\"whale\",\"recv_unix_ms\":");
    out.push_str(&recv_unix_ms.to_string());
    out.push_str(",\"sig\":\"");
    emit::escape_json_into(sig, &mut out);
    out.push_str("\",\"slot\":");
    out.push_str(&slot.to_string());
    out.push_str(",\"ts\":");
    out.push_str(&ts.to_string());
    out.push_str(",\"kind\":\"");
    emit::escape_json_into(kind, &mut out);
    out.push_str("\",\"wallets\":[");
    for (n, w) in wallets.iter().enumerate() {
        if n > 0 {
            out.push(',');
        }
        out.push('"');
        emit::escape_json_into(w, &mut out);
        out.push('"');
    }
    out.push_str("],\"mints\":[");
    for (n, m) in mints.iter().enumerate() {
        if n > 0 {
            out.push(',');
        }
        out.push('"');
        emit::escape_json_into(m, &mut out);
        out.push('"');
    }
    out.push_str("],\"native_moved_lamports\":");
    out.push_str(&native_sum.to_string());
    out.push_str(",\"largest_token_move\":");
    match largest {
        Some((mint, amount)) => {
            out.push_str("{\"mint\":\"");
            emit::escape_json_into(mint, &mut out);
            out.push_str("\",\"amount\":");
            out.push_str(amount);
            out.push('}');
        }
        None => out.push_str("null"),
    }
    out.push('}');
    out
}

/// Per-payload processing stats (stderr diagnostics + tests).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ProcessStats {
    /// Transaction objects emitted (raw + whale line each).
    pub emitted: usize,
    /// Duplicates skipped by the signature ring.
    pub deduped: usize,
    /// Objects that were not JSON objects (drift).
    pub malformed: usize,
}

/// Parse one delivered body and emit raw + whale lines for every
/// non-duplicate transaction object. Pure over its arguments (§22: the clock
/// value is injected).
pub fn process_payload(
    body: &str,
    recv_unix_ms: u64,
    ring: &mut DedupeRing,
    out: &mut impl Write,
) -> Result<ProcessStats, String> {
    let parsed = json::parse(body).map_err(|e| format!("unparseable payload: {e}"))?;
    // Helius delivers an ARRAY of enhanced tx objects; tolerate a bare object.
    let items: Vec<&Value> = match &parsed {
        Value::Array(items) => items.iter().collect(),
        obj @ Value::Object(_) => vec![obj],
        _ => return Err("payload is neither array nor object".to_string()),
    };
    let mut stats = ProcessStats::default();
    for tx in items {
        if !matches!(tx, Value::Object(_)) {
            stats.malformed += 1;
            continue;
        }
        let sig = tx.get("signature").and_then(Value::as_str).unwrap_or("");
        if !ring.insert(sig) {
            stats.deduped += 1;
            continue;
        }
        let raw = emit::raw_line("helius_webhook", recv_unix_ms, None, &json::serialize(tx));
        emit::write_line(out, &raw).map_err(|e| format!("stdout write: {e}"))?;
        emit::write_line(out, &whale_line(tx, recv_unix_ms))
            .map_err(|e| format!("stdout write: {e}"))?;
        stats.emitted += 1;
    }
    Ok(stats)
}

// ----------------------------------------------------------------- server

/// Handle one accepted connection: parse, authorize, ACK, then process.
/// Responds before any parsing/emission work (Helius drops after 3 failed
/// retries — the ack must never wait on us).
pub fn handle_conn(
    stream: &mut TcpStream,
    secret: &str,
    now_ms: fn() -> u64,
    ring: &mut DedupeRing,
    out: &mut impl Write,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(CONN_READ_TIMEOUT_SECS)));
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[pq-stream-capture] webhook: clone failed: {e}");
            return;
        }
    });
    let request = match read_request(&mut reader) {
        Ok(r) => r,
        Err(r) => {
            let _ = stream.write_all(&response_bytes(r.status, r.reason, r.reason));
            return;
        }
    };
    if request.method != "POST" {
        let _ = stream.write_all(&response_bytes(405, "Method Not Allowed", "POST only"));
        return;
    }
    if request.authorization.as_deref() != Some(secret) {
        eprintln!("[pq-stream-capture] webhook AUTH_REJECT: bad or missing Authorization");
        let _ = stream.write_all(&response_bytes(401, "Unauthorized", "bad auth"));
        return;
    }
    let recv = now_ms();
    // ACK FIRST (see module docs), then process.
    let _ = stream
        .write_all(&response_bytes(200, "OK", "ok"))
        .and_then(|()| stream.flush());
    let body = match String::from_utf8(request.body) {
        Ok(b) => b,
        Err(_) => {
            eprintln!("[pq-stream-capture] webhook DRIFT: non-UTF-8 body dropped");
            return;
        }
    };
    match process_payload(&body, recv, ring, out) {
        Ok(stats) => {
            if stats.deduped > 0 || stats.malformed > 0 {
                eprintln!(
                    "[pq-stream-capture] webhook: emitted={} deduped={} malformed={}",
                    stats.emitted, stats.deduped, stats.malformed
                );
            }
        }
        Err(e) => eprintln!("[pq-stream-capture] webhook DRIFT: {e}"),
    }
}

/// Accept loop. `max_conns` bounds the loop for tests (`None` = forever).
pub fn serve(
    listener: &TcpListener,
    secret: &str,
    now_ms: fn() -> u64,
    out: &mut impl Write,
    max_conns: Option<usize>,
) {
    let mut ring = DedupeRing::new(WEBHOOK_DEDUPE_CAP);
    let mut handled = 0usize;
    loop {
        if let Some(cap) = max_conns {
            if handled >= cap {
                return;
            }
        }
        match listener.accept() {
            Ok((mut stream, _addr)) => {
                handle_conn(&mut stream, secret, now_ms, &mut ring, out);
                handled += 1;
            }
            Err(e) => eprintln!("[pq-stream-capture] webhook accept failed: {e}"),
        }
    }
}

// ----------------------------------------------------------------- runner

const USAGE: &str = "usage: pq-stream-capture webhook-listener [--bind 127.0.0.1:8787]\n\
  env: WEBHOOK_AUTH_SECRET (required; exit 3 when missing).\n\
  Binds loopback and speaks plain HTTP — put a TLS-terminating reverse proxy\n\
  in front (Helius requires an https webhook URL).";

/// Lane entry point. `now_ms` is the injected capture clock (§22).
pub fn run(args: &[String], now_ms: fn() -> u64) -> u8 {
    let mut bind = DEFAULT_BIND.to_string();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--bind" => match it.next() {
                Some(b) => bind = b.clone(),
                None => {
                    eprintln!("[pq-stream-capture] webhook-listener: --bind needs a value");
                    eprintln!("{USAGE}");
                    return 2;
                }
            },
            other => {
                eprintln!("[pq-stream-capture] webhook-listener: unknown flag {other:?}");
                eprintln!("{USAGE}");
                return 2;
            }
        }
    }
    let secret = match std::env::var("WEBHOOK_AUTH_SECRET") {
        Ok(s) if !s.trim().is_empty() => s,
        _ => {
            eprintln!(
                "[pq-stream-capture] webhook-listener ARMING_FAILED: WEBHOOK_AUTH_SECRET is \
                 not set — refusing to start (fail-closed, exit {EXIT_ARMING})"
            );
            return EXIT_ARMING;
        }
    };
    let listener = match TcpListener::bind(&bind) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[pq-stream-capture] webhook-listener: bind {bind}: {e}");
            return 1;
        }
    };
    eprintln!(
        "[pq-stream-capture] webhook-listener on http://{bind} (POST only; \
         TLS terminates at the reverse proxy; auth via Authorization header)"
    );
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    serve(&listener, &secret, now_ms, &mut out, None);
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn parses_minimal_post() {
        let raw = b"POST /hook HTTP/1.1\r\nAuthorization: s3cret\r\nContent-Length: 4\r\n\r\n[{}]";
        let req = read_request(&mut Cursor::new(&raw[..])).unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/hook");
        assert_eq!(req.authorization.as_deref(), Some("s3cret"));
        assert_eq!(req.body, b"[{}]");
    }

    #[test]
    fn rejects_oversize_declared_body_before_allocation() {
        let raw = format!(
            "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            WEBHOOK_MAX_BODY_BYTES + 1
        );
        let err = read_request(&mut Cursor::new(raw.as_bytes())).unwrap_err();
        assert_eq!(err.status, 413);
    }

    #[test]
    fn rejects_chunked_and_malformed() {
        let chunked = b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n";
        assert_eq!(
            read_request(&mut Cursor::new(&chunked[..]))
                .unwrap_err()
                .status,
            411
        );
        let bad = b"\r\n\r\n";
        assert_eq!(
            read_request(&mut Cursor::new(&bad[..])).unwrap_err().status,
            400
        );
        let badlen = b"POST / HTTP/1.1\r\nContent-Length: nope\r\n\r\n";
        assert_eq!(
            read_request(&mut Cursor::new(&badlen[..]))
                .unwrap_err()
                .status,
            400
        );
    }

    #[test]
    fn truncated_body_is_400_not_hang_or_panic() {
        let raw = b"POST / HTTP/1.1\r\nContent-Length: 10\r\n\r\nshort";
        assert_eq!(
            read_request(&mut Cursor::new(&raw[..])).unwrap_err().status,
            400
        );
    }

    #[test]
    fn cmp_decimal_orders_numerically() {
        use Ordering::*;
        assert_eq!(cmp_decimal("2", "10"), Less);
        assert_eq!(cmp_decimal("10.5", "10.50"), Equal);
        assert_eq!(cmp_decimal("0.9", "0.10"), Greater);
        assert_eq!(
            cmp_decimal("1000000000000000000000", "999999999999999999999"),
            Greater
        );
        assert_eq!(cmp_decimal("-1", "0.001"), Less);
        assert_eq!(cmp_decimal("-2", "-10"), Greater);
        assert_eq!(cmp_decimal("0", "-0"), Equal);
        assert_eq!(cmp_decimal("junk", "1"), Less, "malformed ranks smallest");
    }

    #[test]
    fn whale_line_null_token_move_when_no_token_transfers() {
        let tx = json::parse(
            r#"{"signature":"sigX","slot":5,"timestamp":6,"type":"TRANSFER","feePayer":"W1","nativeTransfers":[{"fromUserAccount":"W1","toUserAccount":"W2","amount":7}]}"#,
        )
        .unwrap();
        let line = whale_line(&tx, 1);
        assert!(line.contains("\"largest_token_move\":null"));
        assert!(line.contains("\"native_moved_lamports\":7"));
        assert!(line.contains("\"wallets\":[\"W1\",\"W2\"]"));
        assert!(json::parse(&line).is_ok());
    }

    #[test]
    fn whale_line_degrades_absent_fields_to_defaults() {
        let tx = json::parse("{}").unwrap();
        let line = whale_line(&tx, 9);
        assert!(line.contains("\"sig\":\"\""));
        assert!(line.contains("\"slot\":0"));
        assert!(line.contains("\"kind\":\"UNKNOWN\""));
        assert!(line.contains("\"wallets\":[]"));
        assert!(json::parse(&line).is_ok());
    }

    #[test]
    fn native_sum_saturates_never_overflows() {
        let tx =
            json::parse(r#"{"nativeTransfers":[{"amount":18446744073709551615},{"amount":10}]}"#)
                .unwrap();
        let line = whale_line(&tx, 0);
        assert!(line.contains(&format!("\"native_moved_lamports\":{}", u64::MAX)));
    }

    #[test]
    fn process_payload_dedupes_by_signature() {
        let body = r#"[{"signature":"A","type":"SWAP"},{"signature":"A","type":"SWAP"},{"signature":"B","type":"TRANSFER"}]"#;
        let mut ring = DedupeRing::new(8);
        let mut out = Vec::new();
        let stats = process_payload(body, 1, &mut ring, &mut out).unwrap();
        assert_eq!(
            stats,
            ProcessStats {
                emitted: 2,
                deduped: 1,
                malformed: 0
            }
        );
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.lines().count(), 4, "raw + whale per emitted tx");
    }

    #[test]
    fn process_payload_rejects_non_json_loudly() {
        let mut ring = DedupeRing::new(8);
        let mut out = Vec::new();
        assert!(process_payload("not json", 1, &mut ring, &mut out).is_err());
        assert!(out.is_empty(), "nothing emitted on drift");
    }

    #[test]
    fn response_bytes_shape() {
        let r = String::from_utf8(response_bytes(200, "OK", "ok")).unwrap();
        assert!(r.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(r.contains("Content-Length: 2\r\n"));
        assert!(r.ends_with("\r\n\r\nok"));
    }
}
