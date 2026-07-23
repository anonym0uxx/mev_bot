//! `pump` subcommand — the Pump.fun-NATIVE social capture lane (per-coin reply
//! threads → normalized SocialEvent NDJSON, tier-3 `[S]`).
//!
//! There is NO official pump.fun data API. This lane polls the undocumented
//! frontend feed (`frontend-api-v3.pump.fun/replies/{mint}`) that the pump.fun
//! web UI itself uses — a **tier-3 source**: reverse-engineered, unversioned
//! for us, historically churned v1→v2→v3, sits behind Cloudflare, and can be
//! revoked or reshaped without notice. Because of that, degradation detection
//! is FIRST-CLASS here (§18.8), not an afterthought:
//!
//! * **shape sentinel** — an FNV-1a hash over the sorted top-level key names
//!   of the first reply object, per endpoint; on change we log `SCHEMA_DRIFT`
//!   with old/new hashes to stderr and KEEP RUNNING on the tolerant parser;
//! * **status-class sentinel** — the HTTP status class per endpoint; a class
//!   change (2xx→5xx, …) is logged so the supervisor sees the trend;
//! * **challenge wall** — an HTML body or Cloudflare challenge markers where
//!   JSON belongs means the WAF interposed: log `CHALLENGE_WALL`, back off
//!   [`CHALLENGE_BACKOFF_SECS`] (5 min), keep running;
//! * **auth wall** — 401/403 without a challenge page means anonymous reads
//!   were revoked: log `AUTH_WALL` and exit with the DISTINCT code 3 so the
//!   supervisor sees the capability loss loudly (§18.8 — silent degradation
//!   is the one unforgivable failure).
//!
//! Absence of data is NEVER an error: an empty replies array is a quiet poll.
//!
//! The emitted event is the shared `normalize.py` schema (platform `"pump"`,
//! author = the replying wallet lowercased, community = the coin's mint
//! VERBATIM — base58 is case-sensitive, engagement zeros, `echo: false`) plus
//! the capture stamp, PLUS one extra trailing field `"mint"`: the thread's
//! mint. The thread context IS a mint-grade coin reference — stronger than
//! any ticker in the text — so it is carried explicitly for the core.
//!
//! Request budget: at most [`BUDGET_PER_MIN`] requests/minute across ALL
//! watched mints, round-robin. `--interval-secs` (full-cycle seconds) defaults
//! to the budget floor for the watchlist size and is clamped up to it — the
//! lane can be slower than the budget, never faster. `--live-list` reserves
//! one request/minute for `GET /coins/currently-live` liveness logging.
//!
//! `[S]` discipline as everywhere: capture only — no decision, no sentiment
//! label (§83); provenance carried verbatim (§29); `--replay` is a pure
//! function of the fixture file (§22).

use crate::json::{self, py_str, py_truthy, Value};
use crate::{dedupe, emit, http};

/// The one replies endpoint the pump.fun web UI itself polls (tier-3,
/// reverse-engineered; v1 and v2 hosts are dead — churn is expected).
pub const BASE: &str = "https://frontend-api-v3.pump.fun/replies";

/// The currently-live coins endpoint (same tier-3 host), polled only with
/// `--live-list` for stderr liveness diagnostics — never emitted.
pub const LIVE_URL: &str = "https://frontend-api-v3.pump.fun/coins/currently-live";

/// Same timeout as the other REST lanes (`urlopen(..., timeout=30)` twins).
const TIMEOUT_SECS: u64 = 30;

/// Global request budget across ALL watched mints: ≤20 requests/minute — the
/// empirically safe anonymous ceiling under pump.fun's Cloudflare WAF.
pub const BUDGET_PER_MIN: u64 = 20;

/// `--live-list` reserves this many requests/minute out of [`BUDGET_PER_MIN`]
/// for the currently-live poll (once per 60 s).
pub const LIVE_RESERVED_PER_MIN: u64 = 1;

/// Cloudflare-challenge backoff: 5 minutes. A challenge wall is a WAF mood,
/// not a schema change — wait it out, then resume.
pub const CHALLENGE_BACKOFF_SECS: u64 = 300;

/// Distinct exit code for the auth wall (anonymous reads revoked) — the
/// supervisor must see this capability loss loudly, not as a generic failure.
pub const EXIT_AUTH_WALL: u8 = 3;

// ---------------------------------------------------------------- budget math

/// Minimum seconds one full round-robin cycle over `n_mints` may take while
/// staying under the global budget (ceil division — never round below the
/// floor). `live_list` reserves [`LIVE_RESERVED_PER_MIN`] req/min.
#[must_use]
pub fn budget_floor_secs(n_mints: usize, live_list: bool) -> u64 {
    let budget = if live_list {
        BUDGET_PER_MIN - LIVE_RESERVED_PER_MIN
    } else {
        BUDGET_PER_MIN
    };
    let n = n_mints.max(1) as u64;
    (n * 60).div_ceil(budget)
}

/// Effective cycle seconds: the operator's `--interval-secs` ask, clamped UP
/// to the budget floor (the lane can run slower than the budget, never
/// faster). `None` = default = exactly the floor.
#[must_use]
pub fn cycle_secs(n_mints: usize, requested: Option<u64>, live_list: bool) -> u64 {
    let floor = budget_floor_secs(n_mints, live_list);
    match requested {
        Some(r) => r.max(floor),
        None => floor,
    }
}

/// Pacing gap between consecutive requests inside a cycle (round-robin: the
/// cycle is spread evenly over the watchlist).
#[must_use]
pub fn request_gap_secs(cycle: u64, n_mints: usize) -> f64 {
    cycle as f64 / n_mints.max(1) as f64
}

// -------------------------------------------------------- degradation sentinel

/// FNV-1a 64-bit over raw bytes — tiny, dependency-free, stable across runs
/// (§22: pure; no `DefaultHasher`, whose seed is unspecified).
#[must_use]
pub fn fnv1a64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Response SHAPE HASH: FNV-1a over the SORTED top-level key names of one
/// reply object, NUL-separated (order-independent — vendors reorder keys
/// freely; a reorder is not drift, a key-set change is). `None` for
/// non-objects (nothing to fingerprint).
#[must_use]
pub fn shape_hash(first: &Value) -> Option<u64> {
    let Value::Object(pairs) = first else {
        return None;
    };
    let mut keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
    keys.sort_unstable();
    keys.dedup();
    let mut bytes = Vec::new();
    for k in keys {
        bytes.extend_from_slice(k.as_bytes());
        bytes.push(0);
    }
    Some(fnv1a64(&bytes))
}

/// Cloudflare-challenge detection on a JSON endpoint: an HTML content-type,
/// an HTML body prefix, or (only when the body does not even look like JSON —
/// a reply whose TEXT mentions these markers must not trip the wall)
/// Cloudflare challenge markers.
#[must_use]
pub fn is_challenge(content_type: &str, body: &str) -> bool {
    if content_type.to_ascii_lowercase().contains("text/html") {
        return true;
    }
    let t = body.trim_start();
    if t.starts_with('<') {
        return true;
    }
    let looks_json = t.starts_with('{') || t.starts_with('[');
    !looks_json
        && ["cf-chl", "Just a moment", "Attention Required"]
            .iter()
            .any(|m| body.contains(m))
}

/// Per-endpoint degradation state — pure state machine (§22): the caller
/// observes, the sentinel reports transitions, the caller logs.
#[derive(Default)]
pub struct Sentinel {
    shape: Option<u64>,
    status_class: Option<u8>,
}

impl Sentinel {
    /// Record a shape hash; `Some(old_hash)` exactly when it CHANGED
    /// (schema drift). First observation is baseline, not drift.
    pub fn observe_shape(&mut self, h: u64) -> Option<u64> {
        let old = self.shape;
        self.shape = Some(h);
        match old {
            Some(prev) if prev != h => Some(prev),
            _ => None,
        }
    }

    /// Record an HTTP status; `Some((old_class, new_class))` exactly when the
    /// status CLASS changed (2xx→5xx, …). First observation is baseline.
    pub fn observe_status(&mut self, code: u16) -> Option<(u8, u8)> {
        let class = (code / 100) as u8;
        let old = self.status_class;
        self.status_class = Some(class);
        match old {
            Some(prev) if prev != class => Some((prev, class)),
            _ => None,
        }
    }
}

// -------------------------------------------------------------- normalization

/// One normalized pump.fun reply, ready for emission.
#[derive(Debug, PartialEq, Eq)]
pub struct Reply {
    /// Vendor reply id (the dedupe key) — REQUIRED; no id = malformed = skip.
    pub id: String,
    /// Replying wallet, lowercased (the lane's author-identity convention;
    /// the raw case-sensitive wallet is not an emitted field on this lane).
    pub author: String,
    /// The thread's mint, base58 VERBATIM case (mints are case-sensitive).
    pub mint: String,
    /// Raw reply text — tickers and contract addresses left intact.
    pub text: String,
}

impl Reply {
    /// Borrow as the shared emission event (platform `"pump"`, community =
    /// the mint, zero engagement — the endpoint carries none we trust —
    /// `echo: false`: a thread reply IS the origination unit on this surface).
    #[must_use]
    pub fn event(&self) -> emit::Event<'_> {
        emit::Event {
            platform: "pump",
            author: &self.author,
            community: &self.mint,
            text: &self.text,
            likes: 0,
            reposts: 0,
            replies: 0,
            echo: false,
            is_designated_caller: false,
        }
    }

    /// The full NDJSON line: the shared schema + capture stamp, then the ONE
    /// pump-specific trailing field `"mint"` — thread context is a mint-grade
    /// coin reference, stronger than any ticker in the text.
    #[must_use]
    pub fn line(&self, observed_at_ns: u64) -> String {
        let mut line = emit::event_line(&self.event(), observed_at_ns);
        line.pop(); // strip the closing '}'
        line.push_str(",\"mint\":\"");
        emit::escape_json_into(&self.mint, &mut line);
        line.push_str("\"}");
        line
    }
}

/// One vendor reply object → normalized reply, or `None` for a malformed
/// entry (not an object, no truthy `id`, or no string `text`) — malformed
/// entries are SKIPPED, never fabricated (§18.8: tolerate unknown fields,
/// refuse to invent missing ones).
#[must_use]
pub fn normalize_reply(v: &Value, thread_mint: &str) -> Option<Reply> {
    if !matches!(v, Value::Object(_)) {
        return None;
    }
    let id = v.get("id").filter(|x| py_truthy(x)).map(py_str)?;
    let text = v.get("text").and_then(Value::as_str)?.to_string();
    let author = v
        .get("user")
        .filter(|x| py_truthy(x))
        .map(py_str)
        .unwrap_or_else(|| "unknown".to_string())
        .to_lowercase();
    let mint = v
        .get("mint")
        .filter(|x| py_truthy(x))
        .map(py_str)
        .unwrap_or_else(|| thread_mint.to_string());
    Some(Reply {
        id,
        author,
        mint,
        text,
    })
}

/// Extract the reply list from a response — the endpoint has shipped BOTH a
/// bare JSON array and a `{"replies": [...]}` wrapper; handle both, and treat
/// anything else as empty (absence of data is never an error).
#[must_use]
pub fn extract_replies(data: &Value) -> &[Value] {
    if let Some(arr) = data.get("replies").and_then(Value::as_array) {
        return arr;
    }
    data.as_array().unwrap_or_default()
}

/// Mints named live by a `currently-live` response (bare array or wrapped
/// under `coins`/`data`), for the stderr liveness log only.
#[must_use]
pub fn extract_live_mints(data: &Value) -> Vec<String> {
    let arr = data
        .get("coins")
        .and_then(Value::as_array)
        .or_else(|| data.get("data").and_then(Value::as_array))
        .or_else(|| data.as_array())
        .unwrap_or_default();
    arr.iter()
        .filter_map(|c| c.get("mint").filter(|m| py_truthy(m)).map(py_str))
        .collect()
}

/// Parse a watchlist file body: one base58 mint per line, `#` comments and
/// blank lines skipped, duplicates dropped (order preserved).
#[must_use]
pub fn parse_mints(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let m = line.trim();
        if m.is_empty() || m.starts_with('#') || out.iter().any(|x| x == m) {
            continue;
        }
        out.push(m.to_string());
    }
    out
}

// ------------------------------------------------------------------------ CLI

/// Parsed CLI for this subcommand.
pub struct Cli {
    /// `--mints-file`: watchlist path (required in live mode).
    pub mints_file: String,
    /// `--interval-secs`: full round-robin cycle; default = the budget floor
    /// for the list size, and always clamped up to it.
    pub interval_secs: Option<u64>,
    /// `--live-list`: also poll `coins/currently-live` once per 60 s and log
    /// (stderr) which watched mints are live.
    pub live_list: bool,
    /// `--once`: exactly one round-robin pass, then exit — the Phase-B
    /// anonymous-read probe mode.
    pub once: bool,
    /// `--replay <fixture>`: deterministic offline mode (§22).
    pub replay: Option<String>,
}

impl Cli {
    /// Parse subcommand arguments.
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut cli = Self {
            mints_file: String::new(),
            interval_secs: None,
            live_list: false,
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
            match a.as_str() {
                "--mints-file" => cli.mints_file = val("--mints-file")?,
                "--interval-secs" => {
                    cli.interval_secs = Some(
                        val("--interval-secs")?
                            .parse()
                            .map_err(|e| format!("bad --interval-secs: {e}"))?,
                    );
                }
                "--live-list" => cli.live_list = true,
                "--once" => cli.once = true,
                "--replay" => cli.replay = Some(val("--replay")?),
                other => return Err(format!("unknown flag {other:?}")),
            }
        }
        Ok(cli)
    }
}

// ---------------------------------------------------------------------- runs

/// `--replay`: pump has its own driver (not [`crate::replay_pages`]) because
/// its NDJSON carries the extra trailing `"mint"` field and its sentinel is
/// exercised deterministically across fixture pages. Zero network, zero wall
/// clock; the thread-mint fallback is empty (fixture replies carry their own
/// `mint`, as the real endpoint's do).
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
    use std::io::Write as _;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut ring = dedupe::DedupeRing::new(dedupe::DEFAULT_CAP);
    let mut sentinel = Sentinel::default();
    let mut emitted: u64 = 0;
    for page in &pages {
        let items = extract_replies(page);
        if let Some(h) = items.first().and_then(shape_hash) {
            if let Some(old) = sentinel.observe_shape(h) {
                eprintln!("[pump] SCHEMA_DRIFT replies: shape {old:016x} -> {h:016x} (tolerant parser continues)");
            }
        }
        for item in items {
            let Some(reply) = normalize_reply(item, "") else {
                continue;
            };
            if !ring.insert(&reply.id) {
                continue;
            }
            let ts = crate::REPLAY_BASE_NS + emitted * crate::REPLAY_STEP_NS;
            let _ = out.write_all(reply.line(ts).as_bytes());
            let _ = out.write_all(b"\n");
            let _ = out.flush();
            emitted += 1;
        }
    }
    eprintln!("[pq-social-capture] replay: emitted {emitted} events");
    0
}

/// Outcome of one live poll, after the sentinel has spoken.
enum PollOutcome {
    /// Keep going (data handled, or a logged-and-skipped anomaly).
    Continue,
    /// Cloudflare challenge wall — the caller backs off 5 minutes.
    Challenge,
    /// Anonymous reads revoked — the caller exits with code 3.
    AuthWall,
}

/// Classify + process one endpoint response through the degradation sentinel.
/// `on_json` handles the parsed body of a healthy 2xx response (it receives
/// the sentinel back for the shape observation).
fn handle_response(
    label: &str,
    meta: &http::Meta,
    sentinel: &mut Sentinel,
    on_json: impl FnOnce(&Value, &mut Sentinel),
) -> PollOutcome {
    // Challenge BEFORE auth: Cloudflare serves its challenge page WITH a 403,
    // and a challenge is a temporary wall, not a revocation.
    if is_challenge(&meta.content_type, &meta.body) {
        eprintln!(
            "[pump] CHALLENGE_WALL {label}: HTTP {} with challenge page; backing off {CHALLENGE_BACKOFF_SECS}s",
            meta.status
        );
        return PollOutcome::Challenge;
    }
    if meta.status == 401 || meta.status == 403 {
        eprintln!(
            "[pump] AUTH_WALL {label}: HTTP {} — anonymous reads revoked; exiting {EXIT_AUTH_WALL}",
            meta.status
        );
        return PollOutcome::AuthWall;
    }
    if let Some((old, new)) = sentinel.observe_status(meta.status) {
        eprintln!("[pump] STATUS_CLASS_DRIFT {label}: {old}xx -> {new}xx");
    }
    if !(200..300).contains(&meta.status) {
        eprintln!("[pump] HTTP {} on {label}; skipping poll", meta.status);
        return PollOutcome::Continue;
    }
    match json::parse(&meta.body) {
        Ok(v) => on_json(&v, sentinel),
        Err(e) => eprintln!("[pump] bad JSON on {label}: {e}; skipping poll"),
    }
    PollOutcome::Continue
}

/// Subcommand entry. `now_ns` is the capture-edge clock injected by `main.rs`
/// (§22 — this module never reads a wall clock itself).
pub fn run(args: &[String], now_ns: fn() -> u64) -> u8 {
    let cli = match Cli::parse(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[pq-social-capture] pump: {e}");
            return 2;
        }
    };

    if let Some(path) = &cli.replay {
        return replay(path);
    }

    if cli.mints_file.is_empty() {
        eprintln!("error: pump needs --mints-file (one base58 mint per line)");
        return 2;
    }
    let mints = match std::fs::read_to_string(&cli.mints_file) {
        Ok(t) => parse_mints(&t),
        Err(e) => {
            eprintln!("error: cannot read --mints-file {}: {e}", cli.mints_file);
            return 2;
        }
    };
    if mints.is_empty() {
        eprintln!("error: {} lists no mints", cli.mints_file);
        return 2;
    }

    let cycle = cycle_secs(mints.len(), cli.interval_secs, cli.live_list);
    let gap = request_gap_secs(cycle, mints.len());
    eprintln!(
        "[pump] watching {} mints; cycle {}s, gap {}s (budget <= {} req/min{})",
        mints.len(),
        cycle,
        crate::py_float(gap),
        BUDGET_PER_MIN,
        if cli.live_list {
            ", 1 reserved for currently-live"
        } else {
            ""
        }
    );

    let http = http::Http::new(TIMEOUT_SECS);
    let mut ring = dedupe::DedupeRing::new(dedupe::DEFAULT_CAP);
    let mut replies_sentinel = Sentinel::default();
    let mut live_sentinel = Sentinel::default();
    // Clock-free 60 s scheduler for --live-list: accumulate slept seconds
    // (§22 — no wall-clock read behind the capture boundary).
    let mut slept_since_live = 60.0f64; // fire on the first opportunity
    let stdout = std::io::stdout();

    loop {
        let mut emitted = 0u64;
        for mint in &mints {
            let url = format!("{BASE}/{mint}?limit=50&offset=0&reverseOrder=true");
            match http.get_meta(&url, &[("Accept", "application/json")]) {
                Ok(meta) => {
                    let mut broken = false;
                    let outcome =
                        handle_response(mint, &meta, &mut replies_sentinel, |page, sentinel| {
                            let items = extract_replies(page);
                            if let Some(h) = items.first().and_then(shape_hash) {
                                if let Some(old) = sentinel.observe_shape(h) {
                                    eprintln!(
                                        "[pump] SCHEMA_DRIFT replies: shape {old:016x} -> \
                                         {h:016x} (tolerant parser continues)"
                                    );
                                }
                            }
                            let mut out = stdout.lock();
                            for item in items {
                                let Some(reply) = normalize_reply(item, mint) else {
                                    continue;
                                };
                                if !ring.insert(&reply.id) {
                                    continue;
                                }
                                if emit::write_line(&mut out, &reply.line(now_ns())).is_err() {
                                    broken = true; // downstream pipe closed
                                    return;
                                }
                                emitted += 1;
                            }
                        });
                    if broken {
                        return 0;
                    }
                    match outcome {
                        PollOutcome::Continue => {}
                        PollOutcome::Challenge => {
                            std::thread::sleep(std::time::Duration::from_secs(
                                CHALLENGE_BACKOFF_SECS,
                            ));
                            slept_since_live += CHALLENGE_BACKOFF_SECS as f64;
                        }
                        PollOutcome::AuthWall => return EXIT_AUTH_WALL,
                    }
                }
                Err(e) => eprintln!("fetch error ({mint}): {e}"),
            }
            std::thread::sleep(std::time::Duration::from_secs_f64(gap));
            slept_since_live += gap;

            if cli.live_list && slept_since_live >= 60.0 {
                slept_since_live -= 60.0;
                match http.get_meta(LIVE_URL, &[("Accept", "application/json")]) {
                    Ok(meta) => {
                        let outcome =
                            handle_response("currently-live", &meta, &mut live_sentinel, |v, _| {
                                let live = extract_live_mints(v);
                                let watched: Vec<&str> = mints
                                    .iter()
                                    .filter(|m| live.iter().any(|l| &l == m))
                                    .map(String::as_str)
                                    .collect();
                                if watched.is_empty() {
                                    eprintln!("[pump] live: none of the watched mints");
                                } else {
                                    eprintln!("[pump] live: {}", watched.join(" "));
                                }
                            });
                        match outcome {
                            PollOutcome::Continue => {}
                            PollOutcome::Challenge => {
                                std::thread::sleep(std::time::Duration::from_secs(
                                    CHALLENGE_BACKOFF_SECS,
                                ));
                                slept_since_live += CHALLENGE_BACKOFF_SECS as f64;
                            }
                            PollOutcome::AuthWall => return EXIT_AUTH_WALL,
                        }
                    }
                    Err(e) => eprintln!("fetch error (currently-live): {e}"),
                }
            }
        }
        eprintln!("[pump] pass: emitted {} new (seen {})", emitted, ring.len());
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

    // -------------------------------------------------------- normalization

    #[test]
    fn normalize_basic_reply() {
        let r = normalize_reply(
            &v(
                r#"{"id":101,"mint":"9BB6NFEcjBCtnNLFko2FqVQBq8HHM13kCyYcdQbgpump",
                   "text":"dev is based","user":"7XkAcCpQ","timestamp":1753120000000}"#,
            ),
            "",
        )
        .unwrap();
        assert_eq!(
            r,
            Reply {
                id: "101".into(),
                author: "7xkaccpq".into(),
                mint: "9BB6NFEcjBCtnNLFko2FqVQBq8HHM13kCyYcdQbgpump".into(),
                text: "dev is based".into(),
            }
        );
    }

    #[test]
    fn author_wallet_is_lowercased_but_mint_keeps_verbatim_case() {
        let r = normalize_reply(
            &v(r#"{"id":"1","mint":"AbCdEfPump","text":"t","user":"WaLLetB58"}"#),
            "",
        )
        .unwrap();
        assert_eq!(r.author, "walletb58");
        assert_eq!(r.mint, "AbCdEfPump", "mints are case-sensitive: verbatim");
    }

    #[test]
    fn missing_user_is_unknown_and_missing_mint_falls_back_to_thread() {
        let r = normalize_reply(&v(r#"{"id":"1","text":"t"}"#), "ThreadMint").unwrap();
        assert_eq!(r.author, "unknown");
        assert_eq!(r.mint, "ThreadMint");
        let r = normalize_reply(&v(r#"{"id":"1","text":"t","mint":""}"#), "TM").unwrap();
        assert_eq!(r.mint, "TM", "empty mint is falsy: thread fallback");
    }

    #[test]
    fn malformed_entries_are_skipped_not_fabricated() {
        assert_eq!(normalize_reply(&v("42"), ""), None, "non-object");
        assert_eq!(normalize_reply(&v(r#"[1]"#), ""), None, "array entry");
        assert_eq!(
            normalize_reply(&v(r#"{"text":"no id here"}"#), ""),
            None,
            "missing id"
        );
        assert_eq!(
            normalize_reply(&v(r#"{"id":0,"text":"t"}"#), ""),
            None,
            "falsy id"
        );
        assert_eq!(
            normalize_reply(&v(r#"{"id":"1","text":123}"#), ""),
            None,
            "non-string text"
        );
        assert_eq!(
            normalize_reply(&v(r#"{"id":"1","user":"u"}"#), ""),
            None,
            "missing text"
        );
    }

    #[test]
    fn unknown_fields_are_tolerated() {
        let r = normalize_reply(
            &v(
                r#"{"id":"1","text":"t","user":"u","mint":"M","total_likes":9,
                   "profile_image":null,"username":"deg","hidden":false,
                   "some_future_field":{"x":[1,2]}}"#,
            ),
            "",
        );
        assert!(r.is_some());
    }

    #[test]
    fn extract_handles_bare_array_and_wrapped_object() {
        assert_eq!(extract_replies(&v(r#"[{"id":"1"},{"id":"2"}]"#)).len(), 2);
        assert_eq!(
            extract_replies(&v(r#"{"replies":[{"id":"1"}],"hasMore":false}"#)).len(),
            1
        );
        assert_eq!(extract_replies(&v(r#"{"error":"x"}"#)).len(), 0);
        assert_eq!(extract_replies(&v(r#"[]"#)).len(), 0, "empty = quiet poll");
    }

    #[test]
    fn reply_line_appends_mint_last_and_stays_valid_json() {
        let r = Reply {
            id: "1".into(),
            author: "w".into(),
            mint: "MintB58".into(),
            text: "gm \"fren\"".into(),
        };
        let line = r.line(42);
        assert_eq!(
            line,
            "{\"platform\":\"pump\",\"author\":\"w\",\"community\":\"MintB58\",\
             \"text\":\"gm \\\"fren\\\"\",\"likes\":0,\"reposts\":0,\"replies\":0,\
             \"echo\":false,\"observed_at_ns\":42,\"mint\":\"MintB58\"}"
        );
        let parsed = json::parse(&line).unwrap();
        assert_eq!(json::serialize(&parsed), line, "round-trip stable");
    }

    // ----------------------------------------------------- dedupe by reply id

    #[test]
    fn reply_id_dedupe_across_polls() {
        let mut ring = dedupe::DedupeRing::new(8);
        let a = normalize_reply(&v(r#"{"id":101,"text":"a"}"#), "M").unwrap();
        let dup = normalize_reply(&v(r#"{"id":"101","text":"edited"}"#), "M").unwrap();
        assert!(ring.insert(&a.id));
        assert!(
            !ring.insert(&dup.id),
            "same id, numeric vs string spelling, is one reply"
        );
    }

    // ------------------------------------------------------------- sentinel

    #[test]
    fn fnv1a64_known_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x85944171f73967e8);
    }

    #[test]
    fn shape_hash_is_key_order_independent_but_key_set_sensitive() {
        let a = shape_hash(&v(r#"{"id":1,"mint":"m","text":"t"}"#)).unwrap();
        let b = shape_hash(&v(r#"{"text":"x","id":9,"mint":"q"}"#)).unwrap();
        assert_eq!(a, b, "reorder + value change is NOT drift");
        let c = shape_hash(&v(r#"{"id":1,"mint":"m","body":"t"}"#)).unwrap();
        assert_ne!(a, c, "renamed key IS drift");
        let d = shape_hash(&v(r#"{"id":1,"mint":"m","text":"t","new_field":0}"#)).unwrap();
        assert_ne!(a, d, "added key IS drift");
    }

    #[test]
    fn shape_hash_of_non_object_is_none() {
        assert_eq!(shape_hash(&v("[1,2]")), None);
        assert_eq!(shape_hash(&v("\"s\"")), None);
        assert_eq!(shape_hash(&v("null")), None);
    }

    #[test]
    fn sentinel_reports_shape_drift_once_per_change() {
        let mut s = Sentinel::default();
        assert_eq!(s.observe_shape(10), None, "baseline is not drift");
        assert_eq!(s.observe_shape(10), None, "stable shape is quiet");
        assert_eq!(s.observe_shape(20), Some(10), "change reports the old hash");
        assert_eq!(s.observe_shape(20), None, "new shape becomes the baseline");
        assert_eq!(s.observe_shape(10), Some(20), "reverting is drift again");
    }

    #[test]
    fn sentinel_reports_status_class_changes_only() {
        let mut s = Sentinel::default();
        assert_eq!(s.observe_status(200), None);
        assert_eq!(s.observe_status(204), None, "same class is quiet");
        assert_eq!(s.observe_status(503), Some((2, 5)));
        assert_eq!(s.observe_status(500), None);
        assert_eq!(s.observe_status(200), Some((5, 2)), "recovery is visible");
    }

    #[test]
    fn challenge_detection_html_and_markers() {
        assert!(is_challenge(
            "text/html; charset=utf-8",
            "<!DOCTYPE html>..."
        ));
        assert!(is_challenge("application/json", "  <html><body>cf</body>"));
        assert!(is_challenge(
            "text/plain",
            "error code: 1020 cf-chl-bypass denied"
        ));
        assert!(is_challenge("text/plain", "Just a moment..."));
    }

    #[test]
    fn challenge_markers_inside_json_reply_text_do_not_trip_the_wall() {
        assert!(!is_challenge(
            "application/json",
            r#"[{"id":1,"text":"lol Just a moment guys, cf-chl who?"}]"#
        ));
        assert!(!is_challenge("application/json", r#"{"replies":[]}"#));
    }

    // ---------------------------------------------------------- budget math

    #[test]
    fn budget_floor_is_ceil_of_list_size_over_budget() {
        assert_eq!(budget_floor_secs(1, false), 3, "60/20 = 3s per request");
        assert_eq!(budget_floor_secs(7, false), 21);
        assert_eq!(budget_floor_secs(20, false), 60);
        assert_eq!(budget_floor_secs(0, false), 3, "empty clamps to 1 mint");
    }

    #[test]
    fn live_list_reserves_one_request_per_minute() {
        assert_eq!(budget_floor_secs(19, true), 60, "19 mints on 19 req/min");
        assert_eq!(budget_floor_secs(7, true), 23, "ceil(420/19)");
    }

    #[test]
    fn interval_is_clamped_up_to_the_budget_floor_never_down() {
        assert_eq!(cycle_secs(5, None, false), 15, "default = floor");
        assert_eq!(cycle_secs(5, Some(10), false), 15, "too fast: clamped");
        assert_eq!(cycle_secs(5, Some(100), false), 100, "slower is allowed");
    }

    #[test]
    fn request_gap_spreads_cycle_evenly() {
        assert!((request_gap_secs(21, 7) - 3.0).abs() < 1e-9);
        assert!((request_gap_secs(60, 20) - 3.0).abs() < 1e-9);
        assert!((request_gap_secs(3, 1) - 3.0).abs() < 1e-9);
    }

    // ------------------------------------------------------------ watchlist

    #[test]
    fn mints_file_parsing_skips_comments_blanks_and_duplicates() {
        let text = "# watchlist\n9BB6pump\n\n  EPjFmint  \n9BB6pump\n# tail\n";
        assert_eq!(parse_mints(text), ["9BB6pump", "EPjFmint"]);
        assert!(parse_mints("").is_empty());
    }

    // ------------------------------------------------------------ live list

    #[test]
    fn live_mints_extract_from_bare_and_wrapped_shapes() {
        assert_eq!(
            extract_live_mints(&v(r#"[{"mint":"A","name":"x"},{"mint":"B"}]"#)),
            ["A", "B"]
        );
        assert_eq!(extract_live_mints(&v(r#"{"coins":[{"mint":"C"}]}"#)), ["C"]);
        assert_eq!(extract_live_mints(&v(r#"{"data":[{"mint":"D"}]}"#)), ["D"]);
        assert!(extract_live_mints(&v(r#"{"error":1}"#)).is_empty());
    }
}
