//! `pumpportal` subcommand — PumpPortal free data WebSocket lane
//! (`wss://pumpportal.fun/api/data`, no auth).
//!
//! After connect it sends `{"method":"subscribeNewToken"}` and
//! `{"method":"subscribeMigration"}` (and `subscribeTokenTrade` for the mints
//! in `--watch-file`, when given). Every inbound message is emitted VERBATIM
//! (§6.3 raw-bytes-first) as
//! `{"lane":"pumpportal","recv_unix_ms":...,"raw":<payload>}` — the payload
//! text is embedded untouched after a parse check (a frame that is not valid
//! JSON is a loud DRIFT sentinel, never a corrupt output line). NO
//! TRANSFORMATION: the workspace's `pump-quant-ingest::pumpportal_parse`
//! already consumes exactly these payload shapes downstream.
//!
//! Third-party tier discipline (§6.6/§28): PumpPortal is a parsed third-party
//! feed — DISCOVERY tier, corroborated on-chain before anything canonical
//! hangs off it. Reconnect uses the suite's deterministic backoff ladder;
//! staleness ([`PUMPPORTAL_STALE_SECS`] with no inbound at all — pump.fun
//! mints many tokens a minute, so a silent hour is a dead pipe) forces
//! reconnect. PumpPortal asks clients NOT to open multiple connections; one
//! process = one socket.

use std::time::{Duration, Instant};

use crate::json;
use crate::ws::{WsConn, WsEvent};
use crate::{backoff, emit};

/// The one public data endpoint (override with `PUMPPORTAL_WS_URL` only for
/// testing against a local echo server).
pub const DEFAULT_WS_URL: &str = "wss://pumpportal.fun/api/data";

/// Staleness watchdog (seconds): no inbound message for this long forces a
/// reconnect. New-token events alone arrive many times a minute in any
/// regime, so 60 s of total silence is a dead pipe.
pub const PUMPPORTAL_STALE_SECS: u64 = 60;

// ---------------------------------------------------------- pure builders

/// `subscribeNewToken` message. Pure.
#[must_use]
pub fn subscribe_new_token() -> String {
    "{\"method\":\"subscribeNewToken\"}".to_string()
}

/// `subscribeMigration` message. Pure.
#[must_use]
pub fn subscribe_migration() -> String {
    "{\"method\":\"subscribeMigration\"}".to_string()
}

/// `subscribeTokenTrade` for a set of mint keys. Pure.
#[must_use]
pub fn subscribe_token_trade(keys: &[String]) -> String {
    let mut out = String::with_capacity(48 + keys.len() * 48);
    out.push_str("{\"method\":\"subscribeTokenTrade\",\"keys\":[");
    for (n, k) in keys.iter().enumerate() {
        if n > 0 {
            out.push(',');
        }
        out.push('"');
        emit::escape_json_into(k, &mut out);
        out.push('"');
    }
    out.push_str("]}");
    out
}

/// The full subscription batch sent after every (re)connect. Pure.
#[must_use]
pub fn subscription_batch(watch_keys: &[String]) -> Vec<String> {
    let mut subs = vec![subscribe_new_token(), subscribe_migration()];
    if !watch_keys.is_empty() {
        subs.push(subscribe_token_trade(watch_keys));
    }
    subs
}

// ----------------------------------------------------------------- runner

const USAGE: &str = "usage: pq-stream-capture pumpportal [--watch-file f]\n\
  no auth required. --watch-file: one mint per line -> subscribeTokenTrade.\n\
  env: PUMPPORTAL_WS_URL (optional endpoint override, testing only)";

/// Lane entry point. `now_ms` is the injected capture clock (§22).
pub fn run(args: &[String], now_ms: fn() -> u64) -> u8 {
    let mut watch_keys: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--watch-file" => {
                let Some(path) = it.next() else {
                    eprintln!("[pq-stream-capture] pumpportal: --watch-file needs a value");
                    eprintln!("{USAGE}");
                    return 2;
                };
                match crate::read_list_file(path) {
                    Ok(keys) => watch_keys = keys,
                    Err(e) => {
                        eprintln!("[pq-stream-capture] pumpportal: {e}");
                        return 2;
                    }
                }
            }
            other => {
                eprintln!("[pq-stream-capture] pumpportal: unknown flag {other:?}");
                eprintln!("{USAGE}");
                return 2;
            }
        }
    }
    let url = std::env::var("PUMPPORTAL_WS_URL").unwrap_or_else(|_| DEFAULT_WS_URL.to_string());
    let subs = subscription_batch(&watch_keys);
    eprintln!(
        "[pq-stream-capture] pumpportal: subscribing newToken + migration{}",
        if watch_keys.is_empty() {
            String::new()
        } else {
            format!(" + tokenTrade({} keys)", watch_keys.len())
        }
    );

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut attempt: u32 = 0;
    loop {
        let mut conn = match WsConn::connect(&url) {
            Ok(c) => c,
            Err(e) => {
                let delay = backoff::step_secs(attempt);
                attempt = attempt.saturating_add(1);
                eprintln!("[pq-stream-capture] pumpportal connect failed ({e}); retry in {delay}s");
                std::thread::sleep(Duration::from_secs(delay));
                continue;
            }
        };
        eprintln!("[pq-stream-capture] pumpportal connected; resubscribing");
        let mut sub_write_failed = false;
        for s in &subs {
            if let Err(e) = conn.send_text(s) {
                eprintln!("[pq-stream-capture] pumpportal subscribe write failed: {e}");
                sub_write_failed = true;
                break;
            }
        }
        if !sub_write_failed {
            session(&mut conn, &mut out, now_ms, &mut attempt);
        }
        let delay = backoff::step_secs(attempt);
        attempt = attempt.saturating_add(1);
        eprintln!("[pq-stream-capture] pumpportal reconnecting in {delay}s");
        std::thread::sleep(Duration::from_secs(delay));
    }
}

fn session(
    conn: &mut WsConn,
    out: &mut impl std::io::Write,
    now_ms: fn() -> u64,
    attempt: &mut u32,
) {
    let mut last_inbound = Instant::now();
    loop {
        if conn.maybe_keepalive().is_err() {
            eprintln!("[pq-stream-capture] pumpportal keepalive write failed");
            return;
        }
        if last_inbound.elapsed() >= Duration::from_secs(PUMPPORTAL_STALE_SECS) {
            eprintln!(
                "[pq-stream-capture] pumpportal STALE: silent for \
                 {PUMPPORTAL_STALE_SECS}s — forcing reconnect"
            );
            return;
        }
        match conn.poll_event() {
            Ok(None) => {}
            Ok(Some(WsEvent::Pong)) => last_inbound = Instant::now(),
            Ok(Some(WsEvent::Binary(_))) => {
                last_inbound = Instant::now();
                eprintln!("[pq-stream-capture] pumpportal DRIFT: unexpected binary frame");
            }
            Ok(Some(WsEvent::Closed(reason))) => {
                eprintln!("[pq-stream-capture] pumpportal closed by server: {reason}");
                return;
            }
            Ok(Some(WsEvent::Text(text))) => {
                last_inbound = Instant::now();
                let recv = now_ms();
                if json::parse(&text).is_err() {
                    eprintln!("[pq-stream-capture] pumpportal DRIFT: non-JSON frame dropped");
                    continue;
                }
                *attempt = 0; // connection proved healthy
                let line = emit::raw_line("pumpportal", recv, None, &text);
                if emit::write_line(out, &line).is_err() {
                    eprintln!("[pq-stream-capture] pumpportal stdout write failed");
                    return;
                }
            }
            Err(e) => {
                eprintln!("[pq-stream-capture] pumpportal transport error: {e}");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_message_shapes() {
        assert_eq!(subscribe_new_token(), "{\"method\":\"subscribeNewToken\"}");
        assert_eq!(subscribe_migration(), "{\"method\":\"subscribeMigration\"}");
        assert_eq!(
            subscribe_token_trade(&["M1".into(), "M2".into()]),
            "{\"method\":\"subscribeTokenTrade\",\"keys\":[\"M1\",\"M2\"]}"
        );
    }

    #[test]
    fn batch_omits_token_trade_without_keys() {
        assert_eq!(subscription_batch(&[]).len(), 2);
        let with = subscription_batch(&["M1".into()]);
        assert_eq!(with.len(), 3);
        assert!(with[2].contains("subscribeTokenTrade"));
    }

    #[test]
    fn batch_messages_are_valid_json() {
        for msg in subscription_batch(&["a\"b".into()]) {
            assert!(json::parse(&msg).is_ok(), "invalid JSON: {msg}");
        }
    }
}
