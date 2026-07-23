//! NDJSON lane-line emission — one flushed line per captured event.
//!
//! Copied in style from `tools/social-ingest-https-rs/src/emit.rs` (the
//! audited hand-rolled escaper, §67: no serde) and reshaped for the stream
//! lanes' contract: every lane emits `{"lane":...,"recv_unix_ms":...,...}`
//! with the RAW vendor payload carried verbatim (§6.3 raw-bytes-first — the
//! capture edge preserves, downstream parses). Everything here is pure (§22):
//! the caller supplies the already-measured `recv_unix_ms`.

/// Append `s` to `out` with JSON string escaping: `"` and `\` are backslash-
/// escaped, C0 control characters become `\n`/`\r`/`\t`/`\b`/`\f` or `\u00xx`;
/// everything else (including non-ASCII) passes through verbatim. Output is
/// always valid JSON. (Verbatim from the HTTPS suite's audited escaper.)
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

/// Build one raw-preserving lane line (no trailing newline). `raw_json` MUST
/// already be valid JSON text (the lanes only pass payloads that round-tripped
/// through [`crate::json::parse`], or verbatim vendor text that parsed) — it
/// is embedded UNQUOTED, untouched (§6.3).
#[must_use]
pub fn raw_line(
    lane: &str,
    recv_unix_ms: u64,
    extra_key: Option<(&str, &str)>,
    raw_json: &str,
) -> String {
    let mut out = String::with_capacity(64 + raw_json.len());
    out.push_str("{\"lane\":\"");
    escape_json_into(lane, &mut out);
    out.push_str("\",\"recv_unix_ms\":");
    out.push_str(&recv_unix_ms.to_string());
    if let Some((k, v)) = extra_key {
        out.push_str(",\"");
        escape_json_into(k, &mut out);
        out.push_str("\":\"");
        escape_json_into(v, &mut out);
        out.push('"');
    }
    out.push_str(",\"raw\":");
    out.push_str(raw_json);
    out.push('}');
    out
}

/// Emit one NDJSON line and flush (real-time friendly: downstream consumers
/// see each event the instant it is captured).
pub fn write_line(out: &mut impl std::io::Write, line: &str) -> std::io::Result<()> {
    out.write_all(line.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()
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
    fn raw_line_exact_shape() {
        assert_eq!(
            raw_line("pumpportal", 42, None, r#"{"mint":"abc"}"#),
            r#"{"lane":"pumpportal","recv_unix_ms":42,"raw":{"mint":"abc"}}"#
        );
    }

    #[test]
    fn raw_line_with_sub_tag() {
        assert_eq!(
            raw_line("helius_ws", 7, Some(("sub", "slot")), r#"{"slot":1}"#),
            r#"{"lane":"helius_ws","recv_unix_ms":7,"sub":"slot","raw":{"slot":1}}"#
        );
    }

    #[test]
    fn raw_line_is_valid_json() {
        let line = raw_line(
            "helius_ws",
            1,
            Some(("sub", "transaction")),
            r#"{"a":[1,2]}"#,
        );
        assert!(crate::json::parse(&line).is_ok());
    }
}
