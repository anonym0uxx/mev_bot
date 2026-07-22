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
    /// `normalize_tweet` in the Python twin).
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
        }
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
    let present_non_null = |key: &str| matches!(t.get(key), Some(v) if !matches!(v, Value::Null));
    let echo = t.get("isReply").is_some_and(py_truthy)
        || present_non_null("retweeted_tweet")
        || present_non_null("quoted_tweet");
    Tweet {
        id: t.get("id").map(py_str).unwrap_or_default(),
        author,
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
                other => return Err(format!("unknown flag {other:?}")),
            }
        }
        Ok(cli)
    }
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

    if let Some(path) = &cli.replay {
        return crate::replay_pages(path, |page, ring, emit_line| {
            let (tweets, _, _) = parse_page(page);
            for t in tweets {
                if ring.insert(&t.id) {
                    emit_line(&t.event());
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
                if emit::write_line(&mut out, &emit::event_line(&t.event(), now_ns())).is_err() {
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

    #[test]
    fn normalize_basic_tweet() {
        let t = normalize_tweet(&v(r#"{"id":1946001111,"author":{"userName":"cryptoKOL"},
                "text":"send it $WIF","likeCount":420,"retweetCount":69,
                "replyCount":12,"isReply":false}"#));
        assert_eq!(
            t,
            Tweet {
                id: "1946001111".into(),
                author: "cryptoKOL".into(),
                text: "send it $WIF".into(),
                likes: 420,
                reposts: 69,
                replies: 12,
                echo: false,
            }
        );
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
