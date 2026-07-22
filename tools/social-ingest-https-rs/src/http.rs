//! The transport edge — the ONE module that touches the network (§22: it and
//! `main.rs`'s clock are the entire impurity surface; every other module is
//! pure and replay-tested without sockets).
//!
//! Hardening over the Python twins' bare `urllib.request.urlopen`:
//! * connect + read timeouts on one shared [`ureq::Agent`] (which also gives
//!   keep-alive connection reuse across polls — `urllib` reconnects per
//!   request);
//! * bounded, deterministic, jitter-free retry inside a fetch for transient
//!   failures (HTTP 429/5xx, transport errors) with `Retry-After` respected —
//!   see [`crate::backoff`];
//! * a hard response-size cap so a hostile or broken endpoint cannot balloon
//!   memory (§99-spirit bounding; the Python twins read unbounded);
//! * permanent HTTP errors (4xx except 429) surface immediately, exactly like
//!   Python's `HTTPError`, for the caller to log-and-continue.
//!
//! Diagnostics never originate here — errors are returned as strings and the
//! adapters print them in their Python twins' exact stderr formats.

use std::io::Read;
use std::thread;
use std::time::Duration;

use crate::backoff;

/// Response-size cap (bytes). Generous: the largest legitimate payload
/// (a Firecrawl markdown scrape, later truncated to 20 000 chars anyway) is
/// well under 1 MiB.
pub const MAX_BODY_BYTES: u64 = 8 * 1024 * 1024;

/// Is this HTTP status worth an in-fetch retry? 429 (rate limit) and all 5xx.
#[must_use]
pub fn is_transient_status(code: u16) -> bool {
    code == 429 || (500..600).contains(&code)
}

/// Parse a `Retry-After` header value — integer-seconds form only; the
/// HTTP-date form falls back to the ladder step.
#[must_use]
pub fn parse_retry_after(header: Option<&str>) -> Option<u64> {
    header.and_then(|s| s.trim().parse::<u64>().ok())
}

/// One shared blocking HTTP client per subcommand process.
pub struct Http {
    agent: ureq::Agent,
}

impl Http {
    /// Build the agent. `timeout_secs` is applied to BOTH connect and read,
    /// mirroring the Python twins' `urlopen(req, timeout=N)` (30 s for
    /// twitterapi/tiktok, 60 s for firecrawl). `try_proxy_from_env` matches
    /// `urllib`'s default proxy handling.
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

    /// GET `url` with `headers`; retries transient failures per the backoff
    /// ladder. Returns the body as text (size-capped, UTF-8-checked).
    pub fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<String, String> {
        self.fetch(url, headers, None)
    }

    /// POST a JSON `body` to `url` with `headers`; same retry/cap contract.
    pub fn post_json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Result<String, String> {
        self.fetch(url, headers, Some(body))
    }

    fn fetch(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&str>,
    ) -> Result<String, String> {
        let mut attempt: u32 = 0;
        loop {
            let mut req = match body {
                Some(_) => self.agent.post(url),
                None => self.agent.get(url),
            };
            for (k, v) in headers {
                req = req.set(k, v);
            }
            let outcome = match body {
                Some(b) => req.send_string(b),
                None => req.call(),
            };
            let (err_text, retry_after) = match outcome {
                Ok(resp) => return read_capped(resp),
                Err(ureq::Error::Status(code, resp)) if is_transient_status(code) => (
                    format!("HTTP Error {code}: {}", resp.status_text()),
                    parse_retry_after(resp.header("retry-after")),
                ),
                Err(ureq::Error::Status(code, resp)) => {
                    // Permanent (auth, bad request, not found): no retry —
                    // Python raises HTTPError straight to the caller's log.
                    return Err(format!("HTTP Error {code}: {}", resp.status_text()));
                }
                Err(transport) => (transport.to_string(), None),
            };
            match backoff::retry_delay_secs(attempt, retry_after) {
                Some(delay) => {
                    eprintln!(
                        "[pq-social-capture] transient fetch failure ({err_text}); \
                         retry in {delay}s"
                    );
                    thread::sleep(Duration::from_secs(delay));
                    attempt += 1;
                }
                None => return Err(err_text),
            }
        }
    }
}

/// Read a response body under [`MAX_BODY_BYTES`]; over-cap or non-UTF-8 is an
/// error (skip the poll, never panic, never truncate silently).
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

    #[test]
    fn retry_after_parses_integer_seconds_only() {
        assert_eq!(parse_retry_after(Some("30")), Some(30));
        assert_eq!(parse_retry_after(Some(" 5 ")), Some(5));
        assert_eq!(
            parse_retry_after(Some("Wed, 21 Oct 2026 07:28:00 GMT")),
            None
        );
        assert_eq!(parse_retry_after(None), None);
    }
}
