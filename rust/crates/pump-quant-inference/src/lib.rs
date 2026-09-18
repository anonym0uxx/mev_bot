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
    management_fraction_bps, parse_decision_payload, resolve_entry_clip_lamports, route, Decision,
    DriftLedger, OffContract, PayloadError, Route, SizeError, SizeTier, DEPLOY_LAMPORTS_CANONICAL,
    FEE_BUFFER_LAMPORTS, MIN_CLIP_LAMPORTS,
};

/// A completion that failed the trained-format contract converts to the client's error type
/// without losing the cause: the caller records [`PayloadError::kind`] on a [`DriftLedger`]
/// before it decides whether to log, alarm, or stay quiet.
impl From<PayloadError> for InferenceError {
    fn from(e: PayloadError) -> Self {
        InferenceError::Unparseable(e.to_string())
    }
}

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
        let body = serde_json::json!({
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
            "temperature": 0.0,
            "max_tokens": 512,
            "stream": false,
        });

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
        let v: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| InferenceError::Transport(e.to_string()))?;
        v["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| InferenceError::Unparseable(text))
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
