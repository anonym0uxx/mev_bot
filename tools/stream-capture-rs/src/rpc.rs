//! Multi-provider JSON-RPC client with DETERMINISTIC failover
//! (SERVER_BUILD_MANIFEST §4: provider order is priority order — no
//! randomization, no load balancing; the same inputs always walk the same
//! path).
//!
//! Providers come from env `RPC_URLS` (comma-separated, first = highest
//! priority). Per-provider health is a pure integer state machine
//! ([`ProviderHealth`]): a consecutive-error count and an EWMA latency in
//! integer MICROSECONDS with alpha 1/8 (`ewma += (sample - ewma) >> 3` —
//! §102 integer-only arithmetic in logic; the EWMA is diagnostic surface for
//! the supervisor, not a routing input, keeping failover deterministic). A
//! provider goes UNHEALTHY after [`RPC_MAX_CONSEC_ERRORS`] consecutive
//! failures and is re-probed after [`RPC_REPROBE_SECS`].
//!
//! The state machine is clock-injected (`now_ms` parameter) and
//! transport-injected ([`Transport`] trait), so the whole failover walk is
//! unit-tested with a mock — no network in tests. The real transport is
//! [`UreqTransport`] (one attempt per call; the pool IS the retry policy).

use std::time::Instant;

use crate::http::Http;

/// Consecutive errors that flip a provider UNHEALTHY.
pub const RPC_MAX_CONSEC_ERRORS: u32 = 3;

/// Seconds an unhealthy provider sits out before one re-probe attempt.
pub const RPC_REPROBE_SECS: u64 = 30;

/// EWMA smoothing shift: alpha = 1/8 (`delta >> 3`).
pub const EWMA_ALPHA_SHIFT: u32 = 3;

/// One JSON-RPC reply with its measured transport latency.
pub struct Reply {
    /// Response body text.
    pub body: String,
    /// Wall latency of the HTTP round trip, integer microseconds.
    pub latency_us: u64,
}

/// The injected transport seam (§22): production is [`UreqTransport`], tests
/// are mocks. One attempt per call — failover policy lives in [`RpcPool`].
pub trait Transport {
    /// POST `body` to `url`, returning the reply and its latency.
    fn post_json(&self, url: &str, body: &str) -> Result<Reply, String>;
}

/// Production transport over the suite's shared [`Http`] agent.
pub struct UreqTransport {
    http: Http,
}

impl UreqTransport {
    /// Build with the standard RPC timeout.
    #[must_use]
    pub fn new() -> Self {
        Self {
            http: Http::new(crate::http::TIMEOUT_SECS),
        }
    }
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for UreqTransport {
    fn post_json(&self, url: &str, body: &str) -> Result<Reply, String> {
        let start = Instant::now();
        let body = self.http.post_json_once(url, body)?;
        let latency_us = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
        Ok(Reply { body, latency_us })
    }
}

/// Strip credentials from a provider URL for logs/records: scheme + host
/// only (Helius keys ride in the query string — NEVER printed). Pure.
#[must_use]
pub fn redact_url(url: &str) -> String {
    let no_query = url.split('?').next().unwrap_or(url);
    match no_query.split_once("://") {
        Some((scheme, rest)) => {
            let host = rest.split('/').next().unwrap_or(rest);
            format!("{scheme}://{host}")
        }
        None => no_query.split('/').next().unwrap_or(no_query).to_string(),
    }
}

/// Per-provider health: pure integer state machine (§102), clock-injected.
pub struct ProviderHealth {
    /// Provider endpoint (may contain credentials; use [`redact_url`] to log).
    pub url: String,
    consec_errors: u32,
    ewma_latency_us: Option<u64>,
    unhealthy_since_ms: Option<u64>,
}

impl ProviderHealth {
    fn new(url: String) -> Self {
        Self {
            url,
            consec_errors: 0,
            ewma_latency_us: None,
            unhealthy_since_ms: None,
        }
    }

    /// Healthy, or unhealthy-but-due-for-reprobe at `now_ms`?
    #[must_use]
    pub fn eligible(&self, now_ms: u64) -> bool {
        match self.unhealthy_since_ms {
            None => true,
            Some(since) => now_ms.saturating_sub(since) >= RPC_REPROBE_SECS * 1000,
        }
    }

    /// Currently marked unhealthy?
    #[must_use]
    pub fn is_unhealthy(&self) -> bool {
        self.unhealthy_since_ms.is_some()
    }

    /// EWMA latency in integer micros (`None` until the first success).
    #[must_use]
    pub fn ewma_latency_us(&self) -> Option<u64> {
        self.ewma_latency_us
    }

    fn record_success(&mut self, latency_us: u64) {
        self.consec_errors = 0;
        self.unhealthy_since_ms = None;
        self.ewma_latency_us = Some(match self.ewma_latency_us {
            None => latency_us,
            Some(ewma) => {
                // Integer EWMA, alpha 1/8: ewma += (sample - ewma) >> 3.
                let delta = latency_us as i64 - ewma as i64;
                (ewma as i64 + (delta >> EWMA_ALPHA_SHIFT)).max(0) as u64
            }
        });
    }

    fn record_failure(&mut self, now_ms: u64) {
        self.consec_errors = self.consec_errors.saturating_add(1);
        if self.consec_errors >= RPC_MAX_CONSEC_ERRORS {
            // (Re)stamp the sit-out window — a failed re-probe waits again.
            self.unhealthy_since_ms = Some(now_ms);
        }
    }
}

/// One successful call's outcome.
#[derive(Debug)]
pub struct CallOutcome {
    /// Index of the provider that answered (priority position).
    pub provider_index: usize,
    /// Response body text (may still be a JSON-RPC `error` member — a
    /// provider that ANSWERS is transport-healthy; the caller judges the
    /// JSON-RPC layer).
    pub body: String,
}

/// The deterministic failover pool.
pub struct RpcPool {
    providers: Vec<ProviderHealth>,
    next_id: u64,
}

impl RpcPool {
    /// Parse the `RPC_URLS` comma-separated priority list.
    pub fn from_urls_csv(csv: &str) -> Result<Self, String> {
        let providers: Vec<ProviderHealth> = csv
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| ProviderHealth::new(s.to_string()))
            .collect();
        if providers.is_empty() {
            return Err("RPC_URLS is empty".to_string());
        }
        Ok(Self {
            providers,
            next_id: 1,
        })
    }

    /// Providers in priority order (read-only view for diagnostics/tests).
    #[must_use]
    pub fn providers(&self) -> &[ProviderHealth] {
        &self.providers
    }

    /// Build one JSON-RPC 2.0 request body. `params_json` must already be
    /// valid JSON (an array). Pure.
    #[must_use]
    pub fn build_request(id: u64, method: &str, params_json: &str) -> String {
        let mut out = String::with_capacity(64 + method.len() + params_json.len());
        out.push_str("{\"jsonrpc\":\"2.0\",\"id\":");
        out.push_str(&id.to_string());
        out.push_str(",\"method\":\"");
        crate::emit::escape_json_into(method, &mut out);
        out.push_str("\",\"params\":");
        out.push_str(params_json);
        out.push('}');
        out
    }

    /// Walk providers in priority order, skipping unhealthy ones (except
    /// those due a re-probe), until one answers. Deterministic: same health
    /// state + same `now_ms` = same walk. `Err` only when every provider
    /// failed this walk.
    pub fn call(
        &mut self,
        transport: &dyn Transport,
        now_ms: u64,
        method: &str,
        params_json: &str,
    ) -> Result<CallOutcome, String> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let body = Self::build_request(id, method, params_json);
        let mut errors: Vec<String> = Vec::new();
        for i in 0..self.providers.len() {
            if !self.providers[i].eligible(now_ms) {
                continue;
            }
            let url = self.providers[i].url.clone();
            match transport.post_json(&url, &body) {
                Ok(reply) => {
                    self.providers[i].record_success(reply.latency_us);
                    return Ok(CallOutcome {
                        provider_index: i,
                        body: reply.body,
                    });
                }
                Err(e) => {
                    self.providers[i].record_failure(now_ms);
                    eprintln!(
                        "[pq-stream-capture] rpc provider {} failed ({e}); trying next",
                        redact_url(&url)
                    );
                    errors.push(format!("{}: {e}", redact_url(&url)));
                }
            }
        }
        Err(format!("all providers failed: [{}]", errors.join("; ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Scripted mock: a closure per call, recording the URLs walked.
    struct Mock<F: Fn(&str) -> Result<Reply, String>> {
        f: F,
        calls: RefCell<Vec<String>>,
    }

    impl<F: Fn(&str) -> Result<Reply, String>> Transport for Mock<F> {
        fn post_json(&self, url: &str, _body: &str) -> Result<Reply, String> {
            self.calls.borrow_mut().push(url.to_string());
            (self.f)(url)
        }
    }

    fn ok(latency_us: u64) -> Result<Reply, String> {
        Ok(Reply {
            body: "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":42}".to_string(),
            latency_us,
        })
    }

    #[test]
    fn build_request_shape() {
        assert_eq!(
            RpcPool::build_request(7, "getSlot", "[]"),
            "{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"getSlot\",\"params\":[]}"
        );
    }

    #[test]
    fn empty_urls_refused() {
        assert!(RpcPool::from_urls_csv("").is_err());
        assert!(RpcPool::from_urls_csv(" , ,").is_err());
    }

    #[test]
    fn first_healthy_provider_wins_deterministically() {
        let mut pool = RpcPool::from_urls_csv("https://a,https://b").unwrap();
        let mock = Mock {
            f: |_| ok(100),
            calls: RefCell::new(Vec::new()),
        };
        let out = pool.call(&mock, 0, "getSlot", "[]").unwrap();
        assert_eq!(out.provider_index, 0);
        assert_eq!(mock.calls.borrow().as_slice(), ["https://a"]);
    }

    #[test]
    fn failover_walks_priority_order() {
        let mut pool = RpcPool::from_urls_csv("https://a,https://b,https://c").unwrap();
        let mock = Mock {
            f: |url| {
                if url == "https://c" {
                    ok(5)
                } else {
                    Err("boom".into())
                }
            },
            calls: RefCell::new(Vec::new()),
        };
        let out = pool.call(&mock, 0, "getSlot", "[]").unwrap();
        assert_eq!(out.provider_index, 2);
        assert_eq!(
            mock.calls.borrow().as_slice(),
            ["https://a", "https://b", "https://c"]
        );
    }

    #[test]
    fn provider_goes_unhealthy_after_three_consecutive_errors_and_is_skipped() {
        let mut pool = RpcPool::from_urls_csv("https://a,https://b").unwrap();
        let fail_a = Mock {
            f: |url| {
                if url == "https://a" {
                    Err("down".into())
                } else {
                    ok(9)
                }
            },
            calls: RefCell::new(Vec::new()),
        };
        for _ in 0..RPC_MAX_CONSEC_ERRORS {
            pool.call(&fail_a, 1000, "m", "[]").unwrap();
        }
        assert!(pool.providers()[0].is_unhealthy());
        // Next call must not touch provider a at all.
        fail_a.calls.borrow_mut().clear();
        pool.call(&fail_a, 2000, "m", "[]").unwrap();
        assert_eq!(fail_a.calls.borrow().as_slice(), ["https://b"]);
    }

    #[test]
    fn unhealthy_provider_reprobed_after_window() {
        let mut pool = RpcPool::from_urls_csv("https://a,https://b").unwrap();
        let fail_a = Mock {
            f: |url| {
                if url == "https://a" {
                    Err("down".into())
                } else {
                    ok(9)
                }
            },
            calls: RefCell::new(Vec::new()),
        };
        for _ in 0..RPC_MAX_CONSEC_ERRORS {
            pool.call(&fail_a, 1000, "m", "[]").unwrap();
        }
        // Inside the sit-out window: skipped.
        assert!(!pool.providers()[0].eligible(1000 + RPC_REPROBE_SECS * 1000 - 1));
        // At the window edge: re-probed (and, still failing, re-stamped).
        let now = 1000 + RPC_REPROBE_SECS * 1000;
        assert!(pool.providers()[0].eligible(now));
        fail_a.calls.borrow_mut().clear();
        pool.call(&fail_a, now, "m", "[]").unwrap();
        assert_eq!(
            fail_a.calls.borrow().as_slice(),
            ["https://a", "https://b"],
            "re-probe walks a first again"
        );
        assert!(
            pool.providers()[0].is_unhealthy(),
            "failed re-probe re-stamps"
        );

        // A healthy re-probe fully recovers the provider.
        let heal = Mock {
            f: |_| ok(3),
            calls: RefCell::new(Vec::new()),
        };
        pool.call(&heal, now + RPC_REPROBE_SECS * 1000, "m", "[]")
            .unwrap();
        assert!(!pool.providers()[0].is_unhealthy());
    }

    #[test]
    fn all_providers_failing_is_an_error() {
        let mut pool = RpcPool::from_urls_csv("https://a?api-key=SECRET").unwrap();
        let mock = Mock {
            f: |_| Err("nope".into()),
            calls: RefCell::new(Vec::new()),
        };
        let err = pool.call(&mock, 0, "m", "[]").unwrap_err();
        assert!(err.contains("all providers failed"));
        assert!(!err.contains("SECRET"), "credentials never in errors");
    }

    #[test]
    fn ewma_is_integer_alpha_one_eighth() {
        let mut pool = RpcPool::from_urls_csv("https://a").unwrap();
        let m100 = Mock {
            f: |_| ok(100),
            calls: RefCell::new(Vec::new()),
        };
        pool.call(&m100, 0, "m", "[]").unwrap();
        assert_eq!(pool.providers()[0].ewma_latency_us(), Some(100), "seeded");
        let m900 = Mock {
            f: |_| ok(900),
            calls: RefCell::new(Vec::new()),
        };
        pool.call(&m900, 0, "m", "[]").unwrap();
        // 100 + (900-100)/8 = 200, integer-exact.
        assert_eq!(pool.providers()[0].ewma_latency_us(), Some(200));
        let m0 = Mock {
            f: |_| ok(0),
            calls: RefCell::new(Vec::new()),
        };
        pool.call(&m0, 0, "m", "[]").unwrap();
        // 200 + (0-200)>>3 = 200 - 25 = 175.
        assert_eq!(pool.providers()[0].ewma_latency_us(), Some(175));
    }

    #[test]
    fn redact_url_strips_query_and_path() {
        assert_eq!(
            redact_url("https://mainnet.helius-rpc.com/?api-key=SECRET"),
            "https://mainnet.helius-rpc.com"
        );
        assert_eq!(
            redact_url("https://rpc.x.com/v1/key/SECRET"),
            "https://rpc.x.com"
        );
        assert_eq!(redact_url("hostonly"), "hostonly");
    }
}
