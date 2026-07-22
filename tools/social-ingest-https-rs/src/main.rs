//! `pq-social-capture` — HTTPS `[S]` social capture lanes → normalized NDJSON.
//!
//! Rust twins of the polling Python adapters in `tools/social-ingest/`, one
//! subcommand per feasible platform:
//!
//! ```text
//! pq-social-capture twitterapi [--class firehose|amplifier|list] [--sources f]
//!                              [--query q] [--type Latest|Top] [--pages n]
//!                              [--watch secs] [--replay fixture.json]
//! pq-social-capture tiktok     [--hashtag h] [--sources f] [--watch secs]
//!                              [--replay fixture.json]
//! pq-social-capture firecrawl  [--url u] [--sources f] [--watch secs]
//!                              [--replay fixture.json]
//! pq-social-capture pump       [--mints-file f] [--interval-secs n]
//!                              [--live-list] [--once] [--replay fixture.json]
//! pq-social-capture coingecko  [--trending] [--category id]
//!                              [--contract-watch f] [--interval-secs n]
//!                              [--budget-per-min n] [--once]
//!                              [--replay fixture.json]
//! ```
//!
//! Telegram is intentionally absent: MTProto requires a heavy SDK (grammers),
//! which violates this lane's minimal-dependency rule — the Python
//! `telegram_stream.py` stays PRIMARY there behind the same NDJSON contract
//! (see README).
//!
//! # Constitution discipline (binding)
//! * **§22 determinism boundary.** [`now_ns`] below is the ONE wall-clock read
//!   in the binary, injected into the adapters at the capture edge. `--replay`
//!   never calls it: synthetic monotone timestamps, zero network, byte-stable.
//! * **§29 provenance.** Platform / author / community are carried verbatim so
//!   downstream trust is *earned* per source (D-ledger), never assumed.
//! * **§29.7e sacrificial identity.** Credentials are read ONLY from the same
//!   env vars the Python twins use (`TWITTERAPI_IO_KEY`, `TIKTOK_API_KEY` +
//!   `TIKTOK_API_BASE`, `FIRECRAWL_API_KEY`) — pay-as-you-go research keys,
//!   never an operator's personal account, never hardcoded. Missing keys
//!   refuse to start with the twins' exact stderr messages.
//! * **§67 removable adapter.** One binary, one dependency, NDJSON on stdout,
//!   diagnostics on stderr exclusively. Delete it and the system loses the
//!   Rust HTTPS lanes; the Python twins still work.

use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use pq_social_capture::{coingecko, firecrawl, pump, sentiment, tiktok, twitterapi};

const USAGE: &str = "usage: pq-social-capture \
<twitterapi|tiktok|firecrawl|pump|coingecko|sentiment-enrich> [flags...]\n\
  twitterapi  --class firehose|amplifier|list  --sources f  --query q\n\
              --type Latest|Top  --pages n  --watch secs  --replay fixture\n\
  tiktok      --hashtag h  --sources f  --watch secs  --replay fixture\n\
  firecrawl   --url u      --sources f  --watch secs  --replay fixture\n\
  pump        --mints-file f  --interval-secs n  --live-list  --once\n\
              --replay fixture   (anonymous tier-3 lane, no key; exits 3 on\n\
              AUTH_WALL so the supervisor sees the capability loss)\n\
  coingecko   --trending  --category id  --contract-watch mints-file\n\
              --interval-secs n  --budget-per-min n  --once  --replay fixture\n\
              (aggregator-LEGIBILITY lane, LATE tier; CG_API_KEY optional:\n\
              free Demo key -> x-cg-demo-api-key, absent -> keyless)\n\
  sentiment-enrich  [--replay responses.json] [--passthrough] [--require]\n\
              (stream FILTER, not a lane: NDJSON stdin -> same lines stdout\n\
              with sentiment_bp/sentiment_conf_bp/sentiment_model spliced in\n\
              via a local llama.cpp server; failure = line unchanged, §6.4)\n\
  NDJSON on stdout, diagnostics on stderr. --replay is deterministic and\n\
  touches no network (fixture = saved raw API responses, one JSON value per\n\
  poll). Env: TWITTERAPI_IO_KEY | TIKTOK_API_KEY + TIKTOK_API_BASE |\n\
  FIRECRAWL_API_KEY (same variables as the Python twins) |\n\
  CG_API_KEY (coingecko, optional) |\n\
  LLAMA_SERVER_URL + LLAMA_MODEL_ID (sentiment-enrich only).";

/// Capture-boundary clock read — the ONE place wall time enters the pipeline
/// (§22). Matches the Twitch lane's stamp exactly.
fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (sub, rest) = match args.split_first() {
        Some((s, r)) => (s.as_str(), r),
        None => {
            eprintln!("[pq-social-capture] no subcommand given");
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let code = match sub {
        "twitterapi" => twitterapi::run(rest, now_ns),
        "tiktok" => tiktok::run(rest, now_ns),
        "firecrawl" => firecrawl::run(rest, now_ns),
        "pump" => pump::run(rest, now_ns),
        "coingecko" => coingecko::run(rest, now_ns),
        // The brain seam is a FILTER, not a capture lane: it stamps nothing,
        // so the capture clock is not injected (its only time reads are
        // monotonic latency diagnostics on stderr).
        "sentiment-enrich" => sentiment::run(rest),
        "-h" | "--help" => {
            eprintln!("{USAGE}");
            0
        }
        other => {
            eprintln!("[pq-social-capture] unknown subcommand {other:?}");
            eprintln!("{USAGE}");
            2
        }
    };
    ExitCode::from(code)
}
