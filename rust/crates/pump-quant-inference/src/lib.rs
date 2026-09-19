//! `pump_quant_inference` — the Qwen decision client.
//!
//! Two responsibilities, both fail-closed:
//!
//! * [`Action`] — the exact trained action vocabulary (7 tokens). Parsing the
//!   model's completion is *bounded*: an unknown or malformed action is an
//!   error, never silently coerced to a plausible default (the same class of
//!   bug as inventing an untrained `SELL` token).
//! * [`InferenceClient`] — a blocking `ureq` call to llama-server's
//!   OpenAI-compatible endpoint. A timeout, transport error, or non-200 is an
//!   `Err`, not a decision. This crate never fabricates a trade when the model
//!   is unreachable.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod seam;

pub use seam::{
    management_base, management_fraction_bps, parse_decision_payload, parse_headline,
    resolve_entry_clip_lamports, resolve_management_clip_lamports, route, Decision, DriftLedger,
    Headline, ManagementBase, OffContract, PayloadError, Route, SizeError, SizeTier,
    DEPLOY_LAMPORTS_CANONICAL, FEE_BUFFER_LAMPORTS, MIN_CLIP_LAMPORTS,
};

/// A completion that failed the trained-format contract converts to the client's error type
/// without losing the cause: the caller records [`PayloadError::kind`] on a [`DriftLedger`]
/// before it decides whether to log, alarm, or stay quiet.
impl From<PayloadError> for InferenceError {
    fn from(e: PayloadError) -> Self {
        InferenceError::Unparseable(e.to_string())
    }
}

use std::io::{BufRead, BufReader, Read};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The seven actions the c11 corpus actually trains (union of the `decision`
/// entry family and the `management_replay` management family). `SELL` is
/// intentionally absent — it was never a trained token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Enter a new position.
    Buy,
    /// Observe without acting.
    Watch,
    /// Do nothing this tick.
    Skip,
    /// Keep the current position unchanged.
    Hold,
    /// Increase the current position.
    Add,
    /// Decrease (but not close) the current position.
    Reduce,
    /// Close the position entirely.
    Exit,
}

impl Action {
    /// Parse a bare action token. Case-sensitive to the corpus vocabulary.
    pub fn parse(s: &str) -> Option<Action> {
        match s.trim() {
            "BUY" => Some(Action::Buy),
            "WATCH" => Some(Action::Watch),
            "SKIP" => Some(Action::Skip),
            "HOLD" => Some(Action::Hold),
            "ADD" => Some(Action::Add),
            "REDUCE" => Some(Action::Reduce),
            "EXIT" => Some(Action::Exit),
            _ => None,
        }
    }

    /// The token exactly as it appears in the corpus.
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Buy => "BUY",
            Action::Watch => "WATCH",
            Action::Skip => "SKIP",
            Action::Hold => "HOLD",
            Action::Add => "ADD",
            Action::Reduce => "REDUCE",
            Action::Exit => "EXIT",
        }
    }
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Failure modes for a decision request. All are fail-closed: the caller must
/// treat every one as "no decision this tick", never as a default action.
#[derive(Debug)]
pub enum InferenceError {
    /// The HTTP transport failed or the server returned a non-200.
    Transport(String),
    /// The completion did not contain a parseable `DECISION: <ACTION>` line.
    Unparseable(String),
}

/// Manual `std::error::Error` impl (no `thiserror` dependency needed).
impl std::fmt::Display for InferenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InferenceError::Transport(m) => write!(f, "inference transport error: {m}"),
            InferenceError::Unparseable(m) => write!(f, "unparseable decision: {m}"),
        }
    }
}

impl std::error::Error for InferenceError {}

/// Extract the action from a model completion. The corpus assistant format
/// opens with `DECISION: <ACTION>`, so we take the first such line and parse
/// its token strictly.
pub fn parse_decision(completion: &str) -> Result<Action, InferenceError> {
    for line in completion.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("DECISION:") {
            let token = rest.trim();
            return Action::parse(token)
                .ok_or_else(|| InferenceError::Unparseable(token.to_string()));
        }
    }
    Err(InferenceError::Unparseable(completion.to_string()))
}

/// A blocking llama-server client with fail-closed semantics.
pub struct InferenceClient {
    endpoint: String,
    timeout: Duration,
    agent: ureq::Agent,
}

/// The pinned SFT chat-template kwargs, mirrored from the training renderer.
///
/// THE SERVED PROMPT MUST EQUAL THE TRAINED PROMPT, and this is the half that byte-parity
/// on the user line cannot prove. Training renders with
/// `apply_chat_template(..., add_generation_prompt=True, enable_thinking=False)`
/// (`/training/v2/code/src/v2/rl/prompt_format.py`), which starts generation inside an
/// **empty, closed** think block: `assistant\n thinking\n\n</think>\n\n`. llama-server's
/// default template does not do that — it opens a ` thinking` block and leaves it
/// unclosed, a shape the model never saw in SFT. It then reasons in a register it was not
/// trained on and drifts off the `DECISION:`/`SIZE:` contract, which the fail-closed parser
/// turns into *no trade* rather than a visible error.
///
/// `enable_thinking: false` is the whole of the training renderer's `TEMPLATE_KWARGS`; the
/// server applies it to its own template, so the wire form stays the canonical chat request.
pub const CHAT_TEMPLATE_KWARGS: &str = "{\"enable_thinking\": false}";

/// Build the chat-completions body. Factored out so the template kwargs are a *testable*
/// property of every request rather than a literal buried in the send path.
#[must_use]
pub fn request_body(system: &str, user: &str) -> serde_json::Value {
    serde_json::json!({
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "temperature": 0.0,
        "max_tokens": 512,
        "stream": false,
        // Served-prompt parity (§9/P1): without this the model is handed an unclosed
        // think block it never saw in SFT.
        "chat_template_kwargs": {"enable_thinking": false},
    })
}

/// The streaming twin of [`request_body`]: byte-identical except `"stream": true`, so the
/// served prompt parity (`enable_thinking: false`, `temperature: 0.0`, same messages) is
/// preserved while the client reads the completion incrementally instead of all at once.
#[must_use]
pub fn streaming_request_body(system: &str, user: &str) -> serde_json::Value {
    let mut body = request_body(system, user);
    body["stream"] = serde_json::json!(true);
    body
}

impl InferenceClient {
    /// Build a client against a llama-server OpenAI-compatible base URL
    /// (e.g. `http://127.0.0.1:8080`), with a per-request timeout.
    pub fn new(endpoint: impl Into<String>, timeout: Duration) -> Self {
        let mut builder = ureq::AgentBuilder::new().timeout(timeout);
        builder = builder.timeout_connect(timeout);
        InferenceClient {
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            timeout,
            agent: builder.build(),
        }
    }

    /// Timeout for this client.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Send a single-turn chat completion (system + user) and return the raw
    /// text. Fail-closed: any transport error, non-200, or malformed body is an
    /// `Err`.
    pub fn complete(&self, system: &str, user: &str) -> Result<String, InferenceError> {
        let body = request_body(system, user);

        let resp = self
            .agent
            .post(&format!("{}/v1/chat/completions", self.endpoint))
            .set("Content-Type", "application/json")
            .send_json(body)
            .map_err(|e| InferenceError::Transport(e.to_string()))?;

        if resp.status() != 200 {
            return Err(InferenceError::Transport(format!(
                "status {}",
                resp.status()
            )));
        }

        let text = resp
            .into_string()
            .map_err(|e| InferenceError::Transport(e.to_string()))?;
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| InferenceError::Transport(e.to_string()))?;
        v["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| InferenceError::Unparseable(text))
    }

    /// Send a streaming completion and return the moment the decision headline
    /// (`DECISION:` + `SIZE:` on a `BUY`) has arrived, before the reasoning tail.
    ///
    /// The returned [`StreamedDecision`] carries the [`Headline`] to act on now; the
    /// caller drains it (in place or on a worker thread) for the full completion. This is
    /// the latency lever: the execution route reads only the headline, so the several
    /// seconds of reasoning the model emits afterwards are no longer on the critical
    /// path — they still get produced and journaled, just not waited on.
    pub fn complete_streaming(
        &self,
        system: &str,
        user: &str,
    ) -> Result<StreamedDecision, InferenceError> {
        let body = streaming_request_body(system, user);
        let resp = self
            .agent
            .post(&format!("{}/v1/chat/completions", self.endpoint))
            .set("Content-Type", "application/json")
            .send_json(body)
            .map_err(|e| InferenceError::Transport(e.to_string()))?;

        if resp.status() != 200 {
            return Err(InferenceError::Transport(format!(
                "status {}",
                resp.status()
            )));
        }

        let reader: Box<dyn Read + Send + Sync> = resp.into_reader();
        let mut sd = StreamedDecision {
            headline: Err(OffContract::NoDecisionLine),
            reader: Some(BufReader::new(reader)),
            buf: String::new(),
        };

        // Drive the SSE stream until the headline is complete (or definitely broken).
        loop {
            match sd.next_event()? {
                SseEvent::Delta(d) => {
                    sd.buf.push_str(&d);
                    if let Some(headline) = parse_headline(&sd.buf) {
                        sd.headline = headline;
                        break;
                    }
                }
                SseEvent::Done => {
                    // Ended before a usable headline: NoDecisionLine (or the more
                    // specific kind already recorded) is the honest answer.
                    sd.reader = None;
                    break;
                }
                SseEvent::Ignore => {}
            }
        }
        Ok(sd)
    }

    /// Prime the pooled connection to a warm llama-server and prove it answers, with one
    /// `GET /health` (llama-server's liveness probe).
    ///
    /// Call once at startup so the first real decision does not pay TCP/TLS setup, and so
    /// an unreachable server is found before capital is in play. The answer is never
    /// cached — health is probed fresh on every call. The `ureq::Agent` this client holds
    /// already reuses the keep-alive connection across decisions, which is the other half
    /// of warm serving; the one requirement is that a single client is shared, never
    /// rebuilt per decision.
    pub fn warmup(&self) -> Result<(), InferenceError> {
        let resp = self
            .agent
            .get(&format!("{}/health", self.endpoint))
            .call()
            .map_err(|e| InferenceError::Transport(e.to_string()))?;
        if resp.status() == 200 {
            Ok(())
        } else {
            Err(InferenceError::Transport(format!(
                "health status {}",
                resp.status()
            )))
        }
    }
}

/// One event off an OpenAI-compatible SSE stream.
enum SseEvent {
    /// A content delta (may be empty — a final `""` delta often precedes the stop).
    Delta(String),
    /// The stream ended (`[DONE]`, a `finish_reason`, or EOF).
    Done,
    /// A blank line, a comment, or a malformed frame — carry on.
    Ignore,
}

/// Classify one SSE text line.
fn sse_event(line: &str) -> SseEvent {
    let payload = match line.strip_prefix("data:") {
        Some(p) => p.trim(),
        None => return SseEvent::Ignore,
    };
    if payload == "[DONE]" {
        return SseEvent::Done;
    }
    match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(v) => {
            if v.pointer("/choices/0/finish_reason")
                .and_then(|f| f.as_str())
                .is_some()
            {
                return SseEvent::Done;
            }
            match v
                .pointer("/choices/0/delta/content")
                .and_then(|c| c.as_str())
            {
                Some(s) => SseEvent::Delta(s.to_string()),
                None => SseEvent::Ignore,
            }
        }
        Err(_) => SseEvent::Ignore,
    }
}

/// A decision returned the moment its headline streamed in, before the reasoning tail
/// finished. Read [`StreamedDecision::headline`] to act now; call
/// [`StreamedDecision::drain`] later (or on a worker thread) to recover the full
/// completion for the journal.
pub struct StreamedDecision {
    headline: Result<Headline, OffContract>,
    reader: Option<BufReader<Box<dyn Read + Send + Sync>>>,
    buf: String,
}

impl StreamedDecision {
    /// The headline (action + size-on-`BUY`) parsed from the first-arrived tokens.
    ///
    /// `Err` means the stream ended or emitted a definitely-untrained token before the
    /// headline completed: fail-closed at streaming speed, without waiting for the tail.
    /// The specific [`OffContract`] cause is recoverable here so the caller can record it
    /// on a [`DriftLedger`] immediately.
    #[must_use]
    pub fn headline(&self) -> Result<Headline, OffContract> {
        self.headline
    }

    /// Finish reading the SSE tail and return the full completion text (headline plus
    /// reasoning) for [`parse_decision_payload`] and the journal.
    pub fn drain(mut self) -> Result<String, InferenceError> {
        while self.reader.is_some() {
            match self.next_event()? {
                SseEvent::Delta(d) => self.buf.push_str(&d),
                SseEvent::Done => self.reader = None,
                SseEvent::Ignore => {}
            }
        }
        Ok(self.buf)
    }

    /// Read one more SSE event. Returns the content delta WITHOUT appending it, so the
    /// caller decides whether to buffer it (headline loop) or append it (drain).
    fn next_event(&mut self) -> Result<SseEvent, InferenceError> {
        let reader = self
            .reader
            .as_mut()
            .ok_or_else(|| InferenceError::Transport("stream already drained".to_string()))?;
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader
                .read_line(&mut line)
                .map_err(|e| InferenceError::Transport(e.to_string()))?;
            if n == 0 {
                self.reader = None;
                return Ok(SseEvent::Done);
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                continue;
            }
            match sse_event(line) {
                SseEvent::Ignore => continue,
                ev @ (SseEvent::Delta(_) | SseEvent::Done) => return Ok(ev),
            }
        }
    }
}

/// A single-slot, latest-wins mailbox for one decision subject (one mint).
///
/// The P6 decision loop's coalesce-to-newest rule: when a newer snapshot for the same
/// subject arrives while an inference is still in flight, the newer snapshot REPLACES the
/// pending one — the loop never queues stale decisions, it runs once for the newest state.
/// This is that rule as a type, shareable across the loop's worker threads.
///
/// A `Mutex` is the honest choice here, not a hot-path shortcut: the slot swap is a
/// register-sized store gated by a ~100 ms inference, so any lock cost is orders of
/// magnitude below the decision itself. The lockfree primitives in `pump_quant_core`
/// exist for the sub-microsecond tape path; this is not that path.
pub struct LatestWins<T> {
    inner: Arc<Mutex<Option<T>>>,
}

impl<T> LatestWins<T> {
    /// An empty mailbox.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
        }
    }

    /// Publish the newest snapshot, discarding whatever is still pending.
    pub fn publish(&self, value: T) {
        *self.inner.lock().expect("latest-wins slot poisoned") = Some(value);
    }

    /// Take the newest snapshot if one is pending, leaving the slot empty.
    #[must_use]
    pub fn take(&self) -> Option<T> {
        self.inner.lock().expect("latest-wins slot poisoned").take()
    }
}

impl<T> Default for LatestWins<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_seven_actions() {
        for (tok, want) in [
            ("BUY", Action::Buy),
            ("WATCH", Action::Watch),
            ("SKIP", Action::Skip),
            ("HOLD", Action::Hold),
            ("ADD", Action::Add),
            ("REDUCE", Action::Reduce),
            ("EXIT", Action::Exit),
        ] {
            assert_eq!(Action::parse(tok), Some(want));
            assert_eq!(want.as_str(), tok);
        }
    }

    #[test]
    fn rejects_untrained_sell_token() {
        assert_eq!(Action::parse("SELL"), None);
    }

    #[test]
    fn parse_decision_extracts_first_decision_line() {
        let c = "DECISION: WATCH\nSIZE: NONE\nEVIDENCE: ...\n";
        assert_eq!(parse_decision(c).unwrap(), Action::Watch);
    }

    #[test]
    fn parse_decision_fails_closed_on_garbage() {
        assert!(parse_decision("hello world").is_err());
        assert!(parse_decision("DECISION: SELL").is_err());
    }
}

#[cfg(test)]
mod served_prompt_parity {
    use super::*;

    /// THE TEST THAT WOULD HAVE CAUGHT THE GAP. Byte-parity on the rendered user line
    /// proves the renderer; it says nothing about what the server hands the model. This
    /// pins the template kwargs on every request body, so the served conversation keeps the
    /// pinned SFT shape (`enable_thinking: false`) instead of llama-server's default.
    #[test]
    fn every_request_pins_the_trained_chat_template_kwargs() {
        let body = request_body("SYSTEM", "USER");
        assert_eq!(
            body["chat_template_kwargs"]["enable_thinking"],
            serde_json::json!(false),
            "served prompt would drift off the trained template"
        );
        assert_eq!(body["temperature"], serde_json::json!(0.0));
        assert_eq!(body["stream"], serde_json::json!(false));
        assert_eq!(body["messages"][0]["role"], serde_json::json!("system"));
        // The documented constant and the wire body must not disagree.
        assert!(CHAT_TEMPLATE_KWARGS.contains("enable_thinking"));
    }

    #[test]
    fn the_streaming_body_differs_only_in_the_stream_flag() {
        let base = request_body("SYSTEM", "USER");
        let stream = streaming_request_body("SYSTEM", "USER");
        assert_eq!(stream["stream"], serde_json::json!(true));
        assert_eq!(base["stream"], serde_json::json!(false));
        // served-prompt parity is untouched: flip the flag back and they must be equal.
        let mut expected = base;
        expected["stream"] = serde_json::json!(true);
        assert_eq!(stream, expected);
        assert_eq!(
            stream["chat_template_kwargs"]["enable_thinking"],
            serde_json::json!(false)
        );
    }
}

#[cfg(test)]
mod streaming_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    /// Build one OpenAI-compatible SSE `data:` frame carrying a content delta.
    fn sse_delta(content: &str) -> String {
        let v = serde_json::json!({"choices": [{"delta": {"content": content}}]});
        format!("data: {}\n\n", v)
    }

    /// Serve a canned SSE completion that TRICKLES: the headline frame is written and
    /// flushed, then the server signals and blocks; only after the test releases it does
    /// the tail get written. Returns (url, headline_sent_rx, release_tail_tx, handle).
    ///
    /// This makes "the decision is returned before the tail" a DETERMINISTIC property
    /// rather than a timing race: the tail cannot even be written while the caller is
    /// still blocked on `complete_streaming`.
    fn spawn_trickle_server(
        headline: &'static str,
        tail: &'static str,
    ) -> (
        String,
        mpsc::Receiver<()>,
        mpsc::Sender<()>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (head_sent_tx, head_sent_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        let handle = thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            // Drain the request head+body so the client's write completes.
            let mut buf = [0u8; 8192];
            let mut req = String::new();
            loop {
                let n = conn.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                req.push_str(&String::from_utf8_lossy(&buf[..n]));
                if req.contains("\r\n\r\n") {
                    break;
                }
            }

            let head_frame = sse_delta(headline);
            let tail_frame = sse_delta(tail);
            let done = "data: [DONE]\n\n";
            let full = format!("{head_frame}{tail_frame}{done}");

            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                full.len()
            );
            conn.write_all(head.as_bytes()).unwrap();
            // write the headline, flush, then stall until the test releases the tail
            conn.write_all(head_frame.as_bytes()).unwrap();
            conn.flush().unwrap();
            head_sent_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            conn.write_all(format!("{tail_frame}{done}").as_bytes())
                .unwrap();
            conn.flush().unwrap();
        });

        (
            format!("http://127.0.0.1:{port}"),
            head_sent_rx,
            release_tx,
            handle,
        )
    }

    #[test]
    fn streaming_returns_the_headline_before_the_tail_is_written() {
        let (url, head_sent, release, handle) = spawn_trickle_server(
            "DECISION: BUY\nSIZE: FULL\n",
            "PRICE LIMIT: 0.02\nINVALIDATION: none\nEVIDENCE: flow sustained\n",
        );
        let client = InferenceClient::new(url, Duration::from_secs(10));

        // complete_streaming returns as soon as the headline is in.
        let sd = client.complete_streaming("SYS", "USER").unwrap();
        let hl = sd.headline().unwrap();
        assert_eq!(hl.action, Action::Buy);
        assert_eq!(hl.size, Some(SizeTier::Full));

        // The only thing the server could have written so far is the headline: it must be
        // stalled on the release channel, proving the tail was NOT yet on the wire.
        head_sent
            .recv_timeout(Duration::from_secs(5))
            .expect("server must have stalled at the headline");

        // Release the tail and drain the full completion for the journal.
        release.send(()).unwrap();
        let full = sd.drain().unwrap();
        assert!(full.contains("PRICE LIMIT: 0.02"));
        assert!(full.contains("EVIDENCE: flow sustained"));

        handle.join().unwrap();
    }

    #[test]
    fn independent_decisions_run_in_parallel_and_finish_independently() {
        let (url1, h1, r1, j1) =
            spawn_trickle_server("DECISION: WATCH\nSIZE: NONE\n", "EVIDENCE: quiet book\n");
        let (url2, h2, r2, j2) =
            spawn_trickle_server("DECISION: BUY\nSIZE: SMALL\n", "EVIDENCE: inflow\n");

        let a = thread::spawn(move || {
            let client = InferenceClient::new(url1, Duration::from_secs(10));
            let sd = client.complete_streaming("S", "U").unwrap();
            let hl = sd.headline().unwrap();
            h1.recv_timeout(Duration::from_secs(5)).unwrap();
            r1.send(()).unwrap();
            (hl, sd.drain().unwrap())
        });
        let b = thread::spawn(move || {
            let client = InferenceClient::new(url2, Duration::from_secs(10));
            let sd = client.complete_streaming("S", "U").unwrap();
            let hl = sd.headline().unwrap();
            h2.recv_timeout(Duration::from_secs(5)).unwrap();
            r2.send(()).unwrap();
            (hl, sd.drain().unwrap())
        });

        let (ha, ta) = a.join().unwrap();
        let (hb, tb) = b.join().unwrap();
        assert_eq!(ha.action, Action::Watch);
        assert_eq!(hb.action, Action::Buy);
        assert_eq!(hb.size, Some(SizeTier::Small));
        assert!(ta.contains("quiet book"));
        assert!(tb.contains("inflow"));
        j1.join().unwrap();
        j2.join().unwrap();
    }

    #[test]
    fn the_client_is_shareable_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<InferenceClient>();
        assert_send_sync::<LatestWins<u64>>();
    }

    #[test]
    fn warmup_probes_the_health_endpoint_and_accepts_200() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = conn.read(&mut buf);
            conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .unwrap();
        });
        let client =
            InferenceClient::new(format!("http://127.0.0.1:{port}"), Duration::from_secs(5));
        client.warmup().unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn latest_wins_discards_stale_snapshots() {
        let slot = LatestWins::new();
        slot.publish(1_u64);
        slot.publish(2_u64); // the newer snapshot replaces the pending one
        assert_eq!(slot.take(), Some(2));
        assert_eq!(slot.take(), None); // consumed
        slot.publish(3_u64);
        assert_eq!(slot.take(), Some(3));
    }
}
