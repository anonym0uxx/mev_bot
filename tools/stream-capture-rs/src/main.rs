//! `pq-stream-capture` — live-stream `[S]` capture lanes → raw-preserving
//! NDJSON on stdout, diagnostics on stderr.
//!
//! ```text
//! pq-stream-capture helius-ws        [--accounts-file f] [--programs p1,p2]
//!                                    [--commitment processed|confirmed|finalized]
//! pq-stream-capture pumpportal       [--watch-file f]
//! pq-stream-capture webhook-listener [--bind 127.0.0.1:8787]
//! pq-stream-capture fee-sampler      [--accounts-file f] [--once]
//! pq-stream-capture selfcheck
//! ```
//!
//! §22 determinism boundary: [`now_ms`] below is the ONE wall-clock read in
//! the binary, injected into every lane at the capture edge. §29.7e: all
//! credentials come from env (`HELIUS_API_KEY`, `WEBHOOK_AUTH_SECRET`,
//! `RPC_URLS`), never flags, never hardcoded; `selfcheck` prints WHETHER each
//! is set — values are never printed.

use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use pq_stream_capture::{discord_gateway, fees, helius_ws, pumpportal_ws, webhook_listener, ws};

const USAGE: &str = "usage: pq-stream-capture \
<helius-ws|pumpportal|discord-gateway|webhook-listener|fee-sampler|selfcheck> [flags...]\n\
  helius-ws         Helius Enhanced WS: transactionSubscribe (Developer plan+)\n\
                    + accountSubscribe + slotSubscribe heartbeat.\n\
                    --accounts-file f  --programs p1,p2  --commitment level\n\
                    env: HELIUS_API_KEY (required, exit 3 if missing),\n\
                         HELIUS_WS_URL (optional base override)\n\
  pumpportal        PumpPortal data WS: subscribeNewToken + subscribeMigration\n\
                    (+ subscribeTokenTrade from --watch-file). No auth.\n\
  discord-gateway   PASSIVE read-only Discord Gateway v10 (paid alpha rooms):\n\
                    invisible presence, only IDENTIFY/RESUME/HEARTBEAT sent,\n\
                    zero REST. --token-kind user|bot --guilds --channels\n\
                    --callers --allowlist-file. env: DISCORD_USER_TOKEN or\n\
                    DISCORD_BOT_TOKEN (per token-kind; exit 3 if missing)\n\
  webhook-listener  Helius enhanced-webhook receiver on loopback (put a\n\
                    TLS-terminating reverse proxy in front). --bind addr\n\
                    env: WEBHOOK_AUTH_SECRET (required, exit 3 if missing)\n\
  fee-sampler       getPriorityFeeEstimate + getRecentPrioritizationFees every\n\
                    15s -> fee_calibration_v1 records. --accounts-file f --once\n\
                    env: RPC_URLS (or HELIUS_API_KEY; neither -> exit 3)\n\
  selfcheck         run codec self-tests + report env-var status (set/missing\n\
                    only; values never printed).\n\
  NDJSON on stdout, diagnostics on stderr.";

/// Capture-boundary clock read — the ONE place wall time enters the pipeline
/// (§22). Milliseconds since the Unix epoch.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// `selfcheck`: prove the pure core in-process (a few load-bearing vectors —
/// the full suite lives in `cargo test`) and report arming status per lane.
fn selfcheck() -> u8 {
    let mut failed = 0u32;
    let mut check = |name: &str, ok: bool| {
        eprintln!("[selfcheck] {}: {name}", if ok { "PASS" } else { "FAIL" });
        if !ok {
            failed += 1;
        }
    };
    check(
        "sha1 RFC 3174 vector",
        ws::sha1(b"abc")
            == [
                0xa9, 0x99, 0x3e, 0x36, 0x47, 0x06, 0x81, 0x6a, 0xba, 0x3e, 0x25, 0x71, 0x78, 0x50,
                0xc2, 0x6c, 0x9c, 0xd0, 0xd8, 0x9d,
            ],
    );
    check(
        "RFC 6455 handshake accept vector",
        ws::accept_for_key("dGhlIHNhbXBsZSBub25jZQ==") == "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=",
    );
    check(
        "frame codec roundtrip (masked, 16-bit length)",
        (|| {
            let payload = vec![0x5Au8; 300];
            let wire = ws::encode_frame(true, ws::OP_BINARY, &payload, Some([1, 2, 3, 4])).ok()?;
            let f = ws::decode_frame(&wire).ok()??;
            Some(f.payload == payload && f.consumed == wire.len())
        })() == Some(true),
    );
    check(
        "frame decode: truncation is need-more, never error",
        matches!(ws::decode_frame(&[0x81]), Ok(None)),
    );
    check(
        "percentile integer-exactness",
        fees::percentile_nearest_rank(&[10, 20, 30, 40], 50) == Some(20)
            && fees::percentile_nearest_rank(&[], 50).is_none(),
    );
    for (var, lanes) in [
        ("HELIUS_API_KEY", "helius-ws, fee-sampler fallback"),
        ("HELIUS_WS_URL", "helius-ws (optional override)"),
        ("WEBHOOK_AUTH_SECRET", "webhook-listener"),
        ("RPC_URLS", "fee-sampler"),
        ("PUMPPORTAL_WS_URL", "pumpportal (optional override)"),
        ("DISCORD_USER_TOKEN", "discord-gateway (--token-kind user)"),
        ("DISCORD_BOT_TOKEN", "discord-gateway (--token-kind bot)"),
    ] {
        let status = match std::env::var(var) {
            Ok(v) if !v.trim().is_empty() => "set",
            _ => "MISSING",
        };
        eprintln!("[selfcheck] env {var}: {status} (used by: {lanes})");
    }
    if failed > 0 {
        eprintln!("[selfcheck] {failed} check(s) FAILED");
        1
    } else {
        eprintln!("[selfcheck] all codec checks passed");
        0
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (sub, rest) = match args.split_first() {
        Some((s, r)) => (s.as_str(), r),
        None => {
            eprintln!("[pq-stream-capture] no subcommand given");
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let code = match sub {
        "helius-ws" => helius_ws::run(rest, now_ms),
        "pumpportal" => pumpportal_ws::run(rest, now_ms),
        "discord-gateway" => discord_gateway::run(rest, now_ms),
        "webhook-listener" => webhook_listener::run(rest, now_ms),
        "fee-sampler" => fees::run(rest, now_ms),
        "selfcheck" => selfcheck(),
        "-h" | "--help" => {
            eprintln!("{USAGE}");
            0
        }
        other => {
            eprintln!("[pq-stream-capture] unknown subcommand {other:?}");
            eprintln!("{USAGE}");
            2
        }
    };
    ExitCode::from(code)
}
