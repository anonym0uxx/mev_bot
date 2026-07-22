//! `coingecko` subcommand — the AGGREGATOR-LEGIBILITY capture lane (CoinGecko
//! REST API → normalized SocialEvent NDJSON, `[S]`, explicitly LATE-tier).
//!
//! CoinGecko is a **legibility clock, not an earliness source**. A pump.fun /
//! letsbonk memecoin exists on-chain for hours-to-days before any aggregator
//! sees it; by the time CoinGecko lists a mint the coin has crossed the
//! aggregator's inclusion bar — exchange tickers, a maintained listing, retail
//! discoverability. Per the pre-legibility doctrine that is a WAVE-TIMING /
//! late-corroboration fact, never an entry signal. What this lane captures:
//!
//! * `--trending` — `GET /search/trending`: the retail search-attention board
//!   (top-15 coins + top-6 categories by search popularity, ~10 min cache).
//!   A watched memecoin appearing here is a LATE retail-attention event.
//! * `--contract-watch <mints-file>` — `GET /coins/solana/contract/{mint}`
//!   per watched mint, round-robin: token lookup BY MINT. A 404 means "not
//!   aggregator-listed yet" (a quiet poll); the first 200 is the
//!   AGGREGATOR-LISTED legibility event, and subsequent polls carry the
//!   aggregator's own community sentiment gauge
//!   (`sentiment_votes_up_percentage` → `sentiment_bp`).
//! * `--category <id>` — `GET /coins/markets?category=<id>`: the aggregator's
//!   roster for a category (real ids: `solana-meme-coins`, `pump-fun`,
//!   `letsbonk-fun-ecosystem`, `meme-token`; verify against
//!   `/coins/categories/list`). A NEW coin appearing in the roster is that
//!   coin's category-legibility event.
//!
//! This is a DOCUMENTED, versioned API (docs.coingecko.com) — the opposite of
//! the tier-3 `pump` lane — so drift is expected rare, but the same
//! shape-hash `SCHEMA_DRIFT` sentinel watches it anyway (§18.8: degradation
//! detection is cheap; silent degradation is unforgivable).
//!
//! Auth (§29.7e): `CG_API_KEY` (a free Demo key, `x-cg-demo-api-key` header,
//! root `api.coingecko.com`) when set; ABSENT = keyless public access
//! (IP-throttled, dynamic, lower limits) — both are allowed and the startup
//! log says which is active. The Demo tier is ~30 req/min with a 10 000
//! calls/month cap, so the global budget pacer (same shape as `pump`'s)
//! defaults to 25 req/min keyed / 10 req/min keyless across ALL modes in the
//! process, and the startup log prints the monthly-sustainable cycle so the
//! operator can slow down with `--interval-secs`.
//!
//! Emission: the shared schema (platform `"coingecko"`, author `"coingecko"`
//! — the aggregator is the actor, community = category id / `"trending"` /
//! the watched mint, engagement zeros, `echo:false`, capture stamp) plus
//! OPTIONAL trailing fields emitted ONLY when the vendor stated them (§6.4 —
//! never fabricate): `"mint"` (base58 VERBATIM case from `platforms.solana`),
//! `"aggregator_listed":true`, `"sentiment_bp"` (0..10000 =
//! `sentiment_votes_up_percentage` × 100, rounded), `"sentiment_conf_bp"`
//! (see [`conf_bp`] for the documented mapping) and
//! `"sentiment_model":"coingecko-votes-v1"`.
//!
//! `[S]` discipline as everywhere: capture only — no decision, no opinion
//! (§83); `--replay` is a pure function of the fixture file (§22).

use std::collections::HashSet;

use crate::json::{self, py_int, py_str, py_truthy, Value};
use crate::pump::{shape_hash, Sentinel};
use crate::{dedupe, emit, http, urlenc};

/// Root URL — Demo keys and keyless access BOTH use the public host (the
/// `pro-api.coingecko.com` host rejects Demo keys).
pub const BASE: &str = "https://api.coingecko.com/api/v3";

/// Demo-key auth header (docs.coingecko.com "Authentication (Demo)").
pub const AUTH_HEADER: &str = "x-cg-demo-api-key";

/// Same timeout as the other REST lanes (`urlopen(..., timeout=30)` twins).
const TIMEOUT_SECS: u64 = 30;

/// Global request budget with a Demo key: ≤25 req/min across ALL modes —
/// under the documented "~30 calls/min (varies with traffic)" Demo ceiling.
pub const DEMO_BUDGET_PER_MIN: u64 = 25;

/// Global request budget keyless: ≤10 req/min — the keyless tier is
/// IP-throttled with DYNAMIC limits ("prioritize fair access"), so we stay
/// well under the keyed ceiling.
pub const KEYLESS_BUDGET_PER_MIN: u64 = 10;

/// Demo plan monthly call cap (credits). The per-minute budget above will
/// exhaust this in hours if run flat-out — the startup log prints the
/// monthly-sustainable cycle so the operator can choose `--interval-secs`.
pub const DEMO_MONTHLY_CAP: u64 = 10_000;

/// Seconds in the 30-day month the cap is metered over.
const MONTH_SECS: u64 = 30 * 24 * 60 * 60;

/// The fixed sentiment-model tag on every sentiment-bearing line.
pub const SENTIMENT_MODEL: &str = "coingecko-votes-v1";

// ---------------------------------------------------------------- budget math

/// Requests one full cycle costs: one for `--trending`, one for `--category`,
/// one per watched mint.
#[must_use]
pub fn requests_per_cycle(trending: bool, category: bool, n_mints: usize) -> u64 {
    u64::from(trending) + u64::from(category) + n_mints as u64
}

/// Minimum seconds one full cycle may take under `budget_per_min` (ceil
/// division — never round below the floor).
#[must_use]
pub fn budget_floor_secs(reqs: u64, budget_per_min: u64) -> u64 {
    (reqs.max(1) * 60).div_ceil(budget_per_min.max(1))
}

/// Effective cycle seconds: the operator's `--interval-secs` ask, clamped UP
/// to the budget floor (slower than the budget is allowed, faster never).
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

/// The cycle length that makes this mode mix sustainable under the Demo
/// monthly cap: `ceil(reqs · month_secs / cap)`. Diagnostics only — the lane
/// never silently slows itself; the operator decides.
#[must_use]
pub fn monthly_sustainable_cycle_secs(reqs: u64) -> u64 {
    (reqs.max(1) * MONTH_SECS).div_ceil(DEMO_MONTHLY_CAP)
}

// -------------------------------------------------------------- normalization

/// Basis-point sentiment from `sentiment_votes_up_percentage`: percentage ×
/// 100, rounded, clamped to 0..=10000. `None` when the vendor sent null /
/// absent / non-numeric — the field is then OMITTED, never fabricated (§6.4).
#[must_use]
pub fn sentiment_bp(v: Option<&Value>) -> Option<u32> {
    let Value::Number(raw) = v? else {
        return None;
    };
    let pct = raw.parse::<f64>().ok()?;
    if !pct.is_finite() {
        return None;
    }
    Some(((pct.clamp(0.0, 100.0)) * 100.0).round() as u32)
}

/// Votes-count-derived confidence, in basis points. CoinGecko does NOT expose
/// the raw sentiment-vote count, so the documented mapping uses the closest
/// audience-size field it does publish, `watchlist_portfolio_users`:
/// **1 watchlist user = 1 bp of confidence, saturating at 10 000 (full
/// confidence at ≥10 000 users)**. Small watchlists ⇒ low confidence in the
/// vote percentage; `None` (field absent / null / zero) ⇒ OMITTED.
#[must_use]
pub fn conf_bp(v: Option<&Value>) -> Option<u32> {
    let v = v?;
    if !py_truthy(v) {
        return None;
    }
    match py_int(Some(v)) {
        0 => None,
        n => Some(n.min(10_000) as u32),
    }
}

/// `market_cap_rank` when the vendor stated a positive one; `None` otherwise
/// (low-caps legitimately have a null rank — omitted, not zero-faked).
#[must_use]
fn rank_of(v: &Value) -> Option<u64> {
    let n = py_int(v.get("market_cap_rank").filter(|r| py_truthy(r)));
    (n > 0).then_some(n)
}

/// String field helper: `""` when absent/non-string (rendering-only fields).
fn str_of(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

/// `$SYMBOL` rendering: ASCII-uppercased (markets sends `"wif"`, trending
/// sends `"WIF"`; the rendering is deterministic either way).
fn sym_of(v: &Value) -> String {
    str_of(v, "symbol").to_ascii_uppercase()
}

/// One normalized CoinGecko observation, ready for emission.
#[derive(Debug, PartialEq, Eq)]
pub struct CgEvent {
    /// Dedupe identity (namespaced — `trending:`, `markets:`, `contract:`).
    pub identity: String,
    /// Category id, `"trending"`, or the watched mint (contract mode).
    pub community: String,
    /// Deterministic rendering (`TRENDING …` / `LISTED …` / `SENTIMENT …`).
    pub text: String,
    /// Solana mint, base58 VERBATIM case — only when the vendor stated it.
    pub mint: Option<String>,
    /// True on coin observations (the coin IS on the aggregator); absent on
    /// category observations.
    pub aggregator_listed: bool,
    /// See [`sentiment_bp`].
    pub sentiment_bp: Option<u32>,
    /// See [`conf_bp`]; only meaningful (and only emitted) with sentiment.
    pub sentiment_conf_bp: Option<u32>,
}

impl CgEvent {
    /// Borrow as the shared emission event (platform `"coingecko"`, author
    /// `"coingecko"` — the aggregator is the actor; engagement zeros;
    /// `echo:false` — an aggregator observation is not an amplification).
    #[must_use]
    pub fn event(&self) -> emit::Event<'_> {
        emit::Event {
            platform: "coingecko",
            author: "coingecko",
            community: &self.community,
            text: &self.text,
            likes: 0,
            reposts: 0,
            replies: 0,
            echo: false,
        }
    }

    /// The full NDJSON line: shared schema + capture stamp, then the optional
    /// trailing fields IN FIXED ORDER (`mint`, `aggregator_listed`,
    /// `sentiment_bp`, `sentiment_conf_bp`, `sentiment_model`), each emitted
    /// only when known — unknown fields are omitted entirely (§6.4).
    #[must_use]
    pub fn line(&self, observed_at_ns: u64) -> String {
        let mut line = emit::event_line(&self.event(), observed_at_ns);
        line.pop(); // strip the closing '}'
        if let Some(m) = &self.mint {
            line.push_str(",\"mint\":\"");
            emit::escape_json_into(m, &mut line);
            line.push('"');
        }
        if self.aggregator_listed {
            line.push_str(",\"aggregator_listed\":true");
        }
        if let Some(bp) = self.sentiment_bp {
            line.push_str(",\"sentiment_bp\":");
            line.push_str(&bp.to_string());
            if let Some(c) = self.sentiment_conf_bp {
                line.push_str(",\"sentiment_conf_bp\":");
                line.push_str(&c.to_string());
            }
            line.push_str(",\"sentiment_model\":\"");
            line.push_str(SENTIMENT_MODEL);
            line.push('"');
        }
        line.push('}');
        line
    }
}

/// `/search/trending` response → coin + category observations. Trending NFTs
/// are deliberately ignored (not a memecoin surface). Entries without a
/// truthy `id` are malformed and SKIPPED, never fabricated.
#[must_use]
pub fn parse_trending(v: &Value) -> Vec<CgEvent> {
    let mut out = Vec::new();
    if let Some(coins) = v.get("coins").and_then(Value::as_array) {
        for c in coins {
            // Documented shape: {"item": {...}}; tolerate a bare object too.
            let item = c.get("item").unwrap_or(c);
            let Some(id) = item.get("id").filter(|x| py_truthy(x)).map(py_str) else {
                continue;
            };
            let (name, sym) = (str_of(item, "name"), sym_of(item));
            let text = match rank_of(item) {
                Some(r) => format!("TRENDING {name} (${sym}) rank={r}"),
                None => format!("TRENDING {name} (${sym})"),
            };
            out.push(CgEvent {
                identity: format!("trending:{id}"),
                community: "trending".into(),
                text,
                mint: None,
                aggregator_listed: true,
                sentiment_bp: None,
                sentiment_conf_bp: None,
            });
        }
    }
    if let Some(cats) = v.get("categories").and_then(Value::as_array) {
        for c in cats {
            let Some(id) = c.get("id").filter(|x| py_truthy(x)).map(py_str) else {
                continue;
            };
            out.push(CgEvent {
                identity: format!("trending-category:{id}"),
                community: "trending".into(),
                text: format!("TRENDING CATEGORY {}", str_of(c, "name")),
                mint: None,
                aggregator_listed: false,
                sentiment_bp: None,
                sentiment_conf_bp: None,
            });
        }
    }
    out
}

/// `/coins/markets?category=<id>` response (a bare array) → one observation
/// per coin in the roster. Non-array / malformed entries are skipped.
#[must_use]
pub fn parse_markets(v: &Value, category: &str) -> Vec<CgEvent> {
    let mut out = Vec::new();
    for c in v.as_array().unwrap_or_default() {
        let Some(id) = c.get("id").filter(|x| py_truthy(x)).map(py_str) else {
            continue;
        };
        let (name, sym) = (str_of(c, "name"), sym_of(c));
        let text = match rank_of(c) {
            Some(r) => format!("LISTED {name} (${sym}) rank={r}"),
            None => format!("LISTED {name} (${sym})"),
        };
        out.push(CgEvent {
            identity: format!("markets:{category}:{id}"),
            community: category.to_string(),
            text,
            mint: None,
            aggregator_listed: true,
            sentiment_bp: None,
            sentiment_conf_bp: None,
        });
    }
    out
}

/// Parsed view of one `/coins/solana/contract/{mint}` response.
#[derive(Debug, PartialEq, Eq)]
pub struct ContractView {
    /// CoinGecko coin id (dedupe anchor).
    pub coin_id: String,
    /// Display name (rendering only).
    pub name: String,
    /// `$SYMBOL` rendering (uppercased).
    pub symbol: String,
    /// The Solana mint, base58 VERBATIM case: `platforms.solana` when the
    /// vendor stated it, else the watched mint we asked about.
    pub mint: String,
    /// See [`rank_of`].
    pub rank: Option<u64>,
    /// See [`sentiment_bp`].
    pub sentiment_bp: Option<u32>,
    /// See [`conf_bp`] — carried only when sentiment itself is present.
    pub sentiment_conf_bp: Option<u32>,
}

/// One contract-lookup response → view, or `None` for a malformed body (no
/// truthy `id` — e.g. an error object) which is SKIPPED, never fabricated.
#[must_use]
pub fn parse_contract(v: &Value, watched_mint: &str) -> Option<ContractView> {
    let coin_id = v.get("id").filter(|x| py_truthy(x)).map(py_str)?;
    let mint = v
        .get("platforms")
        .and_then(|p| p.get("solana"))
        .filter(|x| py_truthy(x))
        .map(py_str)
        .unwrap_or_else(|| watched_mint.to_string());
    let bp = sentiment_bp(v.get("sentiment_votes_up_percentage"));
    Some(ContractView {
        coin_id,
        name: str_of(v, "name"),
        symbol: sym_of(v),
        mint,
        rank: rank_of(v),
        sentiment_bp: bp,
        sentiment_conf_bp: bp.and_then(|_| conf_bp(v.get("watchlist_portfolio_users"))),
    })
}

/// A contract view → observation. `first` = this process has not emitted for
/// this mint before: the AGGREGATOR-LISTED legibility event (`LISTED …`);
/// afterwards a changed sentiment re-emits as `SENTIMENT …`. The identity
/// carries the sentiment values, so an unchanged gauge is a quiet poll.
#[must_use]
pub fn contract_event(view: &ContractView, first: bool) -> CgEvent {
    let ContractView {
        coin_id,
        name,
        symbol: sym,
        mint,
        rank,
        sentiment_bp: bp,
        sentiment_conf_bp: conf,
    } = view;
    let fmt = |o: &Option<u32>| o.map_or_else(|| "-".into(), |n| n.to_string());
    let text = match (first, bp) {
        (false, Some(bp)) => format!("SENTIMENT {bp}bp {name} (${sym}) {mint}"),
        (_, _) => match rank {
            Some(r) => format!("LISTED {name} (${sym}) {mint} rank={r}"),
            None => format!("LISTED {name} (${sym}) {mint}"),
        },
    };
    CgEvent {
        identity: format!("contract:{coin_id}:{}:{}", fmt(bp), fmt(conf)),
        community: mint.clone(),
        text,
        mint: Some(mint.clone()),
        aggregator_listed: true,
        sentiment_bp: *bp,
        sentiment_conf_bp: *conf,
    }
}

// ------------------------------------------------------------- page dispatch

/// Which documented endpoint a saved response came from — replay fixtures are
/// dispatched by SHAPE, so one fixture stream may mix all three surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageKind {
    /// `/search/trending` (`{"coins":[...],"categories":[...]}`).
    Trending,
    /// `/coins/markets?...` (a bare array).
    Markets,
    /// `/coins/solana/contract/{mint}` (an object with an `id`).
    Contract,
}

/// Classify a response body by its documented shape; `None` = unrecognized
/// (logged and skipped by the caller — absence of data is never an error).
#[must_use]
pub fn classify(v: &Value) -> Option<PageKind> {
    if v.get("coins").is_some() {
        return Some(PageKind::Trending);
    }
    if matches!(v, Value::Array(_)) {
        return Some(PageKind::Markets);
    }
    if v.get("id").is_some() {
        return Some(PageKind::Contract);
    }
    None
}

/// The object whose top-level key set the `SCHEMA_DRIFT` sentinel
/// fingerprints, per page kind (the first coin `item` for trending, the first
/// roster entry for markets, the body itself for contract).
#[must_use]
pub fn shape_target(kind: PageKind, page: &Value) -> Option<&Value> {
    match kind {
        PageKind::Trending => page
            .get("coins")
            .and_then(Value::as_array)
            .and_then(<[Value]>::first)
            .map(|c| c.get("item").unwrap_or(c)),
        PageKind::Markets => page.as_array().and_then(<[Value]>::first),
        PageKind::Contract => Some(page),
    }
}

/// One classified page → observations (shared by live and replay drivers).
/// `listed` is the per-process set of mints whose LISTED event already went
/// out (contract mode's first-vs-update distinction).
#[must_use]
pub fn page_events(
    kind: PageKind,
    page: &Value,
    category: &str,
    watched_mint: &str,
    listed: &mut HashSet<String>,
) -> Vec<CgEvent> {
    match kind {
        PageKind::Trending => parse_trending(page),
        PageKind::Markets => parse_markets(page, category),
        PageKind::Contract => match parse_contract(page, watched_mint) {
            Some(view) => {
                let first = listed.insert(view.mint.clone());
                vec![contract_event(&view, first)]
            }
            None => Vec::new(),
        },
    }
}

/// Sentinel label per page kind (stderr diagnostics).
#[must_use]
pub fn kind_label(kind: PageKind) -> &'static str {
    match kind {
        PageKind::Trending => "trending",
        PageKind::Markets => "markets",
        PageKind::Contract => "contract",
    }
}

// ------------------------------------------------------------------------ CLI

/// Parsed CLI for this subcommand. All three modes are flags on ONE process,
/// sharing the global budget pacer.
pub struct Cli {
    /// `--trending`: poll `/search/trending`.
    pub trending: bool,
    /// `--category <id>`: poll `/coins/markets?category=<id>` (empty = off).
    pub category: String,
    /// `--contract-watch <mints-file>`: round-robin contract lookups
    /// (empty = off; file format identical to the pump lane's).
    pub contract_watch: String,
    /// `--interval-secs`: full-cycle seconds; default = the budget floor for
    /// the mode mix, and always clamped up to it.
    pub interval_secs: Option<u64>,
    /// `--budget-per-min`: override the global request budget (still a
    /// ceiling — the cycle floor is derived from it).
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
            trending: false,
            category: String::new(),
            contract_watch: String::new(),
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
            match a.as_str() {
                "--trending" => cli.trending = true,
                "--category" => cli.category = val("--category")?,
                "--contract-watch" => cli.contract_watch = val("--contract-watch")?,
                "--interval-secs" => {
                    cli.interval_secs = Some(
                        val("--interval-secs")?
                            .parse()
                            .map_err(|e| format!("bad --interval-secs: {e}"))?,
                    );
                }
                "--budget-per-min" => {
                    let b: u64 = val("--budget-per-min")?
                        .parse()
                        .map_err(|e| format!("bad --budget-per-min: {e}"))?;
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
/// same dedupe ring and listed-set as live — zero network, zero wall clock,
/// byte-identical run-to-run (§22). The markets community label is
/// `--category` when given, else `"category"`; the contract watched-mint
/// fallback is empty (fixture bodies carry `platforms.solana`, as the real
/// endpoint's do).
fn replay(path: &str, cli: &Cli) -> u8 {
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
    let category = if cli.category.is_empty() {
        "category"
    } else {
        &cli.category
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut ring = dedupe::DedupeRing::new(dedupe::DEFAULT_CAP);
    let mut listed = HashSet::new();
    let mut sentinels = [
        Sentinel::default(),
        Sentinel::default(),
        Sentinel::default(),
    ];
    let mut emitted: u64 = 0;
    for page in &pages {
        let Some(kind) = classify(page) else {
            eprintln!("[coingecko] unrecognized page shape; skipping");
            continue;
        };
        observe_shape(kind, page, &mut sentinels);
        for ev in page_events(kind, page, category, "", &mut listed) {
            if !ring.insert(&ev.identity) {
                continue;
            }
            let ts = crate::REPLAY_BASE_NS + emitted * crate::REPLAY_STEP_NS;
            if emit::write_line(&mut out, &ev.line(ts)).is_err() {
                return 0;
            }
            emitted += 1;
        }
    }
    eprintln!("[pq-social-capture] replay: emitted {emitted} events");
    0
}

/// Feed a page's shape fingerprint to its endpoint family's sentinel and log
/// `SCHEMA_DRIFT` on change (documented API — expected rare, tracked anyway).
fn observe_shape(kind: PageKind, page: &Value, sentinels: &mut [Sentinel; 3]) {
    if let Some(h) = shape_target(kind, page).and_then(shape_hash) {
        if let Some(old) = sentinels[kind as usize].observe_shape(h) {
            eprintln!(
                "[coingecko] SCHEMA_DRIFT {}: shape {old:016x} -> {h:016x} \
                 (tolerant parser continues)",
                kind_label(kind)
            );
        }
    }
}

/// Subcommand entry. `now_ns` is the capture-edge clock injected by `main.rs`
/// (§22 — this module never reads a wall clock itself).
pub fn run(args: &[String], now_ns: fn() -> u64) -> u8 {
    let cli = match Cli::parse(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[pq-social-capture] coingecko: {e}");
            return 2;
        }
    };

    if let Some(path) = &cli.replay {
        return replay(path, &cli);
    }

    if !cli.trending && cli.category.is_empty() && cli.contract_watch.is_empty() {
        eprintln!(
            "error: coingecko needs at least one mode: --trending | \
             --category <id> | --contract-watch <mints-file>"
        );
        return 2;
    }

    let mints: Vec<String> = if cli.contract_watch.is_empty() {
        Vec::new()
    } else {
        match std::fs::read_to_string(&cli.contract_watch) {
            Ok(t) => {
                let m = crate::pump::parse_mints(&t);
                if m.is_empty() {
                    eprintln!("error: {} lists no mints", cli.contract_watch);
                    return 2;
                }
                m
            }
            Err(e) => {
                eprintln!(
                    "error: cannot read --contract-watch {}: {e}",
                    cli.contract_watch
                );
                return 2;
            }
        }
    };

    // Auth: CG_API_KEY set = Demo tier (header auth); absent = keyless public
    // access. BOTH are allowed — the log says which is active (§18.8: the
    // operator must know the lane's capability level).
    let key = std::env::var("CG_API_KEY").unwrap_or_default();
    let key = key.trim().to_string();
    let budget = cli.budget_per_min.unwrap_or(if key.is_empty() {
        KEYLESS_BUDGET_PER_MIN
    } else {
        DEMO_BUDGET_PER_MIN
    });
    if key.is_empty() {
        eprintln!(
            "[coingecko] auth: KEYLESS public access (IP-throttled, dynamic limits); \
             budget <= {budget} req/min — set CG_API_KEY (free Demo key) for \
             ~30 req/min + 10000 calls/month"
        );
    } else {
        eprintln!(
            "[coingecko] auth: Demo key via {AUTH_HEADER}; budget <= {budget} req/min \
             (Demo cap: ~30 req/min, {DEMO_MONTHLY_CAP} calls/month)"
        );
    }

    let reqs = requests_per_cycle(cli.trending, !cli.category.is_empty(), mints.len());
    let cycle = cycle_secs(reqs, cli.interval_secs, budget);
    let gap = request_gap_secs(cycle, reqs);
    eprintln!(
        "[coingecko] modes: trending={} category={:?} contract-watch={} mints; \
         {reqs} req/cycle, cycle {cycle}s, gap {}s",
        cli.trending,
        cli.category,
        mints.len(),
        crate::py_float(gap)
    );
    let sustain = monthly_sustainable_cycle_secs(reqs);
    if cycle < sustain {
        eprintln!(
            "[coingecko] NOTE: cycle {cycle}s exceeds the Demo monthly cap pace — \
             sustainable cycle for this mode mix is {sustain}s \
             (--interval-secs {sustain} to stay under {DEMO_MONTHLY_CAP} calls/month)"
        );
    }

    let http = http::Http::new(TIMEOUT_SECS);
    let mut ring = dedupe::DedupeRing::new(dedupe::DEFAULT_CAP);
    let mut listed: HashSet<String> = HashSet::new();
    let mut sentinels = [
        Sentinel::default(),
        Sentinel::default(),
        Sentinel::default(),
    ];
    let stdout = std::io::stdout();

    loop {
        let mut emitted = 0u64;
        let mut polls: Vec<(PageKind, String, String)> = Vec::new();
        if cli.trending {
            polls.push((
                PageKind::Trending,
                format!("{BASE}/search/trending"),
                String::new(),
            ));
        }
        if !cli.category.is_empty() {
            polls.push((
                PageKind::Markets,
                format!(
                    "{BASE}/coins/markets?{}",
                    urlenc::urlencode(&[
                        ("vs_currency", "usd"),
                        ("category", &cli.category),
                        ("order", "market_cap_desc"),
                        ("per_page", "250"),
                        ("page", "1"),
                    ])
                ),
                String::new(),
            ));
        }
        for mint in &mints {
            polls.push((
                PageKind::Contract,
                format!("{BASE}/coins/solana/contract/{mint}"),
                mint.clone(),
            ));
        }

        for (kind, url, watched_mint) in &polls {
            let mut headers: Vec<(&str, &str)> = vec![("Accept", "application/json")];
            if !key.is_empty() {
                headers.push((AUTH_HEADER, &key));
            }
            let label = kind_label(*kind);
            match http.get_meta(url, &headers) {
                Ok(meta) => {
                    if meta.status == 401 || meta.status == 403 {
                        eprintln!(
                            "[coingecko] AUTH_REJECTED {label}: HTTP {} — check CG_API_KEY \
                             (Demo key, {AUTH_HEADER} header, root api.coingecko.com); \
                             exiting 2",
                            meta.status
                        );
                        return 2;
                    }
                    if let Some((old, new)) = sentinels[*kind as usize].observe_status(meta.status)
                    {
                        eprintln!("[coingecko] STATUS_CLASS_DRIFT {label}: {old}xx -> {new}xx");
                    }
                    if meta.status == 404 && *kind == PageKind::Contract {
                        // The expected answer for a pre-legibility memecoin:
                        // not aggregator-listed yet. A quiet poll, not an error.
                        eprintln!("[coingecko] {watched_mint}: not aggregator-listed (404)");
                    } else if !(200..300).contains(&meta.status) {
                        eprintln!("[coingecko] HTTP {} on {label}; skipping poll", meta.status);
                    } else {
                        match json::parse(&meta.body) {
                            Ok(page) => {
                                observe_shape(*kind, &page, &mut sentinels);
                                let mut out = stdout.lock();
                                for ev in page_events(
                                    *kind,
                                    &page,
                                    &cli.category,
                                    watched_mint,
                                    &mut listed,
                                ) {
                                    if !ring.insert(&ev.identity) {
                                        continue;
                                    }
                                    if emit::write_line(&mut out, &ev.line(now_ns())).is_err() {
                                        return 0; // downstream pipe closed
                                    }
                                    emitted += 1;
                                }
                            }
                            Err(e) => {
                                eprintln!("[coingecko] bad JSON on {label}: {e}; skipping poll");
                            }
                        }
                    }
                }
                Err(e) => eprintln!("fetch error ({label}): {e}"),
            }
            std::thread::sleep(std::time::Duration::from_secs_f64(gap));
        }
        eprintln!(
            "[coingecko] pass: emitted {} new (seen {})",
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
    fn requests_per_cycle_counts_every_mode() {
        assert_eq!(requests_per_cycle(false, false, 0), 0);
        assert_eq!(requests_per_cycle(true, false, 0), 1);
        assert_eq!(requests_per_cycle(true, true, 0), 2);
        assert_eq!(requests_per_cycle(true, true, 5), 7);
    }

    #[test]
    fn budget_floor_is_ceil_and_never_zero_divides() {
        assert_eq!(budget_floor_secs(1, 25), 3, "ceil(60/25)");
        assert_eq!(budget_floor_secs(25, 25), 60);
        assert_eq!(budget_floor_secs(7, 10), 42);
        assert_eq!(budget_floor_secs(0, 25), 3, "empty clamps to 1 request");
        assert_eq!(budget_floor_secs(1, 0), 60, "zero budget clamps to 1/min");
    }

    #[test]
    fn interval_is_clamped_up_to_the_floor_never_down() {
        assert_eq!(cycle_secs(5, None, 25), 12, "default = floor");
        assert_eq!(cycle_secs(5, Some(5), 25), 12, "too fast: clamped");
        assert_eq!(cycle_secs(5, Some(300), 25), 300, "slower is allowed");
    }

    #[test]
    fn gap_spreads_cycle_evenly() {
        assert!((request_gap_secs(12, 5) - 2.4).abs() < 1e-9);
        assert!((request_gap_secs(3, 1) - 3.0).abs() < 1e-9);
        assert!((request_gap_secs(60, 0) - 60.0).abs() < 1e-9, "no zero div");
    }

    #[test]
    fn monthly_sustainable_cycle_matches_the_demo_cap() {
        // 1 req/cycle: 2 592 000 s month / 10 000 calls = 259.2 -> 260 s.
        assert_eq!(monthly_sustainable_cycle_secs(1), 260);
        assert_eq!(monthly_sustainable_cycle_secs(5), 1296);
        assert_eq!(monthly_sustainable_cycle_secs(0), 260, "clamps to 1");
    }

    // ----------------------------------------------------- sentiment mapping

    #[test]
    fn sentiment_bp_is_percentage_times_100_rounded() {
        assert_eq!(sentiment_bp(Some(&v("87.5"))), Some(8750));
        assert_eq!(sentiment_bp(Some(&v("100"))), Some(10000));
        assert_eq!(sentiment_bp(Some(&v("0"))), Some(0));
        assert_eq!(sentiment_bp(Some(&v("66.666"))), Some(6667), "rounded");
    }

    #[test]
    fn sentiment_bp_clamps_and_never_fabricates() {
        assert_eq!(sentiment_bp(Some(&v("250"))), Some(10000), "clamped high");
        assert_eq!(sentiment_bp(Some(&v("-3"))), Some(0), "clamped low");
        assert_eq!(sentiment_bp(Some(&v("null"))), None, "null = omitted");
        assert_eq!(
            sentiment_bp(Some(&v("\"87\""))),
            None,
            "non-number = omitted"
        );
        assert_eq!(sentiment_bp(None), None, "absent = omitted");
    }

    #[test]
    fn conf_bp_is_watchlist_users_capped_at_10000() {
        assert_eq!(conf_bp(Some(&v("1234"))), Some(1234));
        assert_eq!(conf_bp(Some(&v("10000"))), Some(10000));
        assert_eq!(conf_bp(Some(&v("2500000"))), Some(10000), "saturates");
        assert_eq!(conf_bp(Some(&v("0"))), None, "zero users = no confidence");
        assert_eq!(conf_bp(Some(&v("null"))), None);
        assert_eq!(conf_bp(None), None);
    }

    // ------------------------------------------------------------- trending

    #[test]
    fn trending_parses_coins_and_categories_skips_nfts() {
        let page = v(
            r#"{"coins":[{"item":{"id":"dogwifcoin","coin_id":28752,"name":"dogwifhat",
                "symbol":"WIF","market_cap_rank":98,"score":0}}],
                "nfts":[{"id":"some-nft","name":"n"}],
                "categories":[{"id":214,"name":"Solana Meme","coins_count":642}]}"#,
        );
        let evs = parse_trending(&page);
        assert_eq!(evs.len(), 2, "coins + categories; NFTs ignored");
        assert_eq!(evs[0].identity, "trending:dogwifcoin");
        assert_eq!(evs[0].text, "TRENDING dogwifhat ($WIF) rank=98");
        assert_eq!(evs[0].community, "trending");
        assert!(evs[0].aggregator_listed);
        assert_eq!(evs[1].identity, "trending-category:214");
        assert_eq!(evs[1].text, "TRENDING CATEGORY Solana Meme");
        assert!(!evs[1].aggregator_listed, "a category is not a coin");
    }

    #[test]
    fn trending_null_rank_is_omitted_from_the_rendering() {
        let page = v(
            r#"{"coins":[{"item":{"id":"newpump","name":"New","symbol":"np",
                        "market_cap_rank":null}}]}"#,
        );
        assert_eq!(parse_trending(&page)[0].text, "TRENDING New ($NP)");
    }

    #[test]
    fn trending_malformed_entries_are_skipped_not_fabricated() {
        let page = v(
            r#"{"coins":[{"item":{"name":"no id"}},{"item":{"id":""}},42,
                        {"item":{"id":"ok","name":"Ok","symbol":"OK"}}],
                "categories":[{"name":"no id"},{"id":null}]}"#,
        );
        let evs = parse_trending(&page);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].identity, "trending:ok");
    }

    // -------------------------------------------------------------- markets

    #[test]
    fn markets_roster_events_carry_the_category_as_community() {
        let page = v(
            r#"[{"id":"pepe-sol","symbol":"pepe","name":"Pepe Sol","market_cap_rank":1450},
                {"id":"bad"},{"name":"no id"}]"#,
        );
        let evs = parse_markets(&page, "pump-fun");
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].identity, "markets:pump-fun:pepe-sol");
        assert_eq!(evs[0].community, "pump-fun");
        assert_eq!(evs[0].text, "LISTED Pepe Sol ($PEPE) rank=1450");
        assert_eq!(
            evs[1].text, "LISTED  ($)",
            "missing rendering fields degrade"
        );
        assert!(parse_markets(&v(r#"{"error":"x"}"#), "c").is_empty());
    }

    // ------------------------------------------------------------- contract

    #[test]
    fn contract_parses_mint_verbatim_case_from_platforms() {
        let view = parse_contract(
            &v(r#"{"id":"fartcoin","symbol":"fartcoin","name":"Fartcoin",
                   "platforms":{"solana":"9BB6NFEcjBCtnNLFko2FqVQBq8HHM13kCyYcdQbgpump"},
                   "market_cap_rank":120,
                   "sentiment_votes_up_percentage":87.5,
                   "sentiment_votes_down_percentage":12.5,
                   "watchlist_portfolio_users":1234}"#),
            "WatchedFallback",
        )
        .unwrap();
        assert_eq!(view.mint, "9BB6NFEcjBCtnNLFko2FqVQBq8HHM13kCyYcdQbgpump");
        assert_eq!(view.rank, Some(120));
        assert_eq!(view.sentiment_bp, Some(8750));
        assert_eq!(view.sentiment_conf_bp, Some(1234));
    }

    #[test]
    fn contract_missing_platform_falls_back_to_the_watched_mint() {
        let view = parse_contract(
            &v(r#"{"id":"x","platforms":{"solana":""}}"#),
            "WatchedMintB58",
        )
        .unwrap();
        assert_eq!(view.mint, "WatchedMintB58", "empty platform entry is falsy");
        assert_eq!(view.sentiment_bp, None);
    }

    #[test]
    fn contract_conf_is_only_carried_alongside_sentiment() {
        let view = parse_contract(
            &v(r#"{"id":"x","sentiment_votes_up_percentage":null,
                   "watchlist_portfolio_users":999}"#),
            "M",
        )
        .unwrap();
        assert_eq!(view.sentiment_bp, None);
        assert_eq!(view.sentiment_conf_bp, None, "conf without votes is noise");
    }

    #[test]
    fn contract_error_body_is_skipped() {
        assert_eq!(
            parse_contract(&v(r#"{"error":"coin not found"}"#), "M"),
            None
        );
        assert_eq!(parse_contract(&v("[1]"), "M"), None);
    }

    #[test]
    fn contract_first_sighting_is_listed_then_sentiment_updates() {
        let view = parse_contract(
            &v(r#"{"id":"c","name":"Coin","symbol":"c",
                   "platforms":{"solana":"MintB58"},
                   "sentiment_votes_up_percentage":91.25,
                   "watchlist_portfolio_users":40}"#),
            "",
        )
        .unwrap();
        let first = contract_event(&view, true);
        assert_eq!(first.text, "LISTED Coin ($C) MintB58");
        assert_eq!(first.identity, "contract:c:9125:40");
        assert_eq!(first.community, "MintB58");
        let update = contract_event(&view, false);
        assert_eq!(update.text, "SENTIMENT 9125bp Coin ($C) MintB58");
        assert_eq!(update.identity, first.identity, "identity is value-keyed");
    }

    #[test]
    fn contract_update_without_sentiment_renders_listed() {
        let view = parse_contract(&v(r#"{"id":"c","name":"C","symbol":"c"}"#), "M").unwrap();
        assert_eq!(contract_event(&view, false).text, "LISTED C ($C) M");
    }

    // ---------------------------------------------------------- line building

    #[test]
    fn line_emits_optional_fields_in_fixed_order_and_round_trips() {
        let ev = CgEvent {
            identity: "contract:c:8750:1234".into(),
            community: "MintB58".into(),
            text: "LISTED Coin ($C) MintB58 rank=120".into(),
            mint: Some("MintB58".into()),
            aggregator_listed: true,
            sentiment_bp: Some(8750),
            sentiment_conf_bp: Some(1234),
        };
        let line = ev.line(42);
        assert_eq!(
            line,
            "{\"platform\":\"coingecko\",\"author\":\"coingecko\",\
             \"community\":\"MintB58\",\"text\":\"LISTED Coin ($C) MintB58 rank=120\",\
             \"likes\":0,\"reposts\":0,\"replies\":0,\"echo\":false,\
             \"observed_at_ns\":42,\"mint\":\"MintB58\",\"aggregator_listed\":true,\
             \"sentiment_bp\":8750,\"sentiment_conf_bp\":1234,\
             \"sentiment_model\":\"coingecko-votes-v1\"}"
        );
        let parsed = json::parse(&line).unwrap();
        assert_eq!(json::serialize(&parsed), line, "round-trip stable");
    }

    #[test]
    fn line_omits_unknown_fields_entirely() {
        let ev = CgEvent {
            identity: "trending-category:214".into(),
            community: "trending".into(),
            text: "TRENDING CATEGORY Solana Meme".into(),
            mint: None,
            aggregator_listed: false,
            sentiment_bp: None,
            sentiment_conf_bp: None,
        };
        let line = ev.line(7);
        assert!(line.ends_with("\"observed_at_ns\":7}"), "{line}");
        for absent in [
            "mint",
            "aggregator_listed",
            "sentiment_bp",
            "sentiment_conf_bp",
            "sentiment_model",
        ] {
            assert!(!line.contains(absent), "{absent} must be omitted: {line}");
        }
    }

    #[test]
    fn line_with_sentiment_but_no_conf_still_tags_the_model() {
        let ev = CgEvent {
            identity: "i".into(),
            community: "M".into(),
            text: "t".into(),
            mint: Some("M".into()),
            aggregator_listed: true,
            sentiment_bp: Some(5000),
            sentiment_conf_bp: None,
        };
        let line = ev.line(0);
        assert!(line.contains("\"sentiment_bp\":5000,\"sentiment_model\":\"coingecko-votes-v1\""));
        assert!(!line.contains("sentiment_conf_bp"));
    }

    // ------------------------------------------------------------- dispatch

    #[test]
    fn classify_by_documented_shapes() {
        assert_eq!(
            classify(&v(r#"{"coins":[],"categories":[]}"#)),
            Some(PageKind::Trending)
        );
        assert_eq!(classify(&v("[]")), Some(PageKind::Markets));
        assert_eq!(
            classify(&v(r#"{"id":"c","name":"C"}"#)),
            Some(PageKind::Contract)
        );
        assert_eq!(classify(&v(r#"{"error":"nope"}"#)), None);
        assert_eq!(classify(&v("42")), None);
    }

    #[test]
    fn shape_target_fingerprints_the_right_object() {
        let t = v(r#"{"coins":[{"item":{"id":"a","name":"A"}}]}"#);
        assert_eq!(
            shape_target(PageKind::Trending, &t),
            Some(&v(r#"{"id":"a","name":"A"}"#))
        );
        let m = v(r#"[{"id":"a"}]"#);
        assert_eq!(
            shape_target(PageKind::Markets, &m),
            Some(&v(r#"{"id":"a"}"#))
        );
        let c = v(r#"{"id":"a"}"#);
        assert_eq!(shape_target(PageKind::Contract, &c), Some(&c));
        assert_eq!(
            shape_target(PageKind::Trending, &v(r#"{"coins":[]}"#)),
            None
        );
    }

    #[test]
    fn page_events_contract_tracks_first_listing_per_mint() {
        let page = v(r#"{"id":"c","name":"C","symbol":"c",
                        "platforms":{"solana":"MintB58"},
                        "sentiment_votes_up_percentage":80.0}"#);
        let mut listed = HashSet::new();
        let a = page_events(PageKind::Contract, &page, "", "", &mut listed);
        assert!(a[0].text.starts_with("LISTED "), "{}", a[0].text);
        let b = page_events(PageKind::Contract, &page, "", "", &mut listed);
        assert!(b[0].text.starts_with("SENTIMENT "), "{}", b[0].text);
        assert_eq!(
            a[0].identity, b[0].identity,
            "same values dedupe on the ring"
        );
    }

    // ------------------------------------------------------------------ CLI

    #[test]
    fn cli_parses_all_modes_and_rejects_junk() {
        let args: Vec<String> = [
            "--trending",
            "--category",
            "pump-fun",
            "--contract-watch",
            "mints.txt",
            "--interval-secs",
            "300",
            "--budget-per-min",
            "5",
            "--once",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let cli = Cli::parse(&args).unwrap();
        assert!(cli.trending && cli.once);
        assert_eq!(cli.category, "pump-fun");
        assert_eq!(cli.contract_watch, "mints.txt");
        assert_eq!(cli.interval_secs, Some(300));
        assert_eq!(cli.budget_per_min, Some(5));
        assert!(Cli::parse(&["--bogus".to_string()]).is_err());
        assert!(
            Cli::parse(&["--category".to_string()]).is_err(),
            "needs value"
        );
        assert!(
            Cli::parse(&["--budget-per-min".to_string(), "0".to_string()]).is_err(),
            "zero budget rejected"
        );
    }
}
