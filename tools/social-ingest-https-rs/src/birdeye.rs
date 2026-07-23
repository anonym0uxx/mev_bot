//! `birdeye` subcommand — the REQUIRED 1D-candle backfill + token-data
//! capture lane (Birdeye Data Services REST API → MarketIntel NDJSON).
//!
//! Constitutional status: **required source** (§6.7, Amendment A-3,
//! human-directed 2026-07-23; build obligation `SERVER_BUILD_MANIFEST.md` §10;
//! §6.6 evaluation record `docs/BIRDEYE_SOURCE.md`). Birdeye supplies exactly
//! two capabilities, both consumed through **MarketIntelCache only** on the
//! research/feature plane, neither ever authority:
//!
//! * **1D OHLCV backfill/cross-check** for the §21.6 bar / market-structure
//!   family — daily candles extending structure lookback beyond our own
//!   canonical capture window. Own canonical trade flow stays the PRIMARY bar
//!   source; the §6.1 prohibition on Birdeye trade history as raw truth
//!   stands unchanged.
//! * **Token-data enrichment for candle analysis** — overview fields
//!   (liquidity, holders, trade counts, volume, buy/sell pressure, price
//!   frames) and plan-tier-gated security fields: context features, never
//!   standalone signals.
//!
//! This is MARKET data, NOT social evidence — the lane deliberately does NOT
//! emit the shared SocialEvent schema (no `platform`/`author` line, no §29
//! social-plane entry). It emits three new NDJSON record kinds instead
//! (documented in the README), each carrying `provider` provenance for the
//! §21.6 MarketIntelCache carry list:
//!
//! * `birdeye_ohlcv_1d_v1` — daily bars per mint, numeric values preserved
//!   EXACTLY as received (raw JSON tokens passed through verbatim — no float
//!   math in our code, ever);
//! * `birdeye_token_overview_v1` — the vendor `data` object UNTOUCHED under
//!   `"raw"` (§6.3 raw preservation — downstream carries provenance);
//! * `birdeye_token_security_v1` — likewise; the endpoint is plan-tier-gated
//!   (Starter+), and on 401/403 the mode is disabled for the session with one
//!   loud log — **fail-open as absence, never fabricated**.
//!
//! Auth is fail-CLOSED for the lane itself (§6.7 required source): missing
//! `BIRDEYE_API_KEY` refuses to start with exit 3 — the supervisor must see
//! the capability loss loudly, exactly like the pump lane's AUTH_WALL. Every
//! call carries `X-API-KEY` + `x-chain: solana`. Budget pacing uses the
//! suite's standard pacer ([`BUDGET_PER_MIN`] default 30 req/min, overridable
//! by env `BIRDEYE_BUDGET_PER_MIN` or `--budget-per-min`); 429s ride the
//! shared backoff ladder with `Retry-After` respected. Plans additionally
//! meter COMPUTE UNITS per call — the conservative default keeps headroom on
//! the smallest plan tier; the operator slows the lane with
//! `--interval-secs`, never speeds it past the budget floor.
//!
//! Failure behavior (§6.7): outage / 429 / drift → loud stderr, records
//! simply absent — never a halt, delay, or degradation of any strategy lane.
//! The same FNV-1a shape-hash `SCHEMA_DRIFT` + `STATUS_CLASS_DRIFT` sentinels
//! as the `pump`/`coingecko` lanes watch each endpoint family; drift keeps
//! running on raw passthrough. `--replay` is a pure function of the fixture
//! file (§22): zero network, zero wall clock, byte-stable.

use crate::json::{self, py_str, py_truthy, Value};
use crate::pump::{fnv1a64, shape_hash, Sentinel};
use crate::{dedupe, emit, http, urlenc};

/// API root (docs.birdeye.so, verified 2026-07 — re-verify at activation).
pub const BASE: &str = "https://public-api.birdeye.so";

/// Auth header carrying the operator's `BIRDEYE_API_KEY` (§29.7e: env only,
/// never hardcoded, never committed).
pub const AUTH_HEADER: &str = "X-API-KEY";

/// Chain-selector header — this lane is Solana-only.
pub const CHAIN_HEADER: (&str, &str) = ("x-chain", "solana");

/// Same timeout as the other REST lanes (`urlopen(..., timeout=30)` twins).
const TIMEOUT_SECS: u64 = 30;

/// Default global request budget across ALL modes: ≤30 req/min. Deliberately
/// conservative: Birdeye plans meter COMPUTE UNITS as well as requests, and
/// the §6.7 mandate is daily candles — there is nothing latency-critical to
/// buy with a hotter pace. Overridable via env `BIRDEYE_BUDGET_PER_MIN` or
/// `--budget-per-min` (flag wins).
pub const BUDGET_PER_MIN: u64 = 30;

/// Env var overriding [`BUDGET_PER_MIN`] (the flag still wins over the env).
pub const BUDGET_ENV: &str = "BIRDEYE_BUDGET_PER_MIN";

/// Default OHLCV lookback when `--time-from`/`--time-to` are absent: the last
/// 30 days ending now (t0 = now − 30 d, t1 = now).
pub const BIRDEYE_DEFAULT_LOOKBACK_DAYS: u64 = 30;

/// Fail-closed exit code for a missing/rejected `BIRDEYE_API_KEY` — the same
/// distinct capability-loss code as the pump lane's AUTH_WALL: a REQUIRED
/// source (§6.7) that cannot authenticate must be seen loudly, never polled
/// keylessly into silent absence.
pub const EXIT_NO_KEY: u8 = 3;

/// The candle interval this lane is mandated for (§6.7: `1D` only — own
/// canonical flow owns sub-daily).
pub const INTERVAL: &str = "1D";

/// The quote currency requested and stamped on every candle record.
pub const QUOTE: &str = "usd";

// ---------------------------------------------------------------- budget math

/// Requests one full round-robin cycle costs: one per watched mint per ACTIVE
/// mode (a plan-tier-disabled security mode costs nothing).
#[must_use]
pub fn requests_per_cycle(n_ohlcv: usize, n_overview: usize, n_security: usize) -> u64 {
    (n_ohlcv + n_overview + n_security) as u64
}

/// Minimum seconds one full cycle may take under `budget_per_min` (ceil
/// division — never round below the floor). Same shape as the pump/coingecko
/// pacers: the lane can run slower than the budget, never faster.
#[must_use]
pub fn budget_floor_secs(reqs: u64, budget_per_min: u64) -> u64 {
    (reqs.max(1) * 60).div_ceil(budget_per_min.max(1))
}

/// Effective cycle seconds: the operator's `--interval-secs` ask, clamped UP
/// to the budget floor.
#[must_use]
pub fn cycle_secs(reqs: u64, requested: Option<u64>, budget_per_min: u64) -> u64 {
    let floor = budget_floor_secs(reqs, budget_per_min);
    match requested {
        Some(r) => r.max(floor),
        None => floor,
    }
}

/// Pacing gap between consecutive requests inside a cycle.
#[must_use]
pub fn request_gap_secs(cycle: u64, reqs: u64) -> f64 {
    cycle as f64 / reqs.max(1) as f64
}

/// Resolve the effective budget: `--budget-per-min` flag wins, then the
/// `BIRDEYE_BUDGET_PER_MIN` env var, then [`BUDGET_PER_MIN`]. A junk env
/// value is an `Err` (refuse to start on a garbled operator intent rather
/// than silently pacing at a default the operator did not choose).
pub fn resolve_budget(flag: Option<u64>, env: Option<&str>) -> Result<u64, String> {
    if let Some(b) = flag {
        return Ok(b);
    }
    match env {
        None => Ok(BUDGET_PER_MIN),
        Some(raw) => match raw.trim().parse::<u64>() {
            Ok(0) => Err(format!("bad {BUDGET_ENV}: must be >= 1")),
            Ok(b) => Ok(b),
            Err(e) => Err(format!("bad {BUDGET_ENV} {raw:?}: {e}")),
        },
    }
}

/// The OHLCV time range: explicit `--time-from`/`--time-to` when given,
/// otherwise the last [`BIRDEYE_DEFAULT_LOOKBACK_DAYS`] days ending `now`
/// (unix seconds). Pure function of its inputs (§22 — the wall clock is read
/// once in `run` via the injected `now_ns`).
#[must_use]
pub fn time_range(from: Option<u64>, to: Option<u64>, now_secs: u64) -> (u64, u64) {
    let t1 = to.unwrap_or(now_secs);
    let t0 = from.unwrap_or_else(|| t1.saturating_sub(BIRDEYE_DEFAULT_LOOKBACK_DAYS * 86_400));
    (t0, t1)
}

// -------------------------------------------------------------- record lines

/// One daily bar's raw vendor tokens, ready for verbatim passthrough. Every
/// field is the UNMODIFIED [`Value`] the vendor sent (number text kept as-is
/// by the parser — no float math in our code, ever).
#[derive(Debug, PartialEq)]
pub struct Bar<'a> {
    /// `unixTime` — bar open time, unix seconds (emitted as `"t"`).
    pub t: &'a Value,
    /// Open.
    pub o: &'a Value,
    /// High.
    pub h: &'a Value,
    /// Low.
    pub l: &'a Value,
    /// Close.
    pub c: &'a Value,
    /// Base volume.
    pub v: &'a Value,
    /// USD volume — emitted only when the vendor stated it (§6.4).
    pub v_usd: Option<&'a Value>,
}

/// The `data.items` array of a `/defi/v3/ohlcv` response (empty for any
/// other shape — absence of data is never an error).
#[must_use]
pub fn ohlcv_items(page: &Value) -> &[Value] {
    page.get("data")
        .and_then(|d| d.get("items"))
        .and_then(Value::as_array)
        .unwrap_or_default()
}

/// One vendor OHLCV item → [`Bar`], or `None` for a malformed entry (any of
/// `unixTime`/`o`/`h`/`l`/`c`/`v` absent) — malformed bars are SKIPPED, never
/// fabricated (§6.4). Unknown fields are tolerated; `v_usd` is optional.
#[must_use]
pub fn parse_bar(item: &Value) -> Option<Bar<'_>> {
    Some(Bar {
        t: item.get("unixTime")?,
        o: item.get("o")?,
        h: item.get("h")?,
        l: item.get("l")?,
        c: item.get("c")?,
        v: item.get("v")?,
        v_usd: item.get("v_usd"),
    })
}

/// The mint an OHLCV page is about: the first item's `address` when the
/// vendor stated it (they echo the request address per item), else the
/// watched mint we asked for. Base58 VERBATIM case — mints are case-sensitive.
#[must_use]
pub fn ohlcv_mint(items: &[Value], watched: &str) -> String {
    items
        .first()
        .and_then(|i| i.get("address"))
        .filter(|a| py_truthy(a))
        .map(py_str)
        .unwrap_or_else(|| watched.to_string())
}

/// Append one bar as a JSON object — every value written through
/// [`json::serialize`]'s lossless writer, so the vendor's exact tokens
/// (number spelling, or strings should the vendor ever send them) survive
/// unmodified.
fn write_bar(bar: &Bar<'_>, out: &mut String) {
    out.push_str("{\"t\":");
    out.push_str(&json::serialize(bar.t));
    for (k, v) in [
        ("o", bar.o),
        ("h", bar.h),
        ("l", bar.l),
        ("c", bar.c),
        ("v", bar.v),
    ] {
        out.push_str(",\"");
        out.push_str(k);
        out.push_str("\":");
        out.push_str(&json::serialize(v));
    }
    if let Some(vu) = bar.v_usd {
        out.push_str(",\"v_usd\":");
        out.push_str(&json::serialize(vu));
    }
    out.push('}');
}

/// The `birdeye_ohlcv_1d_v1` NDJSON line. Fixed key order:
/// `record, mint, observed_unix_ms, bars, provider, interval, quote`.
#[must_use]
pub fn ohlcv_line(mint: &str, observed_unix_ms: u64, bars: &[Bar<'_>]) -> String {
    let mut out = String::with_capacity(160 + bars.len() * 96);
    out.push_str("{\"record\":\"birdeye_ohlcv_1d_v1\",\"mint\":\"");
    emit::escape_json_into(mint, &mut out);
    out.push_str("\",\"observed_unix_ms\":");
    out.push_str(&observed_unix_ms.to_string());
    out.push_str(",\"bars\":[");
    for (n, bar) in bars.iter().enumerate() {
        if n > 0 {
            out.push(',');
        }
        write_bar(bar, &mut out);
    }
    out.push_str("],\"provider\":\"birdeye\",\"interval\":\"");
    out.push_str(INTERVAL);
    out.push_str("\",\"quote\":\"");
    out.push_str(QUOTE);
    out.push_str("\"}");
    out
}

/// A `birdeye_token_overview_v1` / `birdeye_token_security_v1` NDJSON line:
/// the vendor `data` object UNTOUCHED under `"raw"` (§6.3 raw preservation —
/// key order, number spelling, unknown fields all pass through verbatim).
/// Fixed key order: `record, mint, observed_unix_ms, raw`.
#[must_use]
pub fn raw_line(record: &str, mint: &str, observed_unix_ms: u64, data: &Value) -> String {
    let mut out = String::with_capacity(128);
    out.push_str("{\"record\":\"");
    out.push_str(record);
    out.push_str("\",\"mint\":\"");
    emit::escape_json_into(mint, &mut out);
    out.push_str("\",\"observed_unix_ms\":");
    out.push_str(&observed_unix_ms.to_string());
    out.push_str(",\"raw\":");
    out.push_str(&json::serialize(data));
    out.push('}');
    out
}

/// The `data` object of a token_overview / token_security response — `None`
/// when the vendor sent no object there (an error body; logged and skipped by
/// the caller, never fabricated).
#[must_use]
pub fn data_object(page: &Value) -> Option<&Value> {
    page.get("data").filter(|d| matches!(d, Value::Object(_)))
}

/// The mint a token-data page is about: `data.address` when the vendor
/// stated it (token_overview does), else the watched mint (token_security
/// bodies carry no address — live mode always stamps the watched mint;
/// replay's fallback is the empty string, honest absence).
#[must_use]
pub fn data_mint(data: &Value, watched: &str) -> String {
    data.get("address")
        .filter(|a| py_truthy(a))
        .map(py_str)
        .unwrap_or_else(|| watched.to_string())
}

// ------------------------------------------------------------- page dispatch

/// Which documented endpoint a saved response came from — replay fixtures are
/// dispatched by SHAPE, so one fixture stream may mix all three surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageKind {
    /// `/defi/v3/ohlcv` (`{"data":{"items":[...]}}`).
    Ohlcv,
    /// `/defi/token_overview` (`data` object with a `price` field).
    Overview,
    /// `/defi/token_security` (`data` object without price/items).
    Security,
}

/// Classify a response body by its documented shape: `data.items` → OHLCV;
/// a `data` object with `price` → overview; any other `data` object →
/// security. `None` = unrecognized (logged and skipped by the caller —
/// absence of data is never an error).
#[must_use]
pub fn classify(page: &Value) -> Option<PageKind> {
    let data = page.get("data")?;
    if data.get("items").is_some() {
        return Some(PageKind::Ohlcv);
    }
    if !matches!(data, Value::Object(_)) {
        return None;
    }
    if data.get("price").is_some() {
        return Some(PageKind::Overview);
    }
    Some(PageKind::Security)
}

/// The object whose top-level key set the `SCHEMA_DRIFT` sentinel
/// fingerprints, per page kind (the first candle item for OHLCV, the `data`
/// object for overview/security).
#[must_use]
pub fn shape_target(kind: PageKind, page: &Value) -> Option<&Value> {
    match kind {
        PageKind::Ohlcv => ohlcv_items(page).first(),
        PageKind::Overview | PageKind::Security => data_object(page),
    }
}

/// Sentinel label per page kind (stderr diagnostics).
#[must_use]
pub fn kind_label(kind: PageKind) -> &'static str {
    match kind {
        PageKind::Ohlcv => "ohlcv",
        PageKind::Overview => "token_overview",
        PageKind::Security => "token_security",
    }
}

/// The record tag emitted for a page kind.
#[must_use]
pub fn record_tag(kind: PageKind) -> &'static str {
    match kind {
        PageKind::Ohlcv => "birdeye_ohlcv_1d_v1",
        PageKind::Overview => "birdeye_token_overview_v1",
        PageKind::Security => "birdeye_token_security_v1",
    }
}

/// One classified page → the NDJSON line to emit (or `None`: an error body /
/// empty candle set — a quiet poll) plus the CONTENT-KEYED dedupe identity
/// (`<kind>:<mint>:<fnv of the emitted payload>`): an unchanged response
/// across polls re-hashes identically and stays quiet on the shared ring.
#[must_use]
pub fn page_line(
    kind: PageKind,
    page: &Value,
    watched_mint: &str,
    observed_unix_ms: u64,
) -> Option<(String, String)> {
    let line = match kind {
        PageKind::Ohlcv => {
            let items = ohlcv_items(page);
            let bars: Vec<Bar<'_>> = items.iter().filter_map(parse_bar).collect();
            if bars.is_empty() {
                return None; // no valid candles = a quiet poll, never an error
            }
            ohlcv_line(&ohlcv_mint(items, watched_mint), observed_unix_ms, &bars)
        }
        PageKind::Overview | PageKind::Security => {
            let data = data_object(page)?;
            raw_line(
                record_tag(kind),
                &data_mint(data, watched_mint),
                observed_unix_ms,
                data,
            )
        }
    };
    let identity = line_identity(kind, &line);
    Some((line, identity))
}

/// Content-keyed dedupe identity for an emitted line: the payload with the
/// volatile `observed_unix_ms` stamp masked out, FNV-1a-hashed. Two polls
/// returning identical vendor data produce one emission.
#[must_use]
fn line_identity(kind: PageKind, line: &str) -> String {
    // The stamp is the only per-poll-varying field; everything after
    // `"bars"`/`"raw"` is pure vendor payload. Hash from there.
    let payload = line
        .find(",\"bars\":")
        .or_else(|| line.find(",\"raw\":"))
        .map_or(line, |i| &line[i..]);
    let mut mint = line;
    if let Some(start) = line.find("\"mint\":\"") {
        let rest = &line[start + 8..];
        if let Some(end) = rest.find('"') {
            mint = &rest[..end];
        }
    }
    format!(
        "{}:{}:{:016x}",
        kind_label(kind),
        mint,
        fnv1a64(payload.as_bytes())
    )
}

// ------------------------------------------------------------------------ CLI

/// Parsed CLI for this subcommand. The three watch modes are composable flags
/// on ONE process sharing the global budget pacer, exactly like the coingecko
/// lane's mode mix.
pub struct Cli {
    /// `--ohlcv-watch <mints-file>`: round-robin `/defi/v3/ohlcv` per mint.
    pub ohlcv_watch: String,
    /// `--overview-watch <mints-file>`: round-robin `/defi/token_overview`.
    pub overview_watch: String,
    /// `--security-watch <mints-file>`: round-robin `/defi/token_security`
    /// (Starter+ plan; 401/403 disables the mode for the session).
    pub security_watch: String,
    /// `--time-from <unix-secs>`: OHLCV range start (default now − 30 d).
    pub time_from: Option<u64>,
    /// `--time-to <unix-secs>`: OHLCV range end (default now).
    pub time_to: Option<u64>,
    /// `--interval-secs`: full-cycle seconds; default = the budget floor for
    /// the mode mix, and always clamped up to it.
    pub interval_secs: Option<u64>,
    /// `--budget-per-min`: override the global request budget (wins over the
    /// `BIRDEYE_BUDGET_PER_MIN` env var; still a ceiling).
    pub budget_per_min: Option<u64>,
    /// `--once`: exactly one full cycle, then exit (Phase-B probe mode).
    pub once: bool,
    /// `--replay <fixture>`: deterministic offline mode (§22).
    pub replay: Option<String>,
}

impl Cli {
    /// Parse subcommand arguments.
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut cli = Self {
            ohlcv_watch: String::new(),
            overview_watch: String::new(),
            security_watch: String::new(),
            time_from: None,
            time_to: None,
            interval_secs: None,
            budget_per_min: None,
            once: false,
            replay: None,
        };
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut val = |flag: &str| {
                it.next()
                    .cloned()
                    .ok_or_else(|| format!("{flag} needs a value"))
            };
            let parse_u64 = |flag: &str, raw: String| -> Result<u64, String> {
                raw.parse().map_err(|e| format!("bad {flag}: {e}"))
            };
            match a.as_str() {
                "--ohlcv-watch" => cli.ohlcv_watch = val("--ohlcv-watch")?,
                "--overview-watch" => cli.overview_watch = val("--overview-watch")?,
                "--security-watch" => cli.security_watch = val("--security-watch")?,
                "--time-from" => {
                    cli.time_from = Some(parse_u64("--time-from", val("--time-from")?)?);
                }
                "--time-to" => cli.time_to = Some(parse_u64("--time-to", val("--time-to")?)?),
                "--interval-secs" => {
                    cli.interval_secs =
                        Some(parse_u64("--interval-secs", val("--interval-secs")?)?);
                }
                "--budget-per-min" => {
                    let b = parse_u64("--budget-per-min", val("--budget-per-min")?)?;
                    if b == 0 {
                        return Err("bad --budget-per-min: must be >= 1".into());
                    }
                    cli.budget_per_min = Some(b);
                }
                "--once" => cli.once = true,
                "--replay" => cli.replay = Some(val("--replay")?),
                other => return Err(format!("unknown flag {other:?}")),
            }
        }
        Ok(cli)
    }
}

// ---------------------------------------------------------------------- runs

/// `--replay`: shape-dispatched pages, one sentinel per endpoint family, the
/// same content-keyed dedupe ring as live — zero network, zero wall clock,
/// byte-identical run-to-run (§22). The synthetic stamp is the shared replay
/// clock divided down to milliseconds. The watched-mint fallback is empty:
/// OHLCV items and overview bodies carry their own `address`; token_security
/// bodies do not, so replayed security records carry the empty mint (honest
/// absence — live mode always stamps the watched mint).
fn replay(path: &str) -> u8 {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[pq-social-capture] replay failed: {path}: {e}");
            return 1;
        }
    };
    let pages = match json::parse_stream(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[pq-social-capture] replay failed: bad JSON in {path}: {e}");
            return 1;
        }
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut ring = dedupe::DedupeRing::new(dedupe::DEFAULT_CAP);
    let mut sentinels = [
        Sentinel::default(),
        Sentinel::default(),
        Sentinel::default(),
    ];
    let mut emitted: u64 = 0;
    for page in &pages {
        let Some(kind) = classify(page) else {
            eprintln!("[birdeye] unrecognized page shape; skipping");
            continue;
        };
        observe_shape(kind, page, &mut sentinels);
        let ts_ms = (crate::REPLAY_BASE_NS + emitted * crate::REPLAY_STEP_NS) / 1_000_000;
        let Some((line, identity)) = page_line(kind, page, "", ts_ms) else {
            continue;
        };
        if !ring.insert(&identity) {
            continue;
        }
        if emit::write_line(&mut out, &line).is_err() {
            return 0;
        }
        emitted += 1;
    }
    eprintln!("[pq-social-capture] replay: emitted {emitted} events");
    0
}

/// Feed a page's shape fingerprint to its endpoint family's sentinel and log
/// `SCHEMA_DRIFT` on change — then KEEP RUNNING on raw passthrough (drift is
/// loud, never fatal: absent/odd Birdeye data is absence, §6.7).
fn observe_shape(kind: PageKind, page: &Value, sentinels: &mut [Sentinel; 3]) {
    if let Some(h) = shape_target(kind, page).and_then(shape_hash) {
        if let Some(old) = sentinels[kind as usize].observe_shape(h) {
            eprintln!(
                "[birdeye] SCHEMA_DRIFT {}: shape {old:016x} -> {h:016x} \
                 (raw passthrough continues)",
                kind_label(kind)
            );
        }
    }
}

/// Read a watchlist file (pump-lane format: one base58 mint per line, `#`
/// comments, duplicates dropped); empty flag = no mode = empty list.
fn read_mints(flag_value: &str, flag_name: &str) -> Result<Vec<String>, String> {
    if flag_value.is_empty() {
        return Ok(Vec::new());
    }
    match std::fs::read_to_string(flag_value) {
        Ok(t) => {
            let m = crate::pump::parse_mints(&t);
            if m.is_empty() {
                return Err(format!("error: {flag_value} lists no mints"));
            }
            Ok(m)
        }
        Err(e) => Err(format!("error: cannot read {flag_name} {flag_value}: {e}")),
    }
}

/// Subcommand entry. `now_ns` is the capture-edge clock injected by `main.rs`
/// (§22 — this module never reads a wall clock itself).
pub fn run(args: &[String], now_ns: fn() -> u64) -> u8 {
    let cli = match Cli::parse(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[pq-social-capture] birdeye: {e}");
            return 2;
        }
    };

    if let Some(path) = &cli.replay {
        return replay(path);
    }

    if cli.ohlcv_watch.is_empty() && cli.overview_watch.is_empty() && cli.security_watch.is_empty()
    {
        eprintln!(
            "error: birdeye needs at least one mode: --ohlcv-watch <mints-file> | \
             --overview-watch <mints-file> | --security-watch <mints-file>"
        );
        return 2;
    }

    // Fail-CLOSED auth (§6.7 required source, §29.7e env-only credentials):
    // no key, no lane — checked before any file or socket is touched, with
    // the distinct capability-loss exit code so the supervisor sees it.
    let key = std::env::var("BIRDEYE_API_KEY").unwrap_or_default();
    let key = key.trim().to_string();
    if key.is_empty() {
        eprintln!(
            "error: set BIRDEYE_API_KEY (Birdeye Data Services key, X-API-KEY header) — \
             birdeye is the REQUIRED 1D-candle backfill source (constitution \u{a7}6.7) and \
             refuses to start keyless; exiting {EXIT_NO_KEY}"
        );
        return EXIT_NO_KEY;
    }

    let (ohlcv_mints, overview_mints, security_mints) = match (
        read_mints(&cli.ohlcv_watch, "--ohlcv-watch"),
        read_mints(&cli.overview_watch, "--overview-watch"),
        read_mints(&cli.security_watch, "--security-watch"),
    ) {
        (Ok(a), Ok(b), Ok(c)) => (a, b, c),
        (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
            eprintln!("{e}");
            return 2;
        }
    };

    let budget = match resolve_budget(
        cli.budget_per_min,
        std::env::var(BUDGET_ENV).ok().as_deref(),
    ) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("[pq-social-capture] birdeye: {e}");
            return 2;
        }
    };

    let (t0, t1) = time_range(cli.time_from, cli.time_to, now_ns() / 1_000_000_000);

    // Session-scoped plan-tier gate: token_security needs Starter+; a 401/403
    // there disables ONLY that mode, once, loudly (fail-open as absence).
    let mut security_enabled = !security_mints.is_empty();

    let reqs = requests_per_cycle(
        ohlcv_mints.len(),
        overview_mints.len(),
        security_mints.len(),
    );
    let cycle = cycle_secs(reqs, cli.interval_secs, budget);
    let gap = request_gap_secs(cycle, reqs);
    eprintln!(
        "[birdeye] modes: ohlcv={} overview={} security={} mints; {reqs} req/cycle, \
         cycle {cycle}s, gap {}s (budget <= {budget} req/min; plans also meter \
         compute units — keep the pace conservative); ohlcv range {t0}..{t1} (1D)",
        ohlcv_mints.len(),
        overview_mints.len(),
        security_mints.len(),
        crate::py_float(gap)
    );

    let http = http::Http::new(TIMEOUT_SECS);
    let mut ring = dedupe::DedupeRing::new(dedupe::DEFAULT_CAP);
    let mut sentinels = [
        Sentinel::default(),
        Sentinel::default(),
        Sentinel::default(),
    ];
    let stdout = std::io::stdout();
    let t0s = t0.to_string();
    let t1s = t1.to_string();

    loop {
        let mut emitted = 0u64;
        let mut polls: Vec<(PageKind, String, &String)> = Vec::new();
        for mint in &ohlcv_mints {
            polls.push((
                PageKind::Ohlcv,
                format!(
                    "{BASE}/defi/v3/ohlcv?{}",
                    urlenc::urlencode(&[
                        ("address", mint),
                        ("type", INTERVAL),
                        ("time_from", &t0s),
                        ("time_to", &t1s),
                        ("mode", "range"),
                    ])
                ),
                mint,
            ));
        }
        for mint in &overview_mints {
            polls.push((
                PageKind::Overview,
                format!(
                    "{BASE}/defi/token_overview?{}",
                    urlenc::urlencode(&[("address", mint)])
                ),
                mint,
            ));
        }
        if security_enabled {
            for mint in &security_mints {
                polls.push((
                    PageKind::Security,
                    format!(
                        "{BASE}/defi/token_security?{}",
                        urlenc::urlencode(&[("address", mint)])
                    ),
                    mint,
                ));
            }
        }

        for (kind, url, watched_mint) in &polls {
            if *kind == PageKind::Security && !security_enabled {
                continue; // tier gate tripped mid-cycle
            }
            let headers: Vec<(&str, &str)> = vec![
                ("Accept", "application/json"),
                (AUTH_HEADER, &key),
                CHAIN_HEADER,
            ];
            let label = kind_label(*kind);
            match http.get_meta(url, &headers) {
                Ok(meta) => {
                    if meta.status == 401 || meta.status == 403 {
                        if *kind == PageKind::Security {
                            // Plan tier, not key death: the same key is
                            // serving the other endpoints. Disable the mode
                            // for the session — absence, never fabrication.
                            eprintln!(
                                "[birdeye] token_security unavailable on this plan tier \
                                 (HTTP {}, needs Starter+) — omitting (never fabricated); \
                                 mode disabled for this session",
                                meta.status
                            );
                            security_enabled = false;
                            continue;
                        }
                        eprintln!(
                            "[birdeye] AUTH_REJECTED {label}: HTTP {} — check BIRDEYE_API_KEY \
                             ({AUTH_HEADER} header, x-chain solana); a REQUIRED source \
                             (\u{a7}6.7) with a dead key must be seen loudly; exiting \
                             {EXIT_NO_KEY}",
                            meta.status
                        );
                        return EXIT_NO_KEY;
                    }
                    if let Some((old, new)) = sentinels[*kind as usize].observe_status(meta.status)
                    {
                        eprintln!("[birdeye] STATUS_CLASS_DRIFT {label}: {old}xx -> {new}xx");
                    }
                    if !(200..300).contains(&meta.status) {
                        eprintln!("[birdeye] HTTP {} on {label}; skipping poll", meta.status);
                    } else {
                        match json::parse(&meta.body) {
                            Ok(page) => {
                                observe_shape(*kind, &page, &mut sentinels);
                                let ts_ms = now_ns() / 1_000_000;
                                if let Some((line, identity)) =
                                    page_line(*kind, &page, watched_mint, ts_ms)
                                {
                                    if ring.insert(&identity) {
                                        let mut out = stdout.lock();
                                        if emit::write_line(&mut out, &line).is_err() {
                                            return 0; // downstream pipe closed
                                        }
                                        emitted += 1;
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!("[birdeye] bad JSON on {label}: {e}; skipping poll");
                            }
                        }
                    }
                }
                Err(e) => eprintln!("fetch error ({label}): {e}"),
            }
            std::thread::sleep(std::time::Duration::from_secs_f64(gap));
        }
        eprintln!(
            "[birdeye] pass: emitted {} new (seen {})",
            emitted,
            ring.len()
        );
        if cli.once {
            return 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Value {
        json::parse(s).unwrap()
    }

    // ---------------------------------------------------------- budget math

    #[test]
    fn requests_per_cycle_counts_every_active_mode() {
        assert_eq!(requests_per_cycle(0, 0, 0), 0);
        assert_eq!(requests_per_cycle(3, 0, 0), 3);
        assert_eq!(requests_per_cycle(3, 3, 3), 9);
        assert_eq!(requests_per_cycle(5, 2, 0), 7, "disabled security is free");
    }

    #[test]
    fn budget_floor_is_ceil_and_never_zero_divides() {
        assert_eq!(budget_floor_secs(1, 30), 2, "ceil(60/30)");
        assert_eq!(budget_floor_secs(30, 30), 60);
        assert_eq!(budget_floor_secs(7, 30), 14);
        assert_eq!(budget_floor_secs(0, 30), 2, "empty clamps to 1 request");
        assert_eq!(budget_floor_secs(1, 0), 60, "zero budget clamps to 1/min");
    }

    #[test]
    fn interval_is_clamped_up_to_the_floor_never_down() {
        assert_eq!(cycle_secs(5, None, 30), 10, "default = floor");
        assert_eq!(cycle_secs(5, Some(4), 30), 10, "too fast: clamped");
        assert_eq!(cycle_secs(5, Some(900), 30), 900, "slower is allowed");
    }

    #[test]
    fn gap_spreads_cycle_evenly() {
        assert!((request_gap_secs(10, 5) - 2.0).abs() < 1e-9);
        assert!((request_gap_secs(2, 1) - 2.0).abs() < 1e-9);
        assert!((request_gap_secs(60, 0) - 60.0).abs() < 1e-9, "no zero div");
    }

    #[test]
    fn budget_resolution_flag_beats_env_beats_default() {
        assert_eq!(resolve_budget(Some(5), Some("12")).unwrap(), 5, "flag wins");
        assert_eq!(resolve_budget(None, Some("12")).unwrap(), 12, "env next");
        assert_eq!(resolve_budget(None, Some(" 12 ")).unwrap(), 12, "trimmed");
        assert_eq!(resolve_budget(None, None).unwrap(), BUDGET_PER_MIN);
        assert!(resolve_budget(None, Some("0")).is_err(), "zero refused");
        assert!(resolve_budget(None, Some("fast")).is_err(), "junk refused");
    }

    #[test]
    fn time_range_defaults_to_thirty_days_ending_now() {
        let now = 1_753_228_800; // 2026-07-23T00:00:00Z
        assert_eq!(time_range(None, None, now), (now - 30 * 86_400, now));
        assert_eq!(time_range(Some(7), Some(9), now), (7, 9), "explicit wins");
        assert_eq!(
            time_range(None, Some(100), now),
            (0, 100),
            "lookback from an explicit end saturates at 0, never wraps"
        );
        assert_eq!(time_range(Some(5), None, now), (5, now));
    }

    // --------------------------------------------------------- ohlcv parsing

    const ITEM: &str = r#"{"address":"MintB58pump","unixTime":1750896000,"type":"1D",
        "currency":"usd","o":1.0421,"h":1.1985,"l":0.9873,"c":1.1547,
        "v":182345678.123456,"v_usd":201234567.89}"#;

    #[test]
    fn ohlcv_items_extracts_the_documented_shape() {
        let page = v(&format!(
            r#"{{"success":true,"data":{{"items":[{ITEM}]}}}}"#
        ));
        assert_eq!(ohlcv_items(&page).len(), 1);
        assert!(ohlcv_items(&v(r#"{"data":{"items":[]}}"#)).is_empty());
        assert!(ohlcv_items(&v(r#"{"data":{}}"#)).is_empty());
        assert!(ohlcv_items(&v(r#"{"success":false}"#)).is_empty());
        assert!(ohlcv_items(&v("[1]")).is_empty(), "non-object tolerated");
    }

    #[test]
    fn parse_bar_carries_the_vendor_tokens_verbatim() {
        let item = v(ITEM);
        let bar = parse_bar(&item).unwrap();
        // Raw number TEXT, not a float round-trip: §6.3 raw preservation.
        assert_eq!(bar.t, &Value::Number("1750896000".into()));
        assert_eq!(bar.o, &Value::Number("1.0421".into()));
        assert_eq!(bar.v, &Value::Number("182345678.123456".into()));
        assert_eq!(bar.v_usd, Some(&Value::Number("201234567.89".into())));
    }

    #[test]
    fn parse_bar_skips_malformed_items_never_fabricates() {
        for missing in ["unixTime", "o", "h", "l", "c", "v"] {
            let Value::Object(pairs) = v(ITEM) else {
                unreachable!()
            };
            let item = Value::Object(pairs.into_iter().filter(|(k, _)| k != missing).collect());
            assert!(parse_bar(&item).is_none(), "missing {missing} must skip");
        }
        assert!(parse_bar(&v("42")).is_none(), "non-object");
    }

    #[test]
    fn parse_bar_v_usd_is_optional() {
        let item = v(r#"{"unixTime":1,"o":1,"h":2,"l":0.5,"c":1.5,"v":10}"#);
        let bar = parse_bar(&item).unwrap();
        assert_eq!(bar.v_usd, None);
        let line = ohlcv_line("M", 0, &[bar]);
        assert!(
            !line.contains("v_usd"),
            "absent = omitted, never zero-faked"
        );
    }

    #[test]
    fn ohlcv_mint_prefers_the_item_address_verbatim_case() {
        let page = v(&format!(r#"{{"data":{{"items":[{ITEM}]}}}}"#));
        assert_eq!(ohlcv_items(&page).len(), 1);
        assert_eq!(ohlcv_mint(ohlcv_items(&page), "Watched"), "MintB58pump");
        let bare = v(r#"{"data":{"items":[{"unixTime":1,"o":1,"h":1,"l":1,"c":1,"v":1}]}}"#);
        assert_eq!(
            ohlcv_mint(ohlcv_items(&bare), "WatchedB58"),
            "WatchedB58",
            "no address stated: the watched mint we asked about"
        );
    }

    #[test]
    fn ohlcv_line_exact_shape_and_raw_number_passthrough() {
        let items = [v(ITEM)];
        let bars: Vec<Bar<'_>> = items.iter().filter_map(parse_bar).collect();
        let line = ohlcv_line("MintB58pump", 1000, &bars);
        assert_eq!(
            line,
            "{\"record\":\"birdeye_ohlcv_1d_v1\",\"mint\":\"MintB58pump\",\
             \"observed_unix_ms\":1000,\"bars\":[{\"t\":1750896000,\"o\":1.0421,\
             \"h\":1.1985,\"l\":0.9873,\"c\":1.1547,\"v\":182345678.123456,\
             \"v_usd\":201234567.89}],\"provider\":\"birdeye\",\"interval\":\"1D\",\
             \"quote\":\"usd\"}"
        );
        let parsed = json::parse(&line).unwrap();
        assert_eq!(json::serialize(&parsed), line, "round-trip stable");
    }

    #[test]
    fn ohlcv_line_passes_exotic_number_spellings_and_strings_through() {
        // A vendor exponent spelling or a stringified number must survive
        // BYTE-EXACT — no float math in our code, ever (§6.3).
        let item = v(
            r#"{"unixTime":1750896000,"o":1.2e7,"h":"13000001.5","l":0.00000001,
                        "c":1.2E7,"v":-0.0}"#,
        );
        let bar = parse_bar(&item).unwrap();
        let line = ohlcv_line("M", 0, &[bar]);
        assert!(line.contains("\"o\":1.2e7,"), "{line}");
        assert!(line.contains("\"h\":\"13000001.5\","), "strings verbatim");
        assert!(
            line.contains("\"l\":0.00000001,"),
            "no sci-notation rewrite"
        );
        assert!(line.contains("\"c\":1.2E7,"), "case preserved");
        assert!(line.contains("\"v\":-0.0}"), "negative zero preserved");
    }

    // ----------------------------------------------------- raw-object records

    #[test]
    fn raw_line_carries_the_data_object_untouched() {
        let page = v(
            r#"{"success":true,"data":{"address":"MintB58","price":1.1547,
               "liquidity":8234567.4321,"holder":152341,"unknown_future_field":{"x":[1]}}}"#,
        );
        let data = data_object(&page).unwrap();
        let line = raw_line("birdeye_token_overview_v1", "MintB58", 7, data);
        assert_eq!(
            line,
            format!(
                "{{\"record\":\"birdeye_token_overview_v1\",\"mint\":\"MintB58\",\
                 \"observed_unix_ms\":7,\"raw\":{}}}",
                json::serialize(data)
            )
        );
        // Untouched = key order, unknown fields and number spellings survive.
        assert!(line.contains("\"liquidity\":8234567.4321"));
        assert!(line.contains("\"unknown_future_field\":{\"x\":[1]}"));
        let parsed = json::parse(&line).unwrap();
        assert_eq!(json::serialize(&parsed), line, "round-trip stable");
    }

    #[test]
    fn data_object_rejects_error_bodies() {
        assert!(data_object(&v(r#"{"success":false,"message":"no"}"#)).is_none());
        assert!(data_object(&v(r#"{"data":null}"#)).is_none());
        assert!(data_object(&v(r#"{"data":[1,2]}"#)).is_none(), "non-object");
        assert!(data_object(&v(r#"{"data":{"a":1}}"#)).is_some());
    }

    #[test]
    fn data_mint_prefers_the_stated_address_else_watched() {
        assert_eq!(data_mint(&v(r#"{"address":"AbCd"}"#), "W"), "AbCd");
        assert_eq!(data_mint(&v(r#"{"address":""}"#), "W"), "W", "falsy");
        assert_eq!(data_mint(&v(r#"{"price":1}"#), "W"), "W", "absent");
    }

    #[test]
    fn hostile_mint_strings_stay_valid_json() {
        let line = raw_line("birdeye_token_security_v1", "m\"\\\n", 0, &v(r#"{"a":1}"#));
        let parsed = json::parse(&line).unwrap();
        assert_eq!(
            parsed.get("mint").and_then(Value::as_str),
            Some("m\"\\\n"),
            "escaped and recovered verbatim"
        );
    }

    // ------------------------------------------------------------- dispatch

    #[test]
    fn classify_by_documented_shapes() {
        assert_eq!(
            classify(&v(r#"{"data":{"items":[]},"success":true}"#)),
            Some(PageKind::Ohlcv)
        );
        assert_eq!(
            classify(&v(r#"{"data":{"address":"M","price":1.0}}"#)),
            Some(PageKind::Overview)
        );
        assert_eq!(
            classify(&v(r#"{"data":{"creatorAddress":"C","freezeable":null}}"#)),
            Some(PageKind::Security)
        );
        assert_eq!(classify(&v(r#"{"success":false,"message":"x"}"#)), None);
        assert_eq!(classify(&v(r#"{"data":null}"#)), None);
        assert_eq!(classify(&v("[]")), None);
    }

    #[test]
    fn shape_target_fingerprints_the_right_object() {
        let o = v(r#"{"data":{"items":[{"unixTime":1,"o":1}]}}"#);
        assert_eq!(
            shape_target(PageKind::Ohlcv, &o),
            Some(&v(r#"{"unixTime":1,"o":1}"#))
        );
        let t = v(r#"{"data":{"price":1,"holder":2}}"#);
        assert_eq!(
            shape_target(PageKind::Overview, &t),
            Some(&v(r#"{"price":1,"holder":2}"#))
        );
        assert_eq!(
            shape_target(PageKind::Ohlcv, &v(r#"{"data":{"items":[]}}"#)),
            None,
            "empty candle set: nothing to fingerprint"
        );
    }

    #[test]
    fn record_tags_and_labels_are_fixed() {
        assert_eq!(record_tag(PageKind::Ohlcv), "birdeye_ohlcv_1d_v1");
        assert_eq!(record_tag(PageKind::Overview), "birdeye_token_overview_v1");
        assert_eq!(record_tag(PageKind::Security), "birdeye_token_security_v1");
        assert_eq!(kind_label(PageKind::Ohlcv), "ohlcv");
        assert_eq!(kind_label(PageKind::Overview), "token_overview");
        assert_eq!(kind_label(PageKind::Security), "token_security");
    }

    // ---------------------------------------------------- content-keyed dedupe

    #[test]
    fn identical_payloads_dedupe_across_polls_stamps_ignored() {
        let page = v(&format!(r#"{{"data":{{"items":[{ITEM}]}}}}"#));
        let (_, id1) = page_line(PageKind::Ohlcv, &page, "W", 1000).unwrap();
        let (_, id2) = page_line(PageKind::Ohlcv, &page, "W", 2000).unwrap();
        assert_eq!(id1, id2, "only the stamp differed: one emission");
        let mut ring = dedupe::DedupeRing::new(8);
        assert!(ring.insert(&id1));
        assert!(!ring.insert(&id2), "unchanged candles are a quiet poll");
    }

    #[test]
    fn changed_payloads_re_emit() {
        let a = v(r#"{"data":{"address":"M","price":1.0}}"#);
        let b = v(r#"{"data":{"address":"M","price":1.1}}"#);
        let (_, ida) = page_line(PageKind::Overview, &a, "", 0).unwrap();
        let (_, idb) = page_line(PageKind::Overview, &b, "", 0).unwrap();
        assert_ne!(ida, idb, "a moved price is a new observation");
        assert!(ida.starts_with("token_overview:M:"), "{ida}");
    }

    #[test]
    fn page_line_quiet_polls() {
        assert!(
            page_line(PageKind::Ohlcv, &v(r#"{"data":{"items":[]}}"#), "W", 0).is_none(),
            "no candles in range = quiet, never an error"
        );
        assert!(
            page_line(
                PageKind::Ohlcv,
                &v(r#"{"data":{"items":[{"unixTime":1}]}}"#),
                "W",
                0
            )
            .is_none(),
            "only malformed candles = quiet"
        );
        assert!(
            page_line(PageKind::Overview, &v(r#"{"success":false}"#), "W", 0).is_none(),
            "error body = quiet"
        );
    }

    #[test]
    fn security_page_line_uses_the_watched_mint() {
        // token_security bodies carry no address: live mode stamps the
        // watched mint from the round-robin.
        let page = v(r#"{"data":{"creatorAddress":"C","top10HolderPercent":0.2}}"#);
        let (line, id) = page_line(PageKind::Security, &page, "WatchedB58", 5).unwrap();
        assert!(line.starts_with(
            "{\"record\":\"birdeye_token_security_v1\",\"mint\":\"WatchedB58\",\
             \"observed_unix_ms\":5,\"raw\":{"
        ));
        assert!(id.starts_with("token_security:WatchedB58:"), "{id}");
    }

    // ------------------------------------------------------------------ CLI

    #[test]
    fn cli_parses_all_modes_and_rejects_junk() {
        let args: Vec<String> = [
            "--ohlcv-watch",
            "a.txt",
            "--overview-watch",
            "b.txt",
            "--security-watch",
            "c.txt",
            "--time-from",
            "1750000000",
            "--time-to",
            "1753000000",
            "--interval-secs",
            "600",
            "--budget-per-min",
            "10",
            "--once",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let cli = Cli::parse(&args).unwrap();
        assert_eq!(cli.ohlcv_watch, "a.txt");
        assert_eq!(cli.overview_watch, "b.txt");
        assert_eq!(cli.security_watch, "c.txt");
        assert_eq!(cli.time_from, Some(1_750_000_000));
        assert_eq!(cli.time_to, Some(1_753_000_000));
        assert_eq!(cli.interval_secs, Some(600));
        assert_eq!(cli.budget_per_min, Some(10));
        assert!(cli.once);
        assert!(Cli::parse(&["--bogus".to_string()]).is_err());
        assert!(
            Cli::parse(&["--ohlcv-watch".to_string()]).is_err(),
            "needs value"
        );
        assert!(
            Cli::parse(&["--time-from".to_string(), "soon".to_string()]).is_err(),
            "junk timestamp rejected"
        );
        assert!(
            Cli::parse(&["--budget-per-min".to_string(), "0".to_string()]).is_err(),
            "zero budget rejected"
        );
    }
}
