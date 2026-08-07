//! `tape` — the recorded decision/outcome JSONL schema and a std-only,
//! integer-exact parser (constitution §62 artifact inputs).
//!
//! The `pq-evaluator` and `pq-research-runner` binaries read a recorded
//! decision/outcome *tape*: newline-delimited JSON objects, one record per line,
//! each tagged by a `"kind"` field. This module owns that schema and a minimal
//! hand-rolled JSON reader so the binaries stay thin and depend on no external
//! crate (the workspace is std-only; the laptop build has no network).
//!
//! The parser is deliberately small: it accepts JSON objects whose values are
//! strings, integers, booleans, or arrays of integers — exactly what the tape
//! schema uses. It carries every number as `i128` so lamport magnitudes are
//! exact, and it rejects floating-point tokens outright (§22: no floats enter the
//! evaluator, not even through its input). Deterministic: identical text yields
//! identical records in input order.

use crate::ablation::{AblationVariant, FeatureFamily};
use crate::baseline_family::TapeEvent;
use crate::evaluator_stats::{Lane, ReconTrade};
use crate::fdr::Hypothesis;

// ============================================================================
// Errors
// ============================================================================

/// Why a tape line could not be parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TapeError {
    /// 1-based line number the error occurred on (0 for whole-input errors).
    pub line: usize,
    /// Human-facing reason.
    pub msg: String,
}

impl TapeError {
    fn at(line: usize, msg: impl Into<String>) -> Self {
        TapeError {
            line,
            msg: msg.into(),
        }
    }
}

impl std::fmt::Display for TapeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "tape parse error (line {}): {}", self.line, self.msg)
    }
}

impl std::error::Error for TapeError {}

// ============================================================================
// Minimal JSON value
// ============================================================================

/// A minimal JSON value covering exactly the tape schema: strings, exact
/// integers (`i128`), booleans, and arrays of integers.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Json {
    Str(String),
    Int(i128),
    Bool(bool),
    IntArray(Vec<i128>),
}

/// Parse one JSON object line into a list of `(key, value)` pairs, in text order.
fn parse_object(line: &str, lineno: usize) -> Result<Vec<(String, Json)>, TapeError> {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    skip_ws(bytes, &mut i);
    expect(bytes, &mut i, b'{', lineno)?;
    let mut out: Vec<(String, Json)> = Vec::new();
    skip_ws(bytes, &mut i);
    if peek(bytes, i) == Some(b'}') {
        return Ok(out);
    }
    loop {
        skip_ws(bytes, &mut i);
        let key = parse_string(bytes, &mut i, lineno)?;
        skip_ws(bytes, &mut i);
        expect(bytes, &mut i, b':', lineno)?;
        skip_ws(bytes, &mut i);
        let val = parse_value(bytes, &mut i, lineno)?;
        out.push((key, val));
        skip_ws(bytes, &mut i);
        match peek(bytes, i) {
            Some(b',') => {
                i += 1;
                continue;
            }
            Some(b'}') => {
                i += 1;
                break;
            }
            _ => return Err(TapeError::at(lineno, "expected ',' or '}'")),
        }
    }
    skip_ws(bytes, &mut i);
    if i != bytes.len() {
        return Err(TapeError::at(lineno, "trailing characters after object"));
    }
    Ok(out)
}

fn skip_ws(b: &[u8], i: &mut usize) {
    while let Some(&c) = b.get(*i) {
        if c == b' ' || c == b'\t' || c == b'\r' || c == b'\n' {
            *i += 1;
        } else {
            break;
        }
    }
}

fn peek(b: &[u8], i: usize) -> Option<u8> {
    b.get(i).copied()
}

fn expect(b: &[u8], i: &mut usize, c: u8, lineno: usize) -> Result<(), TapeError> {
    if peek(b, *i) == Some(c) {
        *i += 1;
        Ok(())
    } else {
        Err(TapeError::at(lineno, format!("expected '{}'", c as char)))
    }
}

fn parse_string(b: &[u8], i: &mut usize, lineno: usize) -> Result<String, TapeError> {
    expect(b, i, b'"', lineno)?;
    let mut s = String::new();
    while let Some(&c) = b.get(*i) {
        *i += 1;
        match c {
            b'"' => return Ok(s),
            b'\\' => {
                let e = *b
                    .get(*i)
                    .ok_or_else(|| TapeError::at(lineno, "bad escape"))?;
                *i += 1;
                match e {
                    b'"' => s.push('"'),
                    b'\\' => s.push('\\'),
                    b'/' => s.push('/'),
                    b'n' => s.push('\n'),
                    b't' => s.push('\t'),
                    b'r' => s.push('\r'),
                    _ => return Err(TapeError::at(lineno, "unsupported escape")),
                }
            }
            _ => s.push(c as char),
        }
    }
    Err(TapeError::at(lineno, "unterminated string"))
}

fn parse_int(b: &[u8], i: &mut usize, lineno: usize) -> Result<i128, TapeError> {
    let start = *i;
    if peek(b, *i) == Some(b'-') {
        *i += 1;
    }
    let digit_start = *i;
    while let Some(&c) = b.get(*i) {
        if c.is_ascii_digit() {
            *i += 1;
        } else {
            break;
        }
    }
    if *i == digit_start {
        return Err(TapeError::at(lineno, "expected integer"));
    }
    // Reject floats / exponents explicitly (§22: no floats on the tape).
    if matches!(peek(b, *i), Some(b'.') | Some(b'e') | Some(b'E')) {
        return Err(TapeError::at(lineno, "floating-point not allowed on tape"));
    }
    let text = std::str::from_utf8(&b[start..*i]).unwrap();
    text.parse::<i128>()
        .map_err(|_| TapeError::at(lineno, "integer out of i128 range"))
}

fn parse_value(b: &[u8], i: &mut usize, lineno: usize) -> Result<Json, TapeError> {
    match peek(b, *i) {
        Some(b'"') => Ok(Json::Str(parse_string(b, i, lineno)?)),
        Some(b't') => {
            take_literal(b, i, "true", lineno)?;
            Ok(Json::Bool(true))
        }
        Some(b'f') => {
            take_literal(b, i, "false", lineno)?;
            Ok(Json::Bool(false))
        }
        Some(b'[') => {
            *i += 1;
            let mut arr: Vec<i128> = Vec::new();
            skip_ws(b, i);
            if peek(b, *i) == Some(b']') {
                *i += 1;
                return Ok(Json::IntArray(arr));
            }
            loop {
                skip_ws(b, i);
                arr.push(parse_int(b, i, lineno)?);
                skip_ws(b, i);
                match peek(b, *i) {
                    Some(b',') => {
                        *i += 1;
                        continue;
                    }
                    Some(b']') => {
                        *i += 1;
                        break;
                    }
                    _ => return Err(TapeError::at(lineno, "expected ',' or ']' in array")),
                }
            }
            Ok(Json::IntArray(arr))
        }
        Some(_) => Ok(Json::Int(parse_int(b, i, lineno)?)),
        None => Err(TapeError::at(lineno, "unexpected end of value")),
    }
}

fn take_literal(b: &[u8], i: &mut usize, lit: &str, lineno: usize) -> Result<(), TapeError> {
    let l = lit.as_bytes();
    if b.get(*i..*i + l.len()) == Some(l) {
        *i += l.len();
        Ok(())
    } else {
        Err(TapeError::at(lineno, format!("expected literal '{lit}'")))
    }
}

// ============================================================================
// Field accessors
// ============================================================================

fn get<'a>(obj: &'a [(String, Json)], key: &str) -> Option<&'a Json> {
    obj.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn get_str<'a>(obj: &'a [(String, Json)], key: &str, lineno: usize) -> Result<&'a str, TapeError> {
    match get(obj, key) {
        Some(Json::Str(s)) => Ok(s.as_str()),
        _ => Err(TapeError::at(
            lineno,
            format!("missing/invalid string field '{key}'"),
        )),
    }
}

/// Like `get_str` but returns an owned `String`. Used for `TradeFull` records
/// where the parsed value must outlive the borrow of the JSON object.
fn get_str_owned(obj: &[(String, Json)], key: &str, lineno: usize) -> Result<String, TapeError> {
    match get(obj, key) {
        Some(Json::Str(s)) => Ok(s.clone()),
        _ => Err(TapeError::at(
            lineno,
            format!("missing/invalid string field '{key}'"),
        )),
    }
}

fn get_int(obj: &[(String, Json)], key: &str, lineno: usize) -> Result<i128, TapeError> {
    match get(obj, key) {
        Some(Json::Int(v)) => Ok(*v),
        _ => Err(TapeError::at(
            lineno,
            format!("missing/invalid integer field '{key}'"),
        )),
    }
}

fn get_bool(obj: &[(String, Json)], key: &str, lineno: usize) -> Result<bool, TapeError> {
    match get(obj, key) {
        Some(Json::Bool(v)) => Ok(*v),
        _ => Err(TapeError::at(
            lineno,
            format!("missing/invalid bool field '{key}'"),
        )),
    }
}

fn get_int_array(obj: &[(String, Json)], key: &str, lineno: usize) -> Result<Vec<i128>, TapeError> {
    match get(obj, key) {
        Some(Json::IntArray(v)) => Ok(v.clone()),
        _ => Err(TapeError::at(
            lineno,
            format!("missing/invalid array field '{key}'"),
        )),
    }
}

fn as_u128(v: i128, key: &str, lineno: usize) -> Result<u128, TapeError> {
    u128::try_from(v)
        .map_err(|_| TapeError::at(lineno, format!("field '{key}' must be non-negative")))
}

fn as_u64(v: i128, key: &str, lineno: usize) -> Result<u64, TapeError> {
    u64::try_from(v).map_err(|_| TapeError::at(lineno, format!("field '{key}' out of u64 range")))
}

fn as_u32(v: i128, key: &str, lineno: usize) -> Result<u32, TapeError> {
    u32::try_from(v).map_err(|_| TapeError::at(lineno, format!("field '{key}' out of u32 range")))
}

fn as_i64(v: i128, key: &str, lineno: usize) -> Result<i64, TapeError> {
    i64::try_from(v).map_err(|_| TapeError::at(lineno, format!("field '{key}' out of i64 range")))
}

// ============================================================================
// Parsed tape
// ============================================================================

/// One recorded ablation replay outcome (the research-runner's recorded table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AblationRecord {
    /// Family the perturbation applies to.
    pub family: FeatureFamily,
    /// The variant recorded.
    pub variant: AblationVariant,
    /// Recorded net lamports.
    pub net_lamports: i128,
    /// Recorded right-tail, bps.
    pub right_tail_bps: i64,
}

/// A full-fidelity enriched trade record (kind: "trade_full") carrying all 16
/// fields from the engine's TradeRecord. Used for attribution analysis, A/B
/// testing, and strategy-type discovery. Constitution §43, §62.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TradeFull {
    /// Slot at which the trade decision was made (the timebase).
    pub slot: u64,
    /// The mint that was traded, as a base58 string.
    pub mint_b58: String,
    /// "BUY" or "SELL".
    pub side: String,
    /// Entry price in fixed-point.
    pub entry_price_fp: i128,
    /// Exit price in fixed-point.
    pub exit_price_fp: i128,
    /// Trade size in lamports.
    pub size_lamports: u64,
    /// The strategy ID that produced this trade.
    pub strategy_id: u64,
    /// The ingest source tag.
    pub source: String,
    /// The outcome tag (FILLED, FILLED_SLIP, REJECTED, etc.).
    pub outcome: String,
    /// Realized PnL in lamports (positive = profit, negative = loss).
    pub realized_pnl_lamports: i64,
    /// Total fees paid in lamports.
    pub fees_lamports: u64,
    /// Slippage in lamports.
    pub slippage_lamports: u64,
    /// Internal decision latency in microseconds.
    pub decision_latency_us: u64,
    /// On-chain confirmation latency in microseconds.
    pub confirm_latency_us: u64,
    /// "P" (paper) or "L" (live).
    pub run_mode: String,
    /// Solana program error code if on-chain failure (0 otherwise).
    pub error_code: u32,
    /// Monotonic sequence number.
    pub seq: u64,
}

/// The fully-parsed decision/outcome tape.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tape {
    /// Reconciled trades (for net-SOL). Coarse 5-field format.
    pub trades: Vec<ReconTrade>,
    /// Enriched full-fidelity trades (16-field format, kind: "trade_full").
    /// When present, these supersede `trades` for attribution analysis,
    /// A/B testing, and strategy-type discovery. The coarse `trades` field
    /// is still populated from kind:"trade" records for backward compat.
    pub full_trades: Vec<TradeFull>,
    /// Challenger-vs-baseline hypotheses (for BH-FDR).
    pub pvalues: Vec<Hypothesis>,
    /// CSCV performance matrix rows (for PBO).
    pub perf: Vec<Vec<i64>>,
    /// Decision-tape events (for the baseline family).
    pub baseline_events: Vec<TapeEvent>,
    /// Recorded ablation outcomes (for the ablation harness).
    pub ablation: Vec<AblationRecord>,
    /// Optional candidate id whose promotion is under test (`kind:"candidate"`).
    pub candidate_id: Option<u64>,
}

fn parse_variant(s: &str, lineno: usize) -> Result<AblationVariant, TapeError> {
    Ok(match s {
        "removed" => AblationVariant::Removed,
        "alone" => AblationVariant::Alone,
        "combined" => AblationVariant::Combined,
        "delayed" => AblationVariant::Delayed,
        "noised" => AblationVariant::Noised,
        "shuffled" => AblationVariant::Shuffled,
        _ => {
            return Err(TapeError::at(
                lineno,
                format!("unknown ablation variant '{s}'"),
            ))
        }
    })
}

fn parse_lane(s: &str, lineno: usize) -> Result<Lane, TapeError> {
    match s {
        "scalp" => Ok(Lane::Scalp),
        "early" => Ok(Lane::Early),
        _ => Err(TapeError::at(lineno, format!("unknown lane '{s}'"))),
    }
}

/// Parse a full decision/outcome tape from JSONL text (§62).
///
/// Blank lines and lines whose first non-space byte is `#` (comments) are
/// skipped. Every other line must be a JSON object with a `"kind"` field; an
/// unrecognized kind, a malformed line, or a float token is a hard error naming
/// the line number. Records are collected in input order (except where a leaf
/// imposes its own deterministic ordering downstream). Pure.
pub fn parse_jsonl(input: &str) -> Result<Tape, TapeError> {
    let mut tape = Tape::default();
    for (idx, raw) in input.lines().enumerate() {
        let lineno = idx + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let obj = parse_object(trimmed, lineno)?;
        let kind = get_str(&obj, "kind", lineno)?;
        match kind {
            "trade" => {
                let lane = parse_lane(get_str(&obj, "lane", lineno)?, lineno)?;
                tape.trades.push(ReconTrade {
                    lane,
                    gross_lamports: get_int(&obj, "gross", lineno)?,
                    fees: as_u128(get_int(&obj, "fees", lineno)?, "fees", lineno)?,
                    tips: as_u128(get_int(&obj, "tips", lineno)?, "tips", lineno)?,
                    failed_costs: as_u128(get_int(&obj, "failed", lineno)?, "failed", lineno)?,
                    mint: [0u8; 32],
                    entry_price_fp: 0,
                    exit_price_fp: 0,
                    size_lamports: 0,
                    archetype: 0,
                    exit_reason_code: 0,
                    mfe_bps: 0,
                    mae_bps: 0,
                    entry_tick: 0,
                });
            }
            "trade_full" => {
                tape.full_trades.push(TradeFull {
                    slot: as_u64(get_int(&obj, "slot", lineno)?, "slot", lineno)?,
                    mint_b58: get_str_owned(&obj, "mint", lineno)?,
                    side: get_str_owned(&obj, "side", lineno)?,
                    entry_price_fp: get_int(&obj, "entry_price_fp", lineno)?,
                    exit_price_fp: get_int(&obj, "exit_price_fp", lineno)?,
                    size_lamports: as_u64(get_int(&obj, "size_lamports", lineno)?, "size_lamports", lineno)?,
                    strategy_id: as_u64(get_int(&obj, "strategy_id", lineno)?, "strategy_id", lineno)?,
                    source: get_str_owned(&obj, "source", lineno)?,
                    outcome: get_str_owned(&obj, "outcome", lineno)?,
                    realized_pnl_lamports: as_i64(get_int(&obj, "realized_pnl", lineno)?, "realized_pnl", lineno)?,
                    fees_lamports: as_u64(get_int(&obj, "fees", lineno)?, "fees", lineno)?,
                    slippage_lamports: as_u64(get_int(&obj, "slippage", lineno)?, "slippage", lineno)?,
                    decision_latency_us: as_u64(get_int(&obj, "decision_latency_us", lineno)?, "decision_latency_us", lineno)?,
                    confirm_latency_us: as_u64(get_int(&obj, "confirm_latency_us", lineno)?, "confirm_latency_us", lineno)?,
                    run_mode: get_str_owned(&obj, "run_mode", lineno)?,
                    error_code: as_u32(get_int(&obj, "error_code", lineno)?, "error_code", lineno)?,
                    seq: as_u64(get_int(&obj, "seq", lineno)?, "seq", lineno)?,
                });
            }
            "pvalue" => {
                tape.pvalues.push(Hypothesis {
                    id: as_u64(get_int(&obj, "id", lineno)?, "id", lineno)?,
                    p_ppm: as_u32(get_int(&obj, "p_ppm", lineno)?, "p_ppm", lineno)?,
                });
            }
            "perf" => {
                let row = get_int_array(&obj, "row", lineno)?;
                let mut irow: Vec<i64> = Vec::with_capacity(row.len());
                for v in row {
                    irow.push(as_i64(v, "row", lineno)?);
                }
                tape.perf.push(irow);
            }
            "baseline_event" => {
                tape.baseline_events.push(TapeEvent {
                    index: as_u64(get_int(&obj, "index", lineno)?, "index", lineno)?,
                    eligible: get_bool(&obj, "eligible", lineno)?,
                    launch: get_bool(&obj, "launch", lineno)?,
                    score: as_i64(get_int(&obj, "score", lineno)?, "score", lineno)?,
                    net_hold_to_death: get_int(&obj, "net_hold", lineno)?,
                    net_fixed_tpsl: get_int(&obj, "net_tpsl", lineno)?,
                });
            }
            "ablation" => {
                let variant = parse_variant(get_str(&obj, "variant", lineno)?, lineno)?;
                tape.ablation.push(AblationRecord {
                    family: FeatureFamily(as_u32(
                        get_int(&obj, "family", lineno)?,
                        "family",
                        lineno,
                    )?),
                    variant,
                    net_lamports: get_int(&obj, "net", lineno)?,
                    right_tail_bps: as_i64(get_int(&obj, "tail", lineno)?, "tail", lineno)?,
                });
            }
            "candidate" => {
                tape.candidate_id = Some(as_u64(get_int(&obj, "id", lineno)?, "id", lineno)?);
            }
            other => {
                return Err(TapeError::at(
                    lineno,
                    format!("unknown record kind '{other}'"),
                ));
            }
        }
    }
    Ok(tape)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_kind() {
        let input = concat!(
            "# a comment\n",
            "{\"kind\":\"trade\",\"lane\":\"scalp\",\"gross\":1000,\"fees\":10,\"tips\":5,\"failed\":0}\n",
            "\n",
            "{\"kind\":\"pvalue\",\"id\":1,\"p_ppm\":5000}\n",
            "{\"kind\":\"perf\",\"row\":[5,-3,8,-1]}\n",
            "{\"kind\":\"baseline_event\",\"index\":0,\"eligible\":true,\"launch\":false,\"score\":10,\"net_hold\":5000,\"net_tpsl\":3000}\n",
            "{\"kind\":\"ablation\",\"family\":2,\"variant\":\"removed\",\"net\":1200,\"tail\":30}\n",
            "{\"kind\":\"candidate\",\"id\":1}\n",
        );
        let t = parse_jsonl(input).unwrap();
        assert_eq!(t.trades.len(), 1);
        assert_eq!(t.trades[0].lane, Lane::Scalp);
        assert_eq!(t.trades[0].gross_lamports, 1000);
        assert_eq!(t.pvalues, vec![Hypothesis::new(1, 5000)]);
        assert_eq!(t.perf, vec![vec![5, -3, 8, -1]]);
        assert_eq!(t.baseline_events.len(), 1);
        assert_eq!(t.ablation.len(), 1);
        assert_eq!(t.ablation[0].variant, AblationVariant::Removed);
        assert_eq!(t.candidate_id, Some(1));
    }

    #[test]
    fn rejects_float() {
        let e = parse_jsonl("{\"kind\":\"trade\",\"lane\":\"scalp\",\"gross\":1.5,\"fees\":0,\"tips\":0,\"failed\":0}").unwrap_err();
        assert!(e.msg.contains("floating-point"));
        assert_eq!(e.line, 1);
    }

    #[test]
    fn rejects_negative_fee() {
        let e = parse_jsonl("{\"kind\":\"trade\",\"lane\":\"scalp\",\"gross\":0,\"fees\":-1,\"tips\":0,\"failed\":0}").unwrap_err();
        assert!(e.msg.contains("non-negative"));
    }

    #[test]
    fn rejects_unknown_kind() {
        let e = parse_jsonl("{\"kind\":\"bogus\"}").unwrap_err();
        assert!(e.msg.contains("unknown record kind"));
    }

    #[test]
    fn rejects_unknown_lane() {
        let e = parse_jsonl(
            "{\"kind\":\"trade\",\"lane\":\"nope\",\"gross\":0,\"fees\":0,\"tips\":0,\"failed\":0}",
        )
        .unwrap_err();
        assert!(e.msg.contains("unknown lane"));
    }

    #[test]
    fn empty_input_is_empty_tape() {
        let t = parse_jsonl("").unwrap();
        assert_eq!(t, Tape::default());
    }

    #[test]
    fn negative_gross_and_scores_ok() {
        let t = parse_jsonl("{\"kind\":\"baseline_event\",\"index\":3,\"eligible\":false,\"launch\":true,\"score\":-5,\"net_hold\":-2000,\"net_tpsl\":-1000}").unwrap();
        assert_eq!(t.baseline_events[0].score, -5);
        assert_eq!(t.baseline_events[0].net_hold_to_death, -2000);
    }

    #[test]
    fn deterministic_repeat() {
        let input = "{\"kind\":\"perf\",\"row\":[1,2]}\n{\"kind\":\"perf\",\"row\":[3,4]}";
        assert_eq!(parse_jsonl(input), parse_jsonl(input));
    }
}
