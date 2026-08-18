//! HTTPS transport for the JSON-RPC lanes — adapted from the audited
//! `tools/social-ingest-https-rs/src/http.rs` (same ureq Agent construction,
//! same size cap, same permanent-vs-transient split). The stream suite's RPC
//! failover ([`crate::rpc`]) owns retry POLICY (which provider next), so this
//! module deliberately does NOT carry the in-fetch backoff ladder: one
//! attempt per call, classified, and the pool decides.
//!
//! §22: this module and the WS transport in [`crate::ws`] are the entire
//! network-impurity surface of the library; everything else is pure and
//! fixture-tested without sockets.

use std::io::Read;
use std::time::Duration;

/// Response-size cap (bytes) — same generous bound as the HTTPS suite; a
/// hostile or broken endpoint cannot balloon memory (§99-spirit bounding).
pub const MAX_BODY_BYTES: u64 = 8 * 1024 * 1024;

/// Connect/read/write timeout for JSON-RPC calls (seconds). Short: the fee
/// sampler runs on a 15 s cadence and failover must fit inside it.
pub const TIMEOUT_SECS: u64 = 10;

/// Is this HTTP status worth counting as a transient provider failure?
/// 429 (rate limit) and all 5xx — same classification as the HTTPS suite.
#[must_use]
pub fn is_transient_status(code: u16) -> bool {
    code == 429 || (500..600).contains(&code)
}

/// One shared blocking HTTP client per subcommand process (keep-alive
/// connection reuse across polls — no per-call TCP+TLS re-handshake).
pub struct Http {
    agent: ureq::Agent,
}

impl Http {
    /// Build the agent; `timeout_secs` applies to connect, read and write.
    /// `try_proxy_from_env` mirrors the rest of the suite.
    #[must_use]
    pub fn new(timeout_secs: u64) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(timeout_secs))
            .timeout_read(Duration::from_secs(timeout_secs))
            .timeout_write(Duration::from_secs(timeout_secs))
            .try_proxy_from_env(true)
            .build();
        Self { agent }
    }

    /// POST a JSON `body` to `url` — exactly ONE attempt (the RPC pool owns
    /// retry/failover policy). Any HTTP error status or transport failure is
    /// an `Err`; the body is size-capped and UTF-8-checked.
    pub fn post_json_once(&self, url: &str, body: &str) -> Result<String, String> {
        let req = self.agent.post(url).set("Content-Type", "application/json");
        match req.send_string(body) {
            Ok(resp) => read_capped(resp),
            Err(ureq::Error::Status(code, resp)) => {
                // On HTTP errors, capture the response body for diagnosis.
                let status_text = resp.status_text().to_string();
                let body_text = {
                    let mut text = String::new();
                    let mut reader = resp.into_reader().take(MAX_BODY_BYTES + 1);
                    let _ = reader.read_to_string(&mut text);
                    text
                };
                eprintln!("[http] POST {url} → HTTP {code} {status_text}");
                eprintln!("[http] response body: {body_text}");
                Err(format!("HTTP Error {code}: {status_text} — {body_text}"))
            }
            Err(transport) => Err(transport.to_string()),
        }
    }
}

/// Read a response body under [`MAX_BODY_BYTES`]; over-cap or non-UTF-8 is an
/// error (skip the call, never panic, never truncate silently).
fn read_capped(resp: ureq::Response) -> Result<String, String> {
    let mut text = String::new();
    let mut reader = resp.into_reader().take(MAX_BODY_BYTES + 1);
    reader
        .read_to_string(&mut text)
        .map_err(|e| format!("body read failed: {e}"))?;
    if text.len() as u64 > MAX_BODY_BYTES {
        return Err(format!("response body exceeds {MAX_BODY_BYTES} byte cap"));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_classification() {
        assert!(is_transient_status(429));
        assert!(is_transient_status(500));
        assert!(is_transient_status(503));
        assert!(is_transient_status(599));
        assert!(!is_transient_status(200));
        assert!(!is_transient_status(404));
        assert!(!is_transient_status(403));
    }
}
