//! Minimal, std-only JSON scanner.
//!
//! Responsibility: parse the provider WebSocket payloads (Helius
//! `logsNotification` / PumpPortal trade events) without `serde` or
//! `simd_json`. Numbers are preserved as their raw source text and never
//! converted to `f64` here — callers convert to integer/fixed-point
//! representations themselves (§22). This keeps the whole decode path
//! float-free and deterministic.

/// A parsed JSON value. Numbers are kept as raw text (`Number(String)`) so the
/// parser never introduces floating point; integer/fixed-point conversion is
/// the caller's responsibility.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    /// JSON `null`.
    Null,
    /// JSON `true` / `false`.
    Bool(bool),
    /// A JSON number, preserved verbatim as source text.
    Number(String),
    /// A JSON string (unescaped).
    Str(String),
    /// A JSON array.
    Array(Vec<JsonValue>),
    /// A JSON object, preserving source key order.
    Object(Vec<(String, JsonValue)>),
}

impl JsonValue {
    /// Look up a key in an object; returns `None` for non-objects or absent keys.
    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        match self {
            JsonValue::Object(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Follow a `/`-separated pointer-style path through nested objects
    /// (e.g. `"params/result/value"`). Returns `None` if any segment is absent.
    pub fn path(&self, p: &str) -> Option<&JsonValue> {
        let mut cur = self;
        for seg in p.split('/') {
            if seg.is_empty() {
                continue;
            }
            cur = cur.get(seg)?;
        }
        Some(cur)
    }

    /// Borrow the string contents, or `None` if this is not a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            JsonValue::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Borrow the raw number text, or `None` if this is not a number.
    pub fn as_number_str(&self) -> Option<&str> {
        match self {
            JsonValue::Number(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Borrow the array elements, or `None` if this is not an array.
    pub fn as_array(&self) -> Option<&[JsonValue]> {
        match self {
            JsonValue::Array(v) => Some(v.as_slice()),
            _ => None,
        }
    }

    /// Whether this value is JSON `null`.
    pub fn is_null(&self) -> bool {
        matches!(self, JsonValue::Null)
    }

    /// The boolean value, or `None` if this is not a bool.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            JsonValue::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

/// Parse a UTF-8 JSON payload. Returns `None` on any malformed input.
pub fn parse(input: &[u8]) -> Option<JsonValue> {
    let mut p = Parser { b: input, i: 0 };
    p.skip_ws();
    let v = p.parse_value()?;
    p.skip_ws();
    // Trailing non-whitespace is a parse error.
    if p.i != p.b.len() {
        return None;
    }
    Some(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
                self.i += 1;
            } else {
                break;
            }
        }
    }

    fn parse_value(&mut self) -> Option<JsonValue> {
        self.skip_ws();
        match self.peek()? {
            b'{' => self.parse_object(),
            b'[' => self.parse_array(),
            b'"' => self.parse_string().map(JsonValue::Str),
            b't' => self.parse_lit(b"true", JsonValue::Bool(true)),
            b'f' => self.parse_lit(b"false", JsonValue::Bool(false)),
            b'n' => self.parse_lit(b"null", JsonValue::Null),
            c if c == b'-' || c.is_ascii_digit() => self.parse_number(),
            _ => None,
        }
    }

    fn parse_lit(&mut self, lit: &[u8], val: JsonValue) -> Option<JsonValue> {
        if self.b[self.i..].starts_with(lit) {
            self.i += lit.len();
            Some(val)
        } else {
            None
        }
    }

    fn parse_number(&mut self) -> Option<JsonValue> {
        let start = self.i;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == b'-' || c == b'+' || c == b'.' || c == b'e' || c == b'E' {
                self.i += 1;
            } else {
                break;
            }
        }
        if self.i == start {
            return None;
        }
        let s = core::str::from_utf8(&self.b[start..self.i]).ok()?;
        Some(JsonValue::Number(s.to_string()))
    }

    fn parse_string(&mut self) -> Option<String> {
        // current byte is the opening quote
        self.i += 1;
        let mut out = String::new();
        loop {
            let c = self.peek()?;
            self.i += 1;
            match c {
                b'"' => return Some(out),
                b'\\' => {
                    let e = self.peek()?;
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000C}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let cp = self.parse_hex4()?;
                            out.push(char::from_u32(cp)?);
                        }
                        _ => return None,
                    }
                }
                _ => {
                    // Bytes < 0x80 are single UTF-8 code units; for multi-byte
                    // UTF-8, accumulate the continuation bytes.
                    if c < 0x80 {
                        out.push(c as char);
                    } else {
                        let start = self.i - 1;
                        while let Some(n) = self.peek() {
                            if n & 0xC0 == 0x80 {
                                self.i += 1;
                            } else {
                                break;
                            }
                        }
                        let s = core::str::from_utf8(&self.b[start..self.i]).ok()?;
                        out.push_str(s);
                    }
                }
            }
        }
    }

    fn parse_hex4(&mut self) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..4 {
            let c = self.peek()?;
            self.i += 1;
            let d = match c {
                b'0'..=b'9' => (c - b'0') as u32,
                b'a'..=b'f' => (c - b'a' + 10) as u32,
                b'A'..=b'F' => (c - b'A' + 10) as u32,
                _ => return None,
            };
            v = v * 16 + d;
        }
        Some(v)
    }

    fn parse_array(&mut self) -> Option<JsonValue> {
        self.i += 1; // consume '['
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek()? == b']' {
            self.i += 1;
            return Some(JsonValue::Array(items));
        }
        loop {
            let v = self.parse_value()?;
            items.push(v);
            self.skip_ws();
            match self.peek()? {
                b',' => {
                    self.i += 1;
                }
                b']' => {
                    self.i += 1;
                    return Some(JsonValue::Array(items));
                }
                _ => return None,
            }
        }
    }

    fn parse_object(&mut self) -> Option<JsonValue> {
        self.i += 1; // consume '{'
        let mut entries = Vec::new();
        self.skip_ws();
        if self.peek()? == b'}' {
            self.i += 1;
            return Some(JsonValue::Object(entries));
        }
        loop {
            self.skip_ws();
            if self.peek()? != b'"' {
                return None;
            }
            let key = self.parse_string()?;
            self.skip_ws();
            if self.peek()? != b':' {
                return None;
            }
            self.i += 1;
            let val = self.parse_value()?;
            entries.push((key, val));
            self.skip_ws();
            match self.peek()? {
                b',' => {
                    self.i += 1;
                }
                b'}' => {
                    self.i += 1;
                    return Some(JsonValue::Object(entries));
                }
                _ => return None,
            }
        }
    }
}

/// Parse the integer part of a raw JSON number as `u128`, truncating any
/// fractional/exponent tail. Returns `None` on non-digit leading content or
/// overflow. Used for integer count fields (token amounts, slots, timestamps).
pub fn number_to_u128_trunc(raw: &str) -> Option<u128> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let int_part = raw.split(['.', 'e', 'E']).next().unwrap_or("");
    if int_part.is_empty() {
        return None;
    }
    let mut acc: u128 = 0;
    for b in int_part.bytes() {
        if !b.is_ascii_digit() {
            return None;
        }
        acc = acc.checked_mul(10)?.checked_add((b - b'0') as u128)?;
    }
    Some(acc)
}
