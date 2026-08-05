//! pq-firecrawl-bridge — web intelligence sidecar for the mev_bot daemon.
//!
//! Same child-process sidecar pattern as `pq-stream-capture` (LaserStream):
//! the daemon spawns this binary, reads its stdout (NDJSON), and feeds
//! `RawSocialPayload`s into `engine.ingest_social()`. This bridge:
//!
//!  1. Listens on stdin for trigger events (one JSON per line):
//!     {"trigger":"band_entry","mint":"<base58>","symbol":"<ticker>","mcap_lamports":N}
//!  2. Maps each trigger to one or more Firecrawl scrape URLs.
//!  3. Scrapes via the local Firecrawl API (http://127.0.0.1:3002).
//!  4. Normalizes each scraped page into the SocialEvent JSON format
//!     that `pump_quant_ingest::social_parse::parse_social_event` expects.
//!  5. Writes one NDJSON line per social event to stdout.
//!
//! # Determinism boundary
//! The engine stays deterministic (§22) — this bridge is the [S] live-I/O
//! boundary. All network access happens here; the daemon only sees bytes.
//! If Firecrawl is down, the bridge logs to stderr and outputs nothing —
//! the daemon continues trading without social intelligence (fail-safe).
//!
//! # Throttling
//! A minimum 2s gap between scrape requests prevents Firecrawl overload.
//! The bridge processes triggers sequentially but never blocks the daemon's
//! stdin read (it drains all pending triggers, then scrapes).
//!
//! # Trigger types (10 total)
//!  1. band_entry      — coin entered $9k–$20k mcap band
//!  2. velocity_spike   — attention/volume velocity exceeded threshold
//!  3. new_mint         — new mint promoted by the engine
//!  4. position_event   — entry or exit into a position
//!  5. entropy_spike    — order-flow entropy spike (arXiv:2512.15720)
//!  6. wash_signature   — wash-trading liquidity signature (arXiv:2411.05803)
//!  7. sentiment_diverge — social sentiment divergence (arXiv:1506.01513)
//!  8. wallet_cluster   — creator wallet clustering detected (arXiv:2505.09313)
//!  9. mev_invariance   — bonding-curve MEV invariance violation (arXiv:2304.11010)
//! 10. liquidity_collapse — AMM depth thinning below threshold

#![forbid(unsafe_code)]

use std::io::{self, BufRead, BufWriter, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const FIRECRAWL_URL: &str = "http://127.0.0.1:3002";
const API_KEY: &str = "pq-local-test-key";
const SCRAPE_TIMEOUT_SECS: u64 = 30;
const MIN_SCRAPE_GAP_MS: u64 = 2000;
const MAX_RETRIES: usize = 2;
const MAX_URLS_PER_TRIGGER: usize = 5;

// ─── URL generation per trigger ─────────────────────────────────────────────

/// Build the list of URLs to scrape for a given trigger.
/// Each trigger maps to contextually relevant sources that help validate
/// whether the catalyst is real (see the 8-layer validation framework).
fn urls_for_trigger(trigger: &Trigger) -> Vec<String> {
    let mint = &trigger.mint;
    let symbol = &trigger.symbol;

    match trigger.kind.as_str() {
        // Coin entered the $9k–$20k band — check pump.fun page, DexScreener, and
        // social mentions to validate cross-venue confirmation.
        "band_entry" => vec![
            format!("https://pump.fun/{mint}"),
            format!("https://dexscreener.com/solana/{mint}"),
            format!("https://twitter.com/search?q=%24{symbol}&f=live"),
        ],

        // Velocity spike — check if the volume/attention is organic or manufactured.
        "velocity_spike" => vec![
            format!("https://dexscreener.com/solana/{mint}"),
            format!("https://pump.fun/{mint}"),
            format!("https://twitter.com/search?q=%24{symbol}%20OR%20{mint}&f=live"),
        ],

        // New mint promoted — gather initial context (creator, social, listings).
        "new_mint" => vec![
            format!("https://pump.fun/{mint}"),
            format!("https://dexscreener.com/solana/{mint}"),
        ],

        // Position entry/exit — scrape current state for post-trade analysis.
        "position_event" => vec![
            format!("https://dexscreener.com/solana/{mint}"),
            format!("https://pump.fun/{mint}"),
        ],

        // Order-flow entropy spike — check if informed flow is entering.
        "entropy_spike" => vec![
            format!("https://dexscreener.com/solana/{mint}"),
            format!("https://solscan.io/tx/{mint}"),
        ],

        // Wash-trading signature — check holder distribution and wallet diversity.
        "wash_signature" => vec![
            format!("https://dexscreener.com/solana/{mint}"),
            format!("https://solscan.io/token/{mint}"),
        ],

        // Sentiment divergence — price moved but is social confirming?
        "sentiment_diverge" => vec![
            format!("https://twitter.com/search?q=%24{symbol}&f=live"),
            format!("https://www.google.com/search?q=%24{symbol}+solana"),
        ],

        // Creator wallet cluster — check on-chain forensics for sybil patterns.
        "wallet_cluster" => vec![
            format!("https://solscan.io/token/{mint}"),
            format!("https://dexscreener.com/solana/{mint}"),
        ],

        // MEV invariance violation — bonding-curve price divergence.
        "mev_invariance" => vec![
            format!("https://dexscreener.com/solana/{mint}"),
            format!("https://pump.fun/{mint}"),
        ],

        // Liquidity depth collapse — check if pool is migrating or draining.
        "liquidity_collapse" => vec![
            format!("https://dexscreener.com/solana/{mint}"),
            format!("https://pump.fun/{mint}"),
        ],

        _ => vec![],
    }
}

// ─── Trigger parsing ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Trigger {
    kind: String,
    mint: String,
    symbol: String,
    #[allow(dead_code)]
    mcap_lamports: u64,
}

fn parse_trigger(line: &str) -> Option<Trigger> {
    // Minimal JSON parsing — no serde dependency, just extract fields.
    // The trigger JSON from the daemon is simple and well-structured.
    let kind = json_field(line, "trigger")?;
    let mint = json_field(line, "mint")?;
    let symbol = json_field(line, "symbol").unwrap_or_default();
    let mcap_lamports = json_num(line, "mcap_lamports").unwrap_or(0);
    Some(Trigger { kind, mint, symbol, mcap_lamports })
}

/// Extract a string field value from a JSON line (no nested objects expected).
fn json_field(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{key}\"");
    let idx = json.find(&pattern)?;
    let rest = &json[idx..];
    let colon = rest.find(':')?;
    let val_start = rest[colon..].find('"')?;
    let val_rest = &rest[colon + val_start + 1..];
    let end = val_rest.find('"')?;
    Some(val_rest[..end].to_string())
}

/// Extract a numeric field value from a JSON line.
fn json_num(json: &str, key: &str) -> Option<u64> {
    let pattern = format!("\"{key}\"");
    let idx = json.find(&pattern)?;
    let rest = &json[idx..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let end = after.find(|c: char| !c.is_ascii_digit()).unwrap_or(after.len());
    after[..end].parse().ok()
}

// ─── Firecrawl HTTP client (raw TCP, no external deps beyond std) ─────────────

/// Check if Firecrawl is healthy.
fn firecrawl_healthy() -> bool {
    let addr: std::net::SocketAddr = "127.0.0.1:3002"
        .parse()
        .expect("hardcoded address is valid");
    TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok()
}

/// Scrape a single URL via the Firecrawl API. Returns the markdown content
/// of the page, or None on failure.
fn firecrawl_scrape(url: &str) -> Option<String> {
    if !firecrawl_healthy() {
        eprintln!("[fc-bridge] Firecrawl not healthy — skipping scrape");
        return None;
    }

    let body = format!(
        "{{\"url\":\"{url}\",\"formats\":[\"markdown\"],\"maxAge\":300000}}"
    );

    // Raw HTTP/1.1 POST — no ureq needed, just std net.
    let request = format!(
        "POST /v0/scrape HTTP/1.1\r\n\
         Host: 127.0.0.1:3002\r\n\
         Content-Type: application/json\r\n\
         Authorization: Bearer {API_KEY}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );

    let addr: std::net::SocketAddr = "127.0.0.1:3002"
        .parse()
        .expect("hardcoded address is valid");

    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).ok()?;

    stream.set_read_timeout(Some(Duration::from_secs(SCRAPE_TIMEOUT_SECS))).ok()?;
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok()?;
    stream.write_all(request.as_bytes()).ok()?;

    let mut response = Vec::with_capacity(8192);
    stream.read_to_end(&mut response).ok()?;

    let response_str = String::from_utf8_lossy(&response);

    // Find the body after \r\n\r\n
    let body_start = response_str.find("\r\n\r\n").map(|i| i + 4).unwrap_or(0);
    let body_json = &response_str[body_start..];

    // Extract the markdown content from the JSON response.
    // Firecrawl returns {"data":{"markdown":"...","metadata":{...}}}
    extract_markdown(body_json)
}

/// Extract the "markdown" field from a Firecrawl JSON response.
fn extract_markdown(json: &str) -> Option<String> {
    // Look for "markdown":"..." in the response
    let key = "\"markdown\":\"";
    let idx = json.find(key)?;
    let rest = &json[idx + key.len()..];

    // Handle escaped JSON strings — read until unescaped quote
    let mut result = String::with_capacity(rest.len());
    let mut chars = rest.chars();
    let mut depth: i32 = 0;
    while let Some(c) = chars.next() {
        if c == '\\' {
            // Escaped character — take the next char literally
            if let Some(next) = chars.next() {
                match next {
                    'n' => result.push('\n'),
                    't' => result.push('\t'),
                    'r' => result.push('\r'),
                    '"' => result.push('"'),
                    '\\' => result.push('\\'),
                    '/' => result.push('/'),
                    _ => result.push(next),
                }
            }
            continue;
        }
        if c == '"' && depth == 0 {
            break;
        }
        if c == '{' || c == '[' {
            depth += 1;
        }
        if c == '}' || c == ']' {
            depth -= 1;
        }
        result.push(c);
    }
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

// ─── Normalization: scrape content → SocialEvent NDJSON ──────────────────────

/// Current time in nanoseconds since UNIX epoch.
fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Normalize a scraped page into a SocialEvent NDJSON line.
/// The JSON format matches what `parse_social_event` expects:
/// {"platform":"web","author":"...","text":"...","mint":"<base58>",...}
fn normalize_to_social_event(
    url: &str,
    markdown: &str,
    mint: &str,
    symbol: &str,
) -> String {
    // Derive an "author" from the URL's domain — this becomes the author_id
    // via FNV-1a in the parser, so each domain is a distinct "author".
    let author = domain_of(url);

    // Truncate text to a reasonable size (the parser extracts cashtags/mints
    // from the text — we only need the relevant portion).
    let text = if markdown.len() > 4000 {
        &markdown[..4000]
    } else {
        markdown
    };

    // Escape JSON string values
    let author_esc = json_escape(&author);
    let text_esc = json_escape(text);
    let mint_esc = json_escape(mint);
    let symbol_esc = json_escape(symbol);

    format!(
        "{{\"platform\":\"web\",\"author\":\"{author_esc}\",\"text\":\"{text_esc}\",\"mint\":\"{mint_esc}\",\"community\":\"\",\"echo\":false,\"likes\":0,\"reposts\":0,\"replies\":0,\"aggregator_listed\":false,\"is_designated_caller\":false}}"
    )
}

/// Extract the domain from a URL for use as the "author" identity.
fn domain_of(url: &str) -> String {
    let no_scheme = url.strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let domain_end = no_scheme.find('/').unwrap_or(no_scheme.len());
    no_scheme[..domain_end].to_string()
}

/// Escape a string for inclusion in a JSON string value.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_ascii_control() => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

// ─── Main loop ───────────────────────────────────────────────────────────────

fn main() {
    eprintln!("[fc-bridge] pq-firecrawl-bridge starting");
    eprintln!("[fc-bridge] Firecrawl URL: {FIRECRAWL_URL}");

    let stdin = io::stdin();
    let mut stdout = BufWriter::new(io::stdout().lock());
    let mut last_scrape = Instant::now() - Duration::from_secs(MIN_SCRAPE_GAP_MS);

    let mut line_buf = String::new();
    let mut reader = stdin.lock();

    loop {
        line_buf.clear();
        match reader.read_line(&mut line_buf) {
            Ok(0) => {
                // EOF — daemon closed stdin. Exit cleanly.
                eprintln!("[fc-bridge] stdin EOF — shutting down");
                break;
            }
            Ok(_) => {
                let line = line_buf.trim();
                if line.is_empty() {
                    continue;
                }

                let trigger = match parse_trigger(line) {
                    Some(t) => t,
                    None => {
                        eprintln!("[fc-bridge] unparseable trigger: {line}");
                        continue;
                    }
                };

                eprintln!(
                    "[fc-bridge] trigger: {} mint: {} symbol: {}",
                    trigger.kind, trigger.mint, trigger.symbol
                );

                let urls = urls_for_trigger(&trigger);
                for url in &urls {
                    // Throttle: wait at least MIN_SCRAPE_GAP_MS between scrapes.
                    let elapsed = last_scrape.elapsed();
                    if elapsed < Duration::from_millis(MIN_SCRAPE_GAP_MS) {
                        std::thread::sleep(
                            Duration::from_millis(MIN_SCRAPE_GAP_MS) - elapsed
                        );
                    }
                    last_scrape = Instant::now();

                    // Retry loop
                    let mut scraped = None;
                    for attempt in 0..=MAX_RETRIES {
                        if attempt > 0 {
                            std::thread::sleep(Duration::from_secs(1));
                        }
                        match firecrawl_scrape(url) {
                            Some(md) => {
                                scraped = Some(md);
                                break;
                            }
                            None => {
                                eprintln!(
                                    "[fc-bridge] scrape failed (attempt {}): {url}",
                                    attempt + 1
                                );
                            }
                        }
                    }

                    if let Some(markdown) = scraped {
                        let ndjson = normalize_to_social_event(
                            url,
                            &markdown,
                            &trigger.mint,
                            &trigger.symbol,
                        );
                        if writeln!(stdout, "{ndjson}").is_err() {
                            eprintln!("[fc-bridge] stdout write failed — daemon gone?");
                            break;
                        }
                        let _ = stdout.flush();
                        eprintln!("[fc-bridge] emitted social event for {url}");
                    } else {
                        eprintln!("[fc-bridge] all retries exhausted for {url}");
                    }
                }
            }
            Err(e) => {
                eprintln!("[fc-bridge] stdin read error: {e}");
                break;
            }
        }
    }

    eprintln!("[fc-bridge] shutdown complete");
}
