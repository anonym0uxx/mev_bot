//! `tiktok` subcommand — Rust twin of `tools/social-ingest/tiktok_stream.py`
//! (TikTok → normalized SocialEvent NDJSON, the slow-meta / broad-narrative
//! `[S]` tier).
//!
//! Provider-agnostic REST polling, exactly like the Python twin: point
//! `TIKTOK_API_BASE` + `TIKTOK_API_KEY` at whichever scraper you subscribe to
//! (Data365, ScrapeBadger, ...); the normalizer maps the common video fields
//! defensively (`{videos|data|items|results: [...]}`), same hashtag watchlist
//! from `sources.yaml`, same dedupe by video id. Digg = likes, share, comment;
//! a duet/stitch is an `echo`. Reliability note carried over verbatim: TikTok
//! scrapers degrade after platform updates — treat gaps as missing data, never
//! fabricate. `[S]`: no decision, no sentiment label (§83).

use crate::json::{self, py_int, py_str, py_truthy, Value};
use crate::yaml::Yaml;
use crate::{dedupe, emit, http, urlenc, yaml};

/// Python twin's `urlopen(..., timeout=30)`.
const TIMEOUT_SECS: u64 = 30;

/// Fallback hashtags when `sources.yaml` is missing/empty — Python twin's
/// `["solana", "memecoin"]`.
pub const DEFAULT_HASHTAGS: [&str; 2] = ["solana", "memecoin"];

/// One normalized video, ready for emission.
#[derive(Debug, PartialEq, Eq)]
pub struct Video {
    /// `id` (falling back to `aweme_id`) — the dedupe key.
    pub id: String,
    /// `author.uniqueId` | `author.nickname` | `authorName` | `"unknown"`.
    pub author: String,
    /// Description + `#hashtags`, the text the core scans for cashtags.
    pub text: String,
    /// `diggCount` (falling back to `likeCount`) via Python-`_int`.
    pub likes: u64,
    /// `shareCount` via Python-`_int`.
    pub reposts: u64,
    /// `commentCount` via Python-`_int`.
    pub replies: u64,
    /// Duet / stitch — a reaction, not an origination.
    pub echo: bool,
}

impl Video {
    /// Borrow as an emission event (platform `"tiktok"`, no community).
    #[must_use]
    pub fn event(&self) -> emit::Event<'_> {
        emit::Event {
            platform: "tiktok",
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

/// First truthy value from a key chain on `v` (mirrors Python `a or b or c`).
fn first_truthy<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter()
        .filter_map(|k| v.get(k))
        .find(|val| py_truthy(val))
}

/// TikTok video object → normalized video. Pure (§22); mirrors the Python
/// `normalize_video` field-for-field, including the `stats`-or-self fallback
/// and the presence-based `diggCount`/`likeCount` chain.
#[must_use]
pub fn normalize_video(v: &Value) -> Video {
    let author = v.get("author");
    let handle = author
        .and_then(|a| first_truthy(a, &["uniqueId", "nickname"]))
        .or_else(|| first_truthy(v, &["authorName"]))
        .map(py_str)
        .unwrap_or_else(|| "unknown".to_string());
    let desc = first_truthy(v, &["desc", "description", "title"])
        .map(py_str)
        .unwrap_or_default();
    let tags: &[Value] = first_truthy(v, &["hashtags", "challenges"])
        .and_then(Value::as_array)
        .unwrap_or_default();
    let tagtext = tags
        .iter()
        .map(|t| {
            // Python: f"#{t.get('name', t) if isinstance(t, dict) else t}" —
            // key presence decides, even a null name renders as "None".
            let name = match t {
                Value::Object(_) => t.get("name").map(py_str).unwrap_or_else(|| py_str(t)),
                other => py_str(other),
            };
            format!("#{name}")
        })
        .collect::<Vec<_>>()
        .join(" ");
    // Python `v.get("stats") or v`: absent/empty stats fall back to the video
    // object itself (flat-counter providers).
    let stats = v.get("stats").filter(|s| py_truthy(s)).unwrap_or(v);
    let likes = match stats.get("diggCount") {
        Some(d) => py_int(Some(d)),
        None => py_int(stats.get("likeCount")),
    };
    let echo = ["duetInfo", "stitchInfo", "isDuet", "isStitch"]
        .iter()
        .any(|k| v.get(k).is_some_and(py_truthy));
    let id = v
        .get("id")
        .or_else(|| v.get("aweme_id"))
        .map(py_str)
        .unwrap_or_default();
    Video {
        id,
        author: handle,
        text: format!("{desc} {tagtext}").trim().to_string(),
        likes,
        reposts: py_int(stats.get("shareCount")),
        replies: py_int(stats.get("commentCount")),
        echo,
    }
}

/// Extract the video list from a provider response — defensive to provider
/// shape, exactly like the Python `fetch`: first array under
/// `videos`/`data`/`items`/`results`, else a bare top-level array, else empty.
#[must_use]
pub fn extract_videos(data: &Value) -> &[Value] {
    for key in ["videos", "data", "items", "results"] {
        if let Some(arr) = data.get(key).and_then(Value::as_array) {
            return arr;
        }
    }
    data.as_array().unwrap_or_default()
}

/// Hashtag watchlist: `sources.yaml` `tiktok.hashtags`, defaulting like the
/// Python twin.
#[must_use]
pub fn load_hashtags(src: &Yaml) -> Vec<String> {
    let tags: Vec<String> = src
        .get("tiktok")
        .and_then(|t| t.get("hashtags"))
        .and_then(Yaml::as_list)
        .map(|l| {
            l.iter()
                .filter_map(Yaml::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    if tags.is_empty() {
        DEFAULT_HASHTAGS.iter().map(|s| s.to_string()).collect()
    } else {
        tags
    }
}

/// Parsed CLI — flags and defaults mirror the Python `argparse` setup.
pub struct Cli {
    /// `--hashtag`: single hashtag; else all from `sources.yaml`.
    pub hashtag: String,
    /// `--sources` (default `sources.yaml`).
    pub sources: String,
    /// `--watch`: poll every N seconds if > 0.
    pub watch: f64,
    /// `--replay <fixture>`: deterministic offline mode (§22).
    pub replay: Option<String>,
}

impl Cli {
    /// Parse subcommand arguments.
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut cli = Self {
            hashtag: String::new(),
            sources: "sources.yaml".into(),
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
                "--hashtag" => cli.hashtag = val("--hashtag")?,
                "--sources" => cli.sources = val("--sources")?,
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

/// Subcommand entry. `now_ns` is the capture-edge clock injected by `main.rs`.
pub fn run(args: &[String], now_ns: fn() -> u64) -> u8 {
    let cli = match Cli::parse(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[pq-social-capture] tiktok: {e}");
            return 2;
        }
    };

    if let Some(path) = &cli.replay {
        return crate::replay_pages(path, |page, ring, emit_line| {
            for v in extract_videos(page) {
                let video = normalize_video(v);
                if ring.insert(&video.id) {
                    emit_line(&video.event());
                }
            }
        });
    }

    let key = std::env::var("TIKTOK_API_KEY").unwrap_or_default();
    let base = std::env::var("TIKTOK_API_BASE").unwrap_or_default();
    let (key, base) = (key.trim(), base.trim());
    if key.is_empty() || base.is_empty() {
        eprintln!("error: set TIKTOK_API_KEY and TIKTOK_API_BASE (your provider endpoint)");
        return 2;
    }

    let tags = if cli.hashtag.is_empty() {
        load_hashtags(&yaml::load_file(&cli.sources))
    } else {
        vec![cli.hashtag.clone()]
    };

    let http = http::Http::new(TIMEOUT_SECS);
    let auth = format!("Bearer {key}");
    let mut ring = dedupe::DedupeRing::new(dedupe::DEFAULT_CAP);
    let stdout = std::io::stdout();

    let one_pass = |ring: &mut dedupe::DedupeRing| -> u64 {
        let mut n = 0u64;
        let mut out = stdout.lock();
        for tag in &tags {
            let url = format!(
                "{base}?{}",
                urlenc::urlencode(&[("hashtag", tag), ("count", "50")])
            );
            let page = match http
                .get(&url, &[("Authorization", &auth)])
                .and_then(|body| json::parse(&body).map_err(|e| format!("bad JSON: {e}")))
            {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("fetch error ({tag}): {e}");
                    continue;
                }
            };
            for v in extract_videos(&page) {
                let video = normalize_video(v);
                if !ring.insert(&video.id) {
                    continue;
                }
                if emit::write_line(&mut out, &emit::event_line(&video.event(), now_ns())).is_err()
                {
                    return n;
                }
                n += 1;
            }
        }
        n
    };

    if cli.watch > 0.0 {
        loop {
            let n = one_pass(&mut ring);
            eprintln!("[tiktok] emitted {} (seen {})", n, ring.len());
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
    fn normalize_full_video() {
        let vid = normalize_video(&v(r#"{"id":"777001","author":{"uniqueId":"memelord"},
                "desc":"this $WIF coin is insane",
                "hashtags":[{"name":"solana"},{"name":"memecoin"}],
                "stats":{"diggCount":12000,"shareCount":800,"commentCount":300}}"#));
        assert_eq!(
            vid,
            Video {
                id: "777001".into(),
                author: "memelord".into(),
                text: "this $WIF coin is insane #solana #memecoin".into(),
                likes: 12000,
                reposts: 800,
                replies: 300,
                echo: false,
            }
        );
    }

    #[test]
    fn author_fallback_chain() {
        assert_eq!(
            normalize_video(&v(r#"{"author":{"nickname":"nick"}}"#)).author,
            "nick"
        );
        assert_eq!(
            normalize_video(&v(r#"{"authorName":"flat"}"#)).author,
            "flat"
        );
        assert_eq!(normalize_video(&v(r#"{}"#)).author, "unknown");
        assert_eq!(
            normalize_video(&v(r#"{"author":{"uniqueId":""},"authorName":"x"}"#)).author,
            "x",
            "empty uniqueId is falsy: chain continues"
        );
    }

    #[test]
    fn stats_fall_back_to_flat_counters() {
        let vid = normalize_video(&v(
            r#"{"aweme_id":888002,"desc":"d","diggCount":5,"shareCount":1,"commentCount":2}"#,
        ));
        assert_eq!((vid.likes, vid.reposts, vid.replies), (5, 1, 2));
        assert_eq!(vid.id, "888002", "aweme_id fallback");
        // Empty stats object is falsy in Python: fall back to the video itself.
        let vid = normalize_video(&v(r#"{"stats":{},"likeCount":9}"#));
        assert_eq!(vid.likes, 9);
    }

    #[test]
    fn digg_count_presence_beats_like_count() {
        // Python stats.get("diggCount", stats.get("likeCount", 0)): a PRESENT
        // diggCount — even null — wins over likeCount.
        let vid = normalize_video(&v(r#"{"stats":{"diggCount":null,"likeCount":7}}"#));
        assert_eq!(vid.likes, 0);
        let vid = normalize_video(&v(r#"{"stats":{"likeCount":7}}"#));
        assert_eq!(vid.likes, 7);
    }

    #[test]
    fn scalar_challenge_tags_and_title_fallback() {
        let vid = normalize_video(&v(
            r#"{"title":"title text","challenges":["pumpfun","crypto"]}"#,
        ));
        assert_eq!(vid.text, "title text #pumpfun #crypto");
        // No desc, no tags: strip leaves the empty string.
        assert_eq!(normalize_video(&v(r#"{}"#)).text, "");
    }

    #[test]
    fn duet_and_stitch_are_echo() {
        assert!(normalize_video(&v(r#"{"isDuet":true}"#)).echo);
        assert!(normalize_video(&v(r#"{"duetInfo":{"from":"x"}}"#)).echo);
        assert!(normalize_video(&v(r#"{"stitchInfo":{"from":"x"}}"#)).echo);
        assert!(!normalize_video(&v(r#"{"isDuet":false,"duetInfo":null}"#)).echo);
    }

    #[test]
    fn provider_shape_defensive_extraction() {
        assert_eq!(extract_videos(&v(r#"{"videos":[{"id":"1"}]}"#)).len(), 1);
        assert_eq!(
            extract_videos(&v(r#"{"data":[{"id":"1"},{"id":"2"}]}"#)).len(),
            2
        );
        assert_eq!(extract_videos(&v(r#"{"items":[]}"#)).len(), 0);
        assert_eq!(extract_videos(&v(r#"{"results":[{"id":"9"}]}"#)).len(), 1);
        assert_eq!(extract_videos(&v(r#"[{"id":"1"}]"#)).len(), 1, "bare array");
        assert_eq!(extract_videos(&v(r#"{"error":"x"}"#)).len(), 0);
    }

    #[test]
    fn hashtag_watchlist_defaults() {
        let src = crate::yaml::parse("tiktok:\n  hashtags:\n    - solana\n    - pumpfun\n");
        assert_eq!(load_hashtags(&src), ["solana", "pumpfun"]);
        assert_eq!(
            load_hashtags(&Yaml::Map(vec![])),
            DEFAULT_HASHTAGS,
            "missing sources fall back to the Python default watchlist"
        );
    }
}
