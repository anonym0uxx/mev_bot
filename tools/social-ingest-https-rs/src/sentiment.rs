//! `sentiment-enrich` subcommand — the BRAIN seam: LLM sentiment annotation
//! for the normalized social NDJSON stream, strictly OFF the deterministic
//! hot path.
//!
//! This is a stream FILTER, not a capture lane and not a decision-maker:
//! NDJSON in on stdin → the SAME lines out on stdout, with three fields
//! spliced in before the closing brace when a local llama.cpp server answers
//! in time — `"sentiment_bp"` (0–10000, 5000 = neutral), `"sentiment_conf_bp"`
//! (0–10000) and `"sentiment_model"` (provenance tag). The original line bytes
//! are preserved verbatim otherwise: the filter never re-serializes, never
//! blocks, never drops, never reorders.
//!
//! # Constitution discipline (binding)
//! * **The LLM is never a fact source (§65 crit. 8: "LLM output cannot enter
//!   factual state").** Its output here is an ENRICHMENT annotation with
//!   provenance (`sentiment_model`), recorded at the seam as an INPUT so
//!   replays are byte-identical. Downstream it is corroboration-tier evidence
//!   at most — integers the deterministic core may weigh, never truth it may
//!   cite (§6.5-spirit: a research artifact is never cast into canonical
//!   truth; sentiment annotates social observations only).
//! * **§6.4 fail-open as ABSENCE.** Server unreachable, timeout, non-JSON,
//!   out-of-range → the line passes through UNCHANGED. Unknown stays UNKNOWN;
//!   absent sentiment is never coerced to neutral, and enrichment absence
//!   never blocks capture. `--require` inverts this for supervised runs:
//!   failures exit loudly instead of degrading silently.
//! * **§22 determinism.** `--replay <responses.json>` substitutes a fixture
//!   array for the network — a pure function of stdin + fixture, byte-stable.
//!   `--passthrough` is the identity filter (pipeline stub). The only clock
//!   in this module is a MONOTONIC latency stopwatch whose readings go to
//!   stderr diagnostics exclusively — nothing timed ever reaches stdout.
//! * **§67 removable filter.** Delete this stage from the pipeline
//!   (`capture | sentiment-enrich | core`) and the stream still flows — the
//!   core sees the same events, merely without the optional annotation.
//!
//! The llama.cpp `/completion` request pins `temperature 0`, a fixed `seed`,
//! a small `n_predict` and a `json_schema` grammar constraint, so the model
//! physically cannot emit anything but the two bounded integers.

use std::io::{BufRead, Read, Write};
use std::time::Instant;

use crate::json::{self, Value};
use crate::{emit, http};

/// Default llama.cpp server base URL (`LLAMA_SERVER_URL` overrides) — matches
/// the supervisor's `llama_server.yaml` endpoint block.
pub const DEFAULT_SERVER_URL: &str = "http://127.0.0.1:8080";

/// Default provenance tag (`LLAMA_MODEL_ID` overrides). The tag is recorded
/// on every enriched line so downstream can attribute — and discount — each
/// annotation per model version.
pub const DEFAULT_MODEL_ID: &str = "local-llm-v0";

/// Hard per-request budget (connect AND read): the filter may add at most
/// this much latency per line before failing open as absence.
pub const TIMEOUT_SECS: u64 = 5;

/// Input line-size cap. A line larger than this is passed through untouched
/// (streamed, never buffered whole) and never sent to the model.
pub const MAX_LINE_BYTES: usize = 64 * 1024;

/// Degradation-counter summary cadence (stderr, every N input lines).
pub const SUMMARY_EVERY: u64 = 100;

/// Failure-warning rate limit: warn on the first failure, then every Nth.
pub const WARN_EVERY: u64 = 25;

/// The grammar constraint sent as llama.cpp's `json_schema` field: the model
/// CANNOT emit anything but this two-integer object (server-side GBNF
/// constrained decoding; the supervisor's config already requires
/// `json_schema_support: true`).
pub const RESPONSE_SCHEMA: &str = "{\"type\":\"object\",\"properties\":{\
     \"sentiment_bp\":{\"type\":\"integer\",\"minimum\":0,\"maximum\":10000},\
     \"confidence_bp\":{\"type\":\"integer\",\"minimum\":0,\"maximum\":10000}},\
     \"required\":[\"sentiment_bp\",\"confidence_bp\"]}";

// ------------------------------------------------------------------ sentiment

/// One validated sentiment annotation. Basis points fit `u16` (0..=10000);
/// out-of-range or non-integer server output NEVER constructs this type —
/// it is rejected upstream and the line passes through unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sentiment {
    /// 0 = maximally bearish (scam accusation), 5000 = neutral, 10000 = max
    /// bullish.
    pub sentiment_bp: u16,
    /// Model self-reported confidence, 0..=10000.
    pub confidence_bp: u16,
}

/// Validate a `{"sentiment_bp":..,"confidence_bp":..}` object. Strict:
/// integers only (floats, strings, booleans rejected), 0..=10000 only —
/// a clamp would manufacture certainty from garbage, so garbage is absence.
pub fn sentiment_from_value(v: &Value) -> Result<Sentiment, String> {
    let field = |key: &str| -> Result<u16, String> {
        match v.get(key) {
            Some(Value::Number(raw)) => {
                let n: u64 = raw
                    .parse()
                    .map_err(|_| format!("{key} is not a non-negative integer: {raw}"))?;
                if n > 10_000 {
                    return Err(format!("{key} out of range: {n}"));
                }
                Ok(n as u16)
            }
            Some(other) => Err(format!("{key} is not a number: {other:?}")),
            None => Err(format!("missing {key}")),
        }
    };
    Ok(Sentiment {
        sentiment_bp: field("sentiment_bp")?,
        confidence_bp: field("confidence_bp")?,
    })
}

/// Parse a llama.cpp `/completion` response body → validated [`Sentiment`].
/// The generated text arrives in `"content"`; it must itself be the exact
/// schema-constrained JSON object.
pub fn parse_response(body: &str) -> Result<Sentiment, String> {
    let v = json::parse(body).map_err(|e| format!("bad server JSON: {e}"))?;
    let content = v
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| "response has no \"content\" string".to_string())?;
    let inner =
        json::parse(content.trim()).map_err(|e| format!("model output is not JSON: {e}"))?;
    sentiment_from_value(&inner)
}

// --------------------------------------------------------------- the request

/// The strict classification prompt. The scale anchors are spelled out so a
/// small model cannot invent its own; the grammar constraint (not the prompt)
/// is what actually forbids free text.
#[must_use]
pub fn build_prompt(text: &str) -> String {
    format!(
        "You are a strict sentiment classifier for crypto-trading social posts.\n\
         Classify the sentiment of the POST toward the memecoin/token it mentions.\n\
         Respond with ONLY this JSON object and nothing else:\n\
         {{\"sentiment_bp\":<integer 0-10000>,\"confidence_bp\":<integer 0-10000>}}\n\
         Scale: 0 = maximally bearish (scam or rug accusation), 5000 = exactly\n\
         neutral, 10000 = maximally bullish. confidence_bp: 0 = pure guess,\n\
         10000 = certain.\n\
         POST:\n{text}\nJSON:"
    )
}

/// Build the llama.cpp `/completion` request body: temperature 0, fixed seed,
/// small `n_predict`, `cache_prompt` (the instruction prefix is shared across
/// every line — the server reuses its KV cache) and the [`RESPONSE_SCHEMA`]
/// grammar constraint. Hand-rolled with the audited escaper: hostile post
/// text (quotes, braces, newlines) cannot break out of the JSON string.
#[must_use]
pub fn build_request_body(text: &str) -> String {
    let prompt = build_prompt(text);
    let mut out = String::with_capacity(prompt.len() + RESPONSE_SCHEMA.len() + 96);
    out.push_str("{\"prompt\":\"");
    emit::escape_json_into(&prompt, &mut out);
    out.push_str("\",\"n_predict\":48,\"temperature\":0,\"seed\":7,\"cache_prompt\":true,");
    out.push_str("\"json_schema\":");
    out.push_str(RESPONSE_SCHEMA);
    out.push('}');
    out
}

// ---------------------------------------------------------------- the splice

/// Splice the three annotation fields into an NDJSON object line, immediately
/// before its closing brace. The original bytes are preserved verbatim (the
/// output is byte-prefix-identical up to the final `}`); trailing whitespace
/// or `\r` after the brace survives untouched. Returns `None` when the line
/// does not end in an object close — the caller then passes it through.
#[must_use]
pub fn splice(line: &str, s: Sentiment, model: &str) -> Option<String> {
    let close = line.rfind('}')?;
    if !line[close + 1..].bytes().all(|b| b.is_ascii_whitespace()) {
        return None; // junk after the close: not an object line, not ours
    }
    if !line.trim_start().starts_with('{') {
        return None;
    }
    let head = &line[..close];
    let mut out = String::with_capacity(line.len() + 80 + model.len());
    out.push_str(head);
    // `{}` (pathological but legal) takes no leading comma.
    if !head.trim_end().ends_with('{') {
        out.push(',');
    }
    out.push_str("\"sentiment_bp\":");
    out.push_str(&s.sentiment_bp.to_string());
    out.push_str(",\"sentiment_conf_bp\":");
    out.push_str(&s.confidence_bp.to_string());
    out.push_str(",\"sentiment_model\":\"");
    emit::escape_json_into(model, &mut out);
    out.push('"');
    out.push_str(&line[close..]);
    Some(out)
}

/// Extract the `"text"` field from a normalized event line. `None` (absent /
/// empty / not JSON) means the line is not enrichable — it passes through.
#[must_use]
pub fn extract_text(line: &str) -> Option<String> {
    let text = json::parse(line).ok()?.get("text")?.as_str()?.to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

// ---------------------------------------------------------- capped line read

/// Outcome of one capped stdin line read.
pub enum LineRead {
    /// Complete line within cap, newline stripped; `true` = had a newline.
    Line(bool),
    /// Line exceeds [`MAX_LINE_BYTES`]: `buf` holds the head; the caller must
    /// stream the tail through with [`stream_oversize_tail`].
    OversizeHead,
    /// End of input.
    Eof,
}

/// Read one line into `buf`, reading at most [`MAX_LINE_BYTES`] + 1 bytes —
/// an oversize line is detected without ever buffering it whole (§99-spirit
/// bounding, same discipline as the transport's response cap).
pub fn read_line_capped(input: &mut impl BufRead, buf: &mut Vec<u8>) -> std::io::Result<LineRead> {
    buf.clear();
    let n = input
        .by_ref()
        .take(MAX_LINE_BYTES as u64 + 1)
        .read_until(b'\n', buf)?;
    if n == 0 {
        return Ok(LineRead::Eof);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        return Ok(LineRead::Line(true));
    }
    if buf.len() <= MAX_LINE_BYTES {
        return Ok(LineRead::Line(false)); // final line, no trailing newline
    }
    Ok(LineRead::OversizeHead)
}

/// Stream the remainder of an oversize line (up to and including its newline)
/// straight to `out` in buffer-sized chunks — unchanged bytes, bounded
/// memory. Returns whether a newline terminated the line.
pub fn stream_oversize_tail(
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> std::io::Result<bool> {
    loop {
        let chunk = input.fill_buf()?;
        if chunk.is_empty() {
            return Ok(false);
        }
        match chunk.iter().position(|&b| b == b'\n') {
            Some(p) => {
                out.write_all(&chunk[..p])?;
                input.consume(p + 1);
                return Ok(true);
            }
            None => {
                let len = chunk.len();
                out.write_all(chunk)?;
                input.consume(len);
            }
        }
    }
}

// ------------------------------------------------------------------- the CLI

/// `sentiment-enrich` flags.
pub struct Cli {
    /// `--replay <responses.json>`: fixture array instead of the network
    /// (deterministic, §22).
    pub replay: Option<String>,
    /// `--passthrough`: identity filter — no server, no annotation.
    pub passthrough: bool,
    /// `--require`: enrichment failure exits loudly (code 1) instead of
    /// failing open — for supervised runs where silent degradation must not
    /// hide.
    pub require: bool,
}

impl Cli {
    /// Parse subcommand arguments.
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut cli = Self {
            replay: None,
            passthrough: false,
            require: false,
        };
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--replay" => {
                    cli.replay = Some(
                        it.next()
                            .cloned()
                            .ok_or_else(|| "--replay needs a value".to_string())?,
                    );
                }
                "--passthrough" => cli.passthrough = true,
                "--require" => cli.require = true,
                other => return Err(format!("unknown flag {other:?}")),
            }
        }
        if cli.replay.is_some() && cli.passthrough {
            return Err("--replay and --passthrough are mutually exclusive".to_string());
        }
        Ok(cli)
    }
}

// ------------------------------------------------------------------ counters

#[derive(Default)]
struct Stats {
    lines: u64,
    enriched: u64,
    /// Enrichment attempted and failed → line emitted unchanged (§6.4).
    absent: u64,
    /// Not enrichable (no text / not JSON / oversize) → no attempt made.
    skipped: u64,
}

impl Stats {
    fn summary(&self) -> String {
        format!(
            "[sentiment-enrich] {} lines: {} enriched, {} absent (fail-open), {} skipped",
            self.lines, self.enriched, self.absent, self.skipped
        )
    }
}

/// Where the annotations come from this run.
enum Mode {
    /// Identity filter.
    Passthrough,
    /// Fixture responses consumed in order; exhaustion = absence.
    Replay { entries: Vec<Value>, next: usize },
    /// The live llama.cpp server, one keep-alive connection.
    Live { http: http::Http, url: String },
}

impl Mode {
    /// One enrichment attempt for a line's text. `Err` = absence (or loud
    /// failure under `--require`).
    fn enrich(&mut self, text: &str) -> Result<Sentiment, String> {
        match self {
            Mode::Passthrough => unreachable!("passthrough never attempts enrichment"),
            Mode::Replay { entries, next } => {
                let entry = entries
                    .get(*next)
                    .ok_or_else(|| "replay fixture exhausted".to_string())?;
                *next += 1;
                if matches!(entry, Value::Null) {
                    return Err("replay fixture entry is null (simulated failure)".to_string());
                }
                sentiment_from_value(entry)
            }
            Mode::Live { http, url } => {
                let body = build_request_body(text);
                let resp =
                    http.post_json_once(url, &[("Content-Type", "application/json")], &body)?;
                parse_response(&resp)
            }
        }
    }
}

// ----------------------------------------------------------------------- run

/// Subcommand entry: filter stdin → stdout. No capture clock is injected —
/// this stage never stamps events; its only time reads are monotonic latency
/// diagnostics that go to stderr.
pub fn run(args: &[String]) -> u8 {
    let cli = match Cli::parse(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[pq-social-capture] sentiment-enrich: {e}");
            return 2;
        }
    };
    let model_id = std::env::var("LLAMA_MODEL_ID").unwrap_or_else(|_| DEFAULT_MODEL_ID.to_string());
    let mut mode = if cli.passthrough {
        Mode::Passthrough
    } else if let Some(path) = &cli.replay {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[pq-social-capture] sentiment-enrich: replay failed: {path}: {e}");
                return 2;
            }
        };
        let entries = match json::parse(&text) {
            Ok(Value::Array(items)) => items,
            Ok(_) => {
                eprintln!(
                    "[pq-social-capture] sentiment-enrich: replay fixture must be a JSON array"
                );
                return 2;
            }
            Err(e) => {
                eprintln!("[pq-social-capture] sentiment-enrich: bad JSON in {path}: {e}");
                return 2;
            }
        };
        Mode::Replay { entries, next: 0 }
    } else {
        let base = std::env::var("LLAMA_SERVER_URL")
            .ok()
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_SERVER_URL.to_string());
        Mode::Live {
            http: http::Http::new(TIMEOUT_SECS),
            url: format!("{base}/completion"),
        }
    };

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut stats = Stats::default();
    let mut latencies_us: Vec<u64> = Vec::new();
    let mut buf: Vec<u8> = Vec::with_capacity(4096);

    loop {
        let had_newline = match read_line_capped(&mut input, &mut buf) {
            Ok(LineRead::Eof) => break,
            Ok(LineRead::Line(nl)) => nl,
            Ok(LineRead::OversizeHead) => {
                // Never buffered whole, never dropped: head + streamed tail.
                stats.lines += 1;
                stats.skipped += 1;
                if out.write_all(&buf).is_err() {
                    return 1;
                }
                let nl = match stream_oversize_tail(&mut input, &mut out) {
                    Ok(nl) => nl,
                    Err(_) => return 1,
                };
                if (nl && out.write_all(b"\n").is_err()) || out.flush().is_err() {
                    return 1;
                }
                eprintln!(
                    "[sentiment-enrich] line {} exceeds {MAX_LINE_BYTES} byte cap; \
                     passed through unenriched",
                    stats.lines
                );
                continue;
            }
            Err(e) => {
                eprintln!("[sentiment-enrich] stdin read failed: {e}");
                return 1;
            }
        };
        stats.lines += 1;

        // Decide what to write: the enriched line, or the original bytes.
        let mut fatal: Option<String> = None;
        let enriched: Option<String> = if matches!(mode, Mode::Passthrough) {
            None
        } else {
            match std::str::from_utf8(&buf).ok().and_then(extract_text) {
                None => {
                    stats.skipped += 1;
                    None
                }
                Some(text) => {
                    let started = Instant::now();
                    let outcome = mode.enrich(&text);
                    latencies_us.push(started.elapsed().as_micros() as u64);
                    match outcome {
                        // `buf` was JSON with "text", so it is valid UTF-8.
                        Ok(s) => {
                            let line = std::str::from_utf8(&buf).expect("checked above");
                            match splice(line, s, &model_id) {
                                Some(spliced) => {
                                    stats.enriched += 1;
                                    Some(spliced)
                                }
                                None => {
                                    stats.skipped += 1;
                                    None
                                }
                            }
                        }
                        Err(e) => {
                            stats.absent += 1;
                            if cli.require {
                                fatal = Some(e);
                            } else if stats.absent == 1 || stats.absent % WARN_EVERY == 0 {
                                eprintln!(
                                    "[sentiment-enrich] enrichment failure #{}: {e}; \
                                     line emitted unaltered (absence, §6.4)",
                                    stats.absent
                                );
                            }
                            None
                        }
                    }
                }
            }
        };
        let write_ok = match &enriched {
            Some(line) => out.write_all(line.as_bytes()).is_ok(),
            None => out.write_all(&buf).is_ok(),
        };
        if !write_ok || (had_newline && out.write_all(b"\n").is_err()) || out.flush().is_err() {
            return 1; // downstream is gone; nothing to salvage
        }
        if let Some(e) = fatal {
            eprintln!("[sentiment-enrich] FATAL (--require): {e}");
            eprintln!("{}", stats.summary());
            return 1;
        }
        if stats.lines % SUMMARY_EVERY == 0 {
            eprintln!("{}", stats.summary());
        }
    }

    eprintln!("{}", stats.summary());
    if !latencies_us.is_empty() {
        latencies_us.sort_unstable();
        eprintln!(
            "[sentiment-enrich] enrichment latency: n={} p50={}us max={}us",
            latencies_us.len(),
            latencies_us[latencies_us.len() / 2],
            latencies_us[latencies_us.len() - 1]
        );
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const S: Sentiment = Sentiment {
        sentiment_bp: 9100,
        confidence_bp: 7000,
    };

    // ------------------------------------------------------------- splicing

    #[test]
    fn splice_appends_before_closing_brace() {
        let line = r#"{"platform":"x","text":"send it $WIF","echo":false}"#;
        let got = splice(line, S, "local-llm-v0").unwrap();
        assert_eq!(
            got,
            r#"{"platform":"x","text":"send it $WIF","echo":false,"sentiment_bp":9100,"sentiment_conf_bp":7000,"sentiment_model":"local-llm-v0"}"#
        );
    }

    #[test]
    fn splice_output_is_valid_json_with_originals_untouched() {
        let line = r#"{"platform":"x","author":"a","text":"gm \"quoted\" {brace}","likes":1}"#;
        let got = splice(line, S, "m").unwrap();
        // Byte-prefix identical up to the original closing brace.
        assert!(got.starts_with(line.strip_suffix('}').unwrap()));
        assert!(got.ends_with('}'));
        let v = json::parse(&got).unwrap();
        assert_eq!(v.get("author").unwrap().as_str(), Some("a"));
        assert_eq!(
            v.get("text").unwrap().as_str(),
            Some("gm \"quoted\" {brace}")
        );
        assert_eq!(v.get("sentiment_bp"), Some(&Value::Number("9100".into())));
        assert_eq!(
            v.get("sentiment_conf_bp"),
            Some(&Value::Number("7000".into()))
        );
        assert_eq!(v.get("sentiment_model").unwrap().as_str(), Some("m"));
    }

    #[test]
    fn splice_preserves_trailing_carriage_return() {
        let got = splice("{\"text\":\"x\"}\r", S, "m").unwrap();
        assert!(got.ends_with("}\r"));
        assert!(got.contains("\"sentiment_bp\":9100"));
    }

    #[test]
    fn splice_escapes_hostile_model_id() {
        let got = splice(r#"{"text":"x"}"#, S, "m\"v\\1").unwrap();
        let v = json::parse(&got).unwrap();
        assert_eq!(v.get("sentiment_model").unwrap().as_str(), Some("m\"v\\1"));
    }

    #[test]
    fn splice_rejects_non_object_lines() {
        assert!(splice("not json at all", S, "m").is_none());
        assert!(splice("[1,2,3]", S, "m").is_none());
        assert!(splice("", S, "m").is_none());
    }

    #[test]
    fn splice_empty_object_takes_no_leading_comma() {
        let got = splice("{}", S, "m").unwrap();
        assert!(json::parse(&got).is_ok(), "{got}");
        assert!(got.starts_with("{\"sentiment_bp\":"));
    }

    // ----------------------------------------------------------- validation

    #[test]
    fn sentiment_parses_in_range_integers() {
        let v = json::parse(r#"{"sentiment_bp":0,"confidence_bp":10000}"#).unwrap();
        assert_eq!(
            sentiment_from_value(&v).unwrap(),
            Sentiment {
                sentiment_bp: 0,
                confidence_bp: 10000
            }
        );
    }

    #[test]
    fn out_of_range_is_rejected_not_clamped() {
        for bad in [
            r#"{"sentiment_bp":10001,"confidence_bp":5000}"#,
            r#"{"sentiment_bp":5000,"confidence_bp":12000}"#,
            r#"{"sentiment_bp":-1,"confidence_bp":5000}"#,
        ] {
            let v = json::parse(bad).unwrap();
            assert!(sentiment_from_value(&v).is_err(), "{bad} must be rejected");
        }
    }

    #[test]
    fn non_integer_shapes_are_rejected() {
        for bad in [
            r#"{"sentiment_bp":5000.5,"confidence_bp":5000}"#,
            r#"{"sentiment_bp":"5000","confidence_bp":5000}"#,
            r#"{"sentiment_bp":true,"confidence_bp":5000}"#,
            r#"{"confidence_bp":5000}"#,
            r#"{}"#,
        ] {
            let v = json::parse(bad).unwrap();
            assert!(sentiment_from_value(&v).is_err(), "{bad} must be rejected");
        }
    }

    #[test]
    fn server_response_happy_path() {
        let body = r#"{"content":"{\"sentiment_bp\":8200,\"confidence_bp\":6000}","stop":true}"#;
        assert_eq!(
            parse_response(body).unwrap(),
            Sentiment {
                sentiment_bp: 8200,
                confidence_bp: 6000
            }
        );
    }

    #[test]
    fn malformed_server_responses_are_absence() {
        for bad in [
            "",                                                              // empty body
            "not json",                                                      // not JSON
            r#"{"no_content":true}"#,                                        // missing content
            r#"{"content":"the vibes are good"}"#,                           // content not JSON
            r#"{"content":"{\"sentiment_bp\":99999,\"confidence_bp\":1}"}"#, // out of range
        ] {
            assert!(parse_response(bad).is_err(), "{bad:?} must be absence");
        }
    }

    // ------------------------------------------------------ prompt/request

    #[test]
    fn request_body_survives_hostile_text() {
        let hostile = "\"}{ \\\" inject\n{\"prompt\":\"pwn\"}\r\ttail";
        let body = build_request_body(hostile);
        let v = json::parse(&body).expect("request body must stay valid JSON");
        let prompt = v.get("prompt").unwrap().as_str().unwrap();
        assert!(prompt.contains(hostile), "text carried verbatim");
        assert_eq!(v.get("temperature"), Some(&Value::Number("0".into())));
        assert_eq!(v.get("seed"), Some(&Value::Number("7".into())));
        assert_eq!(v.get("n_predict"), Some(&Value::Number("48".into())));
    }

    #[test]
    fn request_carries_the_schema_constraint() {
        let body = build_request_body("gm $WIF");
        let v = json::parse(&body).unwrap();
        let schema = v.get("json_schema").expect("schema present");
        assert_eq!(schema.get("required").unwrap().as_array().unwrap().len(), 2);
        // The schema itself must be the exact audited constant, verbatim.
        assert!(body.contains(RESPONSE_SCHEMA));
    }

    #[test]
    fn prompt_pins_the_scale_anchors() {
        let p = build_prompt("gm");
        for anchor in ["0-10000", "5000", "bearish", "bullish", "ONLY"] {
            assert!(p.contains(anchor), "prompt must anchor {anchor:?}");
        }
    }

    #[test]
    fn extract_text_requires_a_nonempty_text_field() {
        assert_eq!(
            extract_text(r#"{"text":"gm $WIF","likes":1}"#).as_deref(),
            Some("gm $WIF")
        );
        assert_eq!(extract_text(r#"{"text":""}"#), None);
        assert_eq!(extract_text(r#"{"likes":1}"#), None);
        assert_eq!(extract_text("not json"), None);
        assert_eq!(extract_text(r#"{"text":42}"#), None);
    }

    // -------------------------------------------------------- line-size cap

    #[test]
    fn capped_reader_passes_lines_at_the_cap() {
        let line = "x".repeat(MAX_LINE_BYTES);
        let mut input = Cursor::new(format!("{line}\n"));
        let mut buf = Vec::new();
        assert!(matches!(
            read_line_capped(&mut input, &mut buf).unwrap(),
            LineRead::Line(true)
        ));
        assert_eq!(buf.len(), MAX_LINE_BYTES);
    }

    #[test]
    fn capped_reader_flags_oversize_and_tail_streams_unchanged() {
        let big = "y".repeat(MAX_LINE_BYTES + 10);
        let mut input = Cursor::new(format!("{big}\nnext\n"));
        let mut buf = Vec::new();
        assert!(matches!(
            read_line_capped(&mut input, &mut buf).unwrap(),
            LineRead::OversizeHead
        ));
        let mut tail = Vec::new();
        assert!(stream_oversize_tail(&mut input, &mut tail).unwrap());
        let mut whole = buf.clone();
        whole.extend_from_slice(&tail);
        assert_eq!(String::from_utf8(whole).unwrap(), big, "bytes unchanged");
        // The next line is intact after the tail stream.
        assert!(matches!(
            read_line_capped(&mut input, &mut buf).unwrap(),
            LineRead::Line(true)
        ));
        assert_eq!(buf, b"next");
    }

    // ------------------------------------------------------------------ CLI

    #[test]
    fn cli_rejects_conflicting_and_unknown_flags() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(Cli::parse(&args(&["--replay", "f", "--passthrough"])).is_err());
        assert!(Cli::parse(&args(&["--replay"])).is_err());
        assert!(Cli::parse(&args(&["--frobnicate"])).is_err());
        let ok = Cli::parse(&args(&["--replay", "f", "--require"])).unwrap();
        assert_eq!(ok.replay.as_deref(), Some("f"));
        assert!(ok.require && !ok.passthrough);
    }
}
