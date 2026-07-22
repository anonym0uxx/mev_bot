//! Live end-to-end probe: NDJSON normalized payloads (stdin) → the REAL
//! `pump_quant_ingest::social_parse::parse_social_event` → decoded `SocialEvent`.
//!
//! This is the `[S]` side: it reads the clock at the capture boundary (the one
//! place that is allowed) to stamp each payload's `observed_at_ns`, then hands the
//! bytes to the deterministic core unchanged. It exists to prove that real tweets
//! captured by `twitterapi_stream.py` decode through the exact same parser the
//! bot's discovery lane uses — cashtags, contract addresses, engagement, and
//! echo-vs-origination all extracted deterministically.
//!
//!   python3 twitterapi_stream.py --query '$WIF OR $BONK' \
//!       | cargo run --quiet --manifest-path probe/Cargo.toml

use std::io::{self, BufRead, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use pump_quant_ingest::social_parse::{parse_social_event, SocialPlatform};

fn now_ns() -> u64 {
    // Capture-boundary clock read: allowed here ([S]), never in the core.
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn platform_name(p: SocialPlatform) -> &'static str {
    match p {
        SocialPlatform::X => "X",
        SocialPlatform::TikTok => "TikTok",
        SocialPlatform::Telegram => "Telegram",
        SocialPlatform::Web => "Web",
    }
}

fn main() {
    let stdin = io::stdin();
    let mut out = io::stdout().lock();

    let (mut total, mut decoded, mut with_mint, mut with_cashtag, mut echoes) = (0u64, 0u64, 0u64, 0u64, 0u64);

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        total += 1;
        match parse_social_event(trimmed.as_bytes(), now_ns()) {
            Some(ev) => {
                decoded += 1;
                if ev.n_mints > 0 {
                    with_mint += 1;
                }
                if ev.n_cashtags > 0 {
                    with_cashtag += 1;
                }
                if ev.is_echo {
                    echoes += 1;
                }
                let _ = writeln!(
                    out,
                    "[{}] author={:016x} eng={:>5} echo={} cashtags={} mints={}{}",
                    platform_name(ev.platform),
                    ev.author_id,
                    ev.engagement,
                    ev.is_echo as u8,
                    ev.n_cashtags,
                    ev.n_mints,
                    if ev.is_targeted() { "  <- targeted" } else { "" },
                );
            }
            None => {
                let _ = writeln!(out, "[skip] undecodable payload");
            }
        }
    }

    let _ = writeln!(
        out,
        "\n== {total} payloads: {decoded} decoded, {with_cashtag} with a cashtag, \
         {with_mint} naming a contract address, {echoes} echoes (reply/retweet) =="
    );
}
