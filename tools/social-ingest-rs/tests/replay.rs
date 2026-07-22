//! Integration test: `--replay` over the raw-IRC fixture must produce EXACTLY the
//! expected normalized NDJSON, byte-for-byte, with deterministic synthetic
//! timestamps (§22 — replay is a pure function of the fixture file), and every
//! emitted line must survive an independent JSON round-trip sanity check.

use std::process::Command;

/// Fixed replay clock (must mirror `REPLAY_BASE_NS`/`REPLAY_STEP_NS` in main.rs).
const BASE: u64 = 1_000_000_000;
const STEP: u64 = 1_000_000;

fn run_replay() -> (String, String) {
    let fixture = format!("{}/tests/fixtures/sample.irc", env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(env!("CARGO_BIN_EXE_pq-twitch-capture"))
        .args(["--replay", &fixture])
        .output()
        .expect("binary runs");
    assert!(out.status.success(), "replay exited nonzero: {out:?}");
    (
        String::from_utf8(out.stdout).expect("stdout is UTF-8"),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn expected_line(author: &str, community: &str, text_escaped: &str, i: u64) -> String {
    format!(
        "{{\"platform\":\"twitch\",\"author\":\"{author}\",\"community\":\"{community}\",\
         \"text\":\"{text_escaped}\",\"likes\":0,\"reposts\":0,\"replies\":0,\"echo\":false,\
         \"observed_at_ns\":{}}}",
        BASE + i * STEP
    )
}

#[test]
fn replay_emits_exactly_the_expected_ndjson() {
    let (stdout, stderr) = run_replay();
    let lines: Vec<&str> = stdout.lines().collect();
    let expected = [
        expected_line("degenwif", "pumpwatch", "$WIF to a billion", 0),
        expected_line(
            "coincaller",
            "pumpwatch",
            "ape DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263 now",
            1,
        ),
        expected_line("tagguy", "pumpwatch", "tags tolerated $BONK", 2),
        expected_line("actionman", "pumpwatch", "slurps the dip", 3),
        expected_line("quoter", "pumpwatch", "he said \\\"buy\\\" \\\\ hold", 4),
    ];
    assert_eq!(
        lines.len(),
        expected.len(),
        "stdout was:\n{stdout}\nstderr:\n{stderr}"
    );
    for (got, want) in lines.iter().zip(expected.iter()) {
        assert_eq!(got, want);
    }
    // stdout is NDJSON only; diagnostics live on stderr.
    assert!(stderr.contains("emitted 5 events"), "stderr:\n{stderr}");
}

#[test]
fn replay_is_deterministic_across_runs() {
    let (a, _) = run_replay();
    let (b, _) = run_replay();
    assert_eq!(
        a, b,
        "two replays of the same fixture must be byte-identical"
    );
}

#[test]
fn every_emitted_line_round_trips_as_valid_json() {
    let (stdout, _) = run_replay();
    for line in stdout.lines() {
        let v = json::parse(line).unwrap_or_else(|e| panic!("invalid JSON {line:?}: {e}"));
        // Round-trip: re-serializing with the same compact rules must reproduce
        // the emitted bytes exactly (proves the escape fn is canonical).
        assert_eq!(json::serialize(&v), line, "round-trip drift");
        // Schema sanity: exactly the normalize.py fields (plus the capture stamp).
        let obj = match &v {
            json::Value::Object(pairs) => pairs,
            other => panic!("top level must be an object, got {other:?}"),
        };
        let keys: Vec<&str> = obj.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "platform",
                "author",
                "community",
                "text",
                "likes",
                "reposts",
                "replies",
                "echo",
                "observed_at_ns"
            ]
        );
        assert_eq!(obj[0].1, json::Value::String("twitch".to_string()));
        for (_, val) in &obj[4..7] {
            assert_eq!(
                *val,
                json::Value::Number("0".to_string()),
                "engagement is 0"
            );
        }
        assert_eq!(
            obj[7].1,
            json::Value::Bool(false),
            "chat lines are originations"
        );
    }
}

/// A deliberately tiny recursive-descent JSON reader + compact writer, local to
/// this test (§67: the shipped binary stays dependency-free; the test proves its
/// hand-rolled emission is valid JSON without trusting the code under test).
mod json {
    #[derive(Debug, Clone, PartialEq)]
    pub enum Value {
        Null,
        Bool(bool),
        /// Numbers kept as their raw text (we only emit integers).
        Number(String),
        String(String),
        Array(Vec<Value>),
        Object(Vec<(String, Value)>),
    }

    pub fn parse(s: &str) -> Result<Value, String> {
        let b = s.as_bytes();
        let mut i = 0usize;
        let v = value(b, &mut i)?;
        skip_ws(b, &mut i);
        if i != b.len() {
            return Err(format!("trailing bytes at {i}"));
        }
        Ok(v)
    }

    pub fn serialize(v: &Value) -> String {
        let mut out = String::new();
        write(v, &mut out);
        out
    }

    fn write(v: &Value, out: &mut String) {
        match v {
            Value::Null => out.push_str("null"),
            Value::Bool(true) => out.push_str("true"),
            Value::Bool(false) => out.push_str("false"),
            Value::Number(raw) => out.push_str(raw),
            Value::String(s) => write_string(s, out),
            Value::Array(items) => {
                out.push('[');
                for (n, item) in items.iter().enumerate() {
                    if n > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            Value::Object(pairs) => {
                out.push('{');
                for (n, (k, item)) in pairs.iter().enumerate() {
                    if n > 0 {
                        out.push(',');
                    }
                    write_string(k, out);
                    out.push(':');
                    write(item, out);
                }
                out.push('}');
            }
        }
    }

    fn write_string(s: &str, out: &mut String) {
        out.push('"');
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
                    out.push_str(&format!("\\u{:04x}", c as u32));
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }

    fn skip_ws(b: &[u8], i: &mut usize) {
        while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\n' | b'\r') {
            *i += 1;
        }
    }

    fn value(b: &[u8], i: &mut usize) -> Result<Value, String> {
        skip_ws(b, i);
        match b.get(*i) {
            Some(b'{') => object(b, i),
            Some(b'[') => array(b, i),
            Some(b'"') => Ok(Value::String(string(b, i)?)),
            Some(b't') => lit(b, i, "true", Value::Bool(true)),
            Some(b'f') => lit(b, i, "false", Value::Bool(false)),
            Some(b'n') => lit(b, i, "null", Value::Null),
            Some(c) if c.is_ascii_digit() || *c == b'-' => number(b, i),
            other => Err(format!("unexpected {other:?} at {i}")),
        }
    }

    fn lit(b: &[u8], i: &mut usize, word: &str, v: Value) -> Result<Value, String> {
        if b[*i..].starts_with(word.as_bytes()) {
            *i += word.len();
            Ok(v)
        } else {
            Err(format!("bad literal at {i}"))
        }
    }

    fn number(b: &[u8], i: &mut usize) -> Result<Value, String> {
        let start = *i;
        if b.get(*i) == Some(&b'-') {
            *i += 1;
        }
        while *i < b.len()
            && (b[*i].is_ascii_digit() || matches!(b[*i], b'.' | b'e' | b'E' | b'+' | b'-'))
        {
            *i += 1;
        }
        if *i == start {
            return Err(format!("empty number at {start}"));
        }
        String::from_utf8(b[start..*i].to_vec())
            .map(Value::Number)
            .map_err(|e| e.to_string())
    }

    fn string(b: &[u8], i: &mut usize) -> Result<String, String> {
        if b.get(*i) != Some(&b'"') {
            return Err(format!("expected string at {i}"));
        }
        *i += 1;
        let mut out = String::new();
        loop {
            match b.get(*i) {
                None => return Err("unterminated string".to_string()),
                Some(b'"') => {
                    *i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    *i += 1;
                    match b.get(*i) {
                        Some(b'"') => out.push('"'),
                        Some(b'\\') => out.push('\\'),
                        Some(b'/') => out.push('/'),
                        Some(b'n') => out.push('\n'),
                        Some(b'r') => out.push('\r'),
                        Some(b't') => out.push('\t'),
                        Some(b'b') => out.push('\u{8}'),
                        Some(b'f') => out.push('\u{c}'),
                        Some(b'u') => {
                            let hex = b.get(*i + 1..*i + 5).ok_or("short \\u escape")?;
                            let hex = std::str::from_utf8(hex).map_err(|e| e.to_string())?;
                            let cp = u32::from_str_radix(hex, 16).map_err(|e| e.to_string())?;
                            // BMP only — sufficient for the C0 escapes we emit.
                            out.push(char::from_u32(cp).ok_or("bad code point")?);
                            *i += 4;
                        }
                        other => return Err(format!("bad escape {other:?}")),
                    }
                    *i += 1;
                }
                Some(&c) if c < 0x20 => {
                    return Err(format!("raw control byte {c:#x} in string"));
                }
                Some(_) => {
                    // Consume one UTF-8 encoded char (input is checked UTF-8).
                    let s = std::str::from_utf8(&b[*i..]).map_err(|e| e.to_string())?;
                    let c = s.chars().next().ok_or("empty")?;
                    out.push(c);
                    *i += c.len_utf8();
                }
            }
        }
    }

    fn array(b: &[u8], i: &mut usize) -> Result<Value, String> {
        *i += 1; // '['
        let mut items = Vec::new();
        skip_ws(b, i);
        if b.get(*i) == Some(&b']') {
            *i += 1;
            return Ok(Value::Array(items));
        }
        loop {
            items.push(value(b, i)?);
            skip_ws(b, i);
            match b.get(*i) {
                Some(b',') => *i += 1,
                Some(b']') => {
                    *i += 1;
                    return Ok(Value::Array(items));
                }
                other => return Err(format!("bad array delim {other:?}")),
            }
        }
    }

    fn object(b: &[u8], i: &mut usize) -> Result<Value, String> {
        *i += 1; // '{'
        let mut pairs = Vec::new();
        skip_ws(b, i);
        if b.get(*i) == Some(&b'}') {
            *i += 1;
            return Ok(Value::Object(pairs));
        }
        loop {
            skip_ws(b, i);
            let key = string(b, i)?;
            skip_ws(b, i);
            if b.get(*i) != Some(&b':') {
                return Err(format!("expected ':' at {i}"));
            }
            *i += 1;
            pairs.push((key, value(b, i)?));
            skip_ws(b, i);
            match b.get(*i) {
                Some(b',') => *i += 1,
                Some(b'}') => {
                    *i += 1;
                    return Ok(Value::Object(pairs));
                }
                other => return Err(format!("bad object delim {other:?}")),
            }
        }
    }
}
