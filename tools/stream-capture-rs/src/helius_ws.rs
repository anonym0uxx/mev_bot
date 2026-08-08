//! `helius-ws` subcommand — Helius Enhanced WebSocket lane (Geyser-fed
//! `transactionSubscribe` + standard `accountSubscribe`/`slotSubscribe`).
//!
//! Endpoint: `wss://mainnet.helius-rpc.com/?api-key=KEY` (key from env
//! `HELIUS_API_KEY`; `HELIUS_WS_URL` overrides the base, e.g.
//! `wss://beta.helius-rpc.com` for the Gatekeeper deployment).
//! `transactionSubscribe` is a Helius EXTENSION (Developer plan or above);
//! `accountSubscribe`/`slotSubscribe` are standard Solana RPC. Slot
//! notifications are the STALENESS HEARTBEAT: mainnet produces a slot every
//! ~400 ms, so [`HELIUS_WS_STALE_SECS`] without one means the pipe is dead
//! regardless of TCP liveness → force reconnect + loud log.
//!
//! Every notification is emitted as one NDJSON line preserving the payload
//! (§6.3 raw-bytes-first):
//! `{"lane":"helius_ws","recv_unix_ms":...,"sub":"transaction|account|slot","raw":<result>}`.
//! The `raw` member is the notification's `params.result` subtree,
//! parsed-and-reserialized through the LOSSLESS [`crate::json`] round trip
//! (raw number text and member order preserved byte-for-byte) — nothing is
//! renamed, coerced or dropped.
//!
//! NO REPLAY EXISTS ON THIS LANE: a WebSocket disconnect is a hole in the
//! stream. On resume we log the slot-gap width loudly and carry on — the
//! server-side LaserStream gRPC lane (`grpc-server-only/`, SDK-internal
//! `from_slot` resume) is the replay-capable primary; this lane is the
//! laptop-profile/fallback tap. Fail-closed arming (§18.8): a missing
//! `HELIUS_API_KEY` is exit code [`EXIT_ARMING`] immediately — never a
//! silent retry loop against 401s.

use std::time::{Duration, Instant};

use crate::json::{self, Value};
use crate::ws::{WsConn, WsEvent};
use crate::{backoff, emit};

/// Staleness watchdog (seconds): no slot notification for this long forces a
/// reconnect. Mainnet slots tick every ~400 ms, so 15 s is ~37 missed slots —
/// unambiguously a dead pipe, not jitter.
pub const HELIUS_WS_STALE_SECS: u64 = 15;

/// Fail-closed arming exit code (§18.8) — matches the suite's convention
/// (`pump` lane AUTH_WALL): the supervisor must see a capability loss loudly.
pub const EXIT_ARMING: u8 = 3;

/// JSON-RPC id of the transactionSubscribe request.
pub const ID_TX_SUB: u64 = 1;
/// JSON-RPC id of the slotSubscribe request.
pub const ID_SLOT_SUB: u64 = 2;
/// First JSON-RPC id used for per-address accountSubscribe requests.
pub const ID_ACCOUNT_SUB_BASE: u64 = 3;

// ---------------------------------------------------------- pure builders

fn push_str_array(out: &mut String, items: &[String]) {
    out.push('[');
    for (n, item) in items.iter().enumerate() {
        if n > 0 {
            out.push(',');
        }
        out.push('"');
        emit::escape_json_into(item, out);
        out.push('"');
    }
    out.push(']');
}

/// Build the Helius `transactionSubscribe` request (Developer plan+): filter
/// on `accountInclude`, votes and failed txs excluded, base64-encoded full
/// transactions at the chosen commitment. Pure.
#[must_use]
pub fn transaction_subscribe_request(include: &[String], commitment: &str) -> String {
    let mut out = String::with_capacity(256 + include.len() * 48);
    out.push_str(&format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{ID_TX_SUB},\"method\":\"transactionSubscribe\",\"params\":[{{\"accountInclude\":"
    ));
    push_str_array(&mut out, include);
    out.push_str(",\"vote\":false,\"failed\":false},{\"commitment\":\"");
    emit::escape_json_into(commitment, &mut out);
    out.push_str(
        "\",\"encoding\":\"base64\",\"transactionDetails\":\"full\",\
         \"maxSupportedTransactionVersion\":0}]}",
    );
    out
}

/// Build a standard `accountSubscribe` request for one address. Pure.
#[must_use]
pub fn account_subscribe_request(id: u64, pubkey: &str, commitment: &str) -> String {
    let mut out = String::with_capacity(160);
    out.push_str(&format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"accountSubscribe\",\"params\":[\""
    ));
    emit::escape_json_into(pubkey, &mut out);
    out.push_str("\",{\"encoding\":\"base64\",\"commitment\":\"");
    emit::escape_json_into(commitment, &mut out);
    out.push_str("\"}]}");
    out
}

/// Build a standard `accountUnsubscribe` request for one subscription id. Pure.
/// Used to release Helius server-side subscription slots when we evict locally.
#[must_use]
pub fn account_unsubscribe_request(id: u64, server_sub_id: u64) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"accountUnsubscribe\",\"params\":[{server_sub_id}]}}"
    )
}

/// Build the standard `slotSubscribe` request (no params). Pure.
#[must_use]
pub fn slot_subscribe_request() -> String {
    format!("{{\"jsonrpc\":\"2.0\",\"id\":{ID_SLOT_SUB},\"method\":\"slotSubscribe\"}}")
}

/// The full subscription batch sent after every (re)connect: one
/// transactionSubscribe over the merged account/program include list, the
/// always-on slotSubscribe heartbeat, and one accountSubscribe per watched
/// account. Pure.
#[must_use]
pub fn subscription_batch(
    include: &[String],
    watched_accounts: &[String],
    commitment: &str,
) -> Vec<String> {
    let mut subs = Vec::with_capacity(2 + watched_accounts.len());
    subs.push(transaction_subscribe_request(include, commitment));
    subs.push(slot_subscribe_request());
    for (n, acct) in watched_accounts.iter().enumerate() {
        subs.push(account_subscribe_request(
            ID_ACCOUNT_SUB_BASE + n as u64,
            acct,
            commitment,
        ));
    }
    subs
}

/// Compose the connect URL: base URL (required, no default) + `?api-key=`.
/// A base that already carries `api-key=` is used verbatim. Pure.
#[must_use]
pub fn ws_url(base: &str, api_key: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.contains("api-key=") {
        return base.to_string();
    }
    format!("{base}/?api-key={api_key}")
}

// ------------------------------------------------------ pure classification

/// One classified inbound WebSocket message.
#[derive(Debug, PartialEq)]
pub enum Inbound<'a> {
    /// A subscription notification: lane `sub` tag + the raw `params.result`.
    Notification {
        /// `"transaction"` / `"account"` / `"slot"`.
        sub: &'static str,
        /// The untouched `params.result` subtree.
        result: &'a Value,
    },
    /// A subscription acknowledgment (`{"id":n,"result":subid}`).
    Ack {
        /// Request id being acknowledged.
        id: u64,
    },
    /// A JSON-RPC error (subscription rejected, plan gate, bad params).
    RpcError {
        /// Request id the error answers, if any.
        id: Option<u64>,
        /// Compact error text for the loud log.
        text: String,
    },
    /// Anything else — schema drift, logged loudly, never silently dropped.
    Drift,
}

/// Classify one parsed inbound message. Pure.
#[must_use]
pub fn classify(v: &Value) -> Inbound<'_> {
    if let Some(err) = v.get("error") {
        return Inbound::RpcError {
            id: v.get("id").and_then(Value::as_u64),
            text: json::serialize(err),
        };
    }
    if let Some(method) = v.get("method").and_then(Value::as_str) {
        let sub = match method {
            "transactionNotification" => "transaction",
            "accountNotification" => "account",
            "slotNotification" => "slot",
            _ => return Inbound::Drift,
        };
        match v.get("params").and_then(|p| p.get("result")) {
            Some(result) => return Inbound::Notification { sub, result },
            None => return Inbound::Drift,
        }
    }
    if let (Some(id), Some(_)) = (v.get("id").and_then(Value::as_u64), v.get("result")) {
        return Inbound::Ack { id };
    }
    Inbound::Drift
}

/// Extract the slot number from a `slotNotification` result. Pure.
#[must_use]
pub fn slot_of(result: &Value) -> Option<u64> {
    result.get("slot").and_then(Value::as_u64)
}

/// Width of a slot gap (`None` when contiguous or non-monotonic). Pure,
/// integer-only.
#[must_use]
pub fn slot_gap(last_seen: u64, new_slot: u64) -> Option<u64> {
    if last_seen > 0 && new_slot > last_seen + 1 {
        Some(new_slot - last_seen - 1)
    } else {
        None
    }
}

// ----------------------------------------------------------------- runner

const USAGE: &str = "usage: pq-stream-capture helius-ws \
[--accounts-file f] [--programs p1,p2] [--commitment processed|confirmed|finalized]\n\
  env: HELIUS_API_KEY (required; exit 3 when missing)\n\
       HELIUS_WS_URL  (required base URL, e.g. wss://mainnet.helius-rpc.com; exit 3 when missing)\n\
  at least one of --accounts-file / --programs is required (transactionSubscribe\n\
  needs a non-empty accountInclude filter).";

struct Args {
    include: Vec<String>,
    watched_accounts: Vec<String>,
    commitment: String,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut accounts_file: Option<String> = None;
    let mut programs: Vec<String> = Vec::new();
    let mut commitment = "processed".to_string();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--accounts-file" => {
                accounts_file = Some(it.next().ok_or("--accounts-file needs a value")?.clone());
            }
            "--programs" => {
                let csv = it.next().ok_or("--programs needs a value")?;
                programs.extend(
                    csv.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from),
                );
            }
            "--commitment" => {
                let c = it.next().ok_or("--commitment needs a value")?.clone();
                if !matches!(c.as_str(), "processed" | "confirmed" | "finalized") {
                    return Err(format!("bad --commitment {c:?}"));
                }
                commitment = c;
            }
            other => return Err(format!("unknown flag {other:?}")),
        }
    }
    let watched_accounts = match &accounts_file {
        Some(path) => crate::read_list_file(path)?,
        None => Vec::new(),
    };
    let mut include = watched_accounts.clone();
    include.extend(programs);
    if include.is_empty() {
        return Err("nothing to subscribe: give --accounts-file and/or --programs".to_string());
    }
    Ok(Args {
        include,
        watched_accounts,
        commitment,
    })
}

/// Lane entry point. `now_ms` is the injected capture clock (§22).
pub fn run(args: &[String], now_ms: fn() -> u64) -> u8 {
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[pq-stream-capture] helius-ws: {e}");
            eprintln!("{USAGE}");
            return 2;
        }
    };
    let key = match std::env::var("HELIUS_API_KEY") {
        Ok(k) if !k.trim().is_empty() => k,
        _ => {
            eprintln!(
                "[pq-stream-capture] helius-ws ARMING_FAILED: HELIUS_API_KEY is not set — \
                 refusing to start (fail-closed, exit {EXIT_ARMING}; never a silent retry loop)"
            );
            return EXIT_ARMING;
        }
    };
    // Fail-closed: HELIUS_WS_URL is REQUIRED. No default endpoint — a silent
    // default is a fail-open credential path. Missing = refuse to start.
    let base = match std::env::var("HELIUS_WS_URL") {
        Ok(b) if !b.trim().is_empty() => b,
        _ => {
            eprintln!(
                "[pq-stream-capture] helius-ws ARMING_FAILED: HELIUS_WS_URL is not set — \
                 refusing to start (fail-closed, exit {EXIT_ARMING}; no silent default endpoint)"
            );
            return EXIT_ARMING;
        }
    };
    let url = ws_url(&base, &key);
    let subs = subscription_batch(
        &parsed.include,
        &parsed.watched_accounts,
        &parsed.commitment,
    );
    eprintln!(
        "[pq-stream-capture] helius-ws: {} accountInclude keys, {} accountSubscribe, \
         commitment={} (transactionSubscribe requires Helius Developer plan+)",
        parsed.include.len(),
        parsed.watched_accounts.len(),
        parsed.commitment
    );

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut attempt: u32 = 0;
    let mut last_slot: u64 = 0;
    loop {
        let mut conn = match WsConn::connect(&url) {
            Ok(c) => c,
            Err(e) => {
                let delay = backoff::step_secs(attempt);
                attempt = attempt.saturating_add(1);
                eprintln!("[pq-stream-capture] helius-ws connect failed ({e}); retry in {delay}s");
                std::thread::sleep(Duration::from_secs(delay));
                continue;
            }
        };
        eprintln!(
            "[pq-stream-capture] helius-ws connected; resubscribing {} subs",
            subs.len()
        );
        let mut sub_write_failed = false;
        for s in &subs {
            if let Err(e) = conn.send_text(s) {
                eprintln!("[pq-stream-capture] helius-ws subscribe write failed: {e}");
                sub_write_failed = true;
                break;
            }
        }
        if !sub_write_failed {
            if last_slot > 0 {
                eprintln!(
                    "[pq-stream-capture] helius-ws RESUME_NO_REPLAY: WS has no replay; \
                     stream hole begins after slot {last_slot} (gRPC LaserStream lane \
                     covers replay server-side)"
                );
            }
            session(&mut conn, &mut out, now_ms, &mut last_slot, &mut attempt);
        }
        let delay = backoff::step_secs(attempt);
        attempt = attempt.saturating_add(1);
        eprintln!("[pq-stream-capture] helius-ws reconnecting in {delay}s");
        std::thread::sleep(Duration::from_secs(delay));
    }
}

/// One connected session: pump events until close/error/staleness.
fn session(
    conn: &mut WsConn,
    out: &mut impl std::io::Write,
    now_ms: fn() -> u64,
    last_slot: &mut u64,
    attempt: &mut u32,
) {
    let mut last_heartbeat = Instant::now();
    loop {
        if conn.maybe_keepalive().is_err() {
            eprintln!("[pq-stream-capture] helius-ws keepalive write failed");
            return;
        }
        if last_heartbeat.elapsed() >= Duration::from_secs(HELIUS_WS_STALE_SECS) {
            eprintln!(
                "[pq-stream-capture] helius-ws STALE: no slot notification for \
                 {HELIUS_WS_STALE_SECS}s — forcing reconnect"
            );
            return;
        }
        match conn.poll_event() {
            Ok(None) | Ok(Some(WsEvent::Pong)) => {}
            Ok(Some(WsEvent::Binary(_))) => {
                eprintln!("[pq-stream-capture] helius-ws DRIFT: unexpected binary frame");
            }
            Ok(Some(WsEvent::Closed(reason))) => {
                eprintln!("[pq-stream-capture] helius-ws closed by server: {reason}");
                return;
            }
            Ok(Some(WsEvent::Text(text))) => {
                let recv = now_ms();
                let v = match json::parse(&text) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("[pq-stream-capture] helius-ws DRIFT: unparseable frame: {e}");
                        continue;
                    }
                };
                match classify(&v) {
                    Inbound::Notification { sub, result } => {
                        *attempt = 0; // connection proved healthy
                        if sub == "slot" {
                            last_heartbeat = Instant::now();
                            if let Some(s) = slot_of(result) {
                                if let Some(gap) = slot_gap(*last_slot, s) {
                                    eprintln!(
                                        "[pq-stream-capture] helius-ws SLOT_GAP width={gap} \
                                         (last={last_slot}, now={s})"
                                    );
                                }
                                *last_slot = s;
                            }
                        }
                        let line = emit::raw_line(
                            "helius_ws",
                            recv,
                            Some(("sub", sub)),
                            &json::serialize(result),
                        );
                        if emit::write_line(out, &line).is_err() {
                            // stdout gone: downstream died; exit the session
                            // and let the process-level supervisor decide.
                            eprintln!("[pq-stream-capture] helius-ws stdout write failed");
                            return;
                        }
                    }
                    Inbound::Ack { id } => {
                        eprintln!("[pq-stream-capture] helius-ws subscription ack id={id}");
                    }
                    Inbound::RpcError { id, text } => {
                        eprintln!(
                            "[pq-stream-capture] helius-ws SUBSCRIBE_REJECTED id={id:?}: {text} \
                             (transactionSubscribe needs Developer plan+; check key tier)"
                        );
                    }
                    Inbound::Drift => {
                        eprintln!(
                            "[pq-stream-capture] helius-ws DRIFT: unrecognized message shape"
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!("[pq-stream-capture] helius-ws transport error: {e}");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_subscribe_exact_shape() {
        let req = transaction_subscribe_request(
            &["pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA".into()],
            "processed",
        );
        assert_eq!(
            req,
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"transactionSubscribe\",\
             \"params\":[{\"accountInclude\":[\"pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA\"],\
             \"vote\":false,\"failed\":false},{\"commitment\":\"processed\",\
             \"encoding\":\"base64\",\"transactionDetails\":\"full\",\
             \"maxSupportedTransactionVersion\":0}]}"
        );
        // Must be valid JSON.
        assert!(json::parse(&req).is_ok());
    }

    #[test]
    fn slot_and_account_subscribe_shapes() {
        assert_eq!(
            slot_subscribe_request(),
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"slotSubscribe\"}"
        );
        let req = account_subscribe_request(3, "SomePubkey111", "confirmed");
        assert_eq!(
            req,
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"accountSubscribe\",\
             \"params\":[\"SomePubkey111\",{\"encoding\":\"base64\",\
             \"commitment\":\"confirmed\"}]}"
        );
    }

    #[test]
    fn subscription_batch_orders_and_ids() {
        let batch = subscription_batch(
            &["prog1".into(), "acct1".into()],
            &["acct1".into(), "acct2".into()],
            "processed",
        );
        assert_eq!(batch.len(), 4);
        assert!(batch[0].contains("transactionSubscribe"));
        assert!(batch[1].contains("slotSubscribe"));
        assert!(batch[2].contains("\"id\":3") && batch[2].contains("acct1"));
        assert!(batch[3].contains("\"id\":4") && batch[3].contains("acct2"));
    }

    #[test]
    fn ws_url_composition() {
        assert_eq!(
            ws_url("wss://mainnet.helius-rpc.com", "K"),
            "wss://mainnet.helius-rpc.com/?api-key=K"
        );
        assert_eq!(
            ws_url("wss://beta.helius-rpc.com", "K"),
            "wss://beta.helius-rpc.com/?api-key=K"
        );
        assert_eq!(
            ws_url("wss://x.example/?api-key=inline", "ignored"),
            "wss://x.example/?api-key=inline"
        );
    }

    #[test]
    fn classify_slot_notification() {
        let v = json::parse(
            r#"{"jsonrpc":"2.0","method":"slotNotification","params":{"result":{"parent":100,"root":98,"slot":101},"subscription":2}}"#,
        )
        .unwrap();
        match classify(&v) {
            Inbound::Notification { sub, result } => {
                assert_eq!(sub, "slot");
                assert_eq!(slot_of(result), Some(101));
            }
            other => panic!("misclassified: {other:?}"),
        }
    }

    #[test]
    fn classify_ack_error_and_drift() {
        let ack = json::parse(r#"{"jsonrpc":"2.0","result":9945,"id":1}"#).unwrap();
        assert_eq!(classify(&ack), Inbound::Ack { id: 1 });
        let err = json::parse(
            r#"{"jsonrpc":"2.0","error":{"code":-32602,"message":"plan gate"},"id":1}"#,
        )
        .unwrap();
        match classify(&err) {
            Inbound::RpcError { id, text } => {
                assert_eq!(id, Some(1));
                assert!(text.contains("plan gate"));
            }
            other => panic!("misclassified: {other:?}"),
        }
        let drift = json::parse(r#"{"jsonrpc":"2.0","method":"who knows"}"#).unwrap();
        assert_eq!(classify(&drift), Inbound::Drift);
    }

    #[test]
    fn slot_gap_integer_math() {
        assert_eq!(slot_gap(0, 5), None, "no baseline yet");
        assert_eq!(slot_gap(100, 101), None, "contiguous");
        assert_eq!(slot_gap(100, 105), Some(4));
        assert_eq!(slot_gap(100, 90), None, "non-monotonic never underflows");
    }
}
