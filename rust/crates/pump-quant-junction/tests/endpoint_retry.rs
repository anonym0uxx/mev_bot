//! Item 1: bounded INFRASTRUCTURE endpoint retries (`endpoint_retry`). Each named bound has a pinned value, a pinned
//! worst-case sleep budget, and a behaviour test. Model asks are pinned at ONE attempt over the production client.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pump_quant_junction::endpoint_retry::{
    bootstrap_walk_cost, is_transient, RetryingPages, HELIUS_WS_RECONNECT, LASERSTREAM_RESPAWN,
    LAUNCH_BOOTSTRAP_PAGE, LAUNCH_BOOTSTRAP_WALK_RETRY_BUDGET, MODEL_ASK,
};
use pump_quant_junction::launch_bootstrap::{FetchError, Page, PageSource};
use serde_json::{json, Value};

/// A page source that serves a scripted sequence of results (one per call), then `RpcError` forever.
struct Seq {
    out: RefCell<VecDeque<Result<Page, FetchError>>>,
    calls: RefCell<u32>,
}
impl Seq {
    fn new(v: Vec<Result<Page, FetchError>>) -> Self {
        Self {
            out: RefCell::new(v.into()),
            calls: RefCell::new(0),
        }
    }
}
impl PageSource for Seq {
    fn page(&self, _a: &str, _f: &Value, _t: Option<&str>) -> Result<Page, FetchError> {
        *self.calls.borrow_mut() += 1;
        self.out
            .borrow_mut()
            .pop_front()
            .unwrap_or(Err(FetchError::RpcError("end".into())))
    }
    fn label(&self) -> &str {
        "seq"
    }
}
fn ok() -> Result<Page, FetchError> {
    Ok(Page {
        txs: vec![],
        next: None,
    })
}
fn transport() -> Result<Page, FetchError> {
    Err(FetchError::Http("transport Io".into()))
}
fn nosleep(_d: Duration) {}

#[test]
fn named_bounds_and_budgets_are_pinned() {
    assert_eq!(
        (MODEL_ASK.max_attempts, MODEL_ASK.worst_case_sleep_ms()),
        (1, 0),
        "model asks are never retried"
    );
    assert_eq!(LAUNCH_BOOTSTRAP_PAGE.max_attempts, 3);
    assert_eq!(
        (1..3)
            .map(|a| LAUNCH_BOOTSTRAP_PAGE.backoff_ms(a))
            .collect::<Vec<_>>(),
        vec![250, 500]
    );
    assert_eq!(LAUNCH_BOOTSTRAP_PAGE.worst_case_sleep_ms(), 750);
    assert_eq!(LAUNCH_BOOTSTRAP_WALK_RETRY_BUDGET, 4);
    assert_eq!(
        bootstrap_walk_cost(20),
        24,
        "worst-case RPC calls one walk is charged"
    );
    assert_eq!(HELIUS_WS_RECONNECT.max_attempts, 5);
    assert_eq!(
        (1..6)
            .map(|a| HELIUS_WS_RECONNECT.backoff_ms(a))
            .collect::<Vec<_>>(),
        vec![500, 1_000, 2_000, 4_000, 8_000]
    );
    assert_eq!(
        HELIUS_WS_RECONNECT.worst_case_sleep_ms(),
        7_500,
        "4 sleeps between 5 attempts"
    );
    assert_eq!(
        (1..=5)
            .map(|a| HELIUS_WS_RECONNECT.backoff_ms(a))
            .sum::<u64>(),
        15_500,
        "daemon episode: a sleep after every failed attempt"
    );
    assert_eq!(HELIUS_WS_RECONNECT.backoff_ms(20), 10_000, "capped");
    assert_eq!(
        (
            LASERSTREAM_RESPAWN.max_attempts,
            LASERSTREAM_RESPAWN.base_backoff_ms
        ),
        (5, 15_000)
    );
}

#[test]
fn only_transient_failures_are_retryable() {
    assert!(is_transient(&FetchError::RateLimited));
    assert!(is_transient(&FetchError::Http("transport Io".into())));
    assert!(is_transient(&FetchError::Http("status 503".into())));
    assert!(is_transient(&FetchError::Http("status 429".into())));
    assert!(!is_transient(&FetchError::Http("status 404".into())));
    assert!(!is_transient(&FetchError::RpcError("code -32602".into())));
    assert!(!is_transient(&FetchError::BadShape("non-json".into())));
}

#[test]
fn a_transient_page_failure_is_retried_within_the_per_page_bound_then_succeeds() {
    let s = Seq::new(vec![transport(), transport(), ok()]);
    let r = RetryingPages::with_sleep(&s, LAUNCH_BOOTSTRAP_PAGE, 100, nosleep);
    assert!(r.page("a", &json!({}), None).is_ok());
    assert_eq!(
        (*s.calls.borrow(), r.retries.get(), r.exhausted.get()),
        (3, 2, 0)
    );
}

#[test]
fn a_page_that_keeps_failing_stops_at_max_attempts_and_is_counted_exhausted() {
    let s = Seq::new(vec![transport(), transport(), transport(), ok()]);
    let r = RetryingPages::with_sleep(&s, LAUNCH_BOOTSTRAP_PAGE, 100, nosleep);
    assert!(r.page("a", &json!({}), None).is_err());
    assert_eq!(
        *s.calls.borrow(),
        LAUNCH_BOOTSTRAP_PAGE.max_attempts,
        "never a 4th call"
    );
    assert_eq!(r.exhausted.get(), 1);
}

#[test]
fn an_answer_is_never_retried() {
    let s = Seq::new(vec![Err(FetchError::RpcError("code -32602".into())), ok()]);
    let r = RetryingPages::with_sleep(&s, LAUNCH_BOOTSTRAP_PAGE, 100, nosleep);
    assert!(r.page("a", &json!({}), None).is_err());
    assert_eq!((*s.calls.borrow(), r.retries.get()), (1, 0));
}

#[test]
fn the_walk_retry_budget_caps_retries_across_pages() {
    // 5 pages, each failing once before succeeding: the walk budget (4) lets 4 retries through; the 5th page's
    // failure is returned at once.
    let mut v = Vec::new();
    for _ in 0..5 {
        v.push(transport());
        v.push(ok());
    }
    let s = Seq::new(v);
    let r = RetryingPages::with_sleep(
        &s,
        LAUNCH_BOOTSTRAP_PAGE,
        LAUNCH_BOOTSTRAP_WALK_RETRY_BUDGET,
        nosleep,
    );
    for _ in 0..4 {
        assert!(r.page("a", &json!({}), None).is_ok());
    }
    assert!(r.page("a", &json!({}), None).is_err(), "walk budget spent");
    assert_eq!(r.retries.get(), LAUNCH_BOOTSTRAP_WALK_RETRY_BUDGET);
    assert!(u64::from(r.calls.get()) <= bootstrap_walk_cost(5));
}

/// Model asks travel ONCE: a failing (HTTP 500) endpoint is called exactly once per ask by the production client
/// the daemon arms, never retried. At temperature 0 a re-ask returns the same answer, and a retry would count the
/// decision twice.
#[test]
fn the_production_model_client_makes_exactly_one_attempt_per_ask() {
    use pump_quant_app::model_authority::ModelSource;
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let h = Arc::clone(&hits);
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut s) = c else { continue };
            h.fetch_add(1, Ordering::SeqCst);
            let mut b = [0u8; 8192];
            let _ = s.read(&mut b);
            let _ = s.write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    let client = pump_quant_inference::InferenceClient::new(
        &url,
        pump_quant_junction::model_lifecycle::CLIENT_TIMEOUT,
    );
    let r = client.complete_meta("S", "U");
    assert!(r.is_err());
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(hits.load(Ordering::SeqCst), MODEL_ASK.max_attempts as usize);
}
