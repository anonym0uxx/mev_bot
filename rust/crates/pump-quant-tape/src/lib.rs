//! # pump-quant-tape
//!
//! Durable append-only sink for the live training tape.
//!
//! The c12 training pipeline consumes `/training/v2/canonical/renormalized_v7/trades.jsonl`:
//! one JSON object per line, thirteen keys in a fixed order, integers never quoted, strings
//! JSON-escaped, and nothing between the separators that the pipeline did not put there.
//! This crate writes exactly that line shape — byte for byte, key order included — plus one
//! extra trailing key, `regime`, which tags the market regime the trade was captured in.
//! Anything else (a reordered key, a quoted integer, a missing trailing key) is a silent
//! distribution shift for the model, so the renderer below is the single place the shape is
//! defined and [`TapeRecord::to_jsonl_line`] is the only emitter of it.
//!
//! ## Design constraints
//!
//! * **Integer only.** There is no `f32`/`f64` anywhere in this crate (§22). Every quantity
//!   on the tape is a lamport, a raw token amount, a slot, a millisecond timestamp or a
//!   compute-unit count — all integers — so floating point has nothing to do here and is
//!   prohibited rather than merely unused.
//! * **std only, no serde.** The line codec is hand-rolled so the emitted bytes are ours
//!   rather than a serializer's, and so the crate adds no dependency to the workspace.
//! * **Bounded.** [`TapeWriter`] refuses — writing nothing at all — any record that would
//!   take the file past its byte cap. A cap that one record can cross is not a cap.
//! * **No partial lines.** A record is rendered into one buffer and handed to a single
//!   `write_all` call, so a crash leaves either no line or a whole line, never a torn one.
//! * **Strict parse.** [`TapeRecord::from_jsonl_line`] accepts exactly the fourteen keys in
//!   order and rejects duplicates, unknown keys, reordering, unescaped control characters,
//!   escapes a JSON writer would not have emitted, and non-integer numbers.
//!
//! ## The line
//!
//! ```text
//! {"mint": "...", "trader": "...", "side": "buy", "venue": "pumpfun", "slot": 445669922,
//!  "recv_unix_ms": 1788975568357, "signature": "...", "sol_lamports": -98778145,
//!  "tokens_raw": 1774559103622, "fee_lamports": 1005000, "cu_consumed": 111723,
//!  "status": "success", "resolution": "instruction_accounts", "regime": "trending_up"}
//! ```
//!
//! (shown wrapped here; on disk a record is one line with no newline inside it).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fmt;
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::Read as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The fourteen keys, in the exact order they appear on the wire.
///
/// The order is load-bearing: it is what the corpus has, and the parser enforces it, so a
/// line that reorders keys is rejected rather than silently accepted.
const KEYS: [&str; 14] = [
    "mint",
    "trader",
    "side",
    "venue",
    "slot",
    "recv_unix_ms",
    "signature",
    "sol_lamports",
    "tokens_raw",
    "fee_lamports",
    "cu_consumed",
    "status",
    "resolution",
    "regime",
];

/// Index of a key in [`KEYS`], or `None` when the key is not one of the fourteen.
fn key_index(k: &str) -> Option<usize> {
    KEYS.iter().position(|x| *x == k)
}

/// Which side of the trade this row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The trader bought the mint (rendered `"buy"`).
    Buy,
    /// The trader sold the mint (rendered `"sell"`).
    Sell,
}

impl Side {
    /// The wire spelling of this side (`"buy"` / `"sell"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Side::Buy => "buy",
            Side::Sell => "sell",
        }
    }

    /// Parse the wire spelling. Exactly `"buy"` and `"sell"` are accepted; anything else,
    /// including other capitalisation, is [`TapeError::UnknownSide`].
    pub fn parse(s: &str) -> Result<Side, TapeError> {
        match s {
            "buy" => Ok(Side::Buy),
            "sell" => Ok(Side::Sell),
            other => Err(TapeError::UnknownSide(other.to_string())),
        }
    }
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One captured trade, in the tape's schema plus the regime tag.
///
/// Field order here is the field order on the wire; do not reorder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TapeRecord {
    /// The token mint address (base58).
    pub mint: String,
    /// The wallet that traded (base58).
    pub trader: String,
    /// Buy or sell, from the taker's point of view.
    pub side: Side,
    /// The venue the trade executed on (`pumpfun`, `pumpswap`, ...).
    pub venue: String,
    /// Solana slot the transaction landed in.
    pub slot: u64,
    /// Local receive time of the observed transaction, Unix milliseconds.
    pub recv_unix_ms: i64,
    /// Transaction signature (base58).
    pub signature: String,
    /// Native SOL moved by the trade, in lamports. Negative means the trader paid out.
    pub sol_lamports: i64,
    /// Raw token amount, before decimals. Negative means the trader gave tokens away.
    pub tokens_raw: i64,
    /// Transaction fee in lamports.
    pub fee_lamports: u64,
    /// Compute units consumed by the transaction.
    pub cu_consumed: u64,
    /// Transaction status string as reported by the chain (`success`, ...).
    pub status: String,
    /// How the trade's numbers were resolved (e.g. `instruction_accounts`).
    pub resolution: String,
    /// Market-regime tag for this row — the one key this crate adds to the corpus shape.
    pub regime: String,
}

impl TapeRecord {
    /// Render this record as one tape line: the thirteen corpus keys in corpus order, then
    /// `regime`, with JSON-escaped strings, plain (never quoted) integers, and **no**
    /// trailing newline.
    ///
    /// This is the only place the wire shape is defined. Callers append the newline.
    pub fn to_jsonl_line(&self) -> String {
        let mut s = String::with_capacity(320);
        s.push('{');
        push_str_field(&mut s, "mint", &self.mint, true);
        push_str_field(&mut s, "trader", &self.trader, false);
        push_str_field(&mut s, "side", self.side.as_str(), false);
        push_str_field(&mut s, "venue", &self.venue, false);
        push_num_field(&mut s, "slot", self.slot, false);
        push_num_field(&mut s, "recv_unix_ms", self.recv_unix_ms, false);
        push_str_field(&mut s, "signature", &self.signature, false);
        push_num_field(&mut s, "sol_lamports", self.sol_lamports, false);
        push_num_field(&mut s, "tokens_raw", self.tokens_raw, false);
        push_num_field(&mut s, "fee_lamports", self.fee_lamports, false);
        push_num_field(&mut s, "cu_consumed", self.cu_consumed, false);
        push_str_field(&mut s, "status", &self.status, false);
        push_str_field(&mut s, "resolution", &self.resolution, false);
        push_str_field(&mut s, "regime", &self.regime, false);
        s.push('}');
        s
    }

    /// Parse one tape line back into a record.
    ///
    /// Strict by construction: the fourteen keys must appear exactly once each, in
    /// [`KEYS`] order, with the tape's value shapes. Anything else is a typed [`TapeError`].
    /// `from_jsonl_line(&r.to_jsonl_line())` returns `r` for every record — including
    /// records whose strings carry quotes, backslashes, control characters or non-ASCII
    /// text.
    pub fn from_jsonl_line(line: &str) -> Result<TapeRecord, TapeError> {
        let mut sc = Scanner::new(line.as_bytes());
        sc.skip_ws();
        sc.expect(b'{')?;

        let mut seen = [false; 14];
        let mut rec = TapeRecord {
            mint: String::new(),
            trader: String::new(),
            side: Side::Buy,
            venue: String::new(),
            slot: 0,
            recv_unix_ms: 0,
            signature: String::new(),
            sol_lamports: 0,
            tokens_raw: 0,
            fee_lamports: 0,
            cu_consumed: 0,
            status: String::new(),
            resolution: String::new(),
            regime: String::new(),
        };

        for (n, expected) in KEYS.iter().enumerate() {
            sc.skip_ws();
            let key = sc.parse_string(expected)?;
            match key_index(&key) {
                None => return Err(TapeError::UnknownKey(key)),
                Some(i) if seen[i] => return Err(TapeError::DuplicateKey(key)),
                Some(i) if i != n => {
                    return Err(TapeError::KeyOutOfOrder {
                        expected,
                        found: key,
                    })
                }
                Some(_) => {}
            }
            seen[n] = true;
            sc.skip_ws();
            sc.expect(b':')?;
            sc.skip_ws();
            match n {
                0 => rec.mint = sc.parse_string(expected)?,
                1 => rec.trader = sc.parse_string(expected)?,
                2 => rec.side = Side::parse(&sc.parse_string(expected)?)?,
                3 => rec.venue = sc.parse_string(expected)?,
                4 => rec.slot = sc.parse_u64(expected)?,
                5 => rec.recv_unix_ms = sc.parse_i64(expected)?,
                6 => rec.signature = sc.parse_string(expected)?,
                7 => rec.sol_lamports = sc.parse_i64(expected)?,
                8 => rec.tokens_raw = sc.parse_i64(expected)?,
                9 => rec.fee_lamports = sc.parse_u64(expected)?,
                10 => rec.cu_consumed = sc.parse_u64(expected)?,
                11 => rec.status = sc.parse_string(expected)?,
                12 => rec.resolution = sc.parse_string(expected)?,
                _ => rec.regime = sc.parse_string(expected)?,
            }
            sc.skip_ws();
            if n + 1 == KEYS.len() {
                break;
            }
            sc.expect(b',')?;
        }

        sc.skip_ws();
        sc.expect(b'}')?;
        sc.skip_ws();
        if sc.peek().is_some() {
            return Err(TapeError::TrailingData);
        }
        Ok(rec)
    }
}

/// Render one `"key": "value"` pair, JSON-escaping `value`.
fn push_str_field(s: &mut String, key: &'static str, value: &str, first: bool) {
    if !first {
        s.push_str(", ");
    }
    s.push('"');
    s.push_str(key);
    s.push_str("\": \"");
    escape_into(s, value);
    s.push('"');
}

/// Render one `"key": 123` pair. Integers are never quoted.
fn push_num_field<T: fmt::Display>(s: &mut String, key: &'static str, value: T, first: bool) {
    if !first {
        s.push_str(", ");
    }
    s.push('"');
    s.push_str(key);
    s.push_str("\": ");
    let _ = write!(s, "{value}");
}

/// Append `v` to `s` with JSON string escaping.
///
/// The short forms (`\n`, `\t`, `\r`, `\b`, `\f`, `\"`, `\\`) are used where JSON defines
/// one and `\u00xx` for the remaining control characters, which is what the corpus
/// producer's `json.dumps` emits. Non-ASCII text is passed through as UTF-8 rather than
/// escaped, so a non-ASCII address round-trips byte-exactly.
fn escape_into(s: &mut String, v: &str) {
    for c in v.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\r' => s.push_str("\\r"),
            '\t' => s.push_str("\\t"),
            '\u{8}' => s.push_str("\\b"),
            '\u{c}' => s.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(s, "\\u{:04x}", c as u32);
            }
            c => s.push(c),
        }
    }
}

/// Everything that can go wrong parsing a tape line.
///
/// The variants are deliberately specific: a rejected line is a corpus-integrity event and
/// the operator needs to know *which* property was violated, not merely that JSON failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TapeError {
    /// The line was not an object: it did not open with `{`, or ended mid-value.
    NotAnObject,
    /// The line ended before the object did.
    UnexpectedEnd,
    /// A byte other than the one the grammar required at this position.
    UnexpectedByte {
        /// The byte the grammar required.
        expected: u8,
        /// The byte found instead, or `None` at end of line.
        found: Option<u8>,
    },
    /// A key that is not one of the fourteen tape keys.
    UnknownKey(String),
    /// A key that had already appeared on this line.
    DuplicateKey(String),
    /// A tape key appeared in the wrong position: the tape's key order is fixed.
    KeyOutOfOrder {
        /// The key the grammar required at this position.
        expected: &'static str,
        /// The key found instead.
        found: String,
    },
    /// A key is a tape key but never appeared.
    MissingKey(&'static str),
    /// A value that had to be a JSON string was not one.
    ExpectedString(&'static str),
    /// A string value carried a raw (unescaped) control character.
    UnescapedControl {
        /// The key whose value violated the rule.
        key: &'static str,
        /// The raw control byte found.
        byte: u8,
    },
    /// An invalid or unusable backslash escape inside a string value.
    BadEscape {
        /// The key whose value carried the escape.
        key: &'static str,
        /// The escape as written, including the backslash.
        text: String,
    },
    /// A string value was not valid UTF-8.
    ///
    /// Unreachable for `&str` input, but the scanner works on bytes and this crate prefers
    /// a typed error over an `unwrap` if that ever changes.
    BadUtf8,
    /// A numeric value was not a plain integer (a fraction, an exponent, a plus sign, a
    /// leading zero, an empty field, or a quoted number).
    NotAnInteger {
        /// The key whose value violated the rule.
        key: &'static str,
        /// The offending text as written.
        text: String,
    },
    /// A syntactically integer value that does not fit its field's type.
    IntegerOutOfRange {
        /// The key whose value violated the rule.
        key: &'static str,
        /// The offending text as written.
        text: String,
    },
    /// A `side` value that is neither `"buy"` nor `"sell"`.
    UnknownSide(String),
    /// Bytes remained after the closing `}`.
    TrailingData,
}

impl fmt::Display for TapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TapeError::NotAnObject => write!(f, "tape line is not a JSON object"),
            TapeError::UnexpectedEnd => write!(f, "tape line ended unexpectedly"),
            TapeError::UnexpectedByte { expected, found } => match found {
                Some(b) => write!(
                    f,
                    "expected {:?}, found {:?}",
                    *expected as char, *b as char
                ),
                None => write!(f, "expected {:?}, found end of line", *expected as char),
            },
            TapeError::UnknownKey(k) => write!(f, "unknown key {k:?}"),
            TapeError::DuplicateKey(k) => write!(f, "duplicate key {k:?}"),
            TapeError::KeyOutOfOrder { expected, found } => {
                write!(f, "key {found:?} out of order, expected {expected:?}")
            }
            TapeError::MissingKey(k) => write!(f, "missing key {k:?}"),
            TapeError::ExpectedString(k) => write!(f, "value of {k:?} must be a JSON string"),
            TapeError::UnescapedControl { key, byte } => write!(
                f,
                "value of {key:?} contains an unescaped control byte 0x{byte:02x}"
            ),
            TapeError::BadEscape { key, text } => {
                write!(f, "value of {key:?} contains a bad escape {text:?}")
            }
            TapeError::BadUtf8 => write!(f, "value is not valid UTF-8"),
            TapeError::NotAnInteger { key, text } => {
                write!(f, "value of {key:?} is not an integer: {text:?}")
            }
            TapeError::IntegerOutOfRange { key, text } => {
                write!(f, "value of {key:?} is out of range: {text:?}")
            }
            TapeError::UnknownSide(s) => write!(f, "unknown side {s:?}"),
            TapeError::TrailingData => write!(f, "trailing data after the tape object"),
        }
    }
}

impl std::error::Error for TapeError {}

/// A byte scanner over one line of tape text.
struct Scanner<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Scanner<'a> {
    fn new(b: &'a [u8]) -> Scanner<'a> {
        Scanner { b, i: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek();
        if c.is_some() {
            self.i += 1;
        }
        c
    }

    /// Skip the whitespace JSON allows between tokens. Newlines cannot appear inside a
    /// line, and a bare `\n` inside a value is rejected as an unescaped control byte.
    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ') | Some(b'\t')) {
            self.i += 1;
        }
    }

    fn expect(&mut self, c: u8) -> Result<(), TapeError> {
        match self.bump() {
            Some(x) if x == c => Ok(()),
            found => Err(if c == b'{' && found.is_none() {
                TapeError::NotAnObject
            } else {
                TapeError::UnexpectedByte { expected: c, found }
            }),
        }
    }

    /// Parse a JSON string value, decoding escapes into UTF-8 text.
    fn parse_string(&mut self, key: &'static str) -> Result<String, TapeError> {
        if self.peek() != Some(b'"') {
            return Err(TapeError::ExpectedString(key));
        }
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = self.bump().ok_or(TapeError::UnexpectedEnd)?;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = self.bump().ok_or(TapeError::UnexpectedEnd)?;
                    match e {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0c),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let cp = self.hex4(key)?;
                            let ch = char::from_u32(cp).ok_or_else(|| TapeError::BadEscape {
                                key,
                                text: format!("\\u{cp:04x}"),
                            })?;
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        other => {
                            return Err(TapeError::BadEscape {
                                key,
                                text: format!("\\{}", other as char),
                            })
                        }
                    }
                }
                c if c < 0x20 => return Err(TapeError::UnescapedControl { key, byte: c }),
                c => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| TapeError::BadUtf8)
    }

    /// Read exactly four hex digits and return the code point.
    fn hex4(&mut self, key: &'static str) -> Result<u32, TapeError> {
        let mut v: u32 = 0;
        let mut text = String::from("\\u");
        for _ in 0..4 {
            let c = self.bump().ok_or(TapeError::UnexpectedEnd)?;
            text.push(c as char);
            let d = (c as char)
                .to_digit(16)
                .ok_or_else(|| TapeError::BadEscape {
                    key,
                    text: text.clone(),
                })?;
            v = v * 16 + d;
        }
        Ok(v)
    }

    /// Collect the raw text of a numeric token and validate it as a plain integer.
    fn int_token(&mut self, key: &'static str) -> Result<&'a str, TapeError> {
        if self.peek() == Some(b'"') {
            // A quoted number is a string, and the tape's numbers are never strings.
            let text = self.parse_string(key)?;
            return Err(TapeError::NotAnInteger { key, text });
        }
        let b = self.b;
        let start = self.i;
        while let Some(c) = self.peek() {
            let numeric = c.is_ascii_digit()
                || c == b'-'
                || c == b'+'
                || c == b'.'
                || c == b'e'
                || c == b'E'
                || c == b'_';
            if !numeric {
                break;
            }
            self.i += 1;
        }
        let raw = std::str::from_utf8(&b[start..self.i]).map_err(|_| TapeError::BadUtf8)?;
        if !is_plain_integer(raw) {
            return Err(TapeError::NotAnInteger {
                key,
                text: raw.to_string(),
            });
        }
        Ok(raw)
    }

    fn parse_i64(&mut self, key: &'static str) -> Result<i64, TapeError> {
        let raw = self.int_token(key)?;
        raw.parse::<i64>()
            .map_err(|_| TapeError::IntegerOutOfRange {
                key,
                text: raw.to_string(),
            })
    }

    fn parse_u64(&mut self, key: &'static str) -> Result<u64, TapeError> {
        let raw = self.int_token(key)?;
        raw.parse::<u64>()
            .map_err(|_| TapeError::IntegerOutOfRange {
                key,
                text: raw.to_string(),
            })
    }
}

/// True when `raw` is a plain JSON integer: an optional `-`, then digits, with no leading
/// zero. Rejects `+1`, `1.0`, `1e3`, `01`, `1_000`, the empty string and any quoted form.
fn is_plain_integer(raw: &str) -> bool {
    let d = raw.strip_prefix('-').unwrap_or(raw);
    if d.is_empty() || !d.bytes().all(|c| c.is_ascii_digit()) {
        return false;
    }
    !(d.len() > 1 && d.starts_with('0'))
}

/// The append-only, byte-bounded tape file.
///
/// The writer owns an append-mode handle: records go to the end of the file whatever the
/// handle's offset, and a fresh writer on an existing tape appends instead of truncating.
/// One `append` renders the full `line + "\n"` buffer and hands it to a single `write_all`,
/// so a crash cannot leave a half line on disk. When the next record would take the file
/// past `max_bytes` the writer refuses it — writing nothing — and reports `false`; the
/// caller decides whether to seal and rotate.
pub struct TapeWriter {
    file: File,
    path: PathBuf,
    max_bytes: u64,
    bytes_written: u64,
    records_written: u64,
    flushed_through: u64,
}

impl TapeWriter {
    /// Open (or create) the tape at `path`, creating parent directories as needed.
    ///
    /// An existing file is appended to, never truncated, and its current length counts
    /// against `max_bytes`. `flushed_through()` starts at 0: this writer does not claim
    /// bytes it did not write are durable.
    pub fn open(path: &Path, max_bytes: u64) -> std::io::Result<TapeWriter> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let existing = file.metadata()?.len();
        Ok(TapeWriter {
            file,
            path: path.to_path_buf(),
            max_bytes,
            bytes_written: existing,
            records_written: 0,
            flushed_through: 0,
        })
    }

    /// Append one record as a single line.
    ///
    /// Returns `Ok(false)` — having written **nothing**, not a partial line — when the
    /// record would take the file past `max_bytes`. Returns `Ok(true)` when the whole line
    /// (including its terminating newline) was accepted.
    pub fn append(&mut self, rec: &TapeRecord) -> std::io::Result<bool> {
        let mut buf = rec.to_jsonl_line();
        buf.push('\n');
        let n = buf.len() as u64;
        let end = match self.bytes_written.checked_add(n) {
            Some(e) => e,
            None => return Ok(false),
        };
        if end > self.max_bytes {
            return Ok(false);
        }
        self.file.write_all(buf.as_bytes())?;
        self.bytes_written = end;
        self.records_written += 1;
        Ok(true)
    }

    /// Bytes in the file after the last accepted append, counting any content the file
    /// already had when this writer opened it.
    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Records this writer has appended since it was opened.
    pub fn records_written(&self) -> u64 {
        self.records_written
    }

    /// The byte offset up to which everything is known to be durable. Starts at 0 and
    /// advances to [`TapeWriter::bytes_written`] on each successful [`TapeWriter::flush`].
    pub fn flushed_through(&self) -> u64 {
        self.flushed_through
    }

    /// Force everything appended so far out to stable storage and advance
    /// [`TapeWriter::flushed_through`].
    pub fn flush(&mut self) -> std::io::Result<()> {
        self.file.sync_data()?;
        self.flushed_through = self.bytes_written;
        Ok(())
    }

    /// The path this writer appends to.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Read a tape file back into records — the verification half of a round trip.
///
/// Every complete line must parse; a malformed line is an `InvalidData` error carrying the
/// [`TapeError`]. A trailing chunk with no terminating newline is ignored rather than
/// parsed: this writer never emits one, so it can only be a truncated tail, and refusing to
/// read a whole tape because of its last torn byte would be the wrong failure mode.
pub fn read_back(path: &Path) -> std::io::Result<Vec<TapeRecord>> {
    let mut text = String::new();
    File::open(path)?.read_to_string(&mut text)?;
    let mut out = Vec::new();
    let mut rest = text.as_str();
    while let Some(pos) = rest.find('\n') {
        let line = &rest[..pos];
        if !line.is_empty() {
            out.push(
                TapeRecord::from_jsonl_line(line)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
            );
        }
        rest = &rest[pos + 1..];
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> TapeRecord {
        TapeRecord {
            mint: "Mint111".to_string(),
            trader: "Trader111".to_string(),
            side: Side::Sell,
            venue: "pumpswap".to_string(),
            slot: 445669922,
            recv_unix_ms: 1788975568348,
            signature: "Sig111".to_string(),
            sol_lamports: 47914152,
            tokens_raw: -69608018393,
            fee_lamports: 81811,
            cu_consumed: 105584,
            status: "success".to_string(),
            resolution: "instruction_accounts".to_string(),
            regime: "trending_up".to_string(),
        }
    }

    #[test]
    fn renders_the_corpus_shape_with_the_regime_tail() {
        assert_eq!(
            sample().to_jsonl_line(),
            "{\"mint\": \"Mint111\", \"trader\": \"Trader111\", \"side\": \"sell\", \
             \"venue\": \"pumpswap\", \"slot\": 445669922, \"recv_unix_ms\": 1788975568348, \
             \"signature\": \"Sig111\", \"sol_lamports\": 47914152, \
             \"tokens_raw\": -69608018393, \"fee_lamports\": 81811, \"cu_consumed\": 105584, \
             \"status\": \"success\", \"resolution\": \"instruction_accounts\", \
             \"regime\": \"trending_up\"}"
        );
    }

    #[test]
    fn round_trips_and_appends() {
        let mut f = std::env::temp_dir();
        f.push("pq_tape_unit_roundtrip.jsonl");
        let _ = std::fs::remove_file(&f);
        {
            let mut w = TapeWriter::open(&f, 1 << 20).unwrap();
            assert!(w.append(&sample()).unwrap());
            assert_eq!(w.records_written(), 1);
            assert_eq!(w.bytes_written(), sample().to_jsonl_line().len() as u64 + 1);
        }
        let back = read_back(&f).unwrap();
        assert_eq!(back, vec![sample()]);
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn parser_is_strict() {
        let good = sample().to_jsonl_line();
        assert_eq!(TapeRecord::from_jsonl_line(&good).unwrap(), sample());

        // unknown key
        let unknown = good.replace("\"venue\"", "\"market\"");
        assert_eq!(
            TapeRecord::from_jsonl_line(&unknown).unwrap_err(),
            TapeError::UnknownKey("market".to_string())
        );

        // duplicate key: the second `mint` stands where `trader` belongs
        let dup = good.replace("\"trader\": \"Trader111\"", "\"mint\": \"Trader111\"");
        assert_eq!(
            TapeRecord::from_jsonl_line(&dup).unwrap_err(),
            TapeError::DuplicateKey("mint".to_string())
        );

        // reordered keys
        let swapped = good.replace(
            "\"mint\": \"Mint111\", \"trader\": \"Trader111\"",
            "\"trader\": \"Trader111\", \"mint\": \"Mint111\"",
        );
        assert_eq!(
            TapeRecord::from_jsonl_line(&swapped).unwrap_err(),
            TapeError::KeyOutOfOrder {
                expected: "mint",
                found: "trader".to_string()
            }
        );

        // a quoted integer is not an integer
        let quoted = good.replace("\"slot\": 445669922", "\"slot\": \"445669922\"");
        assert_eq!(
            TapeRecord::from_jsonl_line(&quoted).unwrap_err(),
            TapeError::NotAnInteger {
                key: "slot",
                text: "445669922".to_string()
            }
        );

        // fractions, exponents, signs and leading zeros are all rejected
        for bad in ["445669922.5", "445669922e3", "+445669922", "0445669922"] {
            let line = good.replace("\"slot\": 445669922", &format!("\"slot\": {bad}"));
            assert!(
                matches!(
                    TapeRecord::from_jsonl_line(&line).unwrap_err(),
                    TapeError::NotAnInteger { key: "slot", .. }
                ),
                "{bad} was accepted"
            );
        }

        // out of range for the field's type
        let over = good.replace("\"slot\": 445669922", "\"slot\": 99999999999999999999999");
        assert!(matches!(
            TapeRecord::from_jsonl_line(&over).unwrap_err(),
            TapeError::IntegerOutOfRange { key: "slot", .. }
        ));
        let neg = good.replace("\"fee_lamports\": 81811", "\"fee_lamports\": -81811");
        assert!(matches!(
            TapeRecord::from_jsonl_line(&neg).unwrap_err(),
            TapeError::IntegerOutOfRange {
                key: "fee_lamports",
                ..
            }
        ));

        // a string key given a bare number
        let bare = good.replace("\"venue\": \"pumpswap\"", "\"venue\": 12");
        assert_eq!(
            TapeRecord::from_jsonl_line(&bare).unwrap_err(),
            TapeError::ExpectedString("venue")
        );

        // a raw control byte inside a string value
        let raw_newline = good.replace("\"Mint111\"", "\"Mint\n111\"");
        assert!(matches!(
            TapeRecord::from_jsonl_line(&raw_newline).unwrap_err(),
            TapeError::UnescapedControl { key: "mint", .. }
        ));

        // an escape a JSON writer would never emit, and a lone surrogate
        let bad_escape = good.replace("\"Mint111\"", "\"Mint\\x111\"");
        assert!(matches!(
            TapeRecord::from_jsonl_line(&bad_escape).unwrap_err(),
            TapeError::BadEscape { key: "mint", .. }
        ));
        let surrogate = good.replace("\"Mint111\"", "\"Mint\\ud800111\"");
        assert!(matches!(
            TapeRecord::from_jsonl_line(&surrogate).unwrap_err(),
            TapeError::BadEscape { key: "mint", .. }
        ));

        // unknown side, trailing data, missing tail, truncation, empty input
        let side = good.replace("\"side\": \"sell\"", "\"side\": \"Sell\"");
        assert!(matches!(
            TapeRecord::from_jsonl_line(&side).unwrap_err(),
            TapeError::UnknownSide(_)
        ));
        assert_eq!(
            TapeRecord::from_jsonl_line(&format!("{good} ")).unwrap(),
            sample()
        );
        let trailing = good.replace(
            "\"regime\": \"trending_up\"}",
            "\"regime\": \"trending_up\"} 1",
        );
        assert_eq!(
            TapeRecord::from_jsonl_line(&trailing).unwrap_err(),
            TapeError::TrailingData
        );
        let no_tail = good.replace(", \"regime\": \"trending_up\"", "");
        assert!(TapeRecord::from_jsonl_line(&no_tail).is_err());
        assert!(TapeRecord::from_jsonl_line("").is_err());
        assert!(TapeRecord::from_jsonl_line("[]").is_err());
    }

    #[test]
    fn read_back_ignores_a_torn_tail() {
        let mut f = std::env::temp_dir();
        f.push("pq_tape_unit_torn.jsonl");
        let mut body = sample().to_jsonl_line();
        body.push('\n');
        body.push_str("{\"mint\": \"torn\""); // no newline: a crash tail, not a record
        std::fs::write(&f, body.as_bytes()).unwrap();
        assert_eq!(read_back(&f).unwrap(), vec![sample()]);
        let _ = std::fs::remove_file(&f);
    }
}
