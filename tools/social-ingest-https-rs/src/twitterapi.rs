//! `twitterapi` subcommand — Rust twin of `tools/social-ingest/twitterapi_stream.py`
//! (twitterapi.io → normalized SocialEvent NDJSON, the `[S]` X capture lane).
//!
//! Endpoint, query construction, filter classes, cursor pagination, dedupe and
//! cadence semantics replicate the Python twin EXACTLY — same
//! `advanced_search` URL, same `sources.yaml` keys, same three constitution
//! filter classes (`firehose` breadth, `amplifier` = the PUBLIC_BURNED KOL
//! watchlist for WAVE-TIMING + FADE only (§28/§29), `list` = a curated CT
//! list), same `X-API-Key` env credential (`TWITTERAPI_IO_KEY`, never
//! hardcoded). Reply / retweet / quote map to `echo` (§29: reach is not
//! alpha). This lane is `[S]`: it never computes a decision and emits no
//! sentiment label (§22, §83).
//!
//! Additively, a CURATED designated-caller set (`--follow <file>` and/or
//! `--authors <ids>`, matched by handle OR stable user-id) tags tweets from those
//! authors with `"is_designated_caller":true` — the SAME normalized field a
//! Discord alpha lane emits — so the engine's shared designated-caller weight
//! applies uniformly across X follows and Discord callers (§29). Absent the flag,
//! output is byte-identical to the legacy schema; the tag is provenance, never a
//! decision (the core still judges whether a designated source earns net SOL).

use crate::json::{self, Value};
use crate::yaml::Yaml;
use crate::{dedupe, emit, http, json::py_int, json::py_str, json::py_truthy, urlenc, yaml};

/// The one endpoint, identical to the Python twin's `BASE`.
pub const BASE: &str = "https://api.twitterapi.io/twitter/tweet/advanced_search";

/// Fallback firehose query, identical to the Python twin's `DEFAULT_FIREHOSE`.
pub const DEFAULT_FIREHOSE: &str = "($SOL OR \"pump.fun\" OR url:pump.fun) -is:retweet";

/// Python twin's `urlopen(..., timeout=30)`.
const TIMEOUT_SECS: u64 = 30;

/// One normalized tweet, ready for emission.
#[derive(Debug, PartialEq, Eq)]
pub struct Tweet {
    /// Vendor tweet id (dedupe key); empty when the vendor omitted it.
    pub id: String,
    /// `author.userName`, `"unknown"` when missing/falsy (§29 provenance).
    pub author: String,
    /// Vendor `author.id` (a stable numeric user-id), empty when absent. Kept so a
    /// curated follow set can match by STABLE id as well as by handle — handles are
    /// renamed, ids are not. Never emitted; used only for follow-set membership.
    pub author_id: String,
    /// Raw tweet text, cashtags + contract addresses intact.
    pub text: String,
    /// `likeCount` through Python-`_int` coercion.
    pub likes: u64,
    /// `retweetCount` through Python-`_int` coercion.
    pub reposts: u64,
    /// `replyCount` through Python-`_int` coercion.
    pub replies: u64,
    /// Reply / retweet / quote — the single "not an originator" signal.
    pub echo: bool,
}

impl Tweet {
    /// Borrow as an emission event (platform `"x"`, no community — mirrors
    /// `normalize_tweet` in the Python twin). Not designated by default; the
    /// curated-follow path uses [`Tweet::event_designated`].
    #[must_use]
    pub fn event(&self) -> emit::Event<'_> {
        emit::Event {
            platform: "x",
            author: &self.author,
            community: "",
            text: &self.text,
            likes: self.likes,
            reposts: self.reposts,
            replies: self.replies,
            echo: self.echo,
            is_designated_caller: false,
        }
    }

    /// Borrow as an emission event, tagging designated-caller provenance from the
    /// curated `follow` set (§29): a followed author (matched by handle OR stable
    /// user-id) sets `is_designated_caller`, so the engine's shared designated-
    /// caller weight applies to curated X follows exactly as to Discord alpha
    /// callers. A non-followed author yields a byte-identical base event. Pure.
    #[must_use]
    pub fn event_designated(&self, follow: &FollowSet) -> emit::Event<'_> {
        let mut ev = self.event();
        ev.is_designated_caller = follow.is_designated(self);
        ev
    }
}

/// A curated designated-caller author set for the twitterapi lane (§29).
///
/// Holds followed authors as handles and/or stable numeric user-ids. A tweet is
/// designated when its `author` handle OR its vendor `author_id` is a member, so
/// an operator can pin a caller by whichever identity is stable for them. Matching
/// is EXACT against the vendor's author string (case-sensitive): the honest
/// provenance join, deterministic, no normalization surprises. Pure + bounded by
/// the operator-supplied config.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FollowSet {
    entries: std::collections::BTreeSet<String>,
}

impl FollowSet {
    /// An empty set — the default when neither `--follow` nor `--authors` is given
    /// (no tweet is designated, output byte-identical to the legacy lane).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the set names no authors.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of curated authors.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Add one raw entry: trims surrounding whitespace and a single leading `@`;
    /// an empty entry is ignored (never designates the empty author).
    pub fn insert_raw(&mut self, raw: &str) {
        let e = raw.trim();
        let e = e.strip_prefix('@').unwrap_or(e).trim();
        if !e.is_empty() {
            self.entries.insert(e.to_string());
        }
    }

    /// Extend from an inline `--authors` argument: comma- and/or whitespace-
    /// separated handles / user-ids. Pure (§22).
    pub fn extend_from_arg(&mut self, arg: &str) {
        for tok in arg.split(|c: char| c == ',' || c.is_whitespace()) {
            self.insert_raw(tok);
        }
    }

    /// Extend from a `--follow` file's contents: one entry per line; blank lines
    /// and `#` comments (whole-line or trailing) are skipped. Pure (§22).
    pub fn extend_from_file(&mut self, contents: &str) {
        for line in contents.lines() {
            let body = line.split('#').next().unwrap_or("");
            self.insert_raw(body);
        }
    }

    /// Whether a tweet's author is designated — by handle or by stable user-id.
    #[must_use]
    pub fn is_designated(&self, t: &Tweet) -> bool {
        self.entries.contains(&t.author)
            || (!t.author_id.is_empty() && self.entries.contains(&t.author_id))
    }
}

/// twitterapi.io tweet object → normalized tweet. Pure (§22); mirrors the
/// Python `normalize_tweet` field-for-field including its falsy fallbacks.
#[must_use]
pub fn normalize_tweet(t: &Value) -> Tweet {
    let author = t
        .get("author")
        .and_then(|a| a.get("userName"))
        .filter(|v| py_truthy(v))
        .map(py_str)
        .unwrap_or_else(|| "unknown".to_string());
    // Stable numeric user-id (for follow-set matching only; never emitted). Falsy
    // or absent → empty string.
    let author_id = t
        .get("author")
        .and_then(|a| a.get("id"))
        .filter(|v| py_truthy(v))
        .map(py_str)
        .unwrap_or_default();
    let present_non_null = |key: &str| matches!(t.get(key), Some(v) if !matches!(v, Value::Null));
    let echo = t.get("isReply").is_some_and(py_truthy)
        || present_non_null("retweeted_tweet")
        || present_non_null("quoted_tweet");
    Tweet {
        id: t.get("id").map(py_str).unwrap_or_default(),
        author,
        author_id,
        text: t
            .get("text")
            .filter(|v| py_truthy(v))
            .map(py_str)
            .unwrap_or_default(),
        likes: py_int(t.get("likeCount")),
        reposts: py_int(t.get("retweetCount")),
        replies: py_int(t.get("replyCount")),
        echo,
    }
}

/// One `advanced_search` response page → (tweets, has_next_page, next_cursor).
/// Falsy cursors collapse to `""` (the Python loop breaks on either).
#[must_use]
pub fn parse_page(data: &Value) -> (Vec<Tweet>, bool, String) {
    let tweets = data
        .get("tweets")
        .and_then(Value::as_array)
        .map(|items| items.iter().map(normalize_tweet).collect())
        .unwrap_or_default();
    let has_next = data.get("has_next_page").is_some_and(py_truthy);
    let cursor = data
        .get("next_cursor")
        .filter(|v| py_truthy(v))
        .map(py_str)
        .unwrap_or_default();
    (tweets, has_next, cursor)
}

/// Compose the advanced-search query for the chosen filter class — the Python
/// `build_query` verbatim, driven by the same `sources.yaml` keys.
pub fn build_query(cls: &str, src: &Yaml, override_q: &str) -> Result<String, String> {
    if !override_q.is_empty() {
        return Ok(override_q.to_string());
    }
    let x = src.get("x");
    match cls {
        "firehose" => Ok(x
            .and_then(|x| x.get("firehose_query"))
            .and_then(Yaml::as_str)
            .filter(|q| !q.is_empty())
            .unwrap_or(DEFAULT_FIREHOSE)
            .to_string()),
        "amplifier" => {
            let accts: Vec<&str> = x
                .and_then(|x| x.get("amplifier_accounts"))
                .and_then(Yaml::as_list)
                .map(|l| l.iter().filter_map(Yaml::as_str).collect())
                .unwrap_or_default();
            if accts.is_empty() {
                return Err("no amplifier_accounts in sources.yaml".to_string());
            }
            Ok(accts
                .iter()
                .map(|a| format!("from:{a}"))
                .collect::<Vec<_>>()
                .join(" OR "))
        }
        "list" => {
            let lists = x
                .and_then(|x| x.get("lists"))
                .and_then(Yaml::as_list)
                .unwrap_or_default();
            match lists.first() {
                Some(first) => Ok(format!(
                    "list:{}",
                    first.get("id").and_then(Yaml::as_str).unwrap_or_default()
                )),
                None => Err("no lists in sources.yaml".to_string()),
            }
        }
        other => Err(format!("unknown class {other:?}")),
    }
}

/// Parsed CLI for this subcommand — flags, defaults and choices mirror the
/// Python `argparse` setup.
pub struct Cli {
    /// `--class`: `firehose` | `amplifier` | `list`.
    pub cls: String,
    /// `--sources` (default `sources.yaml`, resolved from the cwd like Python).
    pub sources: String,
    /// `--query`: override the class query.
    pub query: String,
    /// `--type`: `Latest` | `Top`.
    pub query_type: String,
    /// `--pages`: pagination depth per pass.
    pub pages: i64,
    /// `--watch`: poll every N seconds if > 0.
    pub watch: f64,
    /// `--replay <fixture>`: deterministic offline mode (§22).
    pub replay: Option<String>,
    /// `--follow <file>`: curated designated-caller author file (one handle or
    /// user-id per line; `#` comments allowed). Tweets from these authors are
    /// tagged `is_designated_caller` on emission.
    pub follow: Option<String>,
    /// `--authors <ids>`: inline curated designated-caller authors, comma- and/or
    /// whitespace-separated. Unioned with `--follow`.
    pub authors: String,
}

impl Cli {
    /// Parse subcommand arguments; `Err` messages go to stderr with exit 2
    /// (argparse behavior).
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut cli = Self {
            cls: "firehose".into(),
            sources: "sources.yaml".into(),
            query: String::new(),
            query_type: "Latest".into(),
            pages: 1,
            watch: 0.0,
            replay: None,
            follow: None,
            authors: String::new(),
        };
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut val = |flag: &str| {
                it.next()
                    .cloned()
                    .ok_or_else(|| format!("{flag} needs a value"))
            };
            match a.as_str() {
                "--class" => {
                    cli.cls = val("--class")?;
                    if !["firehose", "amplifier", "list"].contains(&cli.cls.as_str()) {
                        return Err(format!("invalid --class {:?}", cli.cls));
                    }
                }
                "--sources" => cli.sources = val("--sources")?,
                "--query" => cli.query = val("--query")?,
                "--type" => {
                    cli.query_type = val("--type")?;
                    if !["Latest", "Top"].contains(&cli.query_type.as_str()) {
                        return Err(format!("invalid --type {:?}", cli.query_type));
                    }
                }
                "--pages" => {
                    cli.pages = val("--pages")?
                        .parse()
                        .map_err(|e| format!("bad --pages: {e}"))?;
                }
                "--watch" => {
                    cli.watch = val("--watch")?
                        .parse()
                        .map_err(|e| format!("bad --watch: {e}"))?;
                }
                "--replay" => cli.replay = Some(val("--replay")?),
                "--follow" => cli.follow = Some(val("--follow")?),
                "--authors" => cli.authors = val("--authors")?,
                other => return Err(format!("unknown flag {other:?}")),
            }
        }
        Ok(cli)
    }
}

/// Assemble the curated follow set from the CLI: inline `--authors` unioned with a
/// `--follow` file. Reading the file is the only I/O; a missing/unreadable file is
/// a LOUD stderr warning that degrades to "no file entries" rather than aborting the
/// capture — the lane still runs, just without those designations (§67 degrade-not-die).
fn build_follow_set(cli: &Cli) -> FollowSet {
    let mut follow = FollowSet::new();
    if !cli.authors.is_empty() {
        follow.extend_from_arg(&cli.authors);
    }
    if let Some(path) = &cli.follow {
        match std::fs::read_to_string(path) {
            Ok(contents) => follow.extend_from_file(&contents),
            Err(e) => eprintln!(
                "[pq-social-capture] twitterapi: --follow {path}: {e} \
                 (continuing without file entries)"
            ),
        }
    }
    follow
}

/// Subcommand entry. `now_ns` is the capture-edge clock injected by `main.rs`
/// (§22 — this module never reads a clock itself).
pub fn run(args: &[String], now_ns: fn() -> u64) -> u8 {
    let cli = match Cli::parse(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[pq-social-capture] twitterapi: {e}");
            return 2;
        }
    };

    // Curated designated-caller set (§29): built once, applied in both replay and
    // live paths so a followed author is tagged identically offline and online.
    let follow = build_follow_set(&cli);
    if !follow.is_empty() {
        eprintln!(
            "[x] designated-caller follow set: {} author(s)",
            follow.len()
        );
    }

    if let Some(path) = &cli.replay {
        return crate::replay_pages(path, |page, ring, emit_line| {
            let (tweets, _, _) = parse_page(page);
            for t in tweets {
                if ring.insert(&t.id) {
                    emit_line(&t.event_designated(&follow));
                }
            }
        });
    }

    let key = std::env::var("TWITTERAPI_IO_KEY").unwrap_or_default();
    let key = key.trim();
    if key.is_empty() {
        eprintln!("error: set TWITTERAPI_IO_KEY (https://twitterapi.io, pay-as-you-go)");
        return 2;
    }

    let sources = yaml::load_file(&cli.sources);
    let query = match build_query(&cli.cls, &sources, &cli.query) {
        Ok(q) => q,
        Err(e) => {
            // Python `raise SystemExit(msg)`: message on stderr, exit 1.
            eprintln!("{e}");
            return 1;
        }
    };
    eprintln!("[x:{}] query: {}", cli.cls, query);

    let http = http::Http::new(TIMEOUT_SECS);
    let mut ring = dedupe::DedupeRing::new(dedupe::DEFAULT_CAP);
    let stdout = std::io::stdout();

    let one_pass = |ring: &mut dedupe::DedupeRing| -> u64 {
        let mut emitted = 0u64;
        let mut cursor = String::new();
        let mut out = stdout.lock();
        for _ in 0..cli.pages.max(1) {
            let url = format!(
                "{BASE}?{}",
                urlenc::urlencode(&[
                    ("query", &query),
                    ("queryType", &cli.query_type),
                    ("cursor", &cursor),
                ])
            );
            let page = match http
                .get(&url, &[("X-API-Key", key)])
                .and_then(|body| json::parse(&body).map_err(|e| format!("bad JSON: {e}")))
            {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("fetch error: {e}");
                    return emitted;
                }
            };
            let (tweets, has_next, next_cursor) = parse_page(&page);
            for t in tweets {
                if !ring.insert(&t.id) {
                    continue;
                }
                if emit::write_line(
                    &mut out,
                    &emit::event_line(&t.event_designated(&follow), now_ns()),
                )
                .is_err()
                {
                    return emitted; // downstream pipe closed: stop the pass
                }
                emitted += 1;
            }
            cursor = next_cursor;
            if !has_next || cursor.is_empty() {
                break;
            }
        }
        emitted
    };

    if cli.watch > 0.0 {
        eprintln!(
            "[watch] polling every {}s; Ctrl-C to stop",
            crate::py_float(cli.watch)
        );
        loop {
            let n = one_pass(&mut ring);
            eprintln!(
                "[watch] {}: emitted {} new (seen {})",
                cli.cls,
                n,
                ring.len()
            );
            std::thread::sleep(std::time::Duration::from_secs_f64(cli.watch));
        }
    } else {
        let n = one_pass(&mut ring);
        eprintln!("emitted {n} normalized events");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Value {
        json::parse(s).unwrap()
    }

    fn tw(author: &str, author_id: &str) -> Tweet {
        Tweet {
            id: "1".into(),
            author: author.into(),
            author_id: author_id.into(),
            text: "$WIF".into(),
            likes: 0,
            reposts: 0,
            replies: 0,
            echo: false,
        }
    }

    #[test]
    fn normalize_basic_tweet() {
        // The vendor `author.id` is captured for follow-matching (never emitted).
        let t = normalize_tweet(&v(
            r#"{"id":1946001111,"author":{"id":777,"userName":"cryptoKOL"},
                "text":"send it $WIF","likeCount":420,"retweetCount":69,
                "replyCount":12,"isReply":false}"#,
        ));
        assert_eq!(
            t,
            Tweet {
                id: "1946001111".into(),
                author: "cryptoKOL".into(),
                author_id: "777".into(),
                text: "send it $WIF".into(),
                likes: 420,
                reposts: 69,
                replies: 12,
                echo: false,
            }
        );
        // Absent author.id → empty (never designates by id).
        assert_eq!(
            normalize_tweet(&v(r#"{"author":{"userName":"x"}}"#)).author_id,
            ""
        );
    }

    #[test]
    fn follow_set_parses_arg_and_file_stripping_at_and_comments() {
        let mut fs = FollowSet::new();
        assert!(fs.is_empty());
        fs.extend_from_arg("@blknoiz06, 777  OrangeSBS");
        // File: whole-line comment + blank + handle + id with a trailing comment.
        fs.extend_from_file("# curated CT callers\n@cryptoKOL\n\n888 # stable id\n");
        assert_eq!(fs.len(), 5, "blknoiz06,777,OrangeSBS,cryptoKOL,888");
        assert!(!fs.is_empty());
    }

    #[test]
    fn designated_when_author_handle_or_id_is_followed() {
        let mut fs = FollowSet::new();
        fs.extend_from_arg("blknoiz06, 777");
        assert!(fs.is_designated(&tw("blknoiz06", "999")), "handle match");
        assert!(
            fs.is_designated(&tw("someoneelse", "777")),
            "stable-id match"
        );
        assert!(!fs.is_designated(&tw("nobody", "123")), "neither matches");
        // Empty set never designates; empty author_id never matches by id.
        assert!(!FollowSet::new().is_designated(&tw("blknoiz06", "777")));
        assert!(!fs.is_designated(&tw("nobody", "")));
    }

    #[test]
    fn followed_author_line_is_tagged_others_untagged() {
        let mut fs = FollowSet::new();
        fs.extend_from_arg("@blknoiz06");
        let followed = emit::event_line(&tw("blknoiz06", "").event_designated(&fs), 5);
        let other = emit::event_line(&tw("randomdegen", "").event_designated(&fs), 5);
        assert!(followed.contains("\"is_designated_caller\":true"));
        assert!(
            !other.contains("is_designated_caller"),
            "non-followed author byte-identical to legacy schema"
        );
        // The plain (undesignated) event never carries the flag.
        assert!(!emit::event_line(&tw("blknoiz06", "").event(), 5).contains("is_designated_caller"));
    }

    #[test]
    fn echo_from_reply_retweet_or_quote() {
        assert!(normalize_tweet(&v(r#"{"isReply":true}"#)).echo);
        assert!(normalize_tweet(&v(r#"{"retweeted_tweet":{"id":"x"}}"#)).echo);
        assert!(normalize_tweet(&v(r#"{"quoted_tweet":{"id":"x"}}"#)).echo);
        // Python: `tweet.get("retweeted_tweet") is not None` — JSON null is None.
        assert!(!normalize_tweet(&v(r#"{"retweeted_tweet":null,"isReply":false}"#)).echo);
    }

    #[test]
    fn engagement_coerces_like_python_int() {
        let t = normalize_tweet(&v(
            r#"{"likeCount":2.9,"retweetCount":-3,"replyCount":null}"#,
        ));
        assert_eq!((t.likes, t.reposts, t.replies), (2, 0, 0));
        let t = normalize_tweet(&v(r#"{"likeCount":"7"}"#));
        assert_eq!(t.likes, 7);
    }

    #[test]
    fn missing_author_is_unknown() {
        assert_eq!(normalize_tweet(&v(r#"{"author":{}}"#)).author, "unknown");
        assert_eq!(normalize_tweet(&v(r#"{}"#)).author, "unknown");
        assert_eq!(
            normalize_tweet(&v(r#"{"author":{"userName":""}}"#)).author,
            "unknown"
        );
    }

    #[test]
    fn page_parse_reads_cursor_semantics() {
        let (tweets, has_next, cursor) = parse_page(&v(
            r#"{"tweets":[{"id":"1"}],"has_next_page":true,"next_cursor":"pg2"}"#,
        ));
        assert_eq!(tweets.len(), 1);
        assert!(has_next);
        assert_eq!(cursor, "pg2");
        let (_, has_next, cursor) = parse_page(&v(r#"{"tweets":[],"next_cursor":null}"#));
        assert!(!has_next);
        assert_eq!(cursor, "", "falsy cursor collapses to empty (loop breaks)");
    }

    #[test]
    fn build_query_firehose_default_and_sources() {
        let empty = Yaml::Map(vec![]);
        assert_eq!(
            build_query("firehose", &empty, "").unwrap(),
            DEFAULT_FIREHOSE
        );
        assert_eq!(
            build_query("firehose", &empty, "override").unwrap(),
            "override"
        );
        let src = yaml::parse("x:\n  firehose_query: 'custom q'\n");
        assert_eq!(build_query("firehose", &src, "").unwrap(), "custom q");
    }

    #[test]
    fn build_query_amplifier_joins_watchlist() {
        let src = yaml::parse("x:\n  amplifier_accounts:\n    - blknoiz06\n    - OrangeSBS\n");
        assert_eq!(
            build_query("amplifier", &src, "").unwrap(),
            "from:blknoiz06 OR from:OrangeSBS"
        );
        assert_eq!(
            build_query("amplifier", &Yaml::Map(vec![]), "").unwrap_err(),
            "no amplifier_accounts in sources.yaml"
        );
    }

    #[test]
    fn build_query_list_uses_first_list_id() {
        let src =
            yaml::parse("x:\n  lists:\n    - id: '2074150651030876515'\n      label: greek\n");
        assert_eq!(
            build_query("list", &src, "").unwrap(),
            "list:2074150651030876515"
        );
        assert_eq!(
            build_query("list", &Yaml::Map(vec![]), "").unwrap_err(),
            "no lists in sources.yaml"
        );
    }
}
