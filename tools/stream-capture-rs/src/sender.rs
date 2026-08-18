//! Helius Sender submission transport.
//!
//! Sender fans one transaction to Helius staked connections and the Jito block
//! engine simultaneously. This module owns the wire format only: URL assembly,
//! request construction, response parsing, and the safety checks that belong at
//! the boundary. It does **not** build, sign, or size transactions — the tier and
//! tip decision is `pump_quant_execution::ex_sender_route`, and signing does not
//! exist in this workspace yet by design.
//!
//! ## Dependencies: none added
//! Every byte here is std plus the suite's existing [`Transport`] seam. No
//! serde, no tokio, no reqwest. The JSON is hand-rolled in the same style as
//! [`crate::json`], because the request shape is fixed and tiny and the response
//! has exactly two forms. This matters concretely: the crate's dependency tree
//! is vendored through an uncommitted source replacement, so a new dependency
//! would not resolve on the build box.
//!
//! ## Testability
//! [`SenderClient`] takes `&dyn Transport`, so the whole path — URL, body,
//! parsing, error classification — is exercised against a mock with no socket.
//! The production transport is the suite's `UreqTransport`.
//!
//! ## §22
//! No floats. Latency is integer microseconds from the transport, converted to
//! integer milliseconds once, at the boundary.

use crate::rpc::{redact_url, Reply, Transport};

/// Global endpoint. Auto-routes to the nearest region and speaks TLS.
pub const GLOBAL_ENDPOINT: &str = "https://sender.helius-rpc.com/fast";

/// Maximum transactions in one `sendBundle` call (Sender's documented cap).
pub const MAX_BUNDLE_TXS: usize = 4;

/// Maximum accepted length of a base64 transaction. A Solana packet is 1232
/// bytes, which is 1644 base64 characters; the slack covers padding and a
/// bundle's largest member without admitting an unbounded body.
pub const MAX_TX_BASE64_LEN: usize = 2048;

/// What went wrong with a submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SenderError {
    /// The endpoint URL was rejected before any request was made.
    BadEndpoint(String),
    /// The transaction payload failed its boundary checks.
    BadPayload(String),
    /// Transport failure. The URL in this string is already redacted.
    Transport(String),
    /// The endpoint returned a JSON-RPC error object.
    Rpc { code: i64, message: String },
    /// A 200 response that contained neither a result nor an error.
    Unparseable(String),
}

impl core::fmt::Display for SenderError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadEndpoint(m) => write!(f, "sender endpoint rejected: {m}"),
            Self::BadPayload(m) => write!(f, "sender payload rejected: {m}"),
            Self::Transport(m) => write!(f, "sender transport failed: {m}"),
            Self::Rpc { code, message } => write!(f, "sender rpc error {code}: {message}"),
            Self::Unparseable(m) => write!(f, "sender reply unparseable: {m}"),
        }
    }
}

/// A Sender endpoint plus its routing options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderEndpoint {
    base: String,
    swqos_only: bool,
    mev_protect: bool,
    /// Optional Helius API key appended as ?api-key=... for the Sender endpoint.
    /// Required for the global (non-colocated) endpoint to authenticate.
    api_key: Option<String>,
}

impl SenderEndpoint {
    /// Build an endpoint, refusing plaintext HTTP.
    ///
    /// # Why plaintext is refused by default
    /// Sender's regional endpoints are `http://` and are intended for callers
    /// inside the datacentre. A signed transaction sent in the clear over the
    /// public internet is readable by any on-path observer *before* it lands,
    /// which for a memecoin entry is a free front-run against us. The global
    /// endpoint is TLS and its handshake amortises across a reused connection.
    ///
    /// Use [`Self::new_allow_plaintext`] only when genuinely colocated, and say
    /// so in the config where the operator can see it.
    pub fn new(base: &str, swqos_only: bool, mev_protect: bool) -> Result<Self, SenderError> {
        if !base.starts_with("https://") {
            return Err(SenderError::BadEndpoint(format!(
                "{} is not https; a signed transaction in plaintext can be front-run before it lands. \
                 Use new_allow_plaintext only when colocated.",
                redact_url(base)
            )));
        }
        Self::new_allow_plaintext(base, swqos_only, mev_protect)
    }

    /// Build an endpoint without the TLS requirement. Colocated callers only.
    pub fn new_allow_plaintext(
        base: &str,
        swqos_only: bool,
        mev_protect: bool,
    ) -> Result<Self, SenderError> {
        let trimmed = base.trim();
        if trimmed.is_empty() {
            return Err(SenderError::BadEndpoint("endpoint is empty".to_string()));
        }
        if trimmed.contains('?') || trimmed.contains('#') {
            return Err(SenderError::BadEndpoint(
                "endpoint must not carry a query string or fragment; routing options are set here"
                    .to_string(),
            ));
        }
        if !(trimmed.starts_with("https://") || trimmed.starts_with("http://")) {
            return Err(SenderError::BadEndpoint(
                "endpoint must be an absolute http(s) URL".to_string(),
            ));
        }
        Ok(Self {
            base: trimmed.trim_end_matches('/').to_string(),
            swqos_only,
            mev_protect,
            api_key: None,
        })
    }

    /// The full URL including routing query parameters.
    ///
    /// Mirrors `ex_sender_route::query_suffix`. The two are asserted equal in
    /// this module's tests so the policy and the wire cannot drift apart.
    #[must_use]
    pub fn url(&self) -> String {
        let suffix = match (self.swqos_only, self.mev_protect) {
            (true, false) => "?swqos_only=true",
            (true, true) => "?swqos_only=true&mev-protect=true",
            (false, false) => "",
            (false, true) => "?mev-protect=true",
        };
        let base_with_suffix = format!("{}{}", self.base, suffix);
        match &self.api_key {
            Some(key) if !key.is_empty() => {
                if suffix.is_empty() {
                    format!("{}?api-key={}", self.base, key)
                } else {
                    format!("{}&api-key={}", base_with_suffix, key)
                }
            }
            _ => base_with_suffix,
        }
    }

    /// Set the Helius API key for Sender authentication (global endpoint only).
    /// The key is appended as `?api-key=...` in the request URL.
    #[must_use]
    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// Whether this endpoint requests the SWQoS-only single fast path.
    #[must_use]
    pub const fn swqos_only(&self) -> bool {
        self.swqos_only
    }

    /// Whether this endpoint requests sandwich-resistant validator routing.
    #[must_use]
    pub const fn mev_protect(&self) -> bool {
        self.mev_protect
    }
}

/// A submitted transaction that the endpoint accepted.
///
/// **Accepted is not landed.** The signature here means Sender took the
/// transaction, nothing more. Confirmation is a separate observation, and
/// recording an acceptance as a success is how a route with a healthy API and
/// poor inclusion comes to look good — see
/// `pump_quant_execution::ex_route_health::Attempt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    /// Transaction signature returned by the endpoint.
    pub signature: String,
    /// Round-trip latency of the submission call, integer milliseconds.
    pub submit_latency_ms: u64,
}

/// Reject anything that is not plain base64 before it reaches the wire.
///
/// The request body is assembled by string concatenation, so this is also the
/// injection guard: a payload containing `"` or `\` could otherwise reshape the
/// JSON. Base64 has none of those characters, so rejecting non-base64 closes it
/// completely rather than escaping around it.
fn validate_base64_tx(tx_base64: &str) -> Result<(), SenderError> {
    if tx_base64.is_empty() {
        return Err(SenderError::BadPayload("transaction is empty".to_string()));
    }
    if tx_base64.len() > MAX_TX_BASE64_LEN {
        return Err(SenderError::BadPayload(format!(
            "transaction is {} base64 chars, over the {MAX_TX_BASE64_LEN} cap",
            tx_base64.len()
        )));
    }
    let ok = tx_base64
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || c == b'+' || c == b'/' || c == b'=');
    if !ok {
        return Err(SenderError::BadPayload(
            "transaction is not plain base64".to_string(),
        ));
    }
    Ok(())
}

/// Request ids are echoed back by the endpoint and must not reshape the JSON.
fn validate_id(id: &str) -> Result<(), SenderError> {
    if id.is_empty() || id.len() > 64 {
        return Err(SenderError::BadPayload(
            "request id must be 1..=64 characters".to_string(),
        ));
    }
    if !id
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err(SenderError::BadPayload(
            "request id must be alphanumeric, '-' or '_'".to_string(),
        ));
    }
    Ok(())
}

/// Build the `sendTransaction` request body.
///
/// `skip_preflight` controls the `skipPreflight` RPC parameter:
/// - **true** (buys): preflight simulation is skipped to save a round trip on
///   the latency-critical entry path. Speed is paramount — a missed buy is a
///   missed opportunity, and the confirmation feedback loop (Rev-19) catches
///   failed buys asynchronously.
/// - **false** (sells): preflight simulation runs so that sell txs that would
///   fail on-chain (e.g. slippage exceeded, token account missing, curve
///   complete) are rejected BEFORE submission. This prevents silent on-chain
///   failures from consuming a blockhash slot and burning fees on a tx that
///   was never going to land. Sells are less latency-critical than buys
///   because the position is already held — a 50ms preflight is acceptable
///   insurance against losing the exit entirely.
///
/// `maxRetries: 0` because Sender owns retry across its own routing pathways —
/// client-side retry would submit a second transaction competing with the first.
pub fn build_send_body(id: &str, tx_base64: &str, skip_preflight: bool) -> Result<String, SenderError> {
    validate_id(id)?;
    validate_base64_tx(tx_base64)?;
    Ok(format!(
        r#"{{"id":"{id}","jsonrpc":"2.0","method":"sendTransaction","params":["{tx_base64}",{{"encoding":"base64","skipPreflight":{skip_preflight},"maxRetries":0}}]}}"#
    ))
}

/// Build a `sendBundle` request body for atomic all-or-nothing execution.
///
/// At least one transaction in the bundle must carry the tip; that is the
/// caller's responsibility because this module never inspects instructions.
pub fn build_bundle_body(id: &str, txs_base64: &[&str]) -> Result<String, SenderError> {
    validate_id(id)?;
    if txs_base64.is_empty() {
        return Err(SenderError::BadPayload("bundle is empty".to_string()));
    }
    if txs_base64.len() > MAX_BUNDLE_TXS {
        return Err(SenderError::BadPayload(format!(
            "bundle has {} transactions, over the {MAX_BUNDLE_TXS} cap",
            txs_base64.len()
        )));
    }
    for tx in txs_base64 {
        validate_base64_tx(tx)?;
    }
    let joined = txs_base64
        .iter()
        .map(|t| format!("\"{t}\""))
        .collect::<Vec<_>>()
        .join(",");
    Ok(format!(
        r#"{{"id":"{id}","jsonrpc":"2.0","method":"sendBundle","params":[[{joined}],{{"encoding":"base64"}}]}}"#
    ))
}

/// Extract the string value of a top-level `"key":"value"` pair.
fn extract_str(body: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let start = body.find(&needle)? + needle.len();
    let rest = body.get(start..)?;
    let colon = rest.find(':')? + 1;
    let after = rest.get(colon..)?.trim_start();
    let after = after.strip_prefix('"')?;
    let end = after.find('"')?;
    Some(after[..end].to_string())
}

/// Extract an integer value for a top-level `"key":<number>` pair.
fn extract_i64(body: &str, key: &str) -> Option<i64> {
    let needle = format!("\"{key}\"");
    let start = body.find(&needle)? + needle.len();
    let rest = body.get(start..)?;
    let colon = rest.find(':')? + 1;
    let after = rest.get(colon..)?.trim_start();
    let end = after
        .find(|c: char| !(c.is_ascii_digit() || c == '-'))
        .unwrap_or(after.len());
    after[..end].parse::<i64>().ok()
}

/// Parse a Sender reply into a signature or a classified error.
///
/// `"error"` is checked **first**. JSON-RPC forbids both members being present,
/// and checking result first would let an error whose message happened to
/// contain `"result":"` be read as success — a parser that fails open on
/// malformed input is the wrong kind of parser for a submission path.
pub fn parse_send_reply(body: &str) -> Result<String, SenderError> {
    if body.contains("\"error\"") {
        let code = extract_i64(body, "code").unwrap_or(0);
        let message = extract_str(body, "message")
            .unwrap_or_else(|| "no message field in error object".to_string());
        return Err(SenderError::Rpc { code, message });
    }
    match extract_str(body, "result") {
        Some(sig) if !sig.is_empty() => Ok(sig),
        _ => Err(SenderError::Unparseable(
            "reply contained neither an error object nor a non-empty result".to_string(),
        )),
    }
}

/// Submits transactions to a Helius Sender endpoint.
pub struct SenderClient<'a> {
    transport: &'a dyn Transport,
    endpoint: SenderEndpoint,
}

impl<'a> SenderClient<'a> {
    /// Bind a transport to an endpoint.
    #[must_use]
    pub fn new(transport: &'a dyn Transport, endpoint: SenderEndpoint) -> Self {
        Self {
            transport,
            endpoint,
        }
    }

    /// The endpoint this client submits to.
    #[must_use]
    pub const fn endpoint(&self) -> &SenderEndpoint {
        &self.endpoint
    }

    /// Submit one signed, base64-encoded transaction.
    ///
    /// The caller is responsible for the two instructions Sender requires in
    /// every transaction: a tip transfer to one of the tip accounts, and a
    /// compute-unit-price instruction. This module cannot verify either without
    /// decoding the transaction, and a check that only *sometimes* runs is worse
    /// than an explicit contract.
    pub fn send_transaction(&self, id: &str, tx_base64: &str, skip_preflight: bool) -> Result<Accepted, SenderError> {
        let body = build_send_body(id, tx_base64, skip_preflight)?;
        // Diagnostic: log the tx payload size being submitted.
        eprintln!("[sender] send_transaction: id={id}, tx_b64_len={}, skip_preflight={skip_preflight}", tx_base64.len());
        self.post(&body)
    }

    /// Submit an atomic bundle of up to [`MAX_BUNDLE_TXS`] transactions.
    pub fn send_bundle(&self, id: &str, txs_base64: &[&str]) -> Result<Accepted, SenderError> {
        let body = build_bundle_body(id, txs_base64)?;
        self.post(&body)
    }

    fn post(&self, body: &str) -> Result<Accepted, SenderError> {
        let url = self.endpoint.url();
        let Reply { body, latency_us } = self
            .transport
            .post_json(&url, body)
            // The URL carries no credential today, but redacting unconditionally
            // means that stays true if an api-key is ever appended for a raised
            // TPS limit. Redaction you have to remember to add is redaction that
            // gets forgotten.
            .map_err(|e| SenderError::Transport(format!("{}: {e}", redact_url(&url))))?;
        // Diagnostic: log the full Sender response for debugging.
        eprintln!("[sender] POST response ({} bytes): {body}", body.len());
        let signature = parse_send_reply(&body)?;
        Ok(Accepted {
            signature,
            submit_latency_ms: latency_us / 1_000,
        })
    }
}
