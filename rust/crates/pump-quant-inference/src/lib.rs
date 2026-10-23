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

pub mod probe_sl;
pub mod seam;

pub use seam::{
    management_base, management_fraction_bps, parse_decision_payload, parse_headline,
    resolve_clip_at_fraction_bps, resolve_entry_clip_lamports, resolve_management_clip_lamports,
    route, Decision, DriftLedger, EntryVenue, Headline, ManagementBase, OffContract, PayloadError,
    Route, SizeError, SizeTier, DEPLOY_LAMPORTS_CANONICAL, FEE_BUFFER_LAMPORTS, MIN_CLIP_LAMPORTS,
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
use std::time::{Duration, Instant};

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

impl InferenceError {
    /// A transport timeout (socket closed by the client's deadline) — the request-lifecycle v2
    /// cancellation signal, distinct from a server error or an unreadable body.
    #[must_use]
    pub fn is_timeout(&self) -> bool {
        match self {
            InferenceError::Transport(m) => {
                let l = m.to_lowercase();
                l.contains("timed out") || l.contains("timeout")
            }
            _ => false,
        }
    }
}

impl InferenceError {
    /// Whether this failure is transient and worth retrying. A [`Transport`] error —
    /// unreachable server, timeout, dropped connection, non-200 — may be transient;
    /// an [`Unparseable`] one is a contract failure the server has already answered,
    /// so a retry would not change it.
    ///
    /// [`Transport`]: InferenceError::Transport
    /// [`Unparseable`]: InferenceError::Unparseable
    #[must_use]
    pub fn is_transport(&self) -> bool {
        matches!(self, InferenceError::Transport(_))
    }
}

/// Backoff between transport retries. Kept small: the retry budget is governed by
/// the caller's deadline, not by the sleep.
const RETRY_BACKOFF: Duration = Duration::from_millis(50);

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
    /// Served model id to put in every request (`"model"`). `None` keeps the historical body
    /// byte-for-byte (the server's default model). Required for a runtime-LoRA server, where the
    /// id selects the adapter (entry vs management) — without it the base model answers.
    model: Option<String>,
    /// `max_tokens` for every request. Historical default 512 (unaddressed lane). The identified
    /// runtime-LoRA paper lane sets 128 = the qualified engine's frozen cap. A `finish_reason="length"`
    /// is reported as truncated and REFUSED by the authority; there is no retry at a larger cap.
    max_tokens: u32,
    /// Optional server-side stop sequences (compact-record contracts).
    stop: Option<Vec<String>>,
}

/// Historical request cap.
pub const DEFAULT_MAX_TOKENS: u32 = 512;

/// What `/v1/models` reported, recorded so a paper session can prove WHICH served identity it
/// talked to (model/adapter id, root checkpoint, parent) instead of assuming it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ServedModel {
    /// Served id (what goes in `"model"`).
    pub id: String,
    /// Root checkpoint path reported by the server, if any.
    pub root: Option<String>,
    /// Parent model for adapters, if reported.
    pub parent: Option<String>,
}

/// Why served-identity verification refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityRefusal {
    /// `/v1/models` unreachable / non-200 / unparseable.
    Transport(String),
    /// An expected id is not served.
    Missing {
        /// Expected ids the server does not serve.
        missing: Vec<String>,
        /// Ids the server does serve.
        served: Vec<String>,
    },
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
    request_body_for(None, system, user)
}

/// [`request_body`] with an explicit served model id. `None` is byte-identical to the historical
/// body; `Some(id)` adds `"model": id` and changes nothing else (same messages, temperature,
/// max_tokens, template kwargs).
#[must_use]
pub fn request_body_for(model: Option<&str>, system: &str, user: &str) -> serde_json::Value {
    request_body_with(model, DEFAULT_MAX_TOKENS, system, user)
}

/// [`request_body_for`] with an explicit `max_tokens`.
#[must_use]
pub fn request_body_with(model: Option<&str>, max_tokens: u32, system: &str, user: &str) -> serde_json::Value {
    let mut body = serde_json::json!({
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "temperature": 0.0,
        "max_tokens": max_tokens,
        "stream": false,
        // Served-prompt parity (§9/P1): without this the model is handed an unclosed
        // think block it never saw in SFT.
        "chat_template_kwargs": {"enable_thinking": false},
    });
    if let Some(m) = model {
        body["model"] = serde_json::json!(m);
    }
    body
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

/// A completion plus the server's own stop reason (F5b).
///
/// `finish_reason="length"` means the server hit `max_tokens`: the text is a TRUNCATED
/// completion, and a truncated decision parses exactly like a complete one — just shorter. That
/// is why the reason travels with the text instead of being dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// The message content, as returned.
    pub text: String,
    /// `choices[0].finish_reason`, when the server sent one.
    pub finish_reason: Option<String>,
    /// `usage.completion_tokens`, when the server reported it (probe_sl contract: cap check).
    pub completion_tokens: Option<u32>,
    /// `choices[0].stop_reason` (vLLM extension): the matched stop STRING when generation ended on a
    /// client-supplied `stop` sequence, `None` on EOS / when the server does not report it. Compact
    /// contracts require it to prove the server reached the record boundary (not EOS, not the cap).
    pub stop_reason: Option<String>,
}

impl Completion {
    /// Whether the server cut this completion at the token budget.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.finish_reason.as_deref() == Some(seam::FINISH_REASON_LENGTH)
    }
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
            model: None,
            max_tokens: DEFAULT_MAX_TOKENS,
            stop: None,
        }
    }

    /// Compact-record contracts: server-side stop sequences appended to the request body. The
    /// server ends generation at the first match and reports it in `choices[0].stop_reason`; the
    /// GPU does not keep producing the explanation (verified by server token accounting on the
    /// paper engine: generation_tokens delta == client completion_tokens sum, 4,000/4,000).
    #[must_use]
    pub fn with_stop(mut self, stop: Vec<String>) -> Self {
        self.stop = if stop.is_empty() { None } else { Some(stop) };
        self
    }

    /// The stop sequences this client sends, if any.
    pub fn stop(&self) -> Option<&[String]> {
        self.stop.as_deref()
    }

    /// Set the request cap (the identified paper lane uses the engine's frozen 128).
    #[must_use]
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// The request cap this client sends.
    pub fn max_tokens(&self) -> u32 {
        self.max_tokens
    }

    /// Address every request to the served model/adapter `id` (`"model"` field).
    #[must_use]
    pub fn with_model(mut self, id: impl Into<String>) -> Self {
        self.model = Some(id.into());
        self
    }

    /// The served model id this client addresses, if any.
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Ask the server (`GET /v1/models`) what it serves and refuse unless every `expected` id is
    /// present. Returns the full served list for the session ledger. Fail-closed: transport
    /// problems refuse too; nothing is assumed from configuration.
    pub fn verify_served_models(&self, expected: &[&str]) -> Result<Vec<ServedModel>, IdentityRefusal> {
        let resp = self
            .agent
            .get(&format!("{}/v1/models", self.endpoint))
            .call()
            .map_err(|e| IdentityRefusal::Transport(e.to_string()))?;
        if resp.status() != 200 {
            return Err(IdentityRefusal::Transport(format!("status {}", resp.status())));
        }
        let v: serde_json::Value = resp
            .into_json()
            .map_err(|e| IdentityRefusal::Transport(format!("bad json: {e}")))?;
        let served: Vec<ServedModel> = v["data"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| {
                        Some(ServedModel {
                            id: m["id"].as_str()?.to_string(),
                            root: m["root"].as_str().map(str::to_string),
                            parent: m["parent"].as_str().map(str::to_string),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let missing: Vec<String> = expected
            .iter()
            .filter(|e| !served.iter().any(|s| s.id == **e))
            .map(|e| e.to_string())
            .collect();
        if missing.is_empty() {
            Ok(served)
        } else {
            Err(IdentityRefusal::Missing { missing, served: served.into_iter().map(|s| s.id).collect() })
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
        self.complete_with_meta(system, user).map(|c| c.text)
    }

    /// [`complete`](Self::complete) **plus the server's own `finish_reason`** (F5b).
    ///
    /// The reason used to be discarded here, which made the one failure the caller cannot see
    /// from the text — the server cutting the completion at `max_tokens`
    /// (`finish_reason="length"`) — invisible: a truncated decision parses as a complete one,
    /// just shorter. Callers that care (the decision path) record it on the drift ledger.
    pub fn complete_with_meta(
        &self,
        system: &str,
        user: &str,
    ) -> Result<Completion, InferenceError> {
        self.complete_with_meta_timeout(system, user, self.timeout)
    }

    /// [`complete_with_meta`](Self::complete_with_meta) with a PER-REQUEST transport timeout.
    ///
    /// Request-lifecycle v2: the caller passes the request's remaining authority budget plus a
    /// bounded cancellation lag. On expiry `ureq` closes the socket; the pinned vLLM server runs
    /// non-streaming chat completions under `with_cancellation`, which cancels the handler task on
    /// the HTTP disconnect and aborts the engine request — so request-owned server work is released
    /// instead of generating an answer nobody may act on. The error is `Transport("timeout …")`;
    /// callers classify it by [`InferenceError::is_timeout`].
    pub fn complete_with_meta_timeout(
        &self,
        system: &str,
        user: &str,
        timeout: Duration,
    ) -> Result<Completion, InferenceError> {
        self.complete_with_meta_timeout_id(system, user, timeout, None)
    }

    /// As [`complete_with_meta_timeout`](Self::complete_with_meta_timeout), optionally tagging the
    /// HTTP request with `X-Request-Id` (vLLM adopts it as the engine request id), so a server-side
    /// abort can be matched to THIS engine request rather than inferred from a closed socket.
    ///
    /// `ureq` 2.12 `Request::timeout` is an ABSOLUTE deadline (`DeadlineStream`): it bounds the whole
    /// operation — connect, send, headers and body — and does not restart per read.
    pub fn complete_with_meta_timeout_id(
        &self,
        system: &str,
        user: &str,
        timeout: Duration,
        request_id: Option<&str>,
    ) -> Result<Completion, InferenceError> {
        let mut body = request_body_with(self.model.as_deref(), self.max_tokens, system, user);
        if let Some(stop) = &self.stop {
            body["stop"] = serde_json::json!(stop);
        }

        let mut req = self
            .agent
            .post(&format!("{}/v1/chat/completions", self.endpoint))
            .set("Content-Type", "application/json")
            .timeout(timeout);
        if let Some(rid) = request_id {
            req = req.set("X-Request-Id", rid);
        }
        let resp = req
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
        // A 200 with a body that is not JSON is a CONTRACT failure (the server
        // answered but the response is malformed), not a transient transport one —
        // so it is Unparseable, and a retry would not change it.
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|_| InferenceError::Unparseable(text.clone()))?;
        let content = v["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| InferenceError::Unparseable(text))?;
        Ok(Completion {
            text: content,
            finish_reason: v["choices"][0]["finish_reason"]
                .as_str()
                .map(str::to_string),
            completion_tokens: v["usage"]["completion_tokens"].as_u64().and_then(|n| u32::try_from(n).ok()),
            stop_reason: v["choices"][0]["stop_reason"].as_str().map(str::to_string),
        })
    }

    /// [`complete`](Self::complete) with transport retry. Transient failures are
    /// retried (short backoff) until `deadline` is spent; a malformed completion is
    /// returned immediately, because it is a contract failure, not a transient one.
    /// The loop stops the instant the deadline elapses or a decision (or a
    /// parse-refusal) is produced.
    pub fn complete_retrying(
        &self,
        system: &str,
        user: &str,
        deadline: Duration,
    ) -> Result<String, InferenceError> {
        let start = Instant::now();
        loop {
            match self.complete(system, user) {
                Ok(text) => return Ok(text),
                Err(InferenceError::Unparseable(d)) => {
                    return Err(InferenceError::Unparseable(d));
                }
                Err(InferenceError::Transport(d)) => {
                    if start.elapsed() >= deadline {
                        return Err(InferenceError::Transport(d));
                    }
                    std::thread::sleep(RETRY_BACKOFF);
                }
            }
        }
    }

    /// [`complete_streaming`](Self::complete_streaming) with the same transport-retry
    /// semantics as [`complete_retrying`](Self::complete_retrying): transient failures
    /// retried until `deadline`, a parse-refusal (or any returned decision) returned
    /// immediately.
    pub fn complete_streaming_retrying(
        &self,
        system: &str,
        user: &str,
        deadline: Duration,
    ) -> Result<StreamedDecision, InferenceError> {
        let start = Instant::now();
        loop {
            match self.complete_streaming(system, user) {
                Ok(sd) => return Ok(sd),
                Err(InferenceError::Unparseable(d)) => {
                    return Err(InferenceError::Unparseable(d));
                }
                Err(InferenceError::Transport(d)) => {
                    if start.elapsed() >= deadline {
                        return Err(InferenceError::Transport(d));
                    }
                    std::thread::sleep(RETRY_BACKOFF);
                }
            }
        }
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
        let mut body = request_body_with(self.model.as_deref(), self.max_tokens, system, user);
        body["stream"] = serde_json::json!(true);
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
            finish_reason: None,
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
                SseEvent::Done(reason) => {
                    // Ended before a usable headline: NoDecisionLine (or the more
                    // specific kind already recorded) is the honest answer. The
                    // reason still travels, so a cut-at-max_tokens stream is not
                    // mistaken for a model that simply wrote nothing.
                    sd.finish_reason = reason;
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
    /// The stream ended: `[DONE]`, EOF, or a `finish_reason` frame — carrying the server's
    /// stop reason when it sent one (`Some("length")` = cut at `max_tokens`, F5b).
    Done(Option<String>),
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
        return SseEvent::Done(None);
    }
    match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(v) => {
            if let Some(reason) = v
                .pointer("/choices/0/finish_reason")
                .and_then(|f| f.as_str())
            {
                return SseEvent::Done(Some(reason.to_string()));
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
    /// The server's stop reason, once a `finish_reason` frame has been seen (F5b).
    finish_reason: Option<String>,
}

impl StreamedDecision {
    /// The server's `finish_reason`, once the stream has reported one.
    ///
    /// `Some("length")` means the completion was cut at `max_tokens` — the headline may still
    /// have arrived in time to act on, but the reasoning tail (and any field after the cut) is
    /// missing, which is otherwise invisible.
    #[must_use]
    pub fn finish_reason(&self) -> Option<&str> {
        self.finish_reason.as_deref()
    }

    /// Whether the server cut this completion at the token budget (F5b).
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.finish_reason.as_deref() == Some(seam::FINISH_REASON_LENGTH)
    }

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
                SseEvent::Done(reason) => {
                    if reason.is_some() {
                        self.finish_reason = reason;
                    }
                    self.reader = None;
                }
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
                return Ok(SseEvent::Done(None));
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                continue;
            }
            match sse_event(line) {
                SseEvent::Ignore => continue,
                ev @ (SseEvent::Delta(_) | SseEvent::Done(_)) => return Ok(ev),
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

#[cfg(test)]
mod retry_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn is_transport_distinguishes_transient_from_contract_failures() {
        assert!(InferenceError::Transport("up".into()).is_transport());
        assert!(!InferenceError::Unparseable("no".into()).is_transport());
    }

    /// A 200 whose body is not JSON is a CONTRACT failure (Unparseable), never a
    /// transient Transport one — so a retry loop must not re-issue it.
    #[test]
    fn a_malformed_200_body_is_unparseable_not_transport() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0_u8; 8192];
            let _ = conn.read(&mut buf);
            conn.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\ngarbage!!!",
            )
            .unwrap();
        });
        let client =
            InferenceClient::new(format!("http://127.0.0.1:{port}"), Duration::from_secs(5));
        let err = client.complete("SYSTEM", "USER").unwrap_err();
        assert!(matches!(err, InferenceError::Unparseable(_)), "got {err:?}");
        handle.join().unwrap();
    }

    /// A transient 500 is retried until the server succeeds, within the deadline.
    #[test]
    fn transport_failure_is_retried_until_success() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            // First request: a transient 500.
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0_u8; 8192];
            let _ = conn.read(&mut buf);
            conn.write_all(
                b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .unwrap();

            // Second request: a clean decision.
            let (mut conn, _) = listener.accept().unwrap();
            let _ = conn.read(&mut buf);
            let body = serde_json::json!({"choices":[{"message":{"content":"DECISION: BUY\nSIZE: FULL"}}]});
            let body = body.to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            conn.write_all(resp.as_bytes()).unwrap();
        });
        let client =
            InferenceClient::new(format!("http://127.0.0.1:{port}"), Duration::from_secs(5));
        let text = client
            .complete_retrying("SYSTEM", "USER", Duration::from_secs(2))
            .unwrap();
        assert!(text.contains("DECISION: BUY"));
        handle.join().unwrap();
    }
}

/// F5b: the truncation signal must survive the SSE/JSON boundary — the reason used to be
/// dropped on the floor, which made a `max_tokens` cut indistinguishable from a finished stop.
#[cfg(test)]
mod f5b_tests {
    use super::*;

    #[test]
    fn a_finish_reason_frame_carries_the_reason_and_a_done_marker_does_not() {
        match sse_event("data: {\"choices\":[{\"delta\":{\"content\":\"DECISION: BUY\"}}]}") {
            SseEvent::Delta(d) => assert_eq!(d, "DECISION: BUY"),
            _ => panic!("a content delta must be a Delta"),
        }
        match sse_event("data: {\"choices\":[{\"finish_reason\":\"length\"}]}") {
            SseEvent::Done(Some(r)) => assert_eq!(r, "length"),
            _ => panic!("the stop reason must travel with the Done"),
        }
        match sse_event("data: [DONE]") {
            SseEvent::Done(None) => {}
            _ => panic!("[DONE] is a reasonless stop"),
        }
    }

    #[test]
    fn a_completion_knows_whether_it_was_cut_at_the_token_budget() {
        let cut = Completion {
            stop_reason: None,
            text: "DECISION: BUY".into(),
            finish_reason: Some("length".into()),
            completion_tokens: None,
        };
        assert!(cut.truncated());
        assert!(!Completion {
            text: "x".into(),
            finish_reason: Some("stop".into()),
            completion_tokens: None,
            stop_reason: None,
        }
        .truncated());
        assert!(!Completion {
            text: "x".into(),
            completion_tokens: None,
            finish_reason: None,
            stop_reason: None,
        }
        .truncated());
    }
}

#[cfg(test)]
mod served_identity_tests {
    use super::*;

    #[test]
    fn request_body_without_model_is_byte_identical_to_historical() {
        let a = request_body("s", "u");
        let b = request_body_for(None, "s", "u");
        assert_eq!(a, b);
        assert!(a.get("model").is_none());
    }

    #[test]
    fn request_body_with_model_adds_only_the_model_field() {
        let a = request_body("s", "u");
        let mut b = request_body_for(Some("probe-mgmt"), "s", "u");
        assert_eq!(b["model"], "probe-mgmt");
        b.as_object_mut().unwrap().remove("model");
        assert_eq!(a, b, "nothing but the model field may change (served-prompt parity)");
    }

    #[test]
    fn client_model_round_trips() {
        let c = InferenceClient::new("http://127.0.0.1:1", Duration::from_millis(50));
        assert_eq!(c.model(), None);
        let c = c.with_model("entry-a");
        assert_eq!(c.model(), Some("entry-a"));
    }

    #[test]
    fn verify_served_models_refuses_on_transport() {
        let c = InferenceClient::new("http://127.0.0.1:9", Duration::from_millis(200));
        match c.verify_served_models(&["x"]) {
            Err(IdentityRefusal::Transport(_)) => {}
            other => panic!("expected transport refusal, got {other:?}"),
        }
    }

    #[test]
    fn verify_served_models_checks_every_expected_id() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let body = r#"{"object":"list","data":[{"id":"probe_step78","object":"model","root":"/ckpt/probe_step78_hf","parent":null},{"id":"probe-mg3-mgmt","object":"model","root":"/adapters/mgmt","parent":"probe_step78"}]}"#;
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut s, _) = listener.accept().unwrap();
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf);
                let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                s.write_all(resp.as_bytes()).unwrap();
            }
        });
        let c = InferenceClient::new(format!("http://{addr}"), Duration::from_secs(2));
        let served = c.verify_served_models(&["probe_step78", "probe-mg3-mgmt"]).expect("both served");
        assert_eq!(served.len(), 2);
        assert_eq!(served[1].parent.as_deref(), Some("probe_step78"));
        match c.verify_served_models(&["probe-mg3-mgmt", "probe-mg3-entry"]) {
            Err(IdentityRefusal::Missing { missing, served }) => {
                assert_eq!(missing, vec!["probe-mg3-entry".to_string()]);
                assert_eq!(served.len(), 2);
            }
            other => panic!("expected Missing, got {other:?}"),
        }
        server.join().unwrap();
    }
}

#[cfg(test)]
mod max_tokens_tests {
    use super::*;
    #[test]
    fn default_cap_is_historical_512_and_lane_cap_is_explicit() {
        let c = InferenceClient::new("http://127.0.0.1:1", Duration::from_millis(50));
        assert_eq!(c.max_tokens(), 512);
        assert_eq!(request_body("s", "u")["max_tokens"], 512);
        let c = c.with_max_tokens(128).with_model("probe-mg3-base");
        assert_eq!(c.max_tokens(), 128);
        let b = request_body_with(c.model(), c.max_tokens(), "s", "u");
        assert_eq!(b["max_tokens"], 128);
        assert_eq!(b["model"], "probe-mg3-base");
        assert_eq!(b["temperature"], 0.0);
        assert_eq!(b["chat_template_kwargs"]["enable_thinking"], false);
    }
}

#[cfg(test)]
mod compact_stop_body_tests {
    use super::*;

    #[test]
    fn stop_sequences_are_sent_only_when_configured_and_stop_reason_is_parsed() {
        let c = InferenceClient::new("http://127.0.0.1:1", Duration::from_secs(1)).with_model("m").with_max_tokens(128);
        assert!(c.stop().is_none());
        let c2 = c.with_stop(vec!["\nREASON".to_string()]);
        assert_eq!(c2.stop(), Some(&["\nREASON".to_string()][..]));
        // body parity: everything except the stop list is byte-identical to the long-record body
        let mut body = request_body_with(Some("m"), 128, "s", "u");
        let long = body.clone();
        body["stop"] = serde_json::json!(c2.stop().unwrap());
        let mut without = body.clone();
        without.as_object_mut().unwrap().remove("stop");
        assert_eq!(without, long);
        // server response parsing carries stop_reason (vLLM) and None when absent
        let v: serde_json::Value = serde_json::from_str(r#"{"choices":[{"message":{"content":"DECISION: SKIP"},"finish_reason":"stop","stop_reason":"\nREASON"}],"usage":{"completion_tokens":7}}"#).unwrap();
        assert_eq!(v["choices"][0]["stop_reason"].as_str(), Some("\nREASON"));
        let v2: serde_json::Value = serde_json::from_str(r#"{"choices":[{"message":{"content":"x"},"finish_reason":"stop"}],"usage":{"completion_tokens":7}}"#).unwrap();
        assert!(v2["choices"][0]["stop_reason"].as_str().is_none());
    }
}
