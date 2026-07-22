//! Hand-rolled JSON reading — no serde (§67 removable adapter).
//!
//! The recursive-descent reader is promoted from the audited test-local parser
//! in `tools/social-ingest-rs/tests/replay.rs`; here it is production code
//! because the HTTPS lanes must *parse* vendor API responses, not only emit.
//! On top of the reader sit the Python-coercion helpers that make the Rust
//! lanes byte-compatible with `normalize.py`:
//!
//! * [`py_int`] mirrors `normalize._int` — `max(0, int(x))`, coercion failure
//!   is 0, floats truncate toward zero, `True` is 1.
//! * [`py_truthy`] mirrors Python `bool(x)` — `None`/`False`/`0`/`""`/empty
//!   containers are falsy.
//! * [`py_str`] mirrors Python `str(x)` for the scalar shapes vendors send.
//!
//! Everything is a pure `&str -> Value` function (§22): no clock, no I/O.
//! Malformed input returns `Err`, never panics — vendor responses are
//! adversarial by definition (§99-spirit bounding is enforced upstream by the
//! transport's response-size cap).

/// One parsed JSON value. Numbers keep their raw text: the adapters decide
/// per-field how to coerce (Python-`int` vs identity), and re-serialization in
/// tests must be lossless.
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
    /// An object; insertion order preserved (mirrors Python dict).
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
}

/// Python `bool(x)` truthiness over a JSON value.
#[must_use]
pub fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(raw) => raw.parse::<f64>().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(pairs) => !pairs.is_empty(),
    }
}

/// Python `str(x)` for scalars: strings verbatim, numbers as their raw text,
/// booleans/None in Python spelling. Containers are compact-serialized (a
/// pathological vendor shape Python would `repr` — close enough, unreachable
/// with real providers).
#[must_use]
pub fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(raw) => raw.clone(),
        Value::String(s) => s.clone(),
        other => serialize(other),
    }
}

/// `normalize._int`: `max(0, int(x))` with coercion failure -> 0. Numbers
/// truncate toward zero (Python `int(2.9) == 2`), negatives clamp to 0,
/// `True` is 1, integer-looking strings parse (`int("7") == 7`), everything
/// else (null, floats-in-strings, junk) is 0. `None` argument = absent key.
#[must_use]
pub fn py_int(v: Option<&Value>) -> u64 {
    match v {
        None | Some(Value::Null) => 0,
        Some(Value::Bool(b)) => u64::from(*b),
        Some(Value::Number(raw)) => int_from_number_text(raw),
        Some(Value::String(s)) => {
            // Python int("...") accepts surrounding whitespace, integers only.
            let t = s.trim();
            match t.parse::<i64>() {
                Ok(n) if n > 0 => n as u64,
                Ok(_) => 0,
                Err(_) => 0,
            }
        }
        Some(_) => 0, // int([...]) / int({...}) raise TypeError in Python -> 0
    }
}

/// Truncate-toward-zero for JSON number text, clamped at 0 (Python
/// `max(0, int(x))`). Integer text parses exactly; float/exponent text goes
/// through f64 (engagement counters are far below the 2^53 precision edge).
fn int_from_number_text(raw: &str) -> u64 {
    if let Ok(n) = raw.parse::<i64>() {
        return if n > 0 { n as u64 } else { 0 };
    }
    match raw.parse::<f64>() {
        Ok(f) if f.is_finite() && f > 0.0 => f.trunc() as u64,
        _ => 0,
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

/// Parse a whitespace-separated stream of JSON values — a saved raw-response
/// fixture may hold one response per poll (pretty-printed or compact, one or
/// many). This is what `--replay` reads.
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

/// Compact serialization with the same escape rules as `emit` — used by tests
/// to prove round-trip stability and by [`py_str`] for container fallbacks.
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
        assert_eq!(v.get("b").unwrap().get(r"c").unwrap().as_str(), Some("x"));
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
    fn py_int_mirrors_normalize_int() {
        assert_eq!(py_int(Some(&Value::Number("420".into()))), 420);
        assert_eq!(py_int(Some(&Value::Number("2.9".into()))), 2); // trunc
        assert_eq!(py_int(Some(&Value::Number("-3".into()))), 0); // clamp
        assert_eq!(py_int(Some(&Value::Number("-2.5".into()))), 0);
        assert_eq!(py_int(Some(&Value::Bool(true))), 1);
        assert_eq!(py_int(Some(&Value::String("7".into()))), 7);
        assert_eq!(py_int(Some(&Value::String("4.2".into()))), 0); // int("4.2") raises
        assert_eq!(py_int(Some(&Value::String("junk".into()))), 0);
        assert_eq!(py_int(Some(&Value::Null)), 0);
        assert_eq!(py_int(None), 0);
    }

    #[test]
    fn py_truthy_mirrors_python_bool() {
        assert!(!py_truthy(&Value::Null));
        assert!(!py_truthy(&Value::Bool(false)));
        assert!(!py_truthy(&Value::Number("0".into())));
        assert!(!py_truthy(&Value::Number("0.0".into())));
        assert!(!py_truthy(&Value::String(String::new())));
        assert!(!py_truthy(&Value::Array(vec![])));
        assert!(!py_truthy(&Value::Object(vec![])));
        assert!(py_truthy(&Value::Number("-1".into())));
        assert!(py_truthy(&Value::String("x".into())));
    }

    #[test]
    fn py_str_mirrors_python_str() {
        assert_eq!(py_str(&Value::String("s".into())), "s");
        assert_eq!(py_str(&Value::Number("1946001111".into())), "1946001111");
        assert_eq!(py_str(&Value::Null), "None");
        assert_eq!(py_str(&Value::Bool(true)), "True");
    }
}
