//! Normalized-NDJSON emission — the Rust twin of `tools/social-ingest/normalize.py`.
//!
//! Copied from `tools/social-ingest-rs/src/emit.rs` (the Twitch lane) and
//! generalized: the HTTPS lanes carry real engagement counters and a real
//! `echo` flag, so the fixed-zeros Twitch skeleton becomes a parameterized
//! [`Event`]. Field names and order match `normalize.py` EXACTLY
//! (`platform, author, community, text, likes, reposts, replies, echo`),
//! followed by the Twitch lane's `observed_at_ns` capture stamp — the
//! forward-compatible extra field `normalize.py`'s convention allows
//! ("production adapters may stamp it at capture for exact Signal-Horizon
//! latency"). The deterministic core reads only the fields it knows.
//!
//! JSON escaping is done by a tiny audited function below (§67: no serde —
//! this adapter is removable and single-dependency); it emits the minimal
//! valid escape set, matching Python's `json.dumps(..., ensure_ascii=False)`
//! output shape. Everything here is pure (§22): the caller supplies the
//! already-measured `observed_at_ns` (wall clock in live mode, synthetic in
//! replay).

/// One normalized social event, ready for NDJSON emission. Borrowed fields —
/// the capture loop builds it on the stack per vendor object.
pub struct Event<'a> {
    /// Vendor-agnostic platform tag (`"x"`, `"tiktok"`, `"web"`).
    pub platform: &'a str,
    /// Origin identity, carried verbatim (§29 provenance).
    pub author: &'a str,
    /// Channel / domain, or `""` where the platform has none.
    pub community: &'a str,
    /// Raw text with cashtags + contract addresses left intact for the core.
    pub text: &'a str,
    /// Engagement counters, already coerced Python-`_int` style (non-negative).
    pub likes: u64,
    /// See [`Event::likes`].
    pub reposts: u64,
    /// See [`Event::likes`].
    pub replies: u64,
    /// The single "not an originator" signal (reply / retweet / forward /
    /// quote / duet). Reach is not alpha; the core judges, not the edge.
    pub echo: bool,
    /// Whether this event comes from a DESIGNATED caller — a curated X follow
    /// (the twitterapi curated-follow lane) or a Discord alpha room. Emitted as
    /// `"is_designated_caller":true` ONLY when set, so a non-designated event is
    /// byte-identical to the legacy line and the deterministic core reads absence
    /// as `false` (§29 provenance; the same field the Discord lane emits). Reach
    /// is still not alpha — the core judges whether a designated source earns.
    pub is_designated_caller: bool,
}

/// Append `s` to `out` with JSON string escaping: `"` and `\` are backslash-
/// escaped, C0 control characters become `\n`/`\r`/`\t`/`\b`/`\f` or `\u00xx`;
/// everything else (including non-ASCII) passes through verbatim (the stream
/// is UTF-8, mirroring `ensure_ascii=False`). Output is always valid JSON.
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

/// Build ONE normalized NDJSON line (no trailing newline). Pure function of
/// its inputs (§22).
#[must_use]
pub fn event_line(ev: &Event<'_>, observed_at_ns: u64) -> String {
    // Preallocate: fixed skeleton (~130 bytes) + variable fields, avoiding
    // rehash growth on the hot capture path.
    let mut out = String::with_capacity(
        160 + ev.text.len() + ev.author.len() + ev.community.len() + ev.platform.len(),
    );
    out.push_str("{\"platform\":\"");
    escape_json_into(ev.platform, &mut out);
    out.push_str("\",\"author\":\"");
    escape_json_into(ev.author, &mut out);
    out.push_str("\",\"community\":\"");
    escape_json_into(ev.community, &mut out);
    out.push_str("\",\"text\":\"");
    escape_json_into(ev.text, &mut out);
    out.push_str("\",\"likes\":");
    out.push_str(&ev.likes.to_string());
    out.push_str(",\"reposts\":");
    out.push_str(&ev.reposts.to_string());
    out.push_str(",\"replies\":");
    out.push_str(&ev.replies.to_string());
    out.push_str(",\"echo\":");
    out.push_str(if ev.echo { "true" } else { "false" });
    out.push_str(",\"observed_at_ns\":");
    out.push_str(&observed_at_ns.to_string());
    // Designated-caller provenance is emitted only when true: a non-designated
    // line stays byte-identical to the legacy schema, and the core reads an absent
    // field as `false` (§29; mirrors `aggregator_listed`'s omit-when-false in the
    // coingecko lane). Trailing lane fields (coingecko/pump) strip the closing
    // brace and re-append, so keeping this before `}` preserves that convention.
    if ev.is_designated_caller {
        out.push_str(",\"is_designated_caller\":true");
    }
    out.push('}');
    out
}

/// Emit one normalized NDJSON line and flush (real-time friendly: downstream
/// consumers see each event the instant it is captured, exactly like
/// `normalize.write` in the Python twin).
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
    fn event_line_exact_shape() {
        let ev = Event {
            platform: "x",
            author: "degen",
            community: "",
            text: "$WIF up",
            likes: 420,
            reposts: 69,
            replies: 12,
            echo: true,
            is_designated_caller: false,
        };
        assert_eq!(
            event_line(&ev, 42),
            "{\"platform\":\"x\",\"author\":\"degen\",\"community\":\"\",\
             \"text\":\"$WIF up\",\"likes\":420,\"reposts\":69,\"replies\":12,\
             \"echo\":true,\"observed_at_ns\":42}"
        );
    }

    #[test]
    fn designated_caller_field_emitted_only_when_true() {
        let base = Event {
            platform: "discord",
            author: "alpharoom",
            community: "vip",
            text: "$WIF send",
            likes: 0,
            reposts: 0,
            replies: 0,
            echo: false,
            is_designated_caller: true,
        };
        assert_eq!(
            event_line(&base, 7),
            "{\"platform\":\"discord\",\"author\":\"alpharoom\",\"community\":\"vip\",\
             \"text\":\"$WIF send\",\"likes\":0,\"reposts\":0,\"replies\":0,\
             \"echo\":false,\"observed_at_ns\":7,\"is_designated_caller\":true}"
        );
        // False -> byte-identical to the legacy schema (field omitted entirely).
        let off = Event {
            is_designated_caller: false,
            ..base
        };
        assert!(!event_line(&off, 7).contains("is_designated_caller"));
    }

    #[test]
    fn event_line_with_hostile_text_stays_valid_json() {
        let ev = Event {
            platform: "web",
            author: "a",
            community: "c",
            text: "\"\\\n\u{0}",
            likes: 0,
            reposts: 0,
            replies: 0,
            echo: false,
            is_designated_caller: false,
        };
        let line = event_line(&ev, 0);
        assert!(line.contains("\\\"\\\\\\n\\u0000"));
        // No raw control bytes or unescaped quotes may survive inside the line.
        assert!(!line.chars().any(|c| (c as u32) < 0x20));
    }
}
