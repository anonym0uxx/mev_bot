//! Bounded retries for the daemon's INFRASTRUCTURE endpoints (RPC / Helius / launch bootstrap / stream respawn).
//!
//! MODEL ASKS ARE NOT RETRIED ([`MODEL_ASK`] = one attempt). At temperature 0 a re-ask returns the same answer, so a
//! retry adds latency and nothing else. It would also count one decision twice in endpoint health and in the request
//! table. A failed ask is a named failure ("no verdict"), charged to health once per request id.
//!
//! Every retried endpoint gets a NAMED [`RetryBound`]. It holds a maximum attempt count, a capped exponential backoff
//! ladder and an explicit worst-case sleep budget, and the daemon uses these exact values. Only transient failures are
//! retried (transport errors, HTTP 429/5xx). A malformed body or a JSON-RPC error answer is a real answer, and a retry
//! would not change it.

use std::cell::Cell;
use std::time::Duration;

use serde_json::Value;

use crate::launch_bootstrap::{FetchError, Page, PageSource};

/// A named retry bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryBound {
    /// Stable name (logs, docs, tests).
    pub name: &'static str,
    /// Total attempts including the first (1 = no retry).
    pub max_attempts: u32,
    /// Sleep before the 2nd attempt, ms. Doubles per further attempt.
    pub base_backoff_ms: u64,
    /// Ceiling on one sleep, ms.
    pub cap_backoff_ms: u64,
}

impl RetryBound {
    /// Sleep before attempt `attempt` (0-based; attempt 0 never sleeps): `min(base * 2^(attempt-1), cap)`.
    #[must_use]
    pub fn backoff_ms(&self, attempt: u32) -> u64 {
        if attempt == 0 {
            return 0;
        }
        let shift = attempt.saturating_sub(1).min(32);
        self.base_backoff_ms
            .saturating_mul(1u64 << shift)
            .min(self.cap_backoff_ms)
    }

    /// Worst-case total sleep when every attempt fails (the time budget this bound can spend sleeping).
    #[must_use]
    pub fn worst_case_sleep_ms(&self) -> u64 {
        (1..self.max_attempts).map(|a| self.backoff_ms(a)).sum()
    }
}

/// Model asks: exactly one attempt. See the module doc. Pinned by a test that counts source calls per ask.
pub const MODEL_ASK: RetryBound = RetryBound {
    name: "model_ask_no_retry",
    max_attempts: 1,
    base_backoff_ms: 0,
    cap_backoff_ms: 0,
};

/// One launch-bootstrap history page (`getTransactionsForAddress`, worker thread, never the engine thread).
/// Three attempts with 250 ms then 500 ms sleeps, so the worst-case sleep is 0.75 s. Each attempt also has the
/// transport's own 20 s timeout. Per-walk retries are capped separately by [`LAUNCH_BOOTSTRAP_WALK_RETRY_BUDGET`].
pub const LAUNCH_BOOTSTRAP_PAGE: RetryBound = RetryBound {
    name: "launch_bootstrap_page",
    max_attempts: 3,
    base_backoff_ms: 250,
    cap_backoff_ms: 2_000,
};

/// Retries (beyond first attempts) one bootstrap walk may spend in total, across all its pages. With the 20-page walk
/// budget, a walk makes at most 20 + 4 = 24 RPC calls. The daemon charges that worst case to the RPC window budget.
pub const LAUNCH_BOOTSTRAP_WALK_RETRY_BUDGET: u32 = 4;

/// Helius WebSocket reconnect (force / error / stale paths): 5 attempts on the ladder 500 ms, 1 s, 2 s, 4 s, 8 s
/// (cap 10 s). The daemon loop sleeps after EVERY failed attempt, including the last one. So one reconnect episode can
/// spend at most 15.5 s sleeping (sum of `backoff_ms(1..=5)`). The sleeps between attempts
/// (`worst_case_sleep_ms`) total 7.5 s. After the last attempt the daemon degrades and does not crash.
pub const HELIUS_WS_RECONNECT: RetryBound = RetryBound {
    name: "helius_ws_reconnect",
    max_attempts: 5,
    base_backoff_ms: 500,
    cap_backoff_ms: 10_000,
};

/// LaserStream child respawn: at most 5 respawns per process, at least 15 s apart. After that, Helius WS becomes the
/// permanent fallback. The cooldown is a minimum spacing, not a sleep.
pub const LASERSTREAM_RESPAWN: RetryBound = RetryBound {
    name: "laserstream_respawn",
    max_attempts: 5,
    base_backoff_ms: 15_000,
    cap_backoff_ms: 15_000,
};

/// RPC calls one bootstrap walk may make in the worst case. The daemon charges this to `bootstrap_budget()`.
#[must_use]
pub fn bootstrap_walk_cost(max_pages: u32) -> u64 {
    u64::from(max_pages).saturating_add(u64::from(LAUNCH_BOOTSTRAP_WALK_RETRY_BUDGET))
}

/// Whether a page failure is transient (worth a retry): transport errors, HTTP 429 and 5xx. A JSON-RPC error answer,
/// a non-JSON or mis-shaped body, and any other HTTP status are answers, and are returned unchanged.
#[must_use]
pub fn is_transient(e: &FetchError) -> bool {
    match e {
        FetchError::RateLimited => true,
        FetchError::Http(s) => {
            s.starts_with("transport")
                || s.strip_prefix("status ")
                    .and_then(|c| c.parse::<u16>().ok())
                    .is_some_and(|c| c == 429 || (500..600).contains(&c))
        }
        FetchError::RpcError(_) | FetchError::BadShape(_) => false,
    }
}

/// A [`PageSource`] that retries transient page failures within `bound` per page and `walk_budget` retries in total.
/// The counters are read-only views for the log/report. `sleep` is injectable so tests never sleep.
pub struct RetryingPages<'a> {
    inner: &'a dyn PageSource,
    bound: RetryBound,
    walk_budget: u32,
    sleep: fn(Duration),
    /// Retries spent so far (all pages).
    pub retries: Cell<u32>,
    /// Calls made to the inner source.
    pub calls: Cell<u32>,
    /// Pages that failed after exhausting their bound or the walk budget.
    pub exhausted: Cell<u32>,
}

impl<'a> RetryingPages<'a> {
    /// Production: real sleeps.
    #[must_use]
    pub fn new(inner: &'a dyn PageSource, bound: RetryBound, walk_budget: u32) -> Self {
        Self::with_sleep(inner, bound, walk_budget, std::thread::sleep)
    }

    /// Test seam: a custom sleeper.
    #[must_use]
    pub fn with_sleep(
        inner: &'a dyn PageSource,
        bound: RetryBound,
        walk_budget: u32,
        sleep: fn(Duration),
    ) -> Self {
        Self {
            inner,
            bound,
            walk_budget,
            sleep,
            retries: Cell::new(0),
            calls: Cell::new(0),
            exhausted: Cell::new(0),
        }
    }
}

impl PageSource for RetryingPages<'_> {
    fn page(
        &self,
        address: &str,
        filters: &Value,
        token: Option<&str>,
    ) -> Result<Page, FetchError> {
        let mut attempt = 0u32;
        loop {
            self.calls.set(self.calls.get().saturating_add(1));
            match self.inner.page(address, filters, token) {
                Ok(p) => return Ok(p),
                Err(e) => {
                    let more = attempt.saturating_add(1) < self.bound.max_attempts
                        && self.retries.get() < self.walk_budget
                        && is_transient(&e);
                    if !more {
                        if is_transient(&e) {
                            self.exhausted.set(self.exhausted.get().saturating_add(1));
                        }
                        return Err(e);
                    }
                    attempt = attempt.saturating_add(1);
                    self.retries.set(self.retries.get().saturating_add(1));
                    (self.sleep)(Duration::from_millis(self.bound.backoff_ms(attempt)));
                }
            }
        }
    }
    fn label(&self) -> &str {
        self.inner.label()
    }
}
