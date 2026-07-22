//! Normalized-NDJSON emission — the Rust twin of `tools/social-ingest/normalize.py`.
//!
//! Every `[S]` adapter, Python or Rust, speaks the ONE vendor-agnostic schema the
//! deterministic core (`pump_quant_ingest::social_parse::parse_social_event`)
//! consumes — one compact JSON object per line on stdout:
//!
//! ```json
//! {"platform":"twitch","author":"<nick>","community":"<channel>",
//!  "text":"<raw chat line>","likes":0,"reposts":0,"replies":0,"echo":false,
//!  "observed_at_ns":1234567890}
//! ```
//!
//! Field-by-field contract (names must match `normalize.py` EXACTLY):
//! * `likes`/`reposts`/`replies` are always `0` — chat has no engagement counters;
//!   the engagement floor is applied downstream, never faked here.
//! * `echo` is always `false` — a chat line is an origination; copy-echo detection
//!   is downstream via content hash (§29: reach is not alpha, but that judgment is
//!   the core's, not the capture edge's).
//! * `observed_at_ns` is the capture-instant stamp `normalize.py`'s convention
//!   allows production adapters to add ("may stamp it at capture for exact
//!   Signal-Horizon latency"). The core's parser reads only the fields it knows,
//!   so the extra field is forward-compatible; in `--replay` mode it is synthetic
//!   and deterministic (§22 — no clock behind the boundary).
//!
//! JSON escaping is done by a tiny audited function below (§67: no serde — this
//! adapter is removable and dependency-free); it emits the minimal valid escape
//! set, matching Python's `json.dumps(..., ensure_ascii=False)` output shape.

/// Append `s` to `out` with JSON string escaping: `"` and `\` are backslash-
/// escaped, C0 control characters become `\n`/`\r`/`\t`/`\b`/`\f` or `\u00xx`;
/// everything else (including non-ASCII) passes through verbatim (the stream is
/// UTF-8, mirroring `ensure_ascii=False`). Output is always valid JSON.
pub fn escape_json_into(s: &str, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let n = c as u32;
                out.push_str("\\u00");
                out.push(HEX[(n >> 4) as usize] as char);
                out.push(HEX[(n & 0xf) as usize] as char);
            }
            c => out.push(c),
        }
    }
}

/// Build ONE normalized NDJSON line (no trailing newline) for a Twitch chat
/// message. Pure function of its inputs (§22): the caller supplies the already-
/// measured `observed_at_ns` (wall clock in live mode, synthetic in replay).
#[must_use]
pub fn event_line(community: &str, author: &str, text: &str, observed_at_ns: u64) -> String {
    // Preallocate: fixed skeleton (~120 bytes) + text, avoiding rehash growth on
    // the hot capture path.
    let mut out = String::with_capacity(128 + text.len() + author.len() + community.len());
    out.push_str("{\"platform\":\"twitch\",\"author\":\"");
    escape_json_into(author, &mut out);
    out.push_str("\",\"community\":\"");
    escape_json_into(community, &mut out);
    out.push_str("\",\"text\":\"");
    escape_json_into(text, &mut out);
    out.push_str("\",\"likes\":0,\"reposts\":0,\"replies\":0,\"echo\":false,\"observed_at_ns\":");
    out.push_str(&observed_at_ns.to_string());
    out.push('}');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn esc(s: &str) -> String {
        let mut out = String::new();
        escape_json_into(s, &mut out);
        out
    }

    #[test]
    fn escapes_quotes_and_backslash() {
        assert_eq!(esc(r#"he said "buy" \ hold"#), r#"he said \"buy\" \\ hold"#);
    }

    #[test]
    fn escapes_control_chars() {
        assert_eq!(esc("a\nb\tc\rd"), "a\\nb\\tc\\rd");
        assert_eq!(esc("\u{8}\u{c}"), "\\b\\f");
        assert_eq!(esc("\u{1}\u{1f}"), "\\u0001\\u001f");
    }

    #[test]
    fn non_ascii_passes_through_verbatim() {
        assert_eq!(esc("gm \u{1F680} $WIF"), "gm \u{1F680} $WIF");
    }

    #[test]
    fn event_line_exact_shape() {
        let line = event_line("pumpwatch", "degen", "$WIF up", 42);
        assert_eq!(
            line,
            "{\"platform\":\"twitch\",\"author\":\"degen\",\"community\":\"pumpwatch\",\
             \"text\":\"$WIF up\",\"likes\":0,\"reposts\":0,\"replies\":0,\"echo\":false,\
             \"observed_at_ns\":42}"
        );
    }

    #[test]
    fn event_line_with_hostile_text_stays_valid_json() {
        let line = event_line("c", "a", "\"\\\n\u{0}", 0);
        assert!(line.contains("\\\"\\\\\\n\\u0000"));
        // No raw control bytes or unescaped quotes may survive inside the line.
        assert!(!line.chars().any(|c| (c as u32) < 0x20));
    }
}
