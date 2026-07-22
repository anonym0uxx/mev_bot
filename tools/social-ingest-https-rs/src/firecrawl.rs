//! `firecrawl` subcommand — Rust twin of `tools/social-ingest/firecrawl_stream.py`
//! (Firecrawl → normalized SocialEvent NDJSON, the `[S]` general-web breadth
//! adapter).
//!
//! The downstream / legibility tier: scrape news, aggregators and project
//! pages to markdown; each page becomes ONE `web` event whose text the core
//! scans for cashtags + contract addresses. NOT real-time and NOT primary — it
//! is the "what has become legible / crowded" clock (a page listing a coin
//! means it is already surfaced). Same endpoint, same request body, same
//! `FIRECRAWL_API_KEY` env credential, same 20 000-char text cap and page list
//! (`sources.yaml` `web.pages`) as the Python twin. Never used to scrape
//! X/TikTok and never with a personal logged-in session (§29.7e — the API key
//! is the sacrificial identity). `[S]`: no decision, no sentiment (§83).

use crate::json::{self, py_str, py_truthy, Value};
use crate::yaml::Yaml;
// NOTE: no dedupe ring here — the `web` lane re-scrapes the same pages every
// pass BY DESIGN (it is a legibility clock, not an id-stream), matching the
// Python twin exactly.
use crate::{emit, http, yaml};

/// The one endpoint, identical to the Python twin's `SCRAPE`.
pub const SCRAPE: &str = "https://api.firecrawl.dev/v1/scrape";
/// Cap page text so one NDJSON line stays manageable — Python `MAX_TEXT`
/// (a CHARACTER cap: Python slices `str`, not bytes).
pub const MAX_TEXT_CHARS: usize = 20_000;
/// Python twin's `urlopen(..., timeout=60)`.
const TIMEOUT_SECS: u64 = 60;

/// Python `markdown[:MAX_TEXT]` — a character slice, UTF-8 safe by
/// construction.
#[must_use]
pub fn truncate_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

/// `urllib.parse.urlparse(url).netloc or "web"` — the netloc is present only
/// after a valid `scheme://` (or a scheme-less `//`) prefix; anything else is
/// the `"web"` fallback.
#[must_use]
pub fn domain_of(url: &str) -> String {
    let rest = match url.split_once(':') {
        Some((scheme, rest)) if is_valid_scheme(scheme) => rest,
        _ => url,
    };
    match rest.strip_prefix("//") {
        Some(after) => {
            let end = after.find(['/', '?', '#']).unwrap_or(after.len());
            let netloc = &after[..end];
            if netloc.is_empty() {
                "web".to_string()
            } else {
                netloc.to_string()
            }
        }
        None => "web".to_string(),
    }
}

fn is_valid_scheme(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// The scrape request body, byte-identical to the Python twin's
/// `json.dumps({"url": url, "formats": ["markdown"]})` (default separators:
/// `", "` / `": "`).
#[must_use]
pub fn request_body(url: &str) -> String {
    let mut out = String::with_capacity(40 + url.len());
    out.push_str("{\"url\": \"");
    emit::escape_json_into(url, &mut out);
    out.push_str("\", \"formats\": [\"markdown\"]}");
    out
}

/// Firecrawl scrape response → markdown string (empty on any miss). Mirrors
/// the Python: `d = data.get("data") or data`, then
/// `(d.get("markdown") or d.get("content") or "")[:MAX_TEXT]`.
#[must_use]
pub fn extract_markdown(data: &Value) -> String {
    let d = data.get("data").filter(|d| py_truthy(d)).unwrap_or(data);
    let md = d
        .get("markdown")
        .filter(|v| py_truthy(v))
        .or_else(|| d.get("content").filter(|v| py_truthy(v)))
        .map(py_str)
        .unwrap_or_default();
    truncate_chars(&md, MAX_TEXT_CHARS).to_string()
}

/// A scraped page → one `web` event (author = domain; no engagement) —
/// the Python `normalize_page`.
#[must_use]
pub fn page_event<'a>(domain: &'a str, markdown: &'a str) -> emit::Event<'a> {
    emit::Event {
        platform: "web",
        author: domain,
        community: domain,
        text: markdown,
        likes: 0,
        reposts: 0,
        replies: 0,
        echo: false,
    }
}

/// Page list: `sources.yaml` `web.pages` (empty when missing, like Python).
#[must_use]
pub fn load_pages(src: &Yaml) -> Vec<String> {
    src.get("web")
        .and_then(|w| w.get("pages"))
        .and_then(Yaml::as_list)
        .map(|l| {
            l.iter()
                .filter_map(Yaml::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Parsed CLI — flags and defaults mirror the Python `argparse` setup.
pub struct Cli {
    /// `--url`: single URL; else all from `sources.yaml`.
    pub url: String,
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
            url: String::new(),
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
                "--url" => cli.url = val("--url")?,
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
            eprintln!("[pq-social-capture] firecrawl: {e}");
            return 2;
        }
    };

    // URL resolution is shared by live and replay: --url wins, else the
    // sources.yaml page list; neither present is the Python twin's exit 2.
    let resolve_urls = || -> Result<Vec<String>, u8> {
        let urls: Vec<String> = if cli.url.is_empty() {
            load_pages(&yaml::load_file(&cli.sources))
        } else {
            vec![cli.url.clone()]
        };
        if urls.is_empty() {
            eprintln!("no pages configured (sources.yaml web.pages) and no --url");
            return Err(2);
        }
        Ok(urls)
    };

    if let Some(path) = &cli.replay {
        let urls = match resolve_urls() {
            Ok(u) => u,
            Err(code) => return code,
        };
        // Replay pairs the Nth saved response with the Nth configured URL
        // (cycling) — a fixture is a saved sequence of scrape responses.
        let mut idx = 0usize;
        return crate::replay_pages(path, |page, _ring, emit_line| {
            let domain = domain_of(&urls[idx % urls.len()]);
            idx += 1;
            let md = extract_markdown(page);
            if !md.is_empty() {
                emit_line(&page_event(&domain, &md));
            }
        });
    }

    // Live: the Python twin checks the credential FIRST, then the page list.
    let key = std::env::var("FIRECRAWL_API_KEY").unwrap_or_default();
    let key = key.trim();
    if key.is_empty() {
        eprintln!("error: set FIRECRAWL_API_KEY (https://firecrawl.dev)");
        return 2;
    }
    let urls = match resolve_urls() {
        Ok(u) => u,
        Err(code) => return code,
    };

    let http = http::Http::new(TIMEOUT_SECS);
    let auth = format!("Bearer {key}");
    let headers = [
        ("Authorization", auth.as_str()),
        ("Content-Type", "application/json"),
    ];
    let stdout = std::io::stdout();

    let one_pass = || -> u64 {
        let mut n = 0u64;
        let mut out = stdout.lock();
        for url in &urls {
            let data = match http
                .post_json(SCRAPE, &headers, &request_body(url))
                .and_then(|body| json::parse(&body).map_err(|e| format!("bad JSON: {e}")))
            {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("scrape error ({url}): {e}");
                    continue;
                }
            };
            let md = extract_markdown(&data);
            if md.is_empty() {
                continue; // Python `if md:` — empty scrapes emit nothing
            }
            let domain = domain_of(url);
            let line = emit::event_line(&page_event(&domain, &md), now_ns());
            if emit::write_line(&mut out, &line).is_err() {
                return n;
            }
            n += 1;
        }
        n
    };

    if cli.watch > 0.0 {
        loop {
            eprintln!("[firecrawl] scraped {} pages", one_pass());
            std::thread::sleep(std::time::Duration::from_secs_f64(cli.watch));
        }
    } else {
        eprintln!("scraped {} pages", one_pass());
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
    fn domain_extraction_mirrors_urlparse() {
        assert_eq!(
            domain_of("https://www.dexscreener.com/solana"),
            "www.dexscreener.com"
        );
        assert_eq!(domain_of("https://pump.fun/board"), "pump.fun");
        assert_eq!(domain_of("//host.only/x"), "host.only");
        assert_eq!(domain_of("no-scheme/path"), "web");
        assert_eq!(domain_of("https://"), "web", "empty netloc falls back");
        assert_eq!(
            domain_of("1abc://weird"),
            "web",
            "invalid scheme has no netloc"
        );
        assert_eq!(domain_of("https://host:8080/x?q#f"), "host:8080");
    }

    #[test]
    fn request_body_matches_python_json_dumps() {
        assert_eq!(
            request_body("https://pump.fun/board"),
            r#"{"url": "https://pump.fun/board", "formats": ["markdown"]}"#
        );
    }

    #[test]
    fn markdown_extraction_with_fallbacks() {
        assert_eq!(
            extract_markdown(&v(r##"{"data":{"markdown":"# md"}}"##)),
            "# md"
        );
        assert_eq!(
            extract_markdown(&v(r#"{"content":"raw fallback"}"#)),
            "raw fallback",
            "no data key: top-level object is used"
        );
        assert_eq!(
            extract_markdown(&v(r#"{"data":{},"content":"c"}"#)),
            "c",
            "empty data object is falsy in Python: fall back to top level"
        );
        assert_eq!(extract_markdown(&v(r#"{"data":{"markdown":""}}"#)), "");
    }

    #[test]
    fn truncation_is_by_chars_not_bytes() {
        let s = "🚀".repeat(10); // 4 bytes per char
        assert_eq!(truncate_chars(&s, 3).chars().count(), 3);
        assert_eq!(truncate_chars("short", 20_000), "short");
        let exact = "x".repeat(5);
        assert_eq!(truncate_chars(&exact, 5), exact);
    }

    #[test]
    fn page_list_from_sources() {
        let src = crate::yaml::parse(
            "web:\n  pages:\n    - https://www.dexscreener.com/solana\n    - https://pump.fun/board\n",
        );
        assert_eq!(
            load_pages(&src),
            [
                "https://www.dexscreener.com/solana",
                "https://pump.fun/board"
            ]
        );
        assert!(load_pages(&Yaml::Map(vec![])).is_empty());
    }
}
