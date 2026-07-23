//! Hand-rolled JSON reading — no serde (§67 removable adapter).
//!
//! Copied from the audited `tools/social-ingest-https-rs/src/json.rs` reader
//! (the Python-coercion helpers were left behind — this suite has no
//! `normalize.py` twin) and extended with the integer accessors the stream
//! lanes need (`as_u64` for slots, lossless raw-number carry for amounts).
//!
//! Round-trip losslessness is LOAD-BEARING here (§6.3 raw-bytes-first):
//! numbers keep their raw source text and object member order is preserved,
//! so `serialize(parse(x))` re-emits every value byte-losslessly (only
//! inter-token whitespace is dropped). That is what lets the lanes embed a
//! parsed-then-reserialized vendor payload as "raw".
//!
//! Everything is a pure `&str -> Value` function (§22): no clock, no I/O.
//! Malformed input returns `Err`, never panics — vendor payloads are
//! adversarial by definition (§99-spirit bounding is enforced upstream by the
//! transports' message/body size caps).

/// One parsed JSON value. Numbers keep their raw text: the lanes decide
/// per-field how to coerce, and re-serialization must be lossless.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `null`
    Null,
    /// `true` / `false`
    Bool(bool),
    /// Any number, kept as its raw source text.
    Number(String),
    /// A string (escapes decoded).
    String(String),
    /// An array.
    Array(Vec<Value>),
    /// An object; insertion order preserved.
    Object(Vec<(String, Value)>),
}

impl Value {
    /// Object member lookup (first match), `None` for non-objects.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// `&str` view of a JSON string, `None` otherwise.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// Array view, `None` otherwise.
    #[must_use]
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    /// Non-negative integer view of a JSON number. Integer text parses
    /// exactly; float/exponent text truncates toward zero through f64 (slots
    /// and lamports are far below the 2^53 precision edge); negatives and
    /// non-numbers are `None`. Integer-only downstream arithmetic starts here.
    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Number(raw) => {
                if let Ok(n) = raw.parse::<u64>() {
                    return Some(n);
                }
                match raw.parse::<f64>() {
                    Ok(f) if f.is_finite() && f >= 0.0 => Some(f.trunc() as u64),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

/// Parse exactly one JSON value (trailing content is an error).
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

/// Parse a whitespace-separated stream of JSON values — fixture files hold
/// one payload per line/poll (pretty-printed or compact, one or many).
pub fn parse_stream(s: &str) -> Result<Vec<Value>, String> {
    let b = s.as_bytes();
    let mut i = 0usize;
    let mut out = Vec::new();
    loop {
        skip_ws(b, &mut i);
        if i == b.len() {
            return Ok(out);
        }
        out.push(value(b, &mut i)?);
    }
}

/// Compact serialization with the same escape rules as `emit` — lossless over
/// [`parse`] output (numbers re-emit their raw text, member order preserved).
#[must_use]
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
        Value::String(s) => {
            out.push('"');
            crate::emit::escape_json_into(s, out);
            out.push('"');
        }
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
                out.push('"');
                crate::emit::escape_json_into(k, out);
                out.push('"');
                out.push(':');
                write(item, out);
            }
            out.push('}');
        }
    }
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
                        if (0xD800..0xE000).contains(&cp) {
                            // Surrogate pair: decode the low half too.
                            let lo_esc = b.get(*i + 5..*i + 11).ok_or("lone surrogate")?;
                            if &lo_esc[..2] != b"\\u" {
                                return Err("lone surrogate".to_string());
                            }
                            let lo_hex =
                                std::str::from_utf8(&lo_esc[2..]).map_err(|e| e.to_string())?;
                            let lo = u32::from_str_radix(lo_hex, 16).map_err(|e| e.to_string())?;
                            if !(0xDC00..0xE000).contains(&lo) {
                                return Err("bad low surrogate".to_string());
                            }
                            let c = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                            out.push(char::from_u32(c).ok_or("bad code point")?);
                            *i += 10;
                        } else {
                            out.push(char::from_u32(cp).ok_or("bad code point")?);
                            *i += 4;
                        }
                    }
                    other => return Err(format!("bad escape {other:?}")),
                }
                *i += 1;
            }
            Some(&c) if c < 0x20 => {
                return Err(format!("raw control byte {c:#x} in string"));
            }
            Some(_) => {
                // Consume one UTF-8 encoded char.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_object() {
        let v = parse(r#"{"a":[1,2.5,-3],"b":{"c":"x","d":null},"e":true}"#).unwrap();
        assert_eq!(v.get("e"), Some(&Value::Bool(true)));
        assert_eq!(v.get("b").unwrap().get("c").unwrap().as_str(), Some("x"));
        assert_eq!(v.get("a").unwrap().as_array().unwrap().len(), 3);
    }

    #[test]
    fn round_trip_is_stable() {
        let src = r#"{"t":"gm \" \\ \n 🚀","n":42,"x":[{"k":null}]}"#;
        let v = parse(src).unwrap();
        let ser = serialize(&v);
        assert_eq!(parse(&ser).unwrap(), v);
    }

    #[test]
    fn round_trip_preserves_number_text_losslessly() {
        // §6.3: big u64s and high-precision decimals must survive untouched —
        // this is the property that lets lanes re-emit parsed payloads as raw.
        let src = r#"{"slot":347649965,"lamports":18446744073709551615,"amt":1234.567890123456789,"e":1e10}"#;
        assert_eq!(serialize(&parse(src).unwrap()), src);
    }

    #[test]
    fn malformed_inputs_error_without_panic() {
        for bad in ["", "{", "[1,", "{\"a\":}", "tru", "\"unterminated", "{}x"] {
            assert!(parse(bad).is_err(), "{bad:?} should fail");
        }
    }

    #[test]
    fn stream_parses_multiple_values() {
        let vs = parse_stream("{\"a\":1}\n\n  {\"b\":2}\n[3]\n").unwrap();
        assert_eq!(vs.len(), 3);
        assert_eq!(vs[2], Value::Array(vec![Value::Number("3".into())]));
    }

    #[test]
    fn surrogate_pairs_decode() {
        let v = parse(r#""🚀""#).unwrap();
        assert_eq!(v.as_str(), Some("\u{1F680}"));
    }

    #[test]
    fn as_u64_coerces_like_the_lanes_need() {
        assert_eq!(
            Value::Number("347649965".into()).as_u64(),
            Some(347_649_965)
        );
        assert_eq!(Value::Number("2.9".into()).as_u64(), Some(2)); // trunc
        assert_eq!(Value::Number("-3".into()).as_u64(), None); // negative
        assert_eq!(Value::String("7".into()).as_u64(), None); // not a number
        assert_eq!(Value::Null.as_u64(), None);
    }
}
