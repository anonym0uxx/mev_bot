//! `pq-daemon` — the persistent autonomous runtime (Phase 1 of the autonomous
//! scalping agent architecture).
//!
//! Evolves `paper_session` into a daemon that runs FOREVER:
//! - No `--duration-secs` bound. The event loop never exits on its own.
//! - Self-healing reconnection (inherited from paper_session's reconnect logic).
//! - Periodic `live_status.json` + `brain_analysis.json` writes (every N ticks).
//! - Periodic brain snapshot (every M ticks) — episodic memory survives crashes.
//! - Graceful shutdown via `data/DAEMON_STOP` sentinel file or Ctrl-C: flushes
//!   the queue, writes final status, snapshots the brain, prints the report.
//! - Emergency stop via `data/EMERGENCY_STOP` sentinel file: immediate exit
//!   with a distinct exit code so the watchdog knows it was an emergency.
//!
//! The engine itself is UNCHANGED — same `Engine::new(cfg, RunMode::Paper)`,
//! same `tick()`. The daemon just calls it in an infinite loop.
//!
//! Usage:
//!   pq-daemon [--junction-cap N] [--commitment processed|confirmed]
//!             [--status-every-ticks N] [--brain-snapshot-every-ticks N]
//!
//! Env (same as paper-session):
//!   PQ_CREDS_FILE, HELIUS_API_KEY, LASERSTREAM_ENDPOINT, PUMPPORTAL_WS_URL
//!
//! Shutdown files (checked each loop iteration):
//!   data/DAEMON_STOP       → graceful shutdown (exit 0)
//!   data/EMERGENCY_STOP    → emergency stop   (exit 99)
//!   The watchdog creates/cleans these; the operator can too.

use std::collections::{HashMap, VecDeque};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use std::io::Write; // needed for fc_child stdin.write_all()
use pump_quant_core::config::Creds;
use pump_quant_junction::decode::decode_onchain_confirm_with_curve;
use pump_quant_junction::pumpportal::{
    handle_create_payload, handle_trade_payload, handle_migration_payload,
};
use pump_quant_junction::reserve_delta::{
    derive_market_trade_from_delta, ReserveSnapshot,
};
use pump_quant_junction::laserstream::{
    parse_ndjson_line, classify_pump_instructions, instructions_to_events,
    LaserStreamUpdate, LaserStreamState,
};
use pump_quant_junction::queue::BoundedJunctionQueue;
use pump_quant_junction::tape_export::{TapeExporter, TapeRecord, TapeLane};
use pump_quant_junction::memory_bank::{MemoryBank, MemoryBankConfig};
use pump_quant_junction::trade_journal::{
    TradeRecord, TradeOutcome, TradeSide, TradeLane,
};
use pump_quant_junction::trade_journal::RunMode as JournalRunMode;
use pump_quant_junction::ProvenanceSource;
use pump_quant_junction::event_stream::EventStreamWriter;
use pump_quant_junction::autonomous_bridge::{
    DefenseState, try_reload_config, RefinerSpawner,
    AutoRevertState, write_auto_revert_state, check_auto_revert,
};
use pq_stream_capture::helius_ws;
use pq_stream_capture::json::{self, Value};
use pq_stream_capture::pumpportal_ws;
use pq_stream_capture::ws::{WsConn, WsEvent};
use solana_program::pubkey::Pubkey;
use pump_quant_ingest::social_source::{RawSocialPayload, SocialSource};

// ─── FirecrawlBatchSource ──────────────────────────────────────────────────
// One-shot SocialSource adapter for the Firecrawl bridge. The daemon drains
// the mpsc channel into a Vec<RawSocialPayload>, wraps it in this struct, and
// feeds it to engine.ingest_social(). The source returns the batch on the first
// next_batch() call and empty on subsequent calls.

struct FirecrawlBatchSource {
    batch: Vec<RawSocialPayload>,
    idx: usize,
}

impl SocialSource for FirecrawlBatchSource {
    fn next_batch(&mut self) -> Vec<RawSocialPayload> {
        if self.idx < self.batch.len() {
            let remaining = self.batch[self.idx..].to_vec();
            self.idx = self.batch.len();
            remaining
        } else {
            Vec::new()
        }
    }
}

// ─── Constants ────────────────────────────────────────────────────────────

const PUMP_PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
const PUMPPORTAL_DEFAULT_URL: &str = "wss://pumpportal.fun/api/data";
/// Max concurrent Helius account subscriptions. Helius allows 1000 concurrent
/// subs per connection. Raised from 64 to 256 (Phase 2) — with the GAP #7
/// accountUnsubscribe leak fixed, we have headroom. At 256 subs, each mint
/// holds a slot for ~588s (vs ~147s at 64), giving far more time for the
/// median 37.6s OnchainConfirm to arrive. We leave room below the 1000 cap
/// for the slot subscription and re-subscribe churn during reconnects.
const MAX_ACCOUNT_SUBS: usize = 256;
const MAX_TRADE_SUBS: usize = 512;
const STALE_SECS: u64 = 30;

/// HELIUS_SUB_CAP is the server-side hard limit on concurrent subscriptions
/// per WS connection. The daemon's local MAX_ACCOUNT_SUBS (256) is well below
/// this, but leaked server-side slots (from evictions where the ACK never
/// arrived, so no accountUnsubscribe could be sent) accumulate toward this
/// cap. Once hit, every new accountSubscribe returns -32006 and the death
/// spiral begins. The self-healing system monitors for this and triggers a
/// reconnect to reset the server-side count.
const HELIUS_SUB_CAP: usize = 1000;

/// If leaked server-side subscriptions exceed this fraction of HELIUS_SUB_CAP,
/// proactively trigger a reconnect before the cap is hit. This is the
/// early-warning threshold — we don't wait for the -32006 error to start
/// recovery. 80% gives us 200 slots of headroom (256 local + ~744 leaked
/// before we trigger). The threshold accounts for the slot subscription
/// (1 slot) and re-subscribe churn during reconnects.
const SUB_CAP_RECONNECT_THRESHOLD: f64 = 0.80;

/// OnchainConfirm stagnation detection: if confirms don't advance for this
/// many seconds while the daemon is running and LS is healthy, the Helius WS
/// lane is dead (likely subscription cap death-spiral). This is the
/// last-resort trigger — even if -32006 detection and leak-count heuristics
/// fail, this catches the symptom (frozen confirms) and forces a reconnect.
/// 120s is conservative — the median OnchainConfirm latency is 37.6s, so
/// 120s of silence means 3x the median with zero confirms = definitely dead.
const ONCHAIN_STAGNATION_SECS: u64 = 120;

/// WS read timeout in millis. Tightened from tick_period_ms (250ms) to prevent
/// blocking on degraded connections. Each poll returns within 100ms max,
/// ensuring the tick loop advances even when both WS lanes are silent.
const WS_READ_TIMEOUT_MS: u64 = 100;
/// Wall-clock status heartbeat interval in seconds. live_status.json is
/// refreshed at most this often, decoupling health reporting from event
/// throughput so the watchdog never kills a healthy-but-starved daemon.
const STATUS_HEARTBEAT_SECS: u64 = 15;
/// Bounded sleep on WS reconnect failures (was 5s which blocked the entire
/// event loop). 500ms gives the server time to recover without starving
/// the tick loop.
const WS_RECONNECT_SLEEP_MS: u64 = 500;
/// Maximum reconnect attempts with exponential backoff before falling back
/// to graceful degradation. The backoff ladder is: 500ms → 1s → 2s → 4s → 8s
/// (capped). After MAX_RECONNECT_ATTEMPTS failures, the daemon keeps the old
/// (broken) connection and continues with PumpPortal/LaserStream — it does
/// NOT crash. The stale-check watchdog will retry on the next tick.
const MAX_RECONNECT_ATTEMPTS: u32 = 5;
/// Backoff cap in milliseconds. The exponential ladder doubles from
/// WS_RECONNECT_SLEEP_MS (500ms) up to this cap. 10s is long enough to let
/// a rate-limited server recover but short enough to not starve the tick loop.
const RECONNECT_BACKOFF_CAP_MS: u64 = 10_000;
/// Minimum seconds between LaserStream respawn attempts. Without this, a
/// binary that exits immediately (e.g. wrong subcommand, missing creds)
/// triggers a tight-loop respawn on every `Disconnected` poll, burning CPU
/// and spamming logs. 15s is long enough to break the cycle but short
/// enough to recover when the issue is transient (network blip).
const LS_RESPAWN_COOLDOWN_SECS: u64 = 15;
/// Maximum LaserStream respawn attempts before giving up and falling back
/// to Helius WS permanently. Prevents infinite respawn loops against a
/// fundamentally broken binary (e.g. pq-stream-capture.exe spawned without
/// a subcommand, or pq-laserstream-grpc.exe with a bad endpoint).
const LS_MAX_RESPAWN_ATTEMPTS: u32 = 5;

/// Exit code on emergency stop.
const EXIT_EMERGENCY: u8 = 99;
/// Path (relative to CWD) for the graceful-shutdown sentinel file.
const DAEMON_STOP_FILE: &str = "data/DAEMON_STOP";
/// Path (relative to CWD) for the emergency-stop sentinel file.
const EMERGENCY_STOP_FILE: &str = "data/EMERGENCY_STOP";
/// Path for the live-status JSON.
const STATUS_PATH: &str = "data/live_status.json";
const TAPE_PATH: &str = "data/tape.jsonl";
/// Path for the cumulative PnL ledger (cross-session, seeded from tape on startup).
/// This is the trustworthy PnL report file: it combines the tape's cumulative
/// realized PnL (all prior daemon sessions) with the current session's realized
/// PnL from `live_status.json`. The cron reads this instead of live_status.json
/// to avoid the restart-amnesia problem (live_status resets to 0 on every
/// Engine::new(), but tape.jsonl is append-forever).
const CUMULATIVE_PNL_PATH: &str = "data/cumulative_pnl.json";
/// Path for the session history ledger (append-only, one line per daemon run).
/// This is the A/B testing ledger: each daemon session's final stats tagged
/// with config fingerprint + strategy label for cross-strategy comparison.
const SESSION_HISTORY_PATH: &str = "data/session_history.jsonl";
/// Path for the raw event stream (for deterministic replay).
const EVENT_STREAM_PATH: &str = "data/event_stream.jsonl";

/// Creator ledger persistence (G3 fix): binary snapshot for cross-session
/// creator track-record survival. The ledger accumulates launch/migration/rug
/// observations across daemon restarts — without persistence, every restart
/// wipes the creator track record to "Unknown", starving the classifier.
const LEDGER_PATH: &str = "data/creator_ledger.bin";

// ─── Args ──────────────────────────────────────────────────────────────────

struct DaemonArgs {
    junction_cap: usize,
    commitment: String,
    status_every_ticks: u64,
    brain_snapshot_every_ticks: u64,
    tape_every_ticks: u64,
    /// How many ticks between refiner triggers. 0 = disabled.
    /// The refiner is spawned as a child process that reads the tape and
    /// writes CONFIG_PROMOTION.json; the daemon hot-reloads it next tick.
    refiner_every_ticks: u64,
    /// Human-readable label for the strategy/config set being tested.
    /// Used in cumulative_pnl.json and session_history.jsonl for A/B testing
    /// attribution. If absent, "unlabeled" is used.
    strategy_label: String,
}

fn parse_args() -> Result<DaemonArgs, u8> {
    let args: Vec<String> = std::env::args().collect();
    let mut a = DaemonArgs {
        junction_cap: 4096,
        commitment: String::from("processed"),
        status_every_ticks: 500,
        brain_snapshot_every_ticks: 5000,
        tape_every_ticks: 1000,
        refiner_every_ticks: 72000, // default: every 2 hours (72000 ticks @ 100ms/tick)
        strategy_label: String::from("unlabeled"),
    };
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--junction-cap" if i + 1 < args.len() => {
                a.junction_cap = args[i + 1].parse().unwrap_or(4096);
                i += 2;
            }
            "--commitment" if i + 1 < args.len() => {
                a.commitment = args[i + 1].clone();
                if !matches!(a.commitment.as_str(), "processed" | "confirmed" | "finalized") {
                    eprintln!("bad --commitment {:?}", a.commitment);
                    return Err(2);
                }
                i += 2;
            }
            "--status-every-ticks" if i + 1 < args.len() => {
                a.status_every_ticks = args[i + 1].parse().unwrap_or(500);
                i += 2;
            }
            "--brain-snapshot-every-ticks" if i + 1 < args.len() => {
                a.brain_snapshot_every_ticks = args[i + 1].parse().unwrap_or(5000);
                i += 2;
            }
            "--tape-every-ticks" if i + 1 < args.len() => {
                a.tape_every_ticks = args[i + 1].parse().unwrap_or(1000);
                i += 2;
            }
            "--refiner-every-ticks" if i + 1 < args.len() => {
                a.refiner_every_ticks = args[i + 1].parse().unwrap_or(72000);
                i += 2;
            }
            "--strategy-label" if i + 1 < args.len() => {
                a.strategy_label = String::from(&args[i + 1]);
                i += 2;
            }
            _ => { i += 1; }
        }
    }
    Ok(a)
}

// ─── Shutdown detection ──────────────────────────────────────────────────

/// Returns true if the emergency-stop sentinel exists.
fn emergency_stop_requested() -> bool {
    std::path::Path::new(EMERGENCY_STOP_FILE).exists()
}

/// Returns true if the graceful-shutdown sentinel exists.
fn daemon_stop_requested() -> bool {
    std::path::Path::new(DAEMON_STOP_FILE).exists()
}

/// Clean up the stop sentinel after consuming it (so a restart doesn't
/// immediately stop again).
fn clean_stop_sentinel() {
    let _ = std::fs::remove_file(DAEMON_STOP_FILE);
}

// ─── Cumulative PnL ledger (cross-session, strategy-aware) ────────────────
// The restart-amnesia fix: live_status.json resets to 0 on every Engine::new(),
// but tape.jsonl is append-forever. We bridge the two by:
//   1. On startup: read tape.jsonl, sum all realized_pnl from trade_full records
//      → prior_realized (the cumulative PnL from ALL prior daemon sessions).
//   2. On every status write: write cumulative_pnl.json = prior_realized +
//      current session_realized. This is the trustworthy number the cron reads.
//   3. On shutdown: append one line to session_history.jsonl with the session's
//      final stats, tagged with the config fingerprint + strategy label.
//
// The config fingerprint is a deterministic FNV-1a hash of cfg.dump_to_text().
// It identifies which strategy/config set produced each session's trades,
// enabling A/B comparison across config versions.

/// Read tape.jsonl and sum all `realized_pnl` from `trade_full` records.
/// This gives the cumulative realized PnL from ALL prior daemon sessions
/// (the tape is append-forever, never reset).
///
/// Returns (cumulative_pnl, trade_count). On read error or missing file,
/// returns (0, 0) — fail-safe: a missing tape means no prior trades.
fn seed_cumulative_from_tape(tape_path: &str) -> (i64, u64) {
    let bytes = match std::fs::read_to_string(tape_path) {
        Ok(s) => s,
        Err(_) => return (0, 0),
    };
    let mut total_pnl: i64 = 0;
    let mut trade_count: u64 = 0;
    for line in bytes.lines() {
        if !line.contains("\"kind\":\"trade_full\"") {
            continue;
        }
        // Extract realized_pnl from the JSON line. The field is:
        //   "realized_pnl":{pnl}
        // We use a simple substring scan to avoid a full JSON parser.
        if let Some(pnl) = extract_json_int(line, "\"realized_pnl\":") {
            total_pnl = total_pnl.saturating_add(pnl);
            trade_count += 1;
        }
    }
    (total_pnl, trade_count)
}

/// Extract an integer value following a JSON key pattern in a single line.
/// e.g. extract_json_int(line, "\"realized_pnl\":") → Some(12345) or None.
/// Handles negative values. This is NOT a general JSON parser — it's a
/// purpose-built scanner for the fixed tape format (§22: integer values,
/// no floats, no nested objects in the trade_full record).
fn extract_json_int(line: &str, key: &str) -> Option<i64> {
    let idx = line.find(key)?;
    let rest = &line[idx + key.len()..];
    // Skip whitespace
    let rest = rest.trim_start();
    // Parse optional minus sign then digits
    let mut chars = rest.chars();
    let negative = match chars.next() {
        Some('-') => true,
        Some(c) if c.is_ascii_digit() => {
            // Put it back — we need to parse from here
            let num_str: String = rest.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            return num_str.parse().ok();
        }
        _ => return None,
    };
    let num_str: String = chars
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if num_str.is_empty() {
        return None;
    }
    num_str.parse::<i64>().ok().map(|v| if negative { -v } else { v })
}

/// Compute a deterministic config fingerprint (FNV-1a hash of dump_to_text).
/// This identifies which strategy/config set is running, for A/B testing.
fn config_fingerprint(cfg_text: &str) -> u64 {
    pump_quant_brain::hash::fnv1a_64(cfg_text.as_bytes())
}

/// Escape a string for safe embedding inside a JSON string value.
/// Handles backslash, double-quote, and control chars (0x00-0x1F).
/// This prevents malformed JSON if the strategy_label contains special chars.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// Write cumulative_pnl.json — the trustworthy cross-session PnL report.
/// Schema: {\"schema\":\"cumulative_pnl/1\",\"config_fingerprint\":\"0x...\",\"strategy_label\":\"...\",\"session_realized_lamports\":N,\"prior_tape_realized_lamports\":N,\"cumulative_realized_lamports\":N,\"prior_tape_trade_count\":N,\"session_admitted\":N,\"session_tick\":N,\"info_time_tick\":N}
fn write_cumulative_pnl(
    path: &str,
    config_fp: u64,
    strategy_label: &str,
    session_realized: i128,
    prior_tape_realized: i64,
    prior_tape_trades: u64,
    session_admitted: u64,
    engine_tick: u64,
) -> std::io::Result<()> {
    let cumulative = prior_tape_realized.saturating_add(session_realized as i64);
    let json = format!(
        concat!(
            "{{\"schema\":\"cumulative_pnl/1\",",
            "\"config_fingerprint\":\"{:#018x}\",",
            "\"strategy_label\":\"{}\",",
            "\"session_realized_lamports\":{},",
            "\"prior_tape_realized_lamports\":{},",
            "\"cumulative_realized_lamports\":{},",
            "\"prior_tape_trade_count\":{},",
            "\"session_admitted\":{},",
            "\"session_tick\":{}}}"
        ),
        config_fp,
        json_escape(strategy_label),
        session_realized,
        prior_tape_realized,
        cumulative,
        prior_tape_trades,
        session_admitted,
        engine_tick,
    );
    let p = std::path::Path::new(path);
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut f = std::fs::File::create(p)?;
    f.write_all(json.as_bytes())?;
    f.write_all(b"\n")?;
    f.flush()
}

/// Append one line to session_history.jsonl — the A/B test ledger.
/// Each daemon run gets tagged with config fingerprint, strategy label, and stats.
/// Called both periodically (final=false, crash resilience) and on shutdown (final=true).
/// Deduplication: the analysis layer keeps the last entry per (config_fingerprint + uptime_secs)
/// pair, or simply the entry with final=true. If the daemon crashes, the last final=false
/// entry is the best available record for that session.
///
/// GAP E fix: includes a `session_id` field — a unique per-daemon-restart identifier
/// (process PID + start timestamp) so A/B comparison can unambiguously attribute
/// PnL to specific sessions, even when consecutive sessions share the same config.
fn append_session_history(
    path: &str,
    config_fp: u64,
    strategy_label: &str,
    session_realized: i128,
    prior_tape_realized: i64,
    prior_tape_trades: u64,
    session_admitted: u64,
    session_tick: u64,
    uptime_secs: u64,
    tape_trades_this_session: u64,
    tape_trades_total_at_shutdown: u64,
    is_final: bool,
    session_id: u64,
) -> std::io::Result<()> {
    // Use a wall-clock timestamp here — this is an append-only audit log, NOT
    // a deterministic replay artifact. The tape/live_status are deterministic
    // (info-time only); the session history is operational metadata.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let cumulative = prior_tape_realized.saturating_add(session_realized as i64);
    let json = format!(
        concat!(
            "{{\"schema\":\"session_history/2\",",
            "\"ts_unix\":{},",
            "\"session_id\":{},",
            "\"config_fingerprint\":\"{:#018x}\",",
            "\"strategy_label\":\"{}\",",
            "\"session_realized_lamports\":{},",
            "\"prior_tape_realized_lamports\":{},",
            "\"cumulative_realized_lamports\":{},",
            "\"prior_tape_trade_count\":{},",
            "\"session_admitted\":{},",
            "\"session_tick\":{},",
            "\"uptime_secs\":{},",
            "\"tape_trades_this_session\":{},",
            "\"tape_trades_total\":{},",
            "\"final\":{}}}\n"
        ),
        ts,
        session_id,
        config_fp,
        json_escape(strategy_label),
        session_realized,
        prior_tape_realized,
        cumulative,
        prior_tape_trades,
        session_admitted,
        session_tick,
        uptime_secs,
        tape_trades_this_session,
        tape_trades_total_at_shutdown,
        is_final,
    );
    let p = std::path::Path::new(path);
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)?;
    f.write_all(json.as_bytes())?;
    f.flush()
}

// ─── PDA derivation ────────────────────────────────────────────────────────

fn bonding_curve_pda(mint: &[u8; 32]) -> Pubkey {
    let program_id = PUMP_PROGRAM_ID
        .parse::<Pubkey>()
        .expect("pump.fun program id is a valid pubkey");
    let all_seeds: [&[u8]; 2] = [b"bonding-curve", mint];
    let (pda, _bump) = Pubkey::find_program_address(&all_seeds, &program_id);
    pda
}

fn hex_short(b: &[u8; 32]) -> String {
    let mut s = String::with_capacity(8);
    for &n in &b[..4] { s.push_str(&format!("{n:02x}")); }
    s
}

/// Kill a child process AND its entire process tree. This is critical for
/// preventing orphaned LaserStream/Firecrawl grandchildren on Windows.
///
/// On Windows, `child.kill()` calls `TerminateProcess` which kills ONLY the
/// immediate process — NOT its children. When the daemon spawns LS via
/// `wsl.exe`, the actual gRPC binary runs as a grandchild inside WSL2.
/// `TerminateProcess` on wsl.exe leaves the Linux gRPC process orphaned,
/// still connected to Helius, burning credits. This function uses
/// `taskkill /T /F /PID` to recursively kill the entire process tree
/// before calling the Rust kill as a fallback.
///
/// GAP #13: This function is called on EVERY child kill path (graceful
/// shutdown, emergency stop, defense-in-depth halt, and LS respawn).
fn kill_process_tree(child: &mut std::process::Child) {
    let pid = child.id();
    #[cfg(windows)]
    {
        // taskkill /T = kill tree, /F = force. This kills the PID and all
        // processes spawned by it recursively — critical for wsl.exe → bash →
        // pq-laserstream-grpc process chains.
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    // Fallback: Rust's own kill (covers non-Windows or if taskkill failed)
    let _ = child.kill();
    let _ = child.wait();
}

/// Kill a LaserStream or Firecrawl child by PID when we only have the PID
/// (e.g. orphans from a previous session detected via health monitoring).
/// Used by the LS orphan reaper (GAP #14).
#[cfg(windows)]
fn kill_pid_tree(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn extract_account_data(result: &Value) -> (Option<String>, Option<u64>) {
    let data_b64 = result
        .get("value")
        .and_then(|v| v.get("data"))
        .and_then(|d| d.as_array())
        .and_then(|arr| arr.first())
        .and_then(|f| f.as_str())
        .map(|s| s.to_string());
    let slot = result
        .get("context")
        .and_then(|c| c.get("slot"))
        .and_then(Value::as_u64);
    (data_b64, slot)
}

fn extract_server_sub_id(params: &Value) -> Option<u64> {
    params.get("subscription").and_then(Value::as_u64)
}

// ─── Session stats (same as paper_session) ──────────────────────────────

struct SessionStats {
    pp_trades_received: u64,
    pp_trades_enqueued: u64,
    pp_creates_received: u64,
    pp_creates_parsed: u64,
    pp_migrations_received: u64,
    pp_migrations_parsed: u64,
    pp_trade_subs_sent: u64,
    helius_account_notifications: u64,
    helius_onchain_confirms_decoded: u64,
    helius_slot_notifications: u64,
    account_subs_active: usize,
    account_subs_total_attempted: usize,
    account_subs_evicted: usize,
    /// Count of subscriptions evicted before their ACK arrived — these are
    /// the leaked server-side slots that accumulate toward HELIUS_SUB_CAP.
    /// Tracked for proactive reconnect trigger and health reporting.
    subs_leaked_no_ack: u64,
    /// Total -32006 "Too many subscriptions" errors received from Helius.
    /// If >0, the death spiral has begun. Tracked for health reporting
    /// and self-healing diagnostics.
    sub_cap_errors: u64,
    /// Timestamp (tick counter) of the last OnchainConfirm decoded. Used
    /// for stagnation detection — if this doesn't advance for
    /// ONCHAIN_STAGNATION_SECS while LS is healthy, the Helius lane is dead.
    last_confirm_tick: u64,
    delta_trades_derived: u64,
    delta_no_trade: u64,
    pdas_derived: usize,
    pda_venue_matches: usize,
    pda_venue_present: usize,
    junction_events_drained: u64,
    junction_overflow_dropped: u64,
    dwell_max_ms: u64,
    dwell_mean_ms: u64,
    dwell_p99_ms: u64,
    pp_reconnects: u64,
    helius_reconnects: u64,
    ws_errors: u64,
    ls_transactions_received: u64,
    ls_instructions_classified: u64,
    ls_events_emitted: u64,
    ls_slots_received: u64,
    ls_spawned: bool,
    ls_reconnects: u64,
    fc_spawned: bool,
    fc_triggers_emitted: u64,
    fc_events_ingested: u64,
    stubbed_or_assumed: Vec<String>,
}

impl SessionStats {
    fn new() -> Self {
        Self {
            pp_trades_received: 0, pp_trades_enqueued: 0,
            pp_creates_received: 0, pp_creates_parsed: 0,
            pp_migrations_received: 0, pp_migrations_parsed: 0,
            pp_trade_subs_sent: 0,
            helius_account_notifications: 0, helius_onchain_confirms_decoded: 0,
            helius_slot_notifications: 0,
            account_subs_active: 0, account_subs_total_attempted: 0, account_subs_evicted: 0,
            subs_leaked_no_ack: 0, sub_cap_errors: 0, last_confirm_tick: 0,
            delta_trades_derived: 0, delta_no_trade: 0,
            pdas_derived: 0, pda_venue_matches: 0, pda_venue_present: 0,
            junction_events_drained: 0, junction_overflow_dropped: 0,
            dwell_max_ms: 0, dwell_mean_ms: 0, dwell_p99_ms: 0,
            pp_reconnects: 0, helius_reconnects: 0,
            ws_errors: 0,
            ls_transactions_received: 0, ls_instructions_classified: 0,
            ls_events_emitted: 0, ls_slots_received: 0,
            ls_spawned: false, ls_reconnects: 0,
            fc_spawned: false, fc_triggers_emitted: 0, fc_events_ingested: 0,
            stubbed_or_assumed: vec![
                "Config: dev_portable (no live config file provided)".to_string(),
            ],
        }
    }
}

// ─── Sub trackers (same as paper_session) ────────────────────────────────
//
// TRADE-AWARE EVICTION (GAP #11 / Phase 3):
// The original FIFO eviction blindly removed the oldest subscription,
// regardless of whether the mint had active MarketTrade activity. This meant
// a mint that was receiving heavy buying pressure could be evicted just
// because it was subscribed early, losing its OnchainConfirm slot precisely
// when it mattered most.
//
// The enhanced SubTracker tracks a `has_trades` flag per subscription. The
// eviction policy is two-tier:
//   1. First, evict the oldest subscription with NO trade activity (dormant).
//   2. If all active subs have trades, fall back to FIFO on the oldest.
//
// This preserves subscription slots for mints with demonstrated market
// interest — the exact population we want to confirm and admit.

struct SubTracker {
    req_to_mint: HashMap<u64, [u8; 32]>,
    req_to_server_sub: HashMap<u64, u64>,
    server_sub_to_mint: HashMap<u64, [u8; 32]>,
    // (req_id, mint, has_trades) — has_trades is set true when a MarketTrade
    // event is observed for this mint while it holds an active subscription.
    subscription_order: Vec<(u64, [u8; 32], bool)>,
}

impl SubTracker {
    fn new() -> Self {
        Self {
            req_to_mint: HashMap::new(),
            req_to_server_sub: HashMap::new(),
            server_sub_to_mint: HashMap::new(),
            subscription_order: Vec::new(),
        }
    }

    fn record_request(&mut self, req_id: u64, mint: [u8; 32]) {
        self.req_to_mint.insert(req_id, mint);
        self.subscription_order.push((req_id, mint, false));
    }

    fn record_ack(&mut self, req_id: u64, server_sub_id: u64) {
        if let Some(mint) = self.req_to_mint.get(&req_id).copied() {
            self.req_to_server_sub.insert(req_id, server_sub_id);
            self.server_sub_to_mint.insert(server_sub_id, mint);
        }
    }

    /// Mark that a MarketTrade event was observed for `mint`. This promotes
    /// the subscription's eviction priority — dormant mints are evicted
    /// before trade-active mints. Returns true if the mint was found.
    fn mark_trade_seen(&mut self, mint: &[u8; 32]) -> bool {
        for entry in self.subscription_order.iter_mut() {
            if &entry.1 == mint {
                entry.2 = true;
                return true;
            }
        }
        false
    }

    fn mint_for_server_sub(&self, server_sub_id: u64) -> Option<[u8; 32]> {
        self.server_sub_to_mint.get(&server_sub_id).copied()
    }

    /// Evict using trade-aware priority: first try the oldest subscription
    /// with `has_trades == false` (dormant). If ALL active subscriptions have
    /// trades, fall back to pure FIFO (evict the oldest regardless of trade
    /// state). Returns (req_id, mint, server_sub_id) where server_sub_id is
    /// Some if Helius has ACKed the subscription (needed to send
    /// accountUnsubscribe), or None if the ACK hasn't arrived yet.
    fn evict_oldest(&mut self) -> Option<(u64, [u8; 32], Option<u64>)> {
        if self.subscription_order.is_empty() {
            return None;
        }
        // Tier 1: find the index of the oldest subscription with no trades.
        let dormant_idx = self.subscription_order
            .iter()
            .position(|(_, _, has_trades)| !*has_trades);

        let idx = dormant_idx.unwrap_or(0);
        // Vec::remove returns the value directly (not Option). The idx is
        // always valid because subscription_order is non-empty (guarded above).
        let item = self.subscription_order.remove(idx);
        let (req_id, mint, _has_trades) = item;
        self.req_to_mint.remove(&req_id);
        let server_sub = self.req_to_server_sub.remove(&req_id);
        if let Some(ssid) = server_sub {
            self.server_sub_to_mint.remove(&ssid);
        }
        Some((req_id, mint, server_sub))
    }

    /// Clear all server-sub mappings (used on reconnect — the server assigns
    /// new sub IDs on a fresh connection, so stale mappings must go).
    /// Also resets has_trades flags — after reconnect, we haven't seen any
    /// trades on the new connection yet.
    fn clear_server_subs(&mut self) {
        self.req_to_server_sub.clear();
        self.server_sub_to_mint.clear();
        // Reset trade flags on reconnect — the new connection hasn't observed
        // any trades yet. This prevents stale trade-flags from permanently
        // protecting subscriptions that may no longer be active.
        for entry in self.subscription_order.iter_mut() {
            entry.2 = false;
        }
    }

    fn active_mints(&self) -> Vec<(u64, [u8; 32])> {
        self.subscription_order
            .iter()
            .map(|(req_id, mint, _)| (*req_id, *mint))
            .collect()
    }

    fn len(&self) -> usize {
        self.subscription_order.len()
    }

    /// Count subscriptions that have been record_request'd but NOT yet
    /// record_ack'd. These are "pending acks" — subscriptions where we sent
    /// accountSubscribe but haven't received the server_sub_id back. If we
    /// evict one of these, we CANNOT send accountUnsubscribe (no
    /// server_sub_id), so the server-side slot leaks until TCP timeout.
    /// This count is the upper bound on potential leaks from eviction.
    fn pending_ack_count(&self) -> usize {
        self.subscription_order
            .iter()
            .filter(|(req_id, _, _)| !self.req_to_server_sub.contains_key(req_id))
            .count()
    }

    /// Count subscriptions that HAVE been ACKed (server_sub_id known).
    /// These can be cleanly unsubscribed on eviction — no leak.
    fn acked_count(&self) -> usize {
        self.req_to_server_sub.len()
    }

    /// Total "server-visible" subscriptions: ACKed + pending. The pending
    /// ones may or may not be live on the server yet (the ACK itself
    /// confirms the server created the subscription), but Helius allocates
    /// the slot at request time, not ACK time. So this is the best
    /// lower-bound estimate of server-side slot usage.
    fn server_visible_count(&self) -> usize {
        self.subscription_order.len()
    }
}

struct TradeSubTracker {
    mint_keys: VecDeque<String>,
    mint_set: std::collections::HashSet<String>,
}

impl TradeSubTracker {
    fn new() -> Self {
        Self {
            mint_keys: VecDeque::new(),
            mint_set: std::collections::HashSet::new(),
        }
    }

    fn add(&mut self, mint_b58: &str) -> bool {
        if self.mint_set.contains(mint_b58) {
            return false;
        }
        if self.mint_keys.len() >= MAX_TRADE_SUBS {
            if let Some(old) = self.mint_keys.pop_front() {
                self.mint_set.remove(&old);
            }
        }
        let s = mint_b58.to_string();
        self.mint_set.insert(s.clone());
        self.mint_keys.push_back(s);
        true
    }

    fn keys(&self) -> Vec<String> {
        self.mint_keys.iter().cloned().collect()
    }
}

// ─── Main ──────────────────────────────────────────────────────────────────

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(code) => return ExitCode::from(code),
    };

    // ─── Credential resolution ─ fail-closed ─────────────────────────────
    let creds = match Creds::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FAIL-CLOSED: {e}");
            eprintln!("Set PQ_CREDS_FILE or HELIUS_API_KEY + LASERSTREAM_ENDPOINT in env.");
            return ExitCode::from(3);
        }
    };
    let helius_url = creds.ws_url().expose().to_string();
    let pp_url = std::env::var("PUMPPORTAL_WS_URL")
        .unwrap_or_else(|_| PUMPPORTAL_DEFAULT_URL.to_string());

    // Load CHAMPION_CONFIG.txt over compiled defaults — the config file is the
    // operator's source of truth for all tunable knobs (TP ladder, brain
    // persistence, reflection, etc). Compiled dev_portable() provides safe
    // fail-closed defaults; the file overrides them. This is NOT the simplest
    // approach (which would be to hardcode brain_persist_enable=true in the
    // defaults) — it is the correct one: the operator's config file governs.
    let mut cfg = Config::dev_portable().with_mcap_band(); // Amendment A-14: $9k-$20k band
    {
        const CHAMPION_CONFIG_FILE: &str = "data/CHAMPION_CONFIG.txt";
        if std::path::Path::new(CHAMPION_CONFIG_FILE).exists() {
            match std::fs::read_to_string(CHAMPION_CONFIG_FILE) {
                Ok(text) => match Config::from_str_over_default(&text) {
                    Ok(loaded) => {
                        cfg = loaded.with_mcap_band();
                        eprintln!(
                            "[pq-daemon] CHAMPION_CONFIG.txt loaded — {} keys applied over dev_portable defaults",
                            text.lines().filter(|l| {
                                let t = l.split('#').next().unwrap_or("").trim();
                                !t.is_empty() && t.contains('=')
                            }).count()
                        );
                    }
                    Err(e) => {
                        eprintln!(
                            "[pq-daemon] CHAMPION_CONFIG.txt parse error — using compiled defaults ({e})"
                        );
                    }
                },
                Err(e) => {
                    eprintln!(
                        "[pq-daemon] CHAMPION_CONFIG.txt unreadable — using compiled defaults ({e})"
                    );
                }
            }
        } else {
            eprintln!("[pq-daemon] no CHAMPION_CONFIG.txt — using compiled dev_portable defaults");
        }
    }
    let tick_period_ms = cfg.paper_tick_period_ms;

    eprintln!("[pq-daemon] === AUTONOMOUS DAEMON STARTING ===");
    eprintln!("[pq-daemon] PumpPortal: {pp_url}");
    eprintln!("[pq-daemon] Helius WS:  {}", creds.ws_url_redacted());
    eprintln!("[pq-daemon] cap={} commitment={} tick={}ms", args.junction_cap, args.commitment, tick_period_ms);
    eprintln!("[pq-daemon] status_every={} ticks  brain_snapshot_every={} ticks", args.status_every_ticks, args.brain_snapshot_every_ticks);
    eprintln!("[pq-daemon] shutdown: {DAEMON_STOP_FILE}  emergency: {EMERGENCY_STOP_FILE}");
    eprintln!("[pq-daemon] NO duration bound — runs forever until shutdown signal");

    // Check for pre-existing emergency stop BEFORE starting feeds
    if emergency_stop_requested() {
        eprintln!("[pq-daemon] EMERGENCY_STOP file present at startup — refusing to start.");
        return ExitCode::from(EXIT_EMERGENCY);
    }

    // ─── Connect PumpPortal ──────────────────────────────────────────────
    let mut pp_conn = match WsConn::connect(&pp_url) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FAIL-CLOSED: PumpPortal WS connect failed: {e}");
            return ExitCode::from(4);
        }
    };
    // Tightened read timeout: 100ms per poll ensures the event loop advances
    // even when both WS lanes are silent. Previously used tick_period_ms (250ms),
    // which combined with two sequential polls = 500ms per iteration.
    let _ = pp_conn.set_read_timeout(Duration::from_millis(WS_READ_TIMEOUT_MS));
    for sub in pumpportal_ws::subscription_batch(&[]) {
        if let Err(e) = pp_conn.send_text(&sub) {
            eprintln!("[pq-daemon] PumpPortal subscribe error: {e}");
            return ExitCode::from(4);
        }
    }

    // ─── Connect Helius ──────────────────────────────────────────────────
    let mut helius_conn = match WsConn::connect(&helius_url) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FAIL-CLOSED: Helius WS connect failed: {e}");
            return ExitCode::from(4);
        }
    };
    let _ = helius_conn.set_read_timeout(Duration::from_millis(WS_READ_TIMEOUT_MS));
    if let Err(e) = helius_conn.send_text(&helius_ws::slot_subscribe_request()) {
        eprintln!("[pq-daemon] Helius slotSubscribe error: {e}");
        return ExitCode::from(4);
    }
    // Track connection establishment time for the stale-check guard (GAP #10).
    // Initialized here on initial connect; reset on every reconnect.
    // (helius_conn_established_at is declared later, before the event loop.)

    // ─── Engine + queue ──────────────────────────────────────────────────
    let queue = BoundedJunctionQueue::with_capacity(args.junction_cap);
    let mut dwell_samples: Vec<u64> = Vec::new();
    let mut engine = Engine::new(cfg, RunMode::Paper);

    // G3 fix: restore the creator ledger from disk (cross-session persistence).
    // Without this, every daemon restart wipes the creator track record to
    // "Unknown", starving the classifier (GAP-B) and discarding accumulated
    // rug/migration observations.
    if std::path::Path::new(LEDGER_PATH).exists() {
        match std::fs::read(LEDGER_PATH) {
            Ok(bytes) => {
                if engine.restore_creator_ledger(&bytes) {
                    let len = engine.measured().creator_ledger_len();
                    eprintln!("[pq-daemon] creator ledger restored: {len} entries from {LEDGER_PATH}");
                }
            }
            Err(e) => eprintln!("[pq-daemon] creator ledger read error: {e} — starting fresh"),
        }
    } else {
        eprintln!("[pq-daemon] no persisted creator ledger — starting fresh");
    }

    // §27 amendment (G5): load the tracked-wallet candidate list.
    // The boost is disabled by default; only load if the operator has
    // enabled it and provided a path.
    if cfg.tracked_wallet_boost_enable && !cfg.tracked_wallet_path.as_str().is_empty() {
        use pump_quant_junction::wallet_loader::load_tracked_wallets_from_json;
        match load_tracked_wallets_from_json(cfg.tracked_wallet_path.as_str()) {
            Ok((matcher, _stats)) => {
                let n = engine.set_tracked_wallet_matcher(matcher);
                if n > 0 {
                    eprintln!("[pq-daemon] tracked-wallet boost ARMED: {n} wallets loaded");
                } else {
                    eprintln!("[pq-daemon] tracked-wallet boost DISABLED: 0 wallets loaded");
                }
            }
            Err(e) => eprintln!("[pq-daemon] tracked-wallet load FAILED: {e:?}"),
        }
    } else {
        eprintln!("[pq-daemon] tracked-wallet boost not configured — skipping load");
    }

    // LAW B5: arm the episodic brain store for persistence. Without this,
    // snapshot_brain() is a silent no-op (store = None → Ok(())). The daemon
    // MUST attach a File blob store so episodic memory survives restarts and
    // the brain_analysis.json + brain snapshot are actually written to disk.
    if cfg.brain_enable && cfg.brain_persist_enable && !cfg.brain_path.is_empty() {
        match engine.attach_brain_store(pump_quant_app::brain::AppBlobStore::File(
            pump_quant_brain::persist::FileBlobStore,
        )) {
            Ok(report) => eprintln!(
                "[pq-daemon] brain store restored: {} episodes ({} snapshot, {} journal){}",
                report.admitted(),
                report.snapshot_admitted,
                report.journal_admitted,
                if report.saw_damage() { " [DAMAGE SEEN]" } else { "" }
            ),
            Err(e) => eprintln!(
                "[pq-daemon] brain persistence disarmed: cannot open {} ({e})",
                cfg.brain_path.as_str()
            ),
        }
    }

    let mut sub_tracker = SubTracker::new();
    let mut trade_sub_tracker = TradeSubTracker::new();
    let mut pending_notifications: VecDeque<(u64, String, u64)> = VecDeque::new();
    let mut next_req_id: u64 = 100;
    let mut stats = SessionStats::new();

    let mut reserve_tracker: HashMap<[u8; 32], ReserveSnapshot> = HashMap::new();
    let mut last_slot_seen: u64 = 0;
    let mut last_slot_time = Instant::now();

    // GAP #10: Track when the current Helius connection was established.
    // The stale check previously required `last_slot_seen > 0` — on a fresh
    // daemon start where Helius dies before the first slot notification,
    // last_slot_seen stayed 0 and the stale check NEVER fired, leaving the
    // daemon spinning on poll errors forever with no recovery.
    //
    // With conn_established_at, the stale check fires based on connection AGE
    // (wall clock since connect), not slot count. If no slot arrives within
    // STALE_SECS of connection establishment, the connection is declared
    // stale and reconnected — regardless of last_slot_seen.
    let mut helius_conn_established_at = Instant::now();

    // ─── LaserStream gRPC primary ingest lane ────────────────────────────
    let (ls_tx, ls_rx) = mpsc::channel::<LaserStreamUpdate>();
    let mut ls_child: Option<std::process::Child> = None;
    let ls_bin: Option<String> = std::env::var("PQ_LASERSTREAM_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let target_dir = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                .map(|d| d.join("pq-laserstream-grpc.exe"));
            target_dir
                .filter(|p| p.exists())
                .and_then(|p| p.to_str().map(|s| s.to_string()))
        })
        .or_else(|| {
            let p = std::path::PathBuf::from("pq-laserstream-grpc.exe");
            if p.exists() { p.to_str().map(|s| s.to_string()) } else { None }
        });

    // ─── LaserStream spawn helper ────────────────────────────────────────
    // Shared closure for initial spawn + respawn. Reads PQ_LASERSTREAM_ARGS
    // (space-separated subcommand + flags, e.g. "helius-ws --programs p1,p2")
    // so the launch script can configure the binary's mode without code changes.
    // If PQ_LASERSTREAM_ARGS is unset, no args are passed (gRPC binary default).
    let ls_extra_args: Vec<String> = std::env::var("PQ_LASERSTREAM_ARGS")
        .ok()
        .map(|s| s.split_whitespace().map(String::from).collect())
        .unwrap_or_default();

    let spawn_ls = |bin_path: &str| -> Option<std::process::Child> {
        let mut cmd = std::process::Command::new(bin_path);
        cmd.stdout(std::process::Stdio::piped())
           .stderr(std::process::Stdio::piped());
        // Pass through credential env vars to the subprocess.
        if let Ok(endpoint) = std::env::var("LASERSTREAM_ENDPOINT") {
            cmd.env("LASERSTREAM_ENDPOINT", endpoint);
        }
        if let Ok(key) = std::env::var("HELIUS_API_KEY") {
            cmd.env("HELIUS_API_KEY", key);
        }
        if let Ok(ws_url) = std::env::var("HELIUS_WS_URL") {
            cmd.env("HELIUS_WS_URL", ws_url);
        }
        // Forward env vars through WSL2 boundary via WSLENV.
        // When PQ_LASERSTREAM_BIN=wsl.exe, these vars are injected into WSL2
        // so the Linux gRPC binary can read them. WSLENV format: VAR/u
        // (the /u suffix converts Windows paths to Linux paths if needed).
        cmd.env("WSLENV", "HELIUS_API_KEY/u:LASERSTREAM_ENDPOINT/u:HELIUS_WS_URL/u");
        // Inject extra args (subcommand + flags) if provided.
        for arg in &ls_extra_args {
            cmd.arg(arg);
        }
        match cmd.spawn() {
            Ok(mut child) => {
                eprintln!(
                    "[pq-daemon] LaserStream spawned: {} {}",
                    bin_path,
                    if ls_extra_args.is_empty() {
                        "(no args)".to_string()
                    } else {
                        ls_extra_args.join(" ")
                    }
                );
                let stdout = child.stdout.take().expect("piped stdout");
                let ls_tx_clone = ls_tx.clone();
                std::thread::spawn(move || {
                    use std::io::BufRead;
                    let reader = std::io::BufReader::new(stdout);
                    for line in reader.lines() {
                        match line {
                            Ok(text) => {
                                if let Some(update) = parse_ndjson_line(&text) {
                                    if ls_tx_clone.send(update).is_err() {
                                        break;
                                    }
                                }
                            }
                            Err(_) => break,
                        }
                    }
                });
                Some(child)
            }
            Err(e) => {
                eprintln!("[pq-daemon] LaserStream spawn FAILED: {e}");
                None
            }
        }
    };

    if let Some(bin_path) = &ls_bin {
        match spawn_ls(bin_path) {
            Some(child) => {
                stats.ls_spawned = true;
                ls_child = Some(child);
            }
            None => {
                eprintln!("[pq-daemon] Proceeding with Helius WS as secondary lane");
                stats.stubbed_or_assumed.push(
                    "LaserStream spawn failed — Helius WS as fallback".to_string()
                );
            }
        }
    } else {
        eprintln!("[pq-daemon] LaserStream binary not found — set PQ_LASERSTREAM_BIN");
        stats.stubbed_or_assumed.push(
            "LaserStream binary not found — Helius WS as fallback".to_string()
        );
    }
    let _ls_state = LaserStreamState::new(); // reserved for future per-slot accounting
    let mut ls_respawn_count: u32 = 0;
    let mut ls_last_respawn: Option<Instant> = None;

    // ─── Firecrawl web-intelligence sidecar ─────────────────────────────────
    // Same sidecar pattern as LaserStream: spawn a child process, read its
    // stdout (NDJSON SocialEvent payloads), feed into engine.ingest_social().
    // The bridge binary reads trigger events on stdin and scrapes via the
    // local Firecrawl API. Fail-safe: if the bridge or Firecrawl is down,
    // the daemon continues trading without social intelligence.
    let (fc_tx, fc_rx) = mpsc::channel::<Vec<u8>>(); // raw NDJSON bytes
    let mut fc_child: Option<std::process::Child> = None;
    let fc_bin: Option<String> = std::env::var("PQ_FIRECRAWL_BIN").ok()
        .or_else(|| {
            // Look for the bridge binary next to the daemon exe
            let target_dir = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                .map(|d| d.join("pq-firecrawl-bridge.exe"));
            target_dir
                .filter(|p| p.exists())
                .and_then(|p| p.to_str().map(|s| s.to_string()))
        })
        .or_else(|| {
            // Check the tools directory
            let p = std::path::PathBuf::from(
                "../../../tools/firecrawl-bridge-rs/target/release/pq-firecrawl-bridge.exe"
            );
            if p.exists() {
                p.canonicalize()
                    .ok()
                    .and_then(|p| p.to_str().map(|s| s.to_string()))
            } else {
                None
            }
        });

    if let Some(bin_path) = &fc_bin {
        let mut cmd = std::process::Command::new(bin_path);
        cmd.stdout(std::process::Stdio::piped())
           .stderr(std::process::Stdio::piped())
           .stdin(std::process::Stdio::piped());
        match cmd.spawn() {
            Ok(mut child) => {
                eprintln!("[pq-daemon] Firecrawl bridge spawned: {bin_path:?}");
                stats.fc_spawned = true;
                let stdout = child.stdout.take().expect("piped stdout");
                let fc_tx_clone = fc_tx.clone();
                std::thread::spawn(move || {
                    use std::io::BufRead;
                    let reader = std::io::BufReader::new(stdout);
                    for line in reader.lines() {
                        match line {
                            Ok(text) => {
                                if !text.is_empty() {
                                    if fc_tx_clone.send(text.into_bytes()).is_err() {
                                        break;
                                    }
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    eprintln!("[pq-daemon] Firecrawl bridge stdout reader exited");
                });
                fc_child = Some(child);
            }
            Err(e) => {
                eprintln!("[pq-daemon] Firecrawl bridge spawn FAILED: {e}");
                stats.stubbed_or_assumed.push(
                    "Firecrawl bridge spawn failed — no social intelligence".to_string()
                );
            }
        }
    } else {
        eprintln!("[pq-daemon] Firecrawl bridge binary not found — set PQ_FIRECRAWL_BIN");
        stats.stubbed_or_assumed.push(
            "Firecrawl bridge not found — no social intelligence".to_string()
        );
    }

    let tick_period = Duration::from_millis(tick_period_ms);
    let mut next_tick = Instant::now() + tick_period;
    let status_path = std::path::Path::new(STATUS_PATH);
    let mut tick_counter: u64 = 0;
    let mut last_status_write_tick: u64 = 0;
    let mut last_status_write_wallclock: Instant = Instant::now();
    // Counter for periodic session_history.jsonl writes (crash resilience).
    // Every 20 heartbeat writes (~5 min), a final=false checkpoint is appended.
    let mut session_history_write_counter: u32 = 0;
    let mut last_brain_snap_tick: u64 = 0;
    let mut last_tape_flush_tick: u64 = 0;

    // ─── Autonomous bridge: config hot-reload + defense-in-depth ────────
    // The bridge connects the evaluator/refiner framework to the live daemon.
    // G2: hot-reload CONFIG_PROMOTION.json (written by pq-refiner)
    // G4: defense-in-depth — cliff veto, circuit breaker, kill switch
    // G1: periodic refiner spawn (evaluator tape → promotion file)
    let mut defense_state = DefenseState::default();
    let mut config_mtime: Option<u64> = None;
    let mut last_refiner_spawn_tick: u64 = 0;
    let mut refiner_spawner = RefinerSpawner::new();
    // ── GAP B: auto-revert state ─────────────────────────────────────────
    // Tracks the config fingerprint + PnL at the moment of each promotion.
    // If post-promotion PnL deteriorates beyond a threshold within the grace
    // period, the daemon auto-reverts to the archived champion config.
    let mut auto_revert_state = AutoRevertState::default();
    let mut promotion_tick: u64 = 0; // tick at which the last promotion was applied
    let mut pre_promotion_fingerprint: u64 = 0; // fingerprint before the promotion
    let mut trades_at_promotion: u64 = 0; // cumulative trade count at promotion time
    eprintln!("[pq-daemon] autonomous bridge: defense-in-depth + config hot-reload + refiner scheduling + auto-revert ACTIVE");

    // Phase 2: tape exporter — drains engine trades to evaluator JSONL format.
    let mut tape_exporter = TapeExporter::new(TAPE_PATH);
    // Memory bank — aggregates trade records into per-mint and per-strategy
    // performance summaries for continuous optimization toward max net SOL.
    // This is the learning loop: every exited trade feeds the bank, which
    // the refiner reads to adapt strategy weights and Thompson posteriors.
    let mut memory_bank = MemoryBank::new(MemoryBankConfig {
        max_mints: 512,
        max_strategies: 64,
        decay_window: 50,
    });
    let memory_bank_path = "data/memory_bank.json";
    eprintln!("[pq-daemon] memory bank path: data/memory_bank.json");
    // Event stream capture for deterministic replay (§13 paper/live parity).
    // Fail-safe: if the file can't be opened, daemon continues without capture.
    let mut event_stream_writer = EventStreamWriter::open(EVENT_STREAM_PATH);
    eprintln!("[pq-daemon] tape export path: {TAPE_PATH}");

    // ─── Cumulative PnL seed (restart-amnesia fix) ──────────────────────
    // Read tape.jsonl ONCE at startup to get the cumulative realized PnL
    // from ALL prior daemon sessions. The tape is append-forever; live_status
    // resets to 0 on every Engine::new(). We bridge them so the cron always
    // reads the trustworthy cumulative number, not the amnesia-prone session
    // counter.
    let (prior_tape_pnl, prior_tape_trades) = seed_cumulative_from_tape(TAPE_PATH);
    let cfg_text = cfg.dump_to_text();
    let cfg_fp = config_fingerprint(&cfg_text);
    eprintln!(
        "[pq-daemon] cumulative PnL seed: prior_tape={}lamports ({} trades), config_fp={:#018x}, label=\"{}\"",
        prior_tape_pnl, prior_tape_trades, cfg_fp, args.strategy_label
    );

    // ─── === PERSISTENT EVENT LOOP === ────────────────────────────────────
    // Unlike paper_session which has `while Instant::now() < deadline`, this
    // loop runs forever. The only exits are:
    //   1. DAEMON_STOP file → graceful shutdown (exit 0)
    //   2. EMERGENCY_STOP file → immediate exit (exit 99)
    //   3. Both WS connections break AND reconnects fail irrecoverably
    eprintln!("[pq-daemon] === ENTERING PERSISTENT EVENT LOOP ===");

    // GAP #14: Track session start time for daemon_health.json uptime reporting.
    let session_start = Instant::now();

    // ── GAP E: generate unique session_id ──────────────────────────────
    // A unique per-daemon-restart identifier (PID + start timestamp) so A/B
    // comparison can unambiguously attribute PnL to specific sessions.
    let session_id = std::process::id() as u64
        | ((std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()) << 32);
    eprintln!("[pq-daemon] session_id={:#018x}", session_id);

    // ── SUBSCRIPTION CAP SELF-HEALING ──────────────────────────────────
    // When Helius returns -32006 "Too many subscriptions", we set this flag
    // to trigger a forced WS reconnect at the top of the next loop iteration.
    // The reconnect closes the old connection (WS Close frame → Helius
    // releases all subscription slots), opens a fresh one, and re-subscribes
    // all active mints. This breaks the subscribe-error-evict death spiral.
    let mut force_reconnect = false;

    loop {
        // ── Emergency stop check (every iteration) ───────────────────────
        if emergency_stop_requested() {
            eprintln!("[pq-daemon] EMERGENCY STOP detected — halting immediately");
            // Don't clean up — the operator triggered this deliberately.
            // Print a minimal status and exit.
            let st = engine.live_status();
            eprintln!("[pq-daemon] EMERGENCY: ticks={} promoted={} admitted={} net={}lamports",
                st.info_time_tick, st.promoted, st.admitted, st.net_realized_lamports);
            // Kill LaserStream child if present — use kill_process_tree to
            // kill the entire process tree (prevents orphaned gRPC in WSL2).
            // GAP #13: bare child.kill() leaves wsl.exe grandchildren alive.
            if let Some(ref mut child) = ls_child { kill_process_tree(child); }
            // Kill Firecrawl bridge child if present — same tree-kill logic.
            if let Some(ref mut child) = fc_child { kill_process_tree(child); }
            return ExitCode::from(EXIT_EMERGENCY);
        }
        
        // ── Graceful shutdown check (every iteration) ────────────────────
        if daemon_stop_requested() {
            eprintln!("[pq-daemon] DAEMON_STOP detected — initiating graceful shutdown");
            clean_stop_sentinel();
            break;
        }

        let mut did_work = false;

        // ── SUBSCRIPTION CAP RECONNECT (self-healing) ───────────────────
        // If -32006 was detected on the previous iteration, force a full WS
        // reconnect NOW — before any other lane processing. This ensures
        // we don't churn in the death spiral for another full poll cycle.
        if force_reconnect {
            force_reconnect = false; // consume the flag
            eprintln!(
                "[pq-daemon] FORCE RECONNECT triggered — closing old Helius WS to release leaked subscriptions"
            );
            // Send a proper WS Close frame so Helius immediately frees ALL
            // subscription slots on the old connection.
            let _ = helius_conn.close();
            stats.helius_reconnects += 1;
            sub_tracker.clear_server_subs();
            stats.sub_cap_errors = 0; // reset after reconnect
            stats.subs_leaked_no_ack = 0; // fresh connection, no leaks
            // Re-subscribe with the standard backoff ladder.
            let mut backoff_ms = WS_RECONNECT_SLEEP_MS;
            let mut reconnected = false;
            for _ in 0..MAX_RECONNECT_ATTEMPTS {
                if !reconnected {
                    match WsConn::connect(&helius_url) {
                        Ok(mut c) => {
                            let _ = c.set_read_timeout(Duration::from_millis(WS_READ_TIMEOUT_MS));
                            let _ = c.send_text(&helius_ws::slot_subscribe_request());
                            for (_, mint) in sub_tracker.active_mints() {
                                let pda = bonding_curve_pda(&mint);
                                let pda_str = pda.to_string();
                                let req_id = next_req_id;
                                next_req_id += 1;
                                let req = helius_ws::account_subscribe_request(
                                    req_id, &pda_str, &args.commitment,
                                );
                                let _ = c.send_text(&req);
                                sub_tracker.record_request(req_id, mint);
                            }
                            helius_conn_established_at = Instant::now();
                            last_slot_time = Instant::now();
                            reconnected = true;
                            helius_conn = c;
                            eprintln!(
                                "[pq-daemon] FORCE RECONNECT succeeded — {} active mints re-subscribed on fresh connection",
                                sub_tracker.len()
                            );
                        }
                        Err(e) => {
                            eprintln!(
                                "[pq-daemon] FORCE RECONNECT failed (backoff={backoff_ms}ms): {e}"
                            );
                            stats.ws_errors += 1;
                            std::thread::sleep(Duration::from_millis(backoff_ms));
                            backoff_ms = (backoff_ms * 2).min(RECONNECT_BACKOFF_CAP_MS);
                        }
                    }
                }
            }
            if !reconnected {
                eprintln!(
                    "[pq-daemon] FORCE RECONNECT exhausted — stale-check will retry on next tick"
                );
                last_slot_time = Instant::now(); // avoid immediate stale trigger
            }
            did_work = true;
        }


        // ── Wall-clock status heartbeat (top-of-loop) ─────────────────────
        // This fires on EVERY loop iteration, independent of tick timing.
        // Even if the loop is blocked by WS reconnect sleeps or slow polls,
        // this ensures live_status.json is refreshed within
        // STATUS_HEARTBEAT_SECS of the last write. This is the critical
        // defense against watchdog health-check kills when the daemon is
        // healthy but event-starved (all WS lanes degraded).
        if last_status_write_wallclock.elapsed() >= Duration::from_secs(STATUS_HEARTBEAT_SECS) {
            let st = engine.live_status();
            match st.write_to_path(status_path) {
                Ok(()) => {}
                Err(e) => eprintln!("[pq-daemon] heartbeat live_status write failed: {e}"),
            }
            // Restart-amnesia fix: write cumulative_pnl.json alongside
            // live_status.json. This gives the cron a trustworthy PnL
            // number that persists across daemon restarts.
            if let Err(e) = write_cumulative_pnl(
                CUMULATIVE_PNL_PATH,
                cfg_fp,
                &args.strategy_label,
                st.net_realized_lamports,
                prior_tape_pnl,
                prior_tape_trades,
                st.admitted,
                st.info_time_tick,
            ) {
                eprintln!("[pq-daemon] cumulative_pnl write failed: {e}");
            }

            // Crash resilience: periodically append to session_history.jsonl
            // (every ~20 heartbeats ≈ 5 min). If the daemon is killed by the
            // watchdog (taskkill /F) or crashes, the last periodic entry is
            // the best available record for that session. On graceful
            // shutdown, a final=true entry is appended (see shutdown section).
            session_history_write_counter += 1;
            if session_history_write_counter >= 20 {
                session_history_write_counter = 0;
                let uptime = session_start.elapsed().as_secs();
                let _ = append_session_history(
                    SESSION_HISTORY_PATH,
                    cfg_fp,
                    &args.strategy_label,
                    st.net_realized_lamports,
                    prior_tape_pnl,
                    prior_tape_trades,
                    st.admitted,
                    st.info_time_tick,
                    uptime,
                    tape_exporter.total_exported(),
                    prior_tape_trades.saturating_add(tape_exporter.total_exported()),
                    false, // final = false (periodic checkpoint)
                    session_id,
                );
            }
            engine.write_brain_analysis();

            // GAP #14: Write a daemon health JSON that the watchdog can read
            // to detect OnchainConfirm starvation. The live_status.json schema
            // is fixed (live_status/2) and can't be changed without breaking
            // the canonical JSON invariant, so we write a SEPARATE file:
            // data/daemon_health.json. This includes the onchain_confirm count
            // and daemon uptime so the watchdog can detect a dead Helius WS
            // lane and restart the daemon to re-establish it.
            let uptime_secs = session_start.elapsed().as_secs();
            let health_json = format!(
                concat!(
                    "{{",
                    "\"onchain_confirms_decoded\":{},",
                    "\"helius_account_notifications\":{},",
                    "\"helius_reconnects\":{},",
                    "\"ls_transactions_received\":{},",
                    "\"ls_spawned\":{},",
                    "\"uptime_secs\":{},",
                    "\"tick\":{},",
                    "\"account_subs_active\":{},",
                    "\"account_subs_evicted\":{},",
                    "\"subs_leaked_no_ack\":{},",
                    "\"sub_cap_errors\":{}",
                    "}}"
                ),
                stats.helius_onchain_confirms_decoded,
                stats.helius_account_notifications,
                stats.helius_reconnects,
                stats.ls_transactions_received,
                stats.ls_spawned,
                uptime_secs,
                tick_counter,
                sub_tracker.len(),
                stats.account_subs_evicted,
                stats.subs_leaked_no_ack,
                stats.sub_cap_errors,
            );
            let _ = std::fs::write("data/daemon_health.json", health_json);

            last_status_write_tick = tick_counter;
            last_status_write_wallclock = Instant::now();
        }

        // ── Poll LaserStream gRPC (PRIMARY ingest lane) ──────────────────
        loop {
            match ls_rx.try_recv() {
                Ok(LaserStreamUpdate::Transaction(tx)) => {
                    did_work = true;
                    stats.ls_transactions_received += 1;
                    let classified = classify_pump_instructions(&tx);
                    stats.ls_instructions_classified += classified.len() as u64;
                    let events = instructions_to_events(&classified, tx.slot, tx.is_live);
                    for ev in &events {
                        stats.ls_events_emitted += 1;
                        if !queue.push(ev.clone(), tx.slot) {
                            stats.junction_overflow_dropped += 1;
                        }
                    }
                }
                Ok(LaserStreamUpdate::Slot { slot }) => {
                    did_work = true;
                    stats.ls_slots_received += 1;
                    last_slot_seen = slot;
                    last_slot_time = Instant::now();
                }
                Ok(LaserStreamUpdate::Account { .. }) => {
                    did_work = true;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    // Reader thread exited — LaserStream stream ended.
                    // In daemon mode: attempt to RE-SPAWN the binary with a
                    // cooldown to prevent tight-loop respawning against a
                    // fundamentally broken binary (e.g. wrong subcommand,
                    // missing creds, bad endpoint).
                    if stats.ls_spawned {
                        // Check respawn cooldown + max attempts
                        let now = Instant::now();
                        let cooldown_ok = ls_last_respawn
                            .map(|t| now.duration_since(t).as_secs() >= LS_RESPAWN_COOLDOWN_SECS)
                            .unwrap_or(true);
                        if !cooldown_ok {
                            // Too soon — skip respawn this iteration
                            break;
                        }
                        if ls_respawn_count >= LS_MAX_RESPAWN_ATTEMPTS {
                            eprintln!(
                                "[pq-daemon] LaserStream respawn limit reached ({}), \
                                giving up — Helius WS as permanent fallback",
                                ls_respawn_count
                            );
                            stats.stubbed_or_assumed.push(
                                "LaserStream exhausted respawns — Helius WS fallback".to_string()
                            );
                            // Mark ls_spawned false so we don't keep trying
                            stats.ls_spawned = false;
                            break;
                        }
                        eprintln!(
                            "[pq-daemon] LaserStream disconnected — respawn attempt {}/{}",
                            ls_respawn_count + 1,
                            LS_MAX_RESPAWN_ATTEMPTS
                        );
                        stats.ls_reconnects += 1;
                        ls_respawn_count += 1;
                        ls_last_respawn = Some(now);
                        if let Some(bin_path) = &ls_bin {
                            match spawn_ls(bin_path) {
                                Some(child) => {
                                    // GAP #12 FIX: Kill the OLD LS child before
                                    // replacing. Without this, Rust's Drop for
                                    // Child on Windows closes the handle but
                                    // does NOT kill the process — the old LS
                                    // survives as an orphan, keeps its gRPC
                                    // connection to Helius alive, and burns
                                    // credits while its stdout pipe goes
                                    // nowhere. This is the root cause of the
                                    // 2M credit leak.
                                    if let Some(ref mut old) = ls_child {
                                        eprintln!(
                                            "[pq-daemon] killing old LaserStream child (pid={}) before respawn to prevent orphan leak",
                                            old.id()
                                        );
                                        // On Windows, child.kill() calls
                                        // TerminateProcess which kills only the
                                        // immediate process. For wsl.exe-spawned
                                        // LS, we also need taskkill /T to kill
                                        // the WSL subprocess tree.
                                        #[cfg(windows)]
                                        {
                                            let old_pid = old.id();
                                            let _ = std::process::Command::new("taskkill")
                                                .args(["/T", "/F", "/PID", &old_pid.to_string()])
                                                .stdout(std::process::Stdio::null())
                                                .stderr(std::process::Stdio::null())
                                                .status();
                                        }
                                        let _ = old.kill();
                                        let _ = old.wait();
                                    }
                                    eprintln!("[pq-daemon] LaserStream respawned");
                                    ls_child = Some(child);
                                }
                                None => {
                                    eprintln!("[pq-daemon] LaserStream respawn FAILED");
                                    stats.ws_errors += 1;
                                }
                            }
                        }
                    }
                    break;
                }
            }
        }

        // ── Poll PumpPortal ──────────────────────────────────────────────
        match pp_conn.poll_event() {
            Ok(Some(WsEvent::Text(text))) => {
                did_work = true;
                let is_create = text.contains("\"txType\":\"create\"");
                let is_migration =
                    text.contains("\"txType\":\"migrate\"")
                    || text.contains("\"txType\":\"migration\"");

                if is_create {
                    stats.pp_creates_received += 1;
                    if handle_create_payload(text.as_bytes(), 0, &queue) {
                        stats.pp_creates_parsed += 1;
                    }
                    if let Some(meta) =
                        pump_quant_ingest::pumpportal_parse::parse_pumpportal_create(text.as_bytes())
                    {
                        let mint_bytes = meta.mint;
                        let mint_b58 = Pubkey::try_from(mint_bytes)
                            .map(|pk| pk.to_string())
                            .unwrap_or_else(|_| hex_short(&mint_bytes));

                        if trade_sub_tracker.add(&mint_b58) {
                            let sub_msg = pumpportal_ws::subscribe_token_trade(&[mint_b58.clone()]);
                            match pp_conn.send_text(&sub_msg) {
                                Ok(()) => {
                                    stats.pp_trade_subs_sent += 1;
                                }
                                Err(e) => {
                                    eprintln!("[pq-daemon] subscribeTokenTrade FAILED: {e}");
                                    stats.ws_errors += 1;
                                }
                            }
                        }

                        let already_subscribed = sub_tracker
                            .active_mints()
                            .iter()
                            .any(|(_, m)| *m == mint_bytes);

                        if !already_subscribed {
                            // ── PROACTIVE RECONNECT CHECK ──────────────────────
                            // Before subscribing, check if leaked server-side
                            // subscriptions are approaching the Helius cap.
                            // The estimate: every eviction where server_sub_id
                            // is None (ACK never arrived) leaks one server slot
                            // that we can't reclaim via accountUnsubscribe.
                            // If cumulative leaks exceed the threshold, force a
                            // reconnect now to reset the server-side count.
                            // This is the PROACTIVE layer — we don't wait for
                            // the -32006 error to know we're leaking.
                            let estimated_server_subs =
                                sub_tracker.server_visible_count() as u64
                                + stats.subs_leaked_no_ack;
                            let threshold = (HELIUS_SUB_CAP as f64
                                * SUB_CAP_RECONNECT_THRESHOLD) as u64;
                            if estimated_server_subs >= threshold {
                                eprintln!(
                                    "[pq-daemon] PROACTIVE RECONNECT: estimated server \
                                     subs ({estimated_server_subs}) >= threshold ({threshold}), \
                                     leaked_no_ack={}, pending_acks={}, acked={}, forcing Helius reconnect to reset cap",
                                    stats.subs_leaked_no_ack,
                                    sub_tracker.pending_ack_count(),
                                    sub_tracker.acked_count()
                                );
                                // Force reconnect by closing the old connection
                                // and re-establishing. The close() sends a WS
                                // Close frame so Helius releases slots NOW.
                                let _ = helius_conn.close();
                                stats.helius_reconnects += 1;
                                stats.sub_cap_errors = 0; // reset after reconnect
                                stats.subs_leaked_no_ack = 0; // reset after reconnect
                                sub_tracker.clear_server_subs();
                                match WsConn::connect(&helius_url) {
                                    Ok(mut c) => {
                                        let _ = c.set_read_timeout(
                                            Duration::from_millis(WS_READ_TIMEOUT_MS),
                                        );
                                        let _ = c.send_text(
                                            &helius_ws::slot_subscribe_request(),
                                        );
                                        for (_, mint) in sub_tracker.active_mints() {
                                            let pda = bonding_curve_pda(&mint);
                                            let pda_str = pda.to_string();
                                            let req_id = next_req_id;
                                            next_req_id += 1;
                                            let req = helius_ws::account_subscribe_request(
                                                req_id, &pda_str, &args.commitment,
                                            );
                                            let _ = c.send_text(&req);
                                            sub_tracker.record_request(req_id, mint);
                                        }
                                        helius_conn_established_at = Instant::now();
                                        last_slot_time = Instant::now();
                                        helius_conn = c;
                                        eprintln!(
                                            "[pq-daemon] PROACTIVE reconnect succeeded"
                                        );
                                    }
                                    Err(e) => {
                                        eprintln!(
                                            "[pq-daemon] PROACTIVE reconnect FAILED: {e} — \
                                             degrading, will retry on next tick"
                                        );
                                        stats.ws_errors += 1;
                                        last_slot_time = Instant::now();
                                    }
                                }
                            }

                            if sub_tracker.len() >= MAX_ACCOUNT_SUBS {
                                if let Some((evicted_req, evicted_mint, evicted_server_sub)) = sub_tracker.evict_oldest() {
                                    stats.account_subs_evicted += 1;
                                    reserve_tracker.remove(&evicted_mint);
                                    // Track leaked subscriptions: if the ACK
                                    // never arrived (server_sub_id is None),
                                    // we CANNOT send accountUnsubscribe, and
                                    // the server-side slot leaks permanently
                                    // until TCP timeout. This is the root cause
                                    // of the 1000-sub cap death spiral.
                                    if evicted_server_sub.is_none() {
                                        stats.subs_leaked_no_ack += 1;
                                        eprintln!(
                                            "[pq-daemon] EVICT LEAK: req={evicted_req} \
                                             mint={:.8} — ACK never arrived, server slot \
                                             leaked (total leaked: {})",
                                            hex_short(&evicted_mint),
                                            stats.subs_leaked_no_ack
                                        );
                                    } else {
                                        // Send accountUnsubscribe to release the Helius
                                        // server-side subscription slot. Without this the
                                        // connection leaks subscriptions until Helius caps
                                        // at 1000, after which no new accountSubscribe
                                        // succeeds and ALL new candidates fail with
                                        // NeedsOnchainConfirmation.
                                        let ssid = evicted_server_sub.unwrap();
                                        let unsub_id = next_req_id;
                                        next_req_id += 1;
                                        let unsub = helius_ws::account_unsubscribe_request(unsub_id, ssid);
                                        if let Err(e) = helius_conn.send_text(&unsub) {
                                            eprintln!(
                                                "[pq-daemon] accountUnsubscribe send error (server_sub={ssid}): {e}"
                                            );
                                            stats.ws_errors += 1;
                                        }
                                    }
                                    eprintln!(
                                        "[pq-daemon] EVICT sub req={evicted_req} mint={:.8}",
                                        hex_short(&evicted_mint)
                                    );
                                }
                            }

                            let pda = bonding_curve_pda(&mint_bytes);
                            let pda_str = pda.to_string();
                            stats.pdas_derived += 1;
                            stats.pda_venue_present += 1;

                            let req_id = next_req_id;
                            next_req_id += 1;
                            let req = helius_ws::account_subscribe_request(
                                req_id, &pda_str, &args.commitment,
                            );
                            match helius_conn.send_text(&req) {
                                Ok(()) => {
                                    sub_tracker.record_request(req_id, mint_bytes);
                                    stats.account_subs_active = sub_tracker.len();
                                    stats.account_subs_total_attempted += 1;
                                }
                                Err(e) => {
                                    eprintln!("[pq-daemon] accountSubscribe send error: {e}");
                                    stats.ws_errors += 1;
                                }
                            }
                        }
                    }
                } else if is_migration {
                    stats.pp_migrations_received += 1;
                    if handle_migration_payload(text.as_bytes(), 0, &queue) {
                        stats.pp_migrations_parsed += 1;
                    }
                } else {
                    stats.pp_trades_received += 1;
                    if handle_trade_payload(text.as_bytes(), 0, &queue) {
                        stats.pp_trades_enqueued += 1;
                    }
                    // Phase 3: Mark the mint as trade-active in SubTracker so
                    // the trade-aware eviction policy preserves its Helius
                    // subscription slot. Without this, a mint receiving heavy
                    // buying pressure could be evicted just because it was
                    // subscribed early, losing its OnchainConfirm slot
                    // precisely when it matters most.
                    if let Some(tx) = pump_quant_ingest::pumpportal_parse::parse_pumpportal(text.as_bytes()) {
                        sub_tracker.mark_trade_seen(&tx.mint);
                    }
                }
            }
            Ok(Some(WsEvent::Closed(reason))) => {
                eprintln!("[pq-daemon] PumpPortal closed: {reason}, reconnecting…");
                stats.pp_reconnects += 1;
                pp_conn = match WsConn::connect(&pp_url) {
                    Ok(mut c) => {
                        let _ = c.set_read_timeout(Duration::from_millis(WS_READ_TIMEOUT_MS));
                        for sub in pumpportal_ws::subscription_batch(&[]) {
                            let _ = c.send_text(&sub);
                        }
                        let keys = trade_sub_tracker.keys();
                        if !keys.is_empty() {
                            let sub_msg = pumpportal_ws::subscribe_token_trade(&keys);
                            let _ = c.send_text(&sub_msg);
                        }
                        c
                    }
                    Err(e) => {
                        eprintln!("[pq-daemon] PumpPortal reconnect failed: {e}");
                        stats.ws_errors += 1;
                        // Bounded sleep: 500ms (was 5s). Keeps tick loop alive.
                        std::thread::sleep(Duration::from_millis(WS_RECONNECT_SLEEP_MS));
                        // Try to reconnect the existing connection object
                        match WsConn::connect(&pp_url) {
                            Ok(mut c) => {
                                let _ = c.set_read_timeout(Duration::from_millis(WS_READ_TIMEOUT_MS));
                                for sub in pumpportal_ws::subscription_batch(&[]) {
                                    let _ = c.send_text(&sub);
                                }
                                let keys = trade_sub_tracker.keys();
                                if !keys.is_empty() {
                                    let sub_msg = pumpportal_ws::subscribe_token_trade(&keys);
                                    let _ = c.send_text(&sub_msg);
                                }
                                c
                            }
                            Err(_) => {
                                // Graceful degradation: keep the old (broken) conn.
                                // The daemon continues with LaserStream + Helius.
                                // Previously this path panicked — crashing the daemon
                                // and burning a watchdog restart for a transient
                                // network issue.
                                eprintln!("[pq-daemon] PumpPortal 2nd reconnect failed — degrading, continuing with LaserStream/Helius");
                                pp_conn
                            }
                        }
                    }
                };
            }
            Ok(Some(WsEvent::Pong)) | Ok(None) => {}
            Ok(Some(WsEvent::Binary(_))) => { stats.ws_errors += 1; }
            Err(e) => {
                eprintln!("[pq-daemon] PumpPortal poll error: {e}");
                stats.ws_errors += 1;
            }
        }

        // ── Poll Helius ──────────────────────────────────────────────────
        match helius_conn.poll_event() {
            Ok(Some(WsEvent::Text(text))) => {
                did_work = true;
                let v = match json::parse(&text) {
                    Ok(v) => v,
                    Err(_) => { stats.ws_errors += 1; continue; }
                };

                if let helius_ws::Inbound::Ack { id } = helius_ws::classify(&v) {
                    if let Some(server_sub_id) = v.get("result").and_then(Value::as_u64) {
                        sub_tracker.record_ack(id, server_sub_id);
                        let mut still_pending = VecDeque::new();
                        while let Some((ssub, data_str, slot)) = pending_notifications.pop_front() {
                            if ssub == server_sub_id {
                                if let Some(mb) = sub_tracker.mint_for_server_sub(ssub) {
                                    if let Ok(account_data) = B64.decode(data_str.as_bytes()) {
                                        if let Some((provenanced, curve)) =
                                            decode_onchain_confirm_with_curve(&mb, &account_data, slot)
                                        {
                                            queue.push(provenanced, slot);
                                            stats.helius_onchain_confirms_decoded += 1;
                                            stats.last_confirm_tick = tick_counter;
                                            stats.pda_venue_matches += 1;

                                            let prev = reserve_tracker.get(&mb).copied();
                                            if let Some(trade_pe) = derive_market_trade_from_delta(
                                                &mb, prev, &curve, slot, true,
                                            ) {
                                                queue.push(trade_pe, slot);
                                                stats.delta_trades_derived += 1;
                                            } else {
                                                stats.delta_no_trade += 1;
                                            }
                                            reserve_tracker.insert(mb, ReserveSnapshot {
                                                virtual_sol: curve.virtual_sol,
                                                virtual_token: curve.virtual_token,
                                                slot,
                                            });
                                        }
                                    }
                                }
                            } else {
                                still_pending.push_back((ssub, data_str, slot));
                            }
                        }
                        pending_notifications = still_pending;
                    }
                    continue;
                }

                match helius_ws::classify(&v) {
                    helius_ws::Inbound::Notification { sub, result } => {
                        match sub {
                            "slot" => {
                                stats.helius_slot_notifications += 1;
                                if let Some(s) = helius_ws::slot_of(result) {
                                    last_slot_seen = s;
                                    last_slot_time = Instant::now();
                                }
                            }
                            "account" => {
                                stats.helius_account_notifications += 1;
                                let params = v.get("params");
                                let server_sub = params
                                    .and_then(|p| extract_server_sub_id(p))
                                    .unwrap_or(0);

                                let mint_bytes = sub_tracker.mint_for_server_sub(server_sub);

                                if let Some(mb) = mint_bytes {
                                    let (data_b64, slot_opt) = extract_account_data(result);
                                    let slot = slot_opt.unwrap_or(0);
                                    if let Some(data_str) = data_b64 {
                                        if let Ok(account_data) = B64.decode(data_str.as_bytes()) {
                                            if let Some((provenanced, curve)) =
                                                decode_onchain_confirm_with_curve(&mb, &account_data, slot)
                                            {
                                                queue.push(provenanced, slot);
                                                stats.helius_onchain_confirms_decoded += 1;
                                            stats.last_confirm_tick = tick_counter;
                                                stats.pda_venue_matches += 1;

                                                let prev = reserve_tracker.get(&mb).copied();
                                                if let Some(trade_pe) = derive_market_trade_from_delta(
                                                    &mb, prev, &curve, slot, true,
                                                ) {
                                                    queue.push(trade_pe, slot);
                                                    stats.delta_trades_derived += 1;
                                                } else {
                                                    stats.delta_no_trade += 1;
                                                }
                                                reserve_tracker.insert(mb, ReserveSnapshot {
                                                    virtual_sol: curve.virtual_sol,
                                                    virtual_token: curve.virtual_token,
                                                    slot,
                                                });
                                            }
                                        }
                                    }
                                } else {
                                    let (data_b64, slot_opt) = extract_account_data(result);
                                    let slot = slot_opt.unwrap_or(0);
                                    let data_str = data_b64.unwrap_or_default();
                                    pending_notifications.push_back((server_sub, data_str, slot));
                                    if pending_notifications.len() > 200 {
                                        pending_notifications.pop_front();
                                    }
                                }
                            }
                            _ => { stats.ws_errors += 1; }
                        }
                    }
                    helius_ws::Inbound::Ack { id } => {
                        if let Some(server_sub_id) = v.get("result").and_then(Value::as_u64) {
                            sub_tracker.record_ack(id, server_sub_id);
                        }
                    }
                    helius_ws::Inbound::RpcError { id, text: err } => {
                        eprintln!("[pq-daemon] Helius RPC error (id={:?}): {err}", id);
                        stats.ws_errors += 1;
                        // ── SUBSCRIPTION CAP SELF-HEALING ──────────────────────
                        // Helius returns -32006 "Too many subscriptions on the
                        // connection" when the server-side subscription count
                        // hits 1000. This happens when subscriptions are evicted
                        // before their ACK arrives — the daemon frees its local
                        // slot but the server still holds the subscription
                        // (no server_sub_id to unsubscribe → leaked slot).
                        //
                        // Once the cap is hit, every new accountSubscribe
                        // returns -32006. The old code just logged and moved
                        // on, churning in a subscribe-error-evict death spiral.
                        //
                        // SELF-HEALING: on detecting -32006, we force a full
                        // WS reconnect. Closing the old connection (with a WS
                        // Close frame) tells Helius to immediately release ALL
                        // subscription state. The new connection starts at 0
                        // subscriptions. We then re-subscribe all active mints
                        // with proper record_request() so ACKs map correctly.
                        if err.contains("-32006")
                            || err.contains("Too many subscriptions")
                            || err.contains("Exceeded max limit")
                        {
                            stats.sub_cap_errors += 1;
                            eprintln!(
                                "[pq-daemon] SUB CAP ERROR #{} — forcing WS reconnect to release leaked subscriptions (leaked={})",
                                stats.sub_cap_errors, stats.subs_leaked_no_ack
                            );
                            // Mark for reconnect — the force_reconnect flag is
                            // checked at the top of the next poll cycle.
                            force_reconnect = true;
                        }
                    }
                    helius_ws::Inbound::Drift => {
                        eprintln!("[pq-daemon] Helius schema drift: {:.200}", text);
                        stats.ws_errors += 1;
                    }
                }
            }
            Ok(Some(WsEvent::Closed(reason))) => {
                eprintln!("[pq-daemon] Helius closed: {reason}, reconnecting…");
                // Send WS Close on the old connection (may already be closed,
                // but best-effort — ensures server-side subscription release).
                let _ = helius_conn.close();
                stats.helius_reconnects += 1;
                sub_tracker.clear_server_subs();
                // GAP #11: Exponential backoff reconnect ladder.
                // The old code tried once, slept 500ms, tried once more, then
                // gave up. Against a rate-limiting or overloaded Helius endpoint,
                // two immediate retries both fail. The backoff ladder doubles
                // the sleep from 500ms → 1s → 2s → 4s → 8s (capped at 10s),
                // giving the server progressively more time to recover.
                let mut backoff_ms = WS_RECONNECT_SLEEP_MS;
                let mut reconnected = false;
                for _ in 0..MAX_RECONNECT_ATTEMPTS {
                    if !reconnected {
                        match WsConn::connect(&helius_url) {
                            Ok(mut c) => {
                                let _ = c.set_read_timeout(Duration::from_millis(WS_READ_TIMEOUT_MS));
                                let _ = c.send_text(&helius_ws::slot_subscribe_request());
                                // GAP #8: record_request() MUST be called for
                                // each re-subscription. Without it, the ACK
                                // from Helius arrives but record_ack() can't
                                // map req_id → mint because record_request
                                // never stored it. server_sub_to_mint stays
                                // empty → all notifications are silently
                                // dropped → 0 OnchainConfirms after reconnect.
                                for (_, mint) in sub_tracker.active_mints() {
                                    let pda = bonding_curve_pda(&mint);
                                    let pda_str = pda.to_string();
                                    let req_id = next_req_id;
                                    next_req_id += 1;
                                    let req = helius_ws::account_subscribe_request(
                                        req_id, &pda_str, &args.commitment,
                                    );
                                    let _ = c.send_text(&req);
                                    // CRITICAL: register the req_id → mint
                                    // mapping so the ACK can be resolved.
                                    sub_tracker.record_request(req_id, mint);
                                }
                                helius_conn_established_at = Instant::now();
                                last_slot_time = Instant::now();
                                reconnected = true;
                                helius_conn = c;
                            }
                            Err(e) => {
                                eprintln!(
                                    "[pq-daemon] Helius reconnect failed (backoff={backoff_ms}ms): {e}"
                                );
                                stats.ws_errors += 1;
                                std::thread::sleep(Duration::from_millis(backoff_ms));
                                backoff_ms = (backoff_ms * 2).min(RECONNECT_BACKOFF_CAP_MS);
                            }
                        }
                    }
                }
                if !reconnected {
                    eprintln!(
                        "[pq-daemon] Helius reconnect exhausted after {MAX_RECONNECT_ATTEMPTS} attempts — degrading, continuing with LaserStream/PumpPortal"
                    );
                    // Graceful degradation: keep the old (broken) conn.
                    // The stale-check watchdog will retry on the next tick.
                }
            }
            Ok(Some(WsEvent::Pong)) | Ok(None) => {}
            Ok(Some(WsEvent::Binary(_))) => { stats.ws_errors += 1; }
            Err(e) => {
                // GAP #9: Err path recovery — THE ACTIVE ROOT CAUSE.
                //
                // The old code here was:
                //   eprintln!("Helius poll error: {e}");
                //   stats.ws_errors += 1;
                //
                // That's it. No reconnect, no state reset, no backoff.
                // When the Helius TCP connection was forcibly closed by the
                // remote host (Windows error WSAECONNRESET, os error 10054),
                // poll_event() returned Err on EVERY subsequent call —
                // 34,379 consecutive poll errors over hours. The daemon
                // kept sending EVICT unsubscribes and new accountSubscribe
                // requests into a dead socket. Zero slot notifications,
                // zero OnchainConfirms, 95% NeedsOnchainConfirmation rejects.
                //
                // The fix: treat Err identically to WsEvent::Closed —
                // clear server subs, reconnect with backoff, re-subscribe
                // all active mints with record_request(), reset timers.
                eprintln!("[pq-daemon] Helius poll error: {e}");
                // Send WS Close on the old connection (best-effort — the TCP
                // socket may already be dead, but if it's half-open this
                // accelerates server-side subscription release).
                let _ = helius_conn.close();
                stats.ws_errors += 1;
                stats.helius_reconnects += 1;
                sub_tracker.clear_server_subs();
                let mut backoff_ms = WS_RECONNECT_SLEEP_MS;
                let mut reconnected = false;
                for _ in 0..MAX_RECONNECT_ATTEMPTS {
                    if !reconnected {
                        match WsConn::connect(&helius_url) {
                            Ok(mut c) => {
                                let _ = c.set_read_timeout(Duration::from_millis(WS_READ_TIMEOUT_MS));
                                let _ = c.send_text(&helius_ws::slot_subscribe_request());
                                for (_, mint) in sub_tracker.active_mints() {
                                    let pda = bonding_curve_pda(&mint);
                                    let pda_str = pda.to_string();
                                    let req_id = next_req_id;
                                    next_req_id += 1;
                                    let req = helius_ws::account_subscribe_request(
                                        req_id, &pda_str, &args.commitment,
                                    );
                                    let _ = c.send_text(&req);
                                    sub_tracker.record_request(req_id, mint);
                                }
                                helius_conn_established_at = Instant::now();
                                last_slot_time = Instant::now();
                                reconnected = true;
                                helius_conn = c;
                                eprintln!(
                                    "[pq-daemon] Helius Err-path reconnect succeeded after poll errors"
                                );
                            }
                            Err(err) => {
                                eprintln!(
                                    "[pq-daemon] Helius Err-path reconnect failed (backoff={backoff_ms}ms): {err}"
                                );
                                stats.ws_errors += 1;
                                std::thread::sleep(Duration::from_millis(backoff_ms));
                                backoff_ms = (backoff_ms * 2).min(RECONNECT_BACKOFF_CAP_MS);
                            }
                        }
                    }
                }
                if !reconnected {
                    eprintln!(
                        "[pq-daemon] Helius Err-path reconnect exhausted after {MAX_RECONNECT_ATTEMPTS} attempts — degrading, stale-check will retry"
                    );
                    // Reset stale timer to avoid immediate re-trigger spam;
                    // the stale check below will retry on the next tick.
                    last_slot_time = Instant::now();
                }
            }
        }

        // ── Keepalive + staleness ────────────────────────────────────────
        let _ = pp_conn.maybe_keepalive();
        let _ = helius_conn.maybe_keepalive();
        // GAP #10: The stale check guard previously required `last_slot_seen > 0`.
        // On a fresh daemon start where Helius dies before the first slot
        // notification, last_slot_seen stays 0 and this check NEVER fired,
        // leaving the daemon spinning on poll errors forever with no recovery.
        //
        // The fix: also check connection age. If the connection has been alive
        // for > STALE_SECS and no slot has arrived (last_slot_time elapsed >
        // STALE_SECS), it's stale. The `last_slot_seen > 0` guard is removed —
        // connection age alone is sufficient to declare staleness.
        if last_slot_time.elapsed() > Duration::from_secs(STALE_SECS)
            && helius_conn_established_at.elapsed() > Duration::from_secs(STALE_SECS)
        {
            eprintln!(
                "[pq-daemon] Helius stale: no slot for {}s (conn age {}s, last_slot_seen={}), reconnecting",
                last_slot_time.elapsed().as_secs(),
                helius_conn_established_at.elapsed().as_secs(),
                last_slot_seen,
            );
            // Send WS Close on the old connection to accelerate server-side
            // subscription slot release before opening a fresh connection.
            let _ = helius_conn.close();
            stats.helius_reconnects += 1;
            sub_tracker.clear_server_subs();
            // GAP #11: Same exponential backoff ladder as the Closed/Err paths.
            let mut backoff_ms = WS_RECONNECT_SLEEP_MS;
            let mut reconnected = false;
            for _ in 0..MAX_RECONNECT_ATTEMPTS {
                if !reconnected {
                    match WsConn::connect(&helius_url) {
                        Ok(mut c) => {
                            let _ = c.set_read_timeout(Duration::from_millis(WS_READ_TIMEOUT_MS));
                            let _ = c.send_text(&helius_ws::slot_subscribe_request());
                            for (_, mint) in sub_tracker.active_mints() {
                                let pda = bonding_curve_pda(&mint);
                                let pda_str = pda.to_string();
                                let req_id = next_req_id;
                                next_req_id += 1;
                                let req = helius_ws::account_subscribe_request(
                                    req_id, &pda_str, &args.commitment,
                                );
                                let _ = c.send_text(&req);
                                sub_tracker.record_request(req_id, mint);
                            }
                            helius_conn_established_at = Instant::now();
                            last_slot_time = Instant::now();
                            reconnected = true;
                            helius_conn = c;
                        }
                        Err(e) => {
                            eprintln!(
                                "[pq-daemon] Helius stale-reconnect failed (backoff={backoff_ms}ms): {e}"
                            );
                            stats.ws_errors += 1;
                            std::thread::sleep(Duration::from_millis(backoff_ms));
                            backoff_ms = (backoff_ms * 2).min(RECONNECT_BACKOFF_CAP_MS);
                        }
                    }
                }
            }
            if !reconnected {
                eprintln!(
                    "[pq-daemon] Helius stale-reconnect exhausted after {MAX_RECONNECT_ATTEMPTS} attempts — degrading"
                );
                // Don't break — keep the daemon alive. Reset stale timer
                // to avoid spam. The next loop iteration will retry.
                last_slot_time = Instant::now();
            }
        }

        // ── Drain junction queue into engine ─────────────────────────────
        while let Some((provenanced, dwell)) = queue.pop_with_dwell() {
            engine.tick(provenanced.event);
            stats.junction_events_drained += 1;
            dwell_samples.push(dwell.as_millis() as u64);

            // ── Capture raw event for deterministic replay ─────────────
            // Each event is serialized to a compact JSON line in
            // data/event_stream.jsonl. The replay engine reads this file
            // to re-execute the engine with mutated configs.
            if let Some(ref mut writer) = event_stream_writer {
                if let Err(e) = writer.write_event(&provenanced.event, last_slot_seen) {
                    eprintln!("[pq-daemon] event_stream write error: {}", e);
                }
            }
        }

        // ── Poll Firecrawl bridge (social intelligence ingest) ──────────
        // The bridge outputs NDJSON SocialEvent payloads. We wrap each line
        // into a RawSocialPayload and feed the batch through engine.ingest_social()
        // which uses the existing SocialSource trait (same path as LaserStream).
        // Fail-safe: if Firecrawl/bridge is down, daemon continues trading.
        {
            let mut batch: Vec<pump_quant_ingest::social_source::RawSocialPayload> = Vec::new();
            let now_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;
            loop {
                match fc_rx.try_recv() {
                    Ok(json_bytes) => {
                        batch.push(
                            pump_quant_ingest::social_source::RawSocialPayload::new(
                                json_bytes,
                                now_ns,
                            ),
                        );
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        if stats.fc_spawned {
                            eprintln!("[pq-daemon] Firecrawl bridge disconnected — social intelligence degraded");
                            stats.stubbed_or_assumed.push(
                                "Firecrawl bridge disconnected mid-session".to_string()
                            );
                        }
                        break;
                    }
                }
            }
            if !batch.is_empty() {
                // Feed the batch through the existing SocialSource trait path.
                // We create a one-shot source that returns the batch once.
                let mut source = FirecrawlBatchSource { batch, idx: 0 };
                let ingested = engine.ingest_social(&mut source);
                stats.fc_events_ingested += ingested as u64;
                did_work = true;
            }
        }

        // ── Emit Firecrawl triggers to bridge stdin ─────────────────────
        // The daemon sends trigger events to the bridge's stdin so it knows
        // what to scrape. Each trigger is a JSON line. The 10 triggers:
        //   1. band_entry        — coin enters $9k-$20k band
        //   2. velocity_spike    — abnormal price/volume velocity
        //   3. mint_promotion    — new mint promoted by engine
        //   4. position_event    — position entry or exit
        //   5. entropy_spike     — order-flow entropy spike (ArXiv 2512.15720)
        //   6. wash_signature    — wash-trading detection (ArXiv 2411.05803)
        //   7. sentiment_div     — social sentiment divergence (ArXiv 1506.01513)
        //   8. wallet_cluster    — creator wallet clustering (ArXiv 2505.09313)
        //   9. mev_invariance    — MEV invariance violation (ArXiv 2304.11010)
        //  10. liquidity_collapse— liquidity depth collapse
        // Triggers are only sent if the bridge child is alive and has stdin.
        // GAP H: Check if the child has exited before writing to stdin. If
        // the child process is dead, writing to its stdin causes a broken
        // pipe which can block or error. We detect this by try_wait() and
        // set fc_child = None so we stop trying to write to a dead pipe.
        if let Some(ref mut child) = fc_child {
            // GAP H: Check if the Firecrawl bridge child has exited
            match child.try_wait() {
                Ok(Some(_status)) => {
                    // Child has exited — stop trying to write to its stdin
                    eprintln!("[pq-daemon] Firecrawl bridge child exited — dropping fc_child, social intelligence disabled");
                    stats.stubbed_or_assumed.push(
                        "Firecrawl bridge exited — social intelligence disabled".to_string()
                    );
                    drop(child.stdin.take()); // drop stdin to close the pipe cleanly
                    fc_child = None;
                }
                Ok(None) => {
                    // Child still running — safe to write to stdin
                    if let Some(ref mut stdin) = child.stdin {
                let st = engine.live_status();
                let ts_ns = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos();

                // Trigger 1: band_entry — check if any promoted coin is in band
                if st.promoted > 0 && st.promoted % 50 == 0 {
                    let trigger = format!(
                        r#"{{"trigger":"band_entry","mint_count":{},"ts":{}}}"#,
                        st.promoted, ts_ns
                    );
                    if stdin.write_all(format!("{trigger}\n").as_bytes()).is_ok() {
                        stats.fc_triggers_emitted += 1;
                    }
                }

                // Trigger 2: velocity_spike — check net realized for spike detection
                if st.net_realized_lamports.abs() > 1_000_000_000 {
                    let trigger = format!(
                        r#"{{"trigger":"velocity_spike","net_lamports":{},"ts":{}}}"#,
                        st.net_realized_lamports, ts_ns
                    );
                    if stdin.write_all(format!("{trigger}\n").as_bytes()).is_ok() {
                        stats.fc_triggers_emitted += 1;
                    }
                }

                // Trigger 3: mint_promotion — on each promotion milestone
                if st.promoted > 0 && st.promoted % 100 == 0 {
                    let trigger = format!(
                        r#"{{"trigger":"mint_promotion","total_promoted":{},"ts":{}}}"#,
                        st.promoted, ts_ns
                    );
                    if stdin.write_all(format!("{trigger}\n").as_bytes()).is_ok() {
                        stats.fc_triggers_emitted += 1;
                    }
                }

                // Trigger 4: position_event — on admission changes
                if st.admitted > 0 && st.admitted % 10 == 0 {
                    let trigger = format!(
                        r#"{{"trigger":"position_event","admitted":{},"ts":{}}}"#,
                        st.admitted, ts_ns
                    );
                    if stdin.write_all(format!("{trigger}\n").as_bytes()).is_ok() {
                        stats.fc_triggers_emitted += 1;
                    }
                }
                }  // close if let Some(ref mut stdin)
                }  // close Ok(None) => arm
                Err(e) => {
                    eprintln!("[pq-daemon] Firecrawl bridge try_wait error: {e}");
                    drop(child.stdin.take());
                    fc_child = None;
                }
            }  // close match child.try_wait()
        }  // close if let Some(ref mut child) = fc_child

        // ── Periodic Tick (engine evaluate) ──────────────────────────────
        if Instant::now() >= next_tick {
            engine.tick(pump_quant_app::event::AppEvent::Tick);
            // Phase 3: write the Tick to the event stream so the replay engine
            // can reproduce the evaluate() calls. Without Ticks in the stream,
            // the engine replay never triggers admission decisions — making
            // the refiner's engine-replay subprocess useless.
            if let Some(ref mut writer) = event_stream_writer {
                if let Err(e) = writer.write_event(
                    &pump_quant_app::event::AppEvent::Tick,
                    last_slot_seen,
                ) {
                    eprintln!("[pq-daemon] event_stream tick write error: {}", e);
                }
            }
            next_tick = Instant::now() + tick_period;
            tick_counter += 1;

            // ── Periodic status write ────────────────────────────────────
            // Tick-count based status write. The wall-clock heartbeat at
            // the top of the loop handles the event-starvation case.
            if tick_counter - last_status_write_tick >= args.status_every_ticks {
                let st = engine.live_status();
                match st.write_to_path(status_path) {
                    Ok(()) => {}
                    Err(e) => eprintln!("[pq-daemon] live_status write failed: {e}"),
                }
                // Restart-amnesia fix: write cumulative_pnl.json alongside
                // live_status.json on every periodic status write too.
                if let Err(e) = write_cumulative_pnl(
                    CUMULATIVE_PNL_PATH,
                    cfg_fp,
                    &args.strategy_label,
                    st.net_realized_lamports,
                    prior_tape_pnl,
                    prior_tape_trades,
                    st.admitted,
                    st.info_time_tick,
                ) {
                    eprintln!("[pq-daemon] cumulative_pnl write failed: {e}");
                    }

                    // Crash resilience: periodically append to session_history.jsonl
                    // (every ~20 heartbeats ≈ 5 min). If the daemon is killed by the
                    // watchdog (taskkill /F) or crashes, the last periodic entry is
                    // the best available record for that session. On graceful
                    // shutdown, a final=true entry is appended (see shutdown section).
                    session_history_write_counter += 1;
                    if session_history_write_counter >= 20 {
                    session_history_write_counter = 0;
                    let uptime = session_start.elapsed().as_secs();
                    let _ = append_session_history(
                        SESSION_HISTORY_PATH,
                        cfg_fp,
                        &args.strategy_label,
                        st.net_realized_lamports,
                        prior_tape_pnl,
                        prior_tape_trades,
                        st.admitted,
                        st.info_time_tick,
                        uptime,
                        tape_exporter.total_exported(),
                        prior_tape_trades.saturating_add(tape_exporter.total_exported()),
                        false, // final = false (periodic checkpoint)
                        session_id,
                    );
                    }
                    engine.write_brain_analysis();
                // Export memory bank summaries for the refiner to consume.
                // This is the learning loop's output: per-mint and per-strategy
                // performance data that feeds progressive refinement.
                let mb_json = memory_bank.global_json();
                let _ = std::fs::write(memory_bank_path, &mb_json);
                last_status_write_tick = tick_counter;
                last_status_write_wallclock = Instant::now();
            }

            // ── Periodic brain snapshot ─────────────────────────────────
            if tick_counter - last_brain_snap_tick >= args.brain_snapshot_every_ticks {
                match engine.snapshot_brain() {
                    Ok(()) => eprintln!("[pq-daemon] brain snapshot saved (tick={tick_counter})"),
                    Err(e) => eprintln!("[pq-daemon] brain snapshot FAILED: {e}"),
                }
                last_brain_snap_tick = tick_counter;
            }

            // ── Periodic tape export ──────────────────────────────────────
            if tick_counter - last_tape_flush_tick >= args.tape_every_ticks {
                let trades = engine.take_tape_trades();
                for t in &trades {
                    let lane = if t.scalp { TapeLane::Scalp } else { TapeLane::Early };
                    let net = t.gross as i64 - t.fees as i64 - t.tips as i64 - t.failed as i64;
                    // Emit enriched TradeFull record (16-field format for replay).
                    // Fields not yet available from engine.take_tape_trades() are
                    // zeroed — future enrichment will populate them from the
                    // decision journal and position exit context.
                    let mint_b58 = Pubkey::try_from(t.mint)
                        .map(|pk| pk.to_string())
                        .unwrap_or_else(|_| hex_short(&t.mint));
                    tape_exporter.push(TapeRecord::TradeFull {
                        slot: last_slot_seen,
                        mint_b58,
                        side_tag: "buy",
                        entry_price_fp: t.entry_price_fp as i128,
                        exit_price_fp: t.exit_price_fp as i128,
                        size_lamports: t.size_lamports,
                        strategy_id: t.archetype as u64,
                        source_tag: if t.scalp { "scalp" } else { "early" },
                        outcome_tag: if net >= 0 { "profit" } else { "loss" },
                        realized_pnl_lamports: net,
                        fees_lamports: (t.fees + t.tips) as u64,
                        slippage_lamports: t.failed as u64,
                        decision_latency_us: 0,
                        confirm_latency_us: 0,
                        run_mode_tag: "paper",
                        error_code: t.exit_reason_code as u32,
                        seq: 0,
                        mfe_bps: t.mfe_bps,
                        mae_bps: t.mae_bps,
                    });
                    // Also emit the coarse 5-field Trade record for backward
                    // compatibility with existing evaluator/refiner code.
                    // S2: Use the actual lane derived from t.scalp instead of
                    // hardcoding TapeLane::Scalp. This gives the refiner per-lane
                    // performance data so it can cross-check reflection's weight
                    // decisions rather than being blind to lane attribution.
                    tape_exporter.push(TapeRecord::Trade {
                        lane,
                        gross: t.gross,
                        fees: t.fees,
                        tips: t.tips,
                        failed: t.failed,
                    });
                    // Feed the memory bank — the learning loop. Every exited
                    // trade is recorded with full provenance so the bank can
                    // build per-mint and per-strategy performance summaries.
                    let trade_lane = if t.scalp { TradeLane::Scalp } else { TradeLane::Early };
                    let rec = TradeRecord {
                        slot: last_slot_seen,
                        mint_b58: Pubkey::try_from(t.mint)
                            .map(|pk| pk.to_string())
                            .unwrap_or_else(|_| hex_short(&t.mint)),
                        side: TradeSide::Buy,
                        entry_price_fp: t.entry_price_fp as i128,
                        exit_price_fp: t.exit_price_fp as i128,
                        size_lamports: t.size_lamports,
                        strategy_id: t.archetype as u64,
                        source: if t.scalp { ProvenanceSource::HeliusAccountSubscribe }
                                else { ProvenanceSource::PumpPortalTrade },
                        outcome: if net >= 0 { TradeOutcome::Filled }
                                 else { TradeOutcome::FilledWithSlippage },
                        realized_pnl_lamports: net,
                        fees_lamports: (t.fees + t.tips) as u64,
                        slippage_lamports: t.failed as u64,
                        decision_latency_us: 0,
                        confirm_latency_us: 0,
                        run_mode: JournalRunMode::Paper,
                        error_code: t.exit_reason_code as u32,
                        seq: 0,
                        lane: Some(trade_lane),
                        mfe_bps: t.mfe_bps,
                        mae_bps: t.mae_bps,
                    };
                    memory_bank.ingest(&rec);
                }
                if tape_exporter.pending_count() > 0 {
                    match tape_exporter.flush() {
                        Ok(n) => eprintln!(
                            "[pq-daemon] tape export: {n} records (total={})",
                            tape_exporter.total_exported()
                        ),
                        Err(e) => eprintln!("[pq-daemon] tape export FAILED: {e}"),
                    }
                }
                // Flush event stream alongside tape.
                if let Some(ref mut writer) = event_stream_writer {
                    let _ = writer.flush();
                }
                last_tape_flush_tick = tick_counter;
            }

            // ── G3 fix: periodic creator ledger persistence ────────────
            // Snapshot the creator ledger to disk every tape flush cycle.
            // This ensures creator track records survive daemon restarts.
            // Atomic write: write to .tmp then rename (prevents corruption
            // if the daemon is killed mid-write).
            {
                let bytes = engine.snapshot_creator_ledger();
                let tmp_path = format!("{LEDGER_PATH}.tmp");
                match std::fs::write(&tmp_path, &bytes) {
                    Ok(()) => {
                        if let Err(e) = std::fs::rename(&tmp_path, LEDGER_PATH) {
                            eprintln!("[pq-daemon] ledger rename FAILED: {e}");
                        }
                    }
                    Err(e) => eprintln!("[pq-daemon] ledger write FAILED: {e}"),
                }
            }

            // ── Autonomous bridge: config hot-reload (G2) ──────────────
            // Check if CONFIG_PROMOTION.json has been written/updated by
            // the refiner. If so, parse mutations and apply to live config.
            {
                let pre_reload_fp = config_fingerprint(&cfg.dump_to_text());
                let reload = try_reload_config(&mut cfg, &mut config_mtime);
                if reload.applied {
                    let n = reload.n_mutations;
                    let post_reload_fp = config_fingerprint(&cfg.dump_to_text());
                    eprintln!(
                        "[pq-daemon] CONFIG HOT-RELOAD: {n} mutations applied. Summary: {}",
                        reload.summary
                    );

                    // ── GAP B: record auto-revert state on promotion ──
                    // Save the pre-promotion fingerprint and current PnL so
                    // we can detect post-promotion deterioration and revert.
                    if post_reload_fp != pre_reload_fp {
                        let st = engine.live_status();
                        let cumulative_pnl = prior_tape_pnl.saturating_add(
                            st.net_realized_lamports as i64
                        );
                        // Snapshot the cumulative trade count at promotion time
                        // so we can compute trades-since-promotion for the
                        // variance-based auto-revert threshold.
                        let cumulative_trades = prior_tape_trades
                            .saturating_add(tape_exporter.total_exported());
                        trades_at_promotion = cumulative_trades;
                        auto_revert_state = AutoRevertState {
                            promoted_fingerprint: post_reload_fp,
                            prior_champion_fingerprint: pre_reload_fp,
                            pnl_at_promotion: cumulative_pnl as i128,
                            ticks_since_promotion: 0,
                            trades_at_promotion: cumulative_trades,
                            reverted: false,
                        };
                        pre_promotion_fingerprint = pre_reload_fp;
                        promotion_tick = tick_counter;
                        write_auto_revert_state(&auto_revert_state);
                        eprintln!(
                            "[pq-daemon] AUTO-REVERT tracking: promoted_fp={:#018x} \
                             prior_fp={:#018x} pnl_at_promotion={}lamports",
                            post_reload_fp, pre_reload_fp, cumulative_pnl
                        );
                    }
                }

                // ── GAP B: auto-revert check ───────────────────────────
                // After the grace period, check if the promoted config is
                // deteriorating PnL. If so, revert to the archived champion.
                if auto_revert_state.promoted_fingerprint != 0
                    && !auto_revert_state.reverted
                {
                    let ticks_since = tick_counter.saturating_sub(promotion_tick);
                    let st = engine.live_status();
                    let cumulative_pnl = prior_tape_pnl.saturating_add(
                        st.net_realized_lamports as i64
                    );
                    // Compute trades-since-promotion for the variance-based
                    // auto-revert threshold.
                    let cumulative_trades = prior_tape_trades
                        .saturating_add(tape_exporter.total_exported());
                    let trades_since = cumulative_trades
                        .saturating_sub(trades_at_promotion);
                    let current_fp = config_fingerprint(&cfg.dump_to_text());
                    if let Some(revert_config_text) = check_auto_revert(
                        current_fp,
                        cumulative_pnl as i128,
                        ticks_since,
                        trades_since,
                    ) {
                        // Revert: parse the archived champion config back
                        eprintln!(
                            "[pq-daemon] AUTO-REVERT: reverting config to archived champion"
                        );
                        if let Ok(reverted_cfg) = pump_quant_app::config::Config::from_str_over_default(
                            &revert_config_text
                        ) {
                            cfg = reverted_cfg;
                            auto_revert_state.reverted = true;
                            write_auto_revert_state(&auto_revert_state);
                            eprintln!(
                                "[pq-daemon] AUTO-REVERT: config restored to fingerprint {:#018x}",
                                pre_promotion_fingerprint
                            );
                        }
                    }
                }
            }

            // ── Autonomous bridge: defense-in-depth (G4) ───────────────
            // Monitor live P&L for cliff veto / circuit breaker / kill switch.
            {
                let st = engine.live_status();
                // Track realized P&L for drawdown.
                defense_state.update_drawdown(
                    st.net_realized_lamports.max(0) as i64
                );
                if !defense_state.trading_allowed() {
                    eprintln!(
                        "[pq-daemon] DEFENSE-IN-DEPTH: TRADING HALTED — reason: {:?}",
                        defense_state.kill_reason()
                    );
                    // Write an EMERGENCY_STOP sentinel so the operator sees it.
                    let _ = std::fs::write(
                        EMERGENCY_STOP_FILE,
                        "defense-in-depth automatic halt",
                    );
                    // Kill LaserStream child if present — tree-kill to prevent
                    // orphaned gRPC processes burning Helius credits. GAP #13.
                    if let Some(ref mut child) = ls_child { kill_process_tree(child); }
                    if let Some(ref mut child) = fc_child { kill_process_tree(child); }
                    return ExitCode::from(EXIT_EMERGENCY);
                }
            }

            // ── Autonomous bridge: periodic refiner spawn (G1) ────────
            // Spawn pq-refiner as a child process to analyze accumulated
            // tape and emit promotion/demotion decisions. The refiner
            // writes CONFIG_PROMOTION.json which we hot-reload above.
            if args.refiner_every_ticks > 0
                && tick_counter - last_refiner_spawn_tick >= args.refiner_every_ticks
            {
                eprintln!(
                    "[pq-daemon] spawning pq-refiner (tick={tick_counter}, tape={TAPE_PATH})"
                );
                // S8: Append reflection state metadata to the champion config
                // dump so the refiner can make reflection-aware decisions.
                let mut config_text = cfg.dump_to_text();
                let snap = engine.reflection_snapshot();
                config_text.push_str(&format!(
                    "\n# S8 reflection_snapshot: tick={} reflect_every_ticks={} brain_reflect_enable={} retired=[{},{},{},{}]\n",
                    snap.tick,
                    snap.reflect_every_ticks,
                    snap.brain_reflect_enable,
                    snap.retired[0], snap.retired[1], snap.retired[2], snap.retired[3],
                ));
                match refiner_spawner.spawn(tick_counter, &config_text) {
                    Ok(pid) => eprintln!(
                        "[pq-daemon] pq-refiner spawned: pid={pid}"
                    ),
                    Err(e) => eprintln!(
                        "[pq-daemon] pq-refiner spawn FAILED: {e}"
                    ),
                }
                last_refiner_spawn_tick = tick_counter;
            }
        }

        if !did_work {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    // ─── === END OF PERSISTENT LOOP === ──────────────────────────────────

    // ─── Graceful shutdown ──────────────────────────────────────────────
    eprintln!("[pq-daemon] === GRACEFUL SHUTDOWN ===");

    // Kill LaserStream child — tree-kill to prevent orphaned gRPC processes.
    // GAP #13: bare child.kill() leaves wsl.exe grandchildren alive in WSL2,
    // still connected to Helius gRPC, burning credits into a dead pipe.
    if let Some(ref mut child) = ls_child {
        kill_process_tree(child);
        eprintln!("[pq-daemon] LaserStream child terminated (process tree killed)");
    }

    // Kill Firecrawl bridge child — same tree-kill logic.
    if let Some(ref mut child) = fc_child {
        kill_process_tree(child);
        eprintln!("[pq-daemon] Firecrawl bridge terminated (process tree killed)");
    }

    // Drain remaining queue
    while let Some((provenanced, dwell)) = queue.pop_with_dwell() {
        engine.tick(provenanced.event);
        stats.junction_events_drained += 1;
        dwell_samples.push(dwell.as_millis() as u64);
    }

    // Compute dwell stats
    if !dwell_samples.is_empty() {
        dwell_samples.sort();
        let n = dwell_samples.len() as u64;
        stats.dwell_max_ms = *dwell_samples.last().unwrap();
        let sum: u64 = dwell_samples.iter().sum();
        stats.dwell_mean_ms = sum / n;
        let p99_idx = ((n as f64 * 0.99).ceil() as u64).saturating_sub(1).min(n - 1);
        stats.dwell_p99_ms = dwell_samples[p99_idx as usize];
    }

    // Final status write
    let st = engine.live_status();
    let _ = st.write_to_path(status_path);

    // Final cumulative PnL write — ensures cumulative_pnl.json reflects
    // this session's final realized PnL before shutdown.
    let _ = write_cumulative_pnl(
        CUMULATIVE_PNL_PATH,
        cfg_fp,
        &args.strategy_label,
        st.net_realized_lamports,
        prior_tape_pnl,
        prior_tape_trades,
        st.admitted,
        st.info_time_tick,
    );

    // Final brain snapshot
    match engine.snapshot_brain() {
        Ok(()) => eprintln!("[pq-daemon] final brain snapshot saved"),
        Err(e) => eprintln!("[pq-daemon] final brain snapshot FAILED: {e}"),
    }

    // Final tape flush — drain any remaining trades to disk
    let trades = engine.take_tape_trades();
    for t in &trades {
        let _lane = if t.scalp { TapeLane::Scalp } else { TapeLane::Early };
        let net = t.gross as i64 - t.fees as i64 - t.tips as i64 - t.failed as i64;
            let mint_b58 = Pubkey::try_from(t.mint)
            .map(|pk| pk.to_string())
            .unwrap_or_else(|_| hex_short(&t.mint));
        // Feed final trades to memory bank too
        let trade_lane = if t.scalp { TradeLane::Scalp } else { TradeLane::Early };
        let rec = TradeRecord {
            slot: last_slot_seen,
            mint_b58: mint_b58.clone(),
            side: TradeSide::Buy,
            entry_price_fp: t.entry_price_fp as i128,
            exit_price_fp: t.exit_price_fp as i128,
            size_lamports: t.size_lamports,
            strategy_id: t.archetype as u64,
            source: if t.scalp { ProvenanceSource::HeliusAccountSubscribe }
                    else { ProvenanceSource::PumpPortalTrade },
            outcome: if net >= 0 { TradeOutcome::Filled }
                     else { TradeOutcome::FilledWithSlippage },
            realized_pnl_lamports: net,
            fees_lamports: (t.fees + t.tips) as u64,
            slippage_lamports: t.failed as u64,
            decision_latency_us: 0,
            confirm_latency_us: 0,
            run_mode: JournalRunMode::Paper,
            error_code: t.exit_reason_code as u32,
            seq: 0,
            lane: Some(trade_lane),
            mfe_bps: t.mfe_bps,
            mae_bps: t.mae_bps,
            };
            memory_bank.ingest(&rec);
        tape_exporter.push(TapeRecord::TradeFull {
            slot: last_slot_seen,
            mint_b58,
            side_tag: "buy",
            entry_price_fp: t.entry_price_fp as i128,
            exit_price_fp: t.exit_price_fp as i128,
            size_lamports: t.size_lamports,
            strategy_id: t.archetype as u64,
            source_tag: if t.scalp { "scalp" } else { "early" },
            outcome_tag: if net >= 0 { "profit" } else { "loss" },
            realized_pnl_lamports: net,
            fees_lamports: (t.fees + t.tips) as u64,
            slippage_lamports: t.failed as u64,
            decision_latency_us: 0,
            confirm_latency_us: 0,
            run_mode_tag: "paper",
            error_code: t.exit_reason_code as u32,
            seq: 0,
            mfe_bps: t.mfe_bps,
            mae_bps: t.mae_bps,
        });
    }
    match tape_exporter.flush() {
        Ok(n) => eprintln!(
            "[pq-daemon] final tape flush: {n} records (total={})",
            tape_exporter.total_exported()
        ),
        Err(e) => eprintln!("[pq-daemon] final tape flush FAILED: {e}"),
    }

    // ─── Session history append (A/B testing ledger) ──────────────────
    // One line per daemon run, appended to session_history.jsonl. This is
    // the strategy-comparison ledger: each session's final stats tagged
    // with config fingerprint + strategy label so we can compare which
    // config set produces the best net SOL over time.
    let uptime_secs = session_start.elapsed().as_secs();
    let _ = append_session_history(
        SESSION_HISTORY_PATH,
        cfg_fp,
        &args.strategy_label,
        st.net_realized_lamports,
        prior_tape_pnl,
        prior_tape_trades,
        st.admitted,
        st.info_time_tick,
        uptime_secs,
        tape_exporter.total_exported(),
        prior_tape_trades.saturating_add(tape_exporter.total_exported()),
        true, // final = true on graceful shutdown
        session_id,
    );

    // Final memory bank export — flush learning summaries to disk
    let mb_json = memory_bank.global_json();
    match std::fs::write(memory_bank_path, &mb_json) {
        Ok(_) => eprintln!(
            "[pq-daemon] final memory bank export: trades={} net={}lamports",
            memory_bank.global_summary().total_trades,
            memory_bank.global_summary().net_lamports
        ),
        Err(e) => eprintln!("[pq-daemon] final memory bank export FAILED: {e}"),
    }

    // G3 fix: final creator ledger persistence on graceful shutdown.
    // Ensures the accumulated creator track record survives to the next session.
    {
        let bytes = engine.snapshot_creator_ledger();
        match std::fs::write(LEDGER_PATH, &bytes) {
            Ok(_) => eprintln!(
                "[pq-daemon] final creator ledger save: {} entries, {} bytes",
                engine.measured().creator_ledger_len(),
                bytes.len()
            ),
            Err(e) => eprintln!("[pq-daemon] final creator ledger save FAILED: {e}"),
        }
    }

    // Final event stream flush
    if let Some(ref mut writer) = event_stream_writer {
        let _ = writer.flush();
        eprintln!(
            "[pq-daemon] event stream: {} events captured",
            writer.events_written()
        );
    }

    // Pin open positions BEFORE report() force-closes them
    let open_positions = engine.open_positions_snapshot();
    let report = engine.report();
    stats.junction_overflow_dropped = queue.overflow_stats().dropped;

    // ─── Final report ────────────────────────────────────────────────────
    println!("=== PQ-DAEMON SHUTDOWN REPORT ===");
    println!("mode:                Paper (daemon)");
    println!("ticks:               {tick_counter}");
    println!();
    println!("-- PumpPortal --");
    println!("  trades_received:       {}", stats.pp_trades_received);
    println!("  trades_enqueued:       {}", stats.pp_trades_enqueued);
    println!("  creates_received:      {}", stats.pp_creates_received);
    println!("  creates_parsed:        {}", stats.pp_creates_parsed);
    println!("  reconnects:            {}", stats.pp_reconnects);
    println!();
    println!("-- Helius --");
    println!("  slot_notifications:        {}", stats.helius_slot_notifications);
    println!("  account_notifications:     {}", stats.helius_account_notifications);
    println!("  onchain_confirms_decoded:  {}", stats.helius_onchain_confirms_decoded);
    println!("  reconnects:                {}", stats.helius_reconnects);
    println!();
    println!("-- LaserStream gRPC --");
    println!("  transactions_received:  {}", stats.ls_transactions_received);
    println!("  events_emitted:         {}", stats.ls_events_emitted);
    println!("  slots_received:         {}", stats.ls_slots_received);
    println!("  reconnects:             {}", stats.ls_reconnects);
    println!();
    println!("-- Firecrawl web intelligence --");
    println!("  bridge_spawned:        {}", stats.fc_spawned);
    println!("  triggers_emitted:      {}", stats.fc_triggers_emitted);
    println!("  events_ingested:       {}", stats.fc_events_ingested);
    println!();
    println!("-- Junction queue --");
    println!("  events_drained:        {}", stats.junction_events_drained);
    println!("  overflow_dropped:      {}", stats.junction_overflow_dropped);
    println!("  dwell_max_ms:          {}", stats.dwell_max_ms);
    println!("  dwell_mean_ms:         {}", stats.dwell_mean_ms);
    println!("  dwell_p99_ms:          {}", stats.dwell_p99_ms);
    println!();
    println!("-- Engine --");
    println!("  ticks:                 {}", report.ticks);
    println!("  promoted:              {}", report.promoted);
    println!("  admitted:              {}", report.admitted);
    println!("  rejected:              {}", report.rejected);
    println!("  net_lamports:          {}", report.net_lamports);
    println!("  journal_digest:        {:#018x}", report.journal_digest);
    println!();
    if open_positions.is_empty() {
        println!("  (no open positions at shutdown)");
    } else {
        println!("-- Open positions at shutdown --");
        for pos in &open_positions {
            let entry_sol = pos.entry_price_fp as f64 / 1e18;
            let pnl_sol = pos.unrealized_pnl_lamports as f64 / 1e9;
            println!("  mint={} entry={:.6} unrealized_pnl={:.6} remaining={}bps",
                Pubkey::from(pos.mint).to_string(),
                entry_sol, pnl_sol, pos.remaining_bps);
        }
    }
    println!();
    println!("-- Errors --");
    println!("  ws_errors:             {}", stats.ws_errors);
    println!();
    println!("[pq-daemon] shutdown complete — exit 0");

    ExitCode::SUCCESS
}
