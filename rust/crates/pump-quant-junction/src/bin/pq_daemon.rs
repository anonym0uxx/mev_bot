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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use pq_stream_capture::helius_ws;
use pq_stream_capture::json::{self, Value};
use pq_stream_capture::pumpportal_ws;
use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_core::config::Creds;
use pump_quant_domain::ids::Mint;
use pump_quant_junction::autonomous_bridge::{
    check_auto_revert, try_reload_config, write_auto_revert_state, AutoRevertState, DefenseState,
};
use pump_quant_junction::decode::decode_onchain_confirm_with_curve;
use pump_quant_junction::event_stream::EventStreamWriter;
use pump_quant_junction::laserstream::{
    classify_pump_instructions, instructions_to_events_with_meta, parse_ndjson_line,
    LaserStreamState, LaserStreamUpdate,
};
use pump_quant_junction::memory_bank::{MemoryBank, MemoryBankConfig};
use pump_quant_junction::pumpportal::{
    handle_create_payload, handle_migration_payload, handle_trade_payload,
};
use pump_quant_junction::queue::BoundedJunctionQueue;
use pump_quant_junction::reserve_delta::{derive_market_trade_from_delta, ReserveSnapshot};
use pump_quant_junction::trade_join::{JoinOutcome, TradeJoin};

/// How many `(mint, slot)` keys the instruction-identity table holds before it drops the
/// oldest, and how far behind the newest slot a key survives. Bounded (§99): an instruction
/// whose reserve print never arrives must not accumulate.
const TRADE_JOIN_CAP: usize = 4_096;
const TRADE_JOIN_HORIZON_SLOTS: u64 = 8;
use pump_quant_junction::tape_export::{TapeExporter, TapeLane, TapeRecord};
use pump_quant_junction::trade_journal::RunMode as JournalRunMode;
use pump_quant_junction::trade_journal::{TradeLane, TradeOutcome, TradeRecord, TradeSide};
use pump_quant_junction::ProvenanceSource;
use pump_quant_junction::ProvenancedEvent;
use pump_quant_protocol::decode::decode_pump_curve_tail;
use std::io::Write; // needed for fc_child stdin.write_all()

// ── R-3 creator on-chain history veto ─────────────────────────────────────
/// Shared map from mint pubkey → creator wallet pubkey, populated on PumpPortal
/// create events and consulted by the R-3 veto sink before buy submission.
type CreatorPubkeyMap = std::sync::Arc<std::sync::Mutex<HashMap<[u8; 32], [u8; 32]>>>;

use pump_quant_execution::ex_creator_history_veto::{
    CreatorHistoryRpc, CreatorHistoryVetoSink, CreatorPubkeyLookup,
};

// ── R-3 daemon-side trait implementations ─────────────────────────────────

/// `CreatorPubkeyLookup` impl backed by the shared `CreatorPubkeyMap`.
/// Populated on PumpPortal create events; consulted by the veto sink on buy.
struct DaemonCreatorLookup {
    map: CreatorPubkeyMap,
}

impl CreatorPubkeyLookup for DaemonCreatorLookup {
    fn lookup_creator_pubkey(&self, mint: &[u8; 32]) -> Option<[u8; 32]> {
        self.map
            .lock()
            .ok()
            .and_then(|guard| guard.get(mint).copied())
    }
}

/// `CreatorHistoryRpc` impl using the Helius JSON-RPC endpoint via
/// `pq_stream_capture::http::Http::post_json_once`. Queries
/// `getSignaturesForAddress` on the creator wallet and counts the returned
/// signatures. Fail-open: any RPC error → `None` (the veto sink treats `None`
/// as "no data → pass through", §6.4).
struct DaemonCreatorHistoryRpc {
    rpc_url: String,
}

impl CreatorHistoryRpc for DaemonCreatorHistoryRpc {
    fn query_signature_count(&self, creator_pubkey: &[u8; 32]) -> Option<u32> {
        let creator_b58 = Pubkey::from(*creator_pubkey).to_string();

        // Request up to 1000 recent signatures (the RPC max per call).
        // Each pump.fun mint creation produces ~1-3 signatures for the creator
        // (create instruction + optional metadata + initial buy). A creator
        // with 50+ mints will have 150+ signatures, well above the 1000 limit
        // for heavy serial ruggers. We count the returned array length.
        let body = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"getSignaturesForAddress","params":["{creator_b58}",{{"limit":1000,"commitment":"confirmed"}}]}}"#
        );

        let http = pq_stream_capture::http::Http::new(10);
        let body_str = http.post_json_once(&self.rpc_url, &body).ok()?;
        let parsed: Value = pq_stream_capture::json::parse(&body_str).ok()?;
        let result = parsed.get("result")?;
        // The result is an array of signature objects. Count its length.
        let arr = result.as_array()?;
        Some(arr.len() as u32)
    }
}
use pq_stream_capture::ws::{WsConn, WsEvent};
use pump_quant_ingest::social_source::{RawSocialPayload, SocialSource};
use solana_program::pubkey::Pubkey;

// ─── Wangr Rev-14: UTC time helper (no chrono dependency) ──────────────────
/// Compute (day_of_week, hour_utc) from `SystemTime::now()`.
/// Returns (0..=6, 0..=23) where dow 0=Sunday, 1=Monday, … 6=Saturday.
/// Uses the Tomohiko Yamamoto algorithm for day-of-week from unix epoch days.
fn utc_dow_hour(now: SystemTime) -> (u8, u8) {
    let secs: i64 = match now.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(_) => 0,
    };
    let hour_utc = ((secs / 3600) % 24) as u8;
    let days = secs / 86400; // days since 1970-01-01 (Thursday)
                             // 1970-01-01 was a Thursday (dow=4). (days + 4) % 7 gives 0=Sunday..6=Saturday.
    let dow = ((days + 4) % 7) as u8;
    (dow, hour_utc)
}

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
/// subs per connection. Raised from 256 to 850 (Phase 3) — the 256-slot cap
/// was causing 526 mints to be evicted before their median-37.6s OnchainConfirm
/// arrived, producing a 79.6% NoNumericConfirmation reject rate. At 850 subs
/// the eviction rate drops sharply: more mints hold their slot long enough for
/// Helius to return the account data, filling `liquidity_lamports`. We leave
/// 150 slots of headroom below the 1000 cap for the slot subscription and
/// re-subscribe churn during reconnects.
const MAX_ACCOUNT_SUBS: usize = 850;
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
/// recovery. 95% gives us 50 slots of headroom (850 local + ~100 leaked
/// before we trigger at 950). The threshold accounts for the slot subscription
/// (1 slot) and re-subscribe churn during reconnects. The tighter headroom is
/// acceptable because the leak-fix (GAP #7) keeps leaked-sub accumulation
/// low and the stagnation detector catches death-spiral symptoms within 120s.
const SUB_CAP_RECONNECT_THRESHOLD: f64 = 0.95;

/// E3: how many submissions ONE outbound lane may have backlogged before the decision
/// thread refuses further ones. Refusal reverses the position (safe direction) and is
/// counted, so a saturated lane is visible rather than silent.
const OUTBOUND_QUEUE_DEPTH: usize = 32;

/// E3: submission lanes. Records are routed to a lane by MINT, so a position's own
/// buy/sell stream stays FIFO while different positions submit in parallel — ten
/// positions are ten independent streams, not one queue.
const OUTBOUND_LANES: usize = pump_quant_junction::async_sink::DEFAULT_LANES;

/// OnchainConfirm stagnation detection: if confirms don't advance for this
/// many seconds while the daemon is running and LS is healthy, the Helius WS
/// lane is dead (likely subscription cap death-spiral). This is the
/// last-resort trigger — even if -32006 detection and leak-count heuristics
/// fail, this catches the symptom (frozen confirms) and forces a reconnect.
/// 120s is conservative — the median OnchainConfirm latency is 37.6s, so
/// 120s of silence means 3x the median with zero confirms = definitely dead.
#[allow(dead_code)] // unimplemented OnchainConfirm stagnation detector: last_confirm_tick is
                    // tracked (2718/3331/3437) but the `tick - last_confirm_tick > ONCHAIN_STAGNATION_SECS`
                    // comparison is not yet wired. Kept as the policy anchor; see N2 report (production dead code).
const ONCHAIN_STAGNATION_SECS: u64 = 120;

/// WS read timeout in millis. Tightened from tick_period_ms (250ms) to prevent
/// blocking on degraded connections. Each poll returns within 100ms max,
/// ensuring the tick loop advances even when both WS lanes are silent.
const WS_READ_TIMEOUT_MS: u64 = 100;
/// Wall-clock status heartbeat interval in seconds. live_status.json is
/// refreshed at most this often, decoupling health reporting from event
/// throughput so the watchdog never kills a healthy-but-starved daemon.
const STATUS_HEARTBEAT_SECS: u64 = 15;
/// Upper bound on the curve-PDA -> mint identity map (memory bound; overflow is counted).
const PDA_MAP_CAP: usize = 500_000;
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
/// `--live` together with a model endpoint: refused (the model lane is paper-only).
const EXIT_MODEL_LIVE_CONFLICT: u8 = 98;
/// A held-state ledger exists but cannot be applied: refusing to start rather than orphan exposure.
const EXIT_HELD_STATE_REFUSED: u8 = 97;
/// `PQ_FLOW_RESUME_MS` was set outside a declared offline paper replay, or its value is invalid.
const EXIT_RESUME_CLOCK_REFUSED: u8 = 96;
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
    /// Live mode: wire real Solana execution (sign, build, submit).
    /// Requires PQ_CREDS_FILE with HELIUS_WS_URL, keypair at
    /// ~/.hermes/keys/wallet-keypair.json, and the wallet address.
    /// When false, the daemon runs in Paper mode (default, safe).
    live_mode: bool,
    /// Wallet address (base58) for live mode. Required if live_mode=true.
    /// Used to bind the keypair and reconcile the on-chain balance.
    wallet_address: String,
}

/// OFFLINE PAPER REPLAY BARRIERS ONLY. Next LaserStream update, except that an update whose wire receive time is at or
/// past the current barrier clock is HELD (and `ready` is raised) so the engine has applied exactly the prefix before
/// that source-time clock. With no barrier left the held update is released and delivery is the plain `try_recv`.
fn next_ls_update(
    rx: &mpsc::Receiver<LaserStreamUpdate>,
    hold: &mut Option<LaserStreamUpdate>,
    limit: Option<i64>,
    ready: &mut bool,
) -> Result<LaserStreamUpdate, mpsc::TryRecvError> {
    let Some(limit) = limit else {
        if let Some(h) = hold.take() {
            return Ok(h);
        }
        return rx.try_recv();
    };
    if *ready {
        return Err(mpsc::TryRecvError::Empty);
    }
    let item = match hold.take() {
        Some(h) => h,
        None => rx.try_recv()?,
    };
    let ms = match &item {
        LaserStreamUpdate::Transaction(tx) => tx.recv_unix_ms,
        LaserStreamUpdate::Account { recv_unix_ms, .. } => *recv_unix_ms,
        _ => None,
    };
    match ms {
        Some(m) if m >= limit => {
            *hold = Some(item);
            *ready = true;
            Err(mpsc::TryRecvError::Empty)
        }
        _ => Ok(item),
    }
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
        live_mode: false,
        wallet_address: String::new(),
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
                if !matches!(
                    a.commitment.as_str(),
                    "processed" | "confirmed" | "finalized"
                ) {
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
            "--live" | "--live-mode" => {
                a.live_mode = true;
                i += 1;
            }
            "--wallet-address" if i + 1 < args.len() => {
                a.wallet_address = String::from(&args[i + 1]);
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }
    // A model endpoint combined with --live is an INCOMPATIBLE configuration: the model lane is paper-only.
    // Silently ignoring PQ_MODEL_ENDPOINT would start LEGACY live trading the operator did not ask for.
    // Checked HERE - the first thing the process does - so nothing (credentials, wallet, feeds) is touched.
    if a.live_mode {
        if let Ok(v) = std::env::var("PQ_MODEL_ENDPOINT") {
            if !v.is_empty() {
                eprintln!(
                    "[pq-daemon] FATAL: --live is incompatible with PQ_MODEL_ENDPOINT ({v}). The model lane is paper-only; \
                     refusing to start rather than silently ignore the model and run legacy live trading. \
                     Unset PQ_MODEL_ENDPOINT or drop --live."
                );
                return Err(EXIT_MODEL_LIVE_CONFLICT);
            }
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
            let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            return num_str.parse().ok();
        }
        _ => return None,
    };
    let num_str: String = chars.take_while(|c| c.is_ascii_digit()).collect();
    if num_str.is_empty() {
        return None;
    }
    num_str
        .parse::<i64>()
        .ok()
        .map(|v| if negative { -v } else { v })
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
#[allow(clippy::too_many_arguments)] // 8 args: report fields, one call site; a struct would obscure the schema order
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
#[allow(clippy::too_many_arguments)] // 13 args: history-row fields, one call site; a struct would obscure the schema order
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
    for &n in &b[..4] {
        s.push_str(&format!("{n:02x}"));
    }
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
    #[cfg(windows)]
    {
        let pid = child.id();
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
    delta_out_of_range: u64,
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
    /// Rev-30: LS account notifications received (bonding curve PDA snapshots).
    ls_account_received: u64,
    /// Rev-30: LS account notifications decoded into OnchainConfirm events.
    ls_onchain_confirms_decoded: u64,
    /// Rev-30: LS account updates where PDA couldn't be resolved to a mint.
    ls_account_unresolved: u64,
    pda_installed_from_tx: u64,
    pda_map_full_refused: u64,
    fc_spawned: bool,
    fc_triggers_emitted: u64,
    fc_events_ingested: u64,
    stubbed_or_assumed: Vec<String>,
}

impl SessionStats {
    fn new() -> Self {
        Self {
            pp_trades_received: 0,
            pp_trades_enqueued: 0,
            pp_creates_received: 0,
            pp_creates_parsed: 0,
            pp_migrations_received: 0,
            pp_migrations_parsed: 0,
            pp_trade_subs_sent: 0,
            helius_account_notifications: 0,
            helius_onchain_confirms_decoded: 0,
            helius_slot_notifications: 0,
            account_subs_active: 0,
            account_subs_total_attempted: 0,
            account_subs_evicted: 0,
            subs_leaked_no_ack: 0,
            sub_cap_errors: 0,
            last_confirm_tick: 0,
            delta_trades_derived: 0,
            delta_no_trade: 0,
            delta_out_of_range: 0,
            pdas_derived: 0,
            pda_venue_matches: 0,
            pda_venue_present: 0,
            junction_events_drained: 0,
            junction_overflow_dropped: 0,
            dwell_max_ms: 0,
            dwell_mean_ms: 0,
            dwell_p99_ms: 0,
            pp_reconnects: 0,
            helius_reconnects: 0,
            ws_errors: 0,
            ls_transactions_received: 0,
            ls_instructions_classified: 0,
            ls_events_emitted: 0,
            ls_slots_received: 0,
            ls_spawned: false,
            ls_reconnects: 0,
            ls_account_received: 0,
            ls_onchain_confirms_decoded: 0,
            ls_account_unresolved: 0,
            pda_installed_from_tx: 0,
            pda_map_full_refused: 0,
            fc_spawned: false,
            fc_triggers_emitted: 0,
            fc_events_ingested: 0,
            stubbed_or_assumed: vec![
                "Config: dev_portable (no live config file provided)".to_string()
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
    #[allow(dead_code)] // convenience wrapper over evict_oldest_protecting; referenced by the
                        // doc link below, not yet called. Kept; see N2 report (production dead code).
    fn evict_oldest(&mut self) -> Option<(u64, [u8; 32], Option<u64>)> {
        self.evict_oldest_protecting(&std::collections::HashSet::new())
    }

    /// Like [`evict_oldest`](Self::evict_oldest) but NEVER evicts a mint in `protected` (held positions):
    /// their reserve feed must not be sacrificed to new-opportunity discovery. Returns `None` when every
    /// subscription is protected (the caller then declines the NEW subscription instead).
    fn evict_oldest_protecting(
        &mut self,
        protected: &std::collections::HashSet<[u8; 32]>,
    ) -> Option<(u64, [u8; 32], Option<u64>)> {
        if self.subscription_order.is_empty() {
            return None;
        }
        // Tier 1: find the index of the oldest subscription with no trades that is not protected.
        let dormant_idx = self
            .subscription_order
            .iter()
            .position(|(_, m, has_trades)| !*has_trades && !protected.contains(m));
        let fallback_idx = self
            .subscription_order
            .iter()
            .position(|(_, m, _)| !protected.contains(m));
        let idx = dormant_idx.or(fallback_idx)?;
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

    /// Clear ALL tracker state (used on reconnect — the server assigns
    /// new sub IDs on a fresh connection, so stale mappings must go).
    /// Returns the list of mints that were actively subscribed, so the
    /// caller can re-subscribe them on the new connection without
    /// duplicating entries in `subscription_order` / `req_to_mint`.
    ///
    /// BUG FIX (death-spiral): The previous version only cleared
    /// `req_to_server_sub` / `server_sub_to_mint` but kept
    /// `subscription_order` and `req_to_mint` intact. The reconnect
    /// code then called `active_mints()` (which reads `subscription_order`)
    /// and re-subscribed each mint via `record_request()` — pushing
    /// NEW entries into `subscription_order` on top of the old ones.
    /// This doubled `server_visible_count()` on every reconnect cycle,
    /// causing the pending-acks to grow exponentially (1700 → 3401 →
    /// 6801 → 13601 …) until the daemon could no longer write status
    /// files and the watchdog killed it. The fix is to snapshot the
    /// active mints, then nuke ALL four maps so the re-subscribe loop
    /// starts from a clean slate.
    fn clear_server_subs(&mut self) -> Vec<[u8; 32]> {
        // Snapshot the mints we need to re-subscribe on the new connection.
        let mints: Vec<[u8; 32]> = self
            .subscription_order
            .iter()
            .map(|(_, mint, _)| *mint)
            .collect();
        self.req_to_mint.clear();
        self.req_to_server_sub.clear();
        self.server_sub_to_mint.clear();
        self.subscription_order.clear();
        mints
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

// ─── Wallet balance reconciliation (live mode) ──────────────────────────────

/// Extract the Helius RPC URL from creds file or env vars.
/// Used by the confirmation poller in main() which doesn't have access to
/// the `construct_live_engine` scope.
fn extract_helius_rpc_url(_args: &DaemonArgs) -> String {
    let mut rpc_url = String::new();
    let mut api_key = String::new();

    // Try creds file first
    let creds_path = std::env::var("PQ_CREDS_FILE")
        .unwrap_or_else(|_| "C:/Users/Alon/.hermes/creds/pump-quant.env".to_string());
    if let Ok(creds) = std::fs::read_to_string(&creds_path) {
        for line in creds.lines() {
            if let Some(v) = line.strip_prefix("HELIUS_WS_URL=") {
                if let Some(rest) = v.strip_prefix("wss://") {
                    rpc_url = format!("https://{rest}");
                } else {
                    rpc_url = v.to_string();
                }
            }
            if let Some(v) = line.strip_prefix("HELIUS_API_KEY=") {
                api_key = v.trim().to_string();
            }
        }
    }

    // Fall back to env vars
    if rpc_url.is_empty() {
        if let Ok(v) = std::env::var("HELIUS_WS_URL") {
            if let Some(rest) = v.strip_prefix("wss://") {
                rpc_url = format!("https://{rest}");
            } else {
                rpc_url = v;
            }
        }
    }
    if api_key.is_empty() {
        if let Ok(v) = std::env::var("HELIUS_API_KEY") {
            api_key = v;
        }
    }

    // Inject api-key
    if !rpc_url.is_empty() && !api_key.is_empty() && !rpc_url.contains("api-key=") {
        rpc_url = format!("{}/?api-key={}", rpc_url, api_key);
    }

    rpc_url
}

/// Query the wallet's SOL balance via RPC `getAccountInfo`.
/// Returns the balance in lamports. Fail-closed: any error → early exit.
fn reconcile_wallet_balance(rpc_url: &str, wallet_address: &str) -> Result<u64, String> {
    // Build the JSON-RPC request for getAccountInfo.
    let body = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"getAccountInfo","params":["{wallet_address}",{{"encoding":"base64","commitment":"confirmed"}}]}}"#
    );

    let http = pq_stream_capture::http::Http::new(15);
    let body_str = http
        .post_json_once(rpc_url, &body)
        .map_err(|e| format!("RPC request failed: {e}"))?;

    let json: Value =
        pq_stream_capture::json::parse(&body_str).map_err(|e| format!("JSON parse failed: {e}"))?;

    // Navigate: result.value.lamports. If value is null, the account doesn't
    // exist → balance = 0 (valid on-chain state, not an error).
    let value = json
        .get("result")
        .and_then(|r| r.get("value"))
        .ok_or_else(|| "malformed RPC response: no result.value".to_string())?;

    // The hand-rolled JSON Value type doesn't have is_null; check via the
    // variant. A null value is represented as Value::Null.
    if matches!(value, Value::Null) {
        return Ok(0);
    }

    let lamports = value
        .get("lamports")
        .and_then(Value::as_u64)
        .ok_or_else(|| "malformed RPC response: no lamports field".to_string())?;

    Ok(lamports)
}

// ─── Rev-19 on-chain confirmation feedback ─────────────────────────────────
//
// The daemon polls `getSignaturesForAddress` for our own wallet to determine
// whether pending buy/sell txs landed on-chain. The results are matched against
// the engine's `pending_buys` / `pending_sells` maps and fed back as
// `AppEvent::OurBuyConfirmed`, `OurBuyFailed`, `OurSellConfirmed`, or
// `OurSellFailed` — closing the architecture gap where paper PnL was recorded
// as live without verifying on-chain outcome.

/// Decode a base58 string into a 64-byte ed25519 signature.
/// Uses the same long-division algorithm as `decode_base58_32` but with a
/// 64-byte output buffer (signatures are 64 bytes, pubkeys are 32).
fn decode_base58_64(s: &str) -> Option<[u8; 64]> {
    const B58: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    if s.is_empty() {
        return None;
    }
    let mut out = [0u8; 64];
    for c in s.bytes() {
        let digit = B58.iter().position(|&a| a == c)? as u32;
        let mut carry = digit;
        for byte in out.iter_mut().rev() {
            let v = u32::from(*byte) * 58 + carry;
            *byte = (v & 0xff) as u8;
            carry = v >> 8;
        }
        if carry != 0 {
            return None;
        }
    }
    let leading_ones = s.bytes().take_while(|&c| c == b'1').count();
    let leading_zeros = out.iter().take_while(|&&b| b == 0).count();
    if leading_ones != leading_zeros {
        return None;
    }
    Some(out)
}

/// A single on-chain confirmation result for a pending tx.
#[derive(Debug)]
struct OnchainConfirmResult {
    /// The signature that was confirmed or failed.
    signature: [u8; 64],
    /// True if the tx landed in a block (confirmed). False if the tx failed
    /// (e.g. simulation error, insufficient funds, slippage).
    confirmed: bool,
    /// Slot at which the tx was confirmed (0 if failed/unavailable).
    slot: u64,
    /// Memo for logging — e.g. "buy" or "sell".
    kind: &'static str,
}

/// Poll `getSignaturesForAddress` for our wallet and match against the engine's
/// pending buy/sell signatures. Returns confirmation results for each matched
/// signature. Unmatched signatures are NOT returned (they may still be pending).
///
/// This function is best-effort: any RPC error returns an empty vec, and the
/// daemon continues trading. The next poll cycle will retry.
fn poll_signature_confirmations(
    rpc_url: &str,
    wallet_address: &str,
    pending_buys: &[([u8; 32], [u8; 64])],
    pending_sells: &[([u8; 32], [u8; 64])],
) -> Vec<OnchainConfirmResult> {
    let total_pending = pending_buys.len() + pending_sells.len();
    if total_pending == 0 {
        return Vec::new();
    }

    // Request up to 1000 recent signatures for our wallet. Solana RPC max is 1000.
    // Rev-31 (2026-08-21): CRITICAL FIX — the old limit of 25 was too small under
    // high throughput. Failed buy txs scrolled past the 25-sig window before the
    // 5-second poll caught them, leaving permanent phantom positions that the exit
    // ladder tried to sell → 48 on-chain 3012 failures. 1000 gives a ~80-minute
    // window at 3 txs/sec, far exceeding the 30-second stale-eviction threshold.
    let body = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"getSignaturesForAddress","params":["{wallet_address}",{{"limit":1000,"commitment":"confirmed"}}]}}"#
    );

    let http = pq_stream_capture::http::Http::new(15);
    let body_str = match http.post_json_once(rpc_url, &body) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[pq-daemon] getSignaturesForAddress failed: {e}");
            return Vec::new();
        }
    };

    let json: Value = match pq_stream_capture::json::parse(&body_str) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[pq-daemon] getSignaturesForAddress JSON parse error: {e}");
            return Vec::new();
        }
    };

    let result_arr = match json.get("result").and_then(|r| r.as_array()) {
        Some(arr) => arr,
        None => {
            eprintln!("[pq-daemon] getSignaturesForAddress: no result array");
            return Vec::new();
        }
    };

    // Build a lookup of our pending signatures → (kind, mint) for fast matching.
    // We compare by signature bytes since each tx has a unique signature.
    use std::collections::BTreeMap;
    let mut sig_lookup: BTreeMap<[u8; 64], ([u8; 32], &'static str)> = BTreeMap::new();
    for (mint, sig) in pending_buys {
        sig_lookup.insert(*sig, (*mint, "buy"));
    }
    for (mint, sig) in pending_sells {
        sig_lookup.insert(*sig, (*mint, "sell"));
    }

    let mut results = Vec::new();
    for entry in result_arr {
        // Each entry is an object like:
        // {"signature":"...","slot":123456,"err":null,"memo":"","confirmationStatus":"confirmed"}
        // or {"signature":"...","slot":0,"err":"InsufficientFunds","memo":"","confirmationStatus":null}
        let sig_str = match entry.get("signature").and_then(|s| s.as_str()) {
            Some(s) => s,
            None => continue,
        };

        let sig_bytes = match decode_base58_64(sig_str) {
            Some(s) => s,
            None => continue, // skip unparseable signatures
        };

        // Check if this signature matches one of our pending txs.
        if let Some((_mint_bytes, kind)) = sig_lookup.remove(&sig_bytes) {
            let err = entry.get("err");
            // err is null → tx succeeded; err is a string/object → tx failed.
            let is_confirmed = matches!(err, Some(Value::Null) | None);
            let slot = entry.get("slot").and_then(|s| s.as_u64()).unwrap_or(0);

            eprintln!(
                "[pq-daemon] ON-CHAIN FEEDBACK: {} tx {:} — {} (slot {})",
                kind,
                &sig_str[..8.min(sig_str.len())],
                if is_confirmed { "CONFIRMED" } else { "FAILED" },
                slot
            );

            results.push(OnchainConfirmResult {
                signature: sig_bytes,
                confirmed: is_confirmed,
                slot,
                kind,
            });

            // Early exit: all pending txs have been resolved.
            if sig_lookup.is_empty() {
                break;
            }
        }
    }

    results
}

// ─── Live engine construction (live mode) ──────────────────────────────────

/// Construct the engine + LiveOutboundSink with real I/O for live trading.
/// Returns `Result<Engine, u8>` so the caller can map the error code to an
/// `ExitCode`. Every failure is fail-closed: the engine is only returned on
/// success.
///
/// R-3: when `r3_creator_history_enable` is true in the config, the sink is
/// wrapped with `CreatorHistoryVetoSink` which queries `getSignaturesForAddress`
/// on the creator wallet (looked up from `creator_pubkey_map`) before forwarding
/// a buy. Fail-open on RPC error or unknown creator (§6.4).
fn construct_live_engine(
    cfg: pump_quant_app::config::Config,
    args: &DaemonArgs,
    creator_pubkey_map: CreatorPubkeyMap,
) -> Result<
    (
        pump_quant_app::engine::Engine,
        &'static pump_quant_junction::async_sink::AsyncOutboundSink,
        std::sync::Arc<pump_quant_junction::live_adapters::RpcLiveStateFetcher>,
    ),
    u8,
> {
    use pump_quant_app::engine::Engine;
    use pump_quant_execution::ex_live_io_traits::{LiveSigner, LiveStateFetcher, LiveSubmitter};
    use pump_quant_execution::ex_live_sink::{LiveOutboundSink, LiveSinkConfig};
    use pump_quant_execution::ex_outbound_sink::OutboundSink;
    use pump_quant_junction::live_adapters::{
        spawn_blockhash_warmer, HeliusSenderSubmitter, LiveWalletSigner, PrefetchConfig,
        RpcLiveStateFetcher,
    };
    use pump_quant_protocol::layout::LayoutRegistry;
    use pump_quant_protocol::tx_build::{ComputePlan, TipPlan};
    use pump_quant_protocol::venue_accounts::FeeTail;
    use std::sync::Arc;

    eprintln!("[pq-daemon] *** LIVE MODE ACTIVATED ***");
    eprintln!("[pq-daemon] Real Solana execution: sign + build + submit");
    eprintln!("[pq-daemon] Wallet: {}", args.wallet_address);

    // Fail-closed: wallet address is required for live mode.
    if args.wallet_address.is_empty() {
        eprintln!("[pq-daemon] FATAL: --wallet-address required for --live mode");
        return Err(1);
    }

    // Load credentials for the RPC URL (Helius).
    let creds_path = std::env::var("PQ_CREDS_FILE").unwrap_or_else(|_| {
        format!(
            "{}/.hermes/creds/pump-quant.env",
            std::env::var("HOME").unwrap_or_else(|_| String::new())
        )
    });
    eprintln!("[pq-daemon] Loading credentials from {creds_path}");

    let creds = std::fs::read_to_string(&creds_path).map_err(|e| {
        eprintln!("[pq-daemon] FATAL: cannot read creds file: {e}");
        1u8
    })?;

    let mut helius_rpc_url = String::new();
    let mut helius_sender_url = String::new();
    let mut helius_api_key = String::new();
    for line in creds.lines() {
        if let Some(v) = line.strip_prefix("HELIUS_WS_URL=") {
            let v = v.trim();
            if let Some(rest) = v.strip_prefix("wss://") {
                helius_rpc_url = format!("https://{rest}");
            } else if v.starts_with("https://") {
                helius_rpc_url = v.to_string();
            }
        }
        if let Some(v) = line.strip_prefix("HELIUS_SENDER_URL=") {
            helius_sender_url = v.trim().to_string();
        }
        if let Some(v) = line.strip_prefix("SENDER_ENDPOINT=") {
            if helius_sender_url.is_empty() {
                helius_sender_url = v.trim().to_string();
            }
        }
        if let Some(v) = line.strip_prefix("HELIUS_API_KEY=") {
            helius_api_key = v.trim().to_string();
        }
    }

    if helius_rpc_url.is_empty() {
        if let Ok(v) = std::env::var("HELIUS_WS_URL") {
            if let Some(rest) = v.strip_prefix("wss://") {
                helius_rpc_url = format!("https://{rest}");
            } else {
                helius_rpc_url = v;
            }
        }
    }

    if helius_api_key.is_empty() {
        if let Ok(v) = std::env::var("HELIUS_API_KEY") {
            helius_api_key = v;
        }
    }

    // Inject api-key into the RPC URL if not already present.
    if !helius_rpc_url.is_empty()
        && !helius_api_key.is_empty()
        && !helius_rpc_url.contains("api-key=")
    {
        helius_rpc_url = format!("{}/?api-key={}", helius_rpc_url, helius_api_key);
    }

    if helius_rpc_url.is_empty() {
        eprintln!("[pq-daemon] FATAL: no HELIUS_WS_URL found in creds or env");
        return Err(1);
    }

    eprintln!(
        "[pq-daemon] RPC URL: {}",
        pq_stream_capture::rpc::redact_url(&helius_rpc_url)
    );

    // ── 1. Reconcile wallet balance via RPC ────────────────────────────
    let wallet_balance_lamports = reconcile_wallet_balance(&helius_rpc_url, &args.wallet_address)
        .map_err(|e| {
        eprintln!("[pq-daemon] FATAL: wallet balance reconciliation failed: {e}");
        1u8
    })?;

    eprintln!(
        "[pq-daemon] Wallet balance reconciled: {} lamports ({} SOL)",
        wallet_balance_lamports,
        wallet_balance_lamports as f64 / 1e9
    );

    // ── 2. Construct engine with LiveReconciled bankroll ──────────────
    let mut eng = Engine::new_live_reconciled(cfg, wallet_balance_lamports);

    // ── 3. Construct the LiveOutboundSink ─────────────────────────────
    let keypair_path = format!(
        "{}/.hermes/keys/wallet-keypair.json",
        std::env::var("HOME").unwrap_or_else(|_| "C:/Users/Alon".to_string())
    );
    eprintln!("[pq-daemon] Loading keypair from {keypair_path}");

    let signer = LiveWalletSigner::load(std::path::Path::new(&keypair_path), &args.wallet_address)
        .map_err(|e| {
            eprintln!("[pq-daemon] FATAL: signer load failed: {e:?}");
            1u8
        })?;

    eprintln!("[pq-daemon] Signer loaded: {}", signer.address());

    let state_fetcher = Arc::new(RpcLiveStateFetcher::new(helius_rpc_url.clone()));
    // C6: keep the blockhash warm in the background so latest_blockhash() never
    // pays a synchronous getLatestBlockhash on the decision hot path. (Multi-mint
    // daemon — warm the global blockhash; curve state stays on-demand.)
    let _prefetch_handle = spawn_blockhash_warmer(state_fetcher.clone(), PrefetchConfig::default());
    eprintln!("[pq-daemon] State fetcher constructed (blockhash warmer started)");

    let sender_url = if helius_sender_url.is_empty() {
        helius_rpc_url.replacen("/rpc", "/rpc/sender", 1)
    } else {
        helius_sender_url
    };

    // SWQOS-only tier: single fast path, minimum tip 5,000 lamports (0.000005 SOL).
    // This is the cost-optimized Sender tier. If trades aren't landing (getting
    // out-competed on contested new pairs), escalate to Sender Max (swqos_only=false)
    // with a 1,000,000 lamport (0.001 SOL) tip for multi-path routing + priority buffer.
    // See: https://www.helius.dev/docs/sending-transactions/sender
    let swqos_only = true;
    let mev_protect = false;
    let submitter = if !helius_api_key.is_empty() {
        HeliusSenderSubmitter::new_with_api_key(
            &sender_url,
            &helius_api_key,
            swqos_only,
            mev_protect,
        )
        .map_err(|e| {
            eprintln!("[pq-daemon] FATAL: submitter construction failed: {e:?}");
            1u8
        })?
    } else {
        HeliusSenderSubmitter::new(&sender_url, swqos_only, mev_protect).map_err(|e| {
            eprintln!("[pq-daemon] FATAL: submitter construction failed: {e:?}");
            1u8
        })?
    };

    eprintln!("[pq-daemon] Submitter constructed (Helius Sender)");

    // ── 4. Build a verified LayoutRegistry ─────────────────────────────
    //
    // §41 parity gate: the registry must hold a VerifiedLayout for every
    // (venue, side, variant) permutation the engine will request.  An
    // empty registry builds nothing — fail-closed by design.
    //
    // The variant dimensions that affect pump.fun bonding-curve layouts:
    //   cashback       — adds user_volume_accumulator on sells (count +1)
    //   token_2022     — changes ATA addresses (pubkey values, NOT count)
    //   non_sol_quote  — not applicable (all pump.fun curves are SOL-quoted)
    //
    // Account counts (verified against mainnet fixtures, 2026-08-17):
    //   Buy:  18 accounts regardless of token_2022/cashback
    //   Sell: 16 accounts (cashback=false), 17 accounts (cashback=true)
    //
    // We register the counts OBSERVED on mainnet (see `layout_fixtures` for the
    // probe that produced them and the real signatures that back them). The
    // registry is fail-closed: a layout this engine builds but never observed on
    // chain is refused rather than waved through.
    let mut registry = LayoutRegistry::new();

    // §41 layouts are populated ONLY from observed mainnet transactions.
    //
    // This block used to hand-roll placeholder signatures (bytes 0 and 31 set,
    // everything else zero) and register BOTH the `FeeTail::None` count and the
    // `BuybackVault` count so the gate would accept whichever the builder
    // emitted. Two things were wrong with that:
    //
    //   1. The 17-account BUY does not exist on chain. A 400-transaction mainnet
    //      probe found `buy` = 18 accounts for every variant; 17 is what
    //      `FeeTail::None` produces, and per Rev-18 a 17-account buy fails
    //      `Custom(6062)` (BuybackFeeRecipientMissing). Registering it meant the
    //      gate waved through a layout the chain rejects.
    //   2. The signatures were fabricated, so "verified" meant nothing.
    //
    // The table in `layout_fixtures` carries the real signature and slot of a
    // transaction that exhibited each count, and `record_verified` now refuses
    // placeholders — so a fabricated fixture can never re-enter.
    pump_quant_junction::layout_fixtures::register_observed_pumpfun_layouts(&mut registry)
        .expect("observed layout fixtures must register");

    // ── 5. Sink config ─────────────────────────────────────────────────
    let compute = ComputePlan {
        unit_limit: 120_000,
        unit_price_micro_lamports: 5_000,
    };
    // Helius Sender tip: required by all Sender tiers. We use SWQOS-only
    // (swqos_only=true) whose minimum is 5,000 lamports (0.000005 SOL).
    // Tip account: 2nyhqdwKcJZR2vcqCyrYsaPVdAnFoJjiksCXJ7hfEYgD (first of 12
    // designated Helius tip accounts, from live endpoint error + docs).
    // See: https://www.helius.dev/docs/sending-transactions/sender-swqos-only
    let tip: Option<TipPlan> = Some(TipPlan {
        to: [
            0x1a, 0xa2, 0xf0, 0x5a, 0x6f, 0x89, 0x50, 0xfc, 0xbf, 0x5d, 0xf9, 0xca, 0x39, 0x48,
            0x1c, 0x6d, 0xf1, 0x33, 0x05, 0xc8, 0xb8, 0x7c, 0x64, 0x4f, 0x4d, 0x8c, 0x6d, 0x82,
            0x0b, 0x37, 0x89, 0xa6,
        ],
        lamports: 5_000, // 0.000005 SOL — SWQOS-only minimum
    });
    let fee_tail = FeeTail::None;

    let sink_config = LiveSinkConfig {
        compute,
        tip,
        fee_tail,
        max_slippage_bps: 500,
        // Rev-36: pass the mcap band config to the live sink for TOCTOU
        // re-validation at execution time. Uses the SAME values the gate
        // uses (from the loaded CHAMPION_CONFIG.txt).
        mcap_band_enable: cfg.mcap_band_enable,
        mcap_band_lo_lamports: cfg.mcap_band_lo_lamports,
        mcap_band_hi_lamports: cfg.mcap_band_hi_lamports,
    };

    // ── 6. Construct the sink and install it ───────────────────────────
    let live_sink = Box::new(LiveOutboundSink::new(
        sink_config,
        Arc::new(registry),
        state_fetcher.clone() as Arc<dyn LiveStateFetcher>,
        Arc::new(signer) as Arc<dyn LiveSigner>,
        Arc::new(submitter) as Arc<dyn LiveSubmitter>,
    ));
    let static_sink: &'static LiveOutboundSink = Box::leak(live_sink);

    // ── R-3: creator on-chain history veto wrapper ────────────────────
    // When r3_creator_history_enable is true, wrap the LiveOutboundSink with
    // CreatorHistoryVetoSink. On buy admits, the wrapper queries
    // getSignaturesForAddress on the creator wallet (looked up from the shared
    // creator_pubkey_map populated on PumpPortal create events) and vetoes if
    // the signature count ≥ r3_creator_max_launches. Fail-open on any error
    // (§6.4): RPC failure or unknown creator → pass through to inner sink.
    let r3_enabled = cfg.r3_creator_history_enable;
    let r3_max_launches = cfg.r3_creator_max_launches;
    // Clone the RPC URL for the R-3 query (already has api-key injected).
    let r3_rpc_url = helius_rpc_url.clone();

    // The veto (when armed) wraps the live sink; the async worker wraps whichever
    // one ends up being the inner pipeline, so the decision thread pays neither the
    // veto's RPC nor the build+sign+submit round trip.
    let inner_sink: &'static dyn OutboundSink = if r3_enabled {
        eprintln!(
            "[pq-daemon] R-3 creator history veto ENABLED (max_launches={})",
            r3_max_launches
        );
        let lookup = DaemonCreatorLookup {
            map: creator_pubkey_map,
        };
        let rpc_impl = DaemonCreatorHistoryRpc {
            rpc_url: r3_rpc_url,
        };
        let veto_sink = Box::new(CreatorHistoryVetoSink::new(
            static_sink as &'static dyn OutboundSink,
            Box::leak(Box::new(lookup)) as &'static dyn CreatorPubkeyLookup,
            Box::leak(Box::new(rpc_impl)) as &'static dyn CreatorHistoryRpc,
            r3_max_launches,
        ));
        let static_veto: &'static CreatorHistoryVetoSink = Box::leak(veto_sink);
        eprintln!("[pq-daemon] CreatorHistoryVetoSink installed — R-3 live execution ARMED");
        static_veto as &'static dyn OutboundSink
    } else {
        static_sink as &'static dyn OutboundSink
    };

    // ── E3: get submission off the decision thread ──────────────────────────
    // The engine's `tick()` no longer pays the build+sign+submit round trip (nor the
    // veto's RPC): `on_admit` hands the record to a worker and returns `Queued`. The
    // worker's verdict is drained in the tick loop below, and the engine finishes the
    // accounting there — including reversing a phantom buy position.
    let async_sink: &'static pump_quant_junction::async_sink::AsyncOutboundSink = Box::leak(
        Box::new(pump_quant_junction::async_sink::AsyncOutboundSink::new(
            inner_sink,
            OUTBOUND_LANES,
            OUTBOUND_QUEUE_DEPTH,
        )),
    );
    eng.install_outbound_sink(async_sink as &'static dyn OutboundSink);
    eprintln!(
        "[pq-daemon] AsyncOutboundSink installed — submissions run off the decision thread ({OUTBOUND_LANES} per-mint lanes, {OUTBOUND_QUEUE_DEPTH} deep each)"
    );

    Ok((eng, async_sink, state_fetcher))
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
    let pp_url =
        std::env::var("PUMPPORTAL_WS_URL").unwrap_or_else(|_| PUMPPORTAL_DEFAULT_URL.to_string());

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
    eprintln!(
        "[pq-daemon] cap={} commitment={} tick={}ms",
        args.junction_cap, args.commitment, tick_period_ms
    );
    eprintln!(
        "[pq-daemon] status_every={} ticks  brain_snapshot_every={} ticks",
        args.status_every_ticks, args.brain_snapshot_every_ticks
    );
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

    // ─── Live mode wiring ────────────────────────────────────────────────
    // When --live is passed, the daemon constructs:
    //   1. Engine::new_live_reconciled (bankroll from on-chain wallet balance)
    //   2. LiveOutboundSink with real I/O adapters (signer, state-fetcher, submitter)
    //   3. Installs the sink into the engine
    //
    // The sink is leaked to `'static` via `Box::leak` because the engine's
    // `install_outbound_sink` requires `&'static dyn OutboundSink`. This is
    // safe because the sink lives for the daemon's entire lifetime (the daemon
    // is the top-level process and never frees it).
    // ── R-3: create the shared creator pubkey map before engine construction ──
    // Populated on PumpPortal create events (traderPublicKey → creator pubkey),
    // consulted by the CreatorHistoryVetoSink on buy admits.
    let creator_pubkey_map: CreatorPubkeyMap =
        std::sync::Arc::new(std::sync::Mutex::new(HashMap::new()));

    // E3: the async outbound handle lives in main's scope so the tick loop can drain
    // the worker's verdicts. `None` in paper mode — there is no sink there at all.
    let mut async_outbound: Option<&'static pump_quant_junction::async_sink::AsyncOutboundSink> =
        None;
    // C1: the curve cache, so decoded stream reserves can be published into it.
    let mut live_state_fetcher: Option<
        std::sync::Arc<pump_quant_junction::live_adapters::RpcLiveStateFetcher>,
    > = None;
    let mut engine = if args.live_mode {
        match construct_live_engine(cfg, &args, creator_pubkey_map.clone()) {
            Ok((e, async_sink, fetcher)) => {
                async_outbound = Some(async_sink);
                live_state_fetcher = Some(fetcher);
                e
            }
            Err(code) => return ExitCode::from(code),
        }
    } else {
        Engine::new(cfg, RunMode::Paper)
    };

    // ── Paper model lane (opt-in, paper only) ────────────────────────────
    // PQ_MODEL_ENDPOINT=http://host:port arms Qwen entry+management through the PRODUCTION
    // InferenceClient; the durable SAFETY_OFF latch is restored from PQ_MODEL_SAFETY_FILE. Absent the
    // variable the legacy path is byte-for-byte unchanged. Never armed in --live.
    let mut model_armed = false;
    if !args.live_mode {
        if let Ok(endpoint) = std::env::var("PQ_MODEL_ENDPOINT") {
            if !endpoint.is_empty() {
                let safety = std::env::var("PQ_MODEL_SAFETY_FILE").unwrap_or_else(|_| {
                    pump_quant_junction::model_lifecycle::DEFAULT_SAFETY_FILE.to_string()
                });
                let armed = pump_quant_junction::model_lifecycle::arm_paper_model(
                    &mut engine,
                    &endpoint,
                    std::path::Path::new(&safety),
                );
                model_armed = true;
                // The event path supplies corpus-definition rows for PumpSwap: the AMM swap's own print must not
                // also feed the trained windows (double count).
                engine.set_corpus_flow_rows(
                    std::env::var("PQ_CURVE_TRADE_SOURCE").as_deref() != Ok("snapshot_delta"),
                );
                eprintln!(
                    "[pq-daemon] paper model lane ARMED endpoint={endpoint} safety_file={safety} load={:?} blocked_at_start={}",
                    armed.load, armed.blocked_at_start
                );
            }
        }
    }
    if model_armed {
        let held_file = std::env::var("PQ_MODEL_HELD_FILE").unwrap_or_else(|_| {
            pump_quant_junction::model_lifecycle::DEFAULT_HELD_FILE.to_string()
        });
        let restore = pump_quant_junction::model_lifecycle::restore_held_state(
            &mut engine,
            std::path::Path::new(&held_file),
        );
        // Missing-history continuity is loaded BEFORE any inference, after the held restore so an
        // absent ledger next to restored exposure is treated as untrusted, never as "no gap".
        {
            let mh_file = std::env::var("PQ_MODEL_MISSING_HISTORY_FILE").unwrap_or_else(|_| {
                pump_quant_junction::model_lifecycle::DEFAULT_MISSING_HISTORY_FILE.to_string()
            });
            let held_restored = matches!(
                restore,
                pump_quant_junction::model_lifecycle::StartupRestore::Restored(_)
            );
            let st = pump_quant_junction::model_lifecycle::attach_missing_history(
                &mut engine,
                std::path::Path::new(&mh_file),
                held_restored,
            );
            eprintln!("[pq-daemon] missing-history ledger {mh_file}: {st:?}");
            if let pump_quant_junction::model_lifecycle::MissingHistoryStartup::ContinuityUnknown(
                why,
            ) = &st
            {
                eprintln!(
                    "[pq-daemon] ALERT: missing-history continuity UNKNOWN ({why}) - Qwen entry and management refuse by name; monitoring, reconciliation and hard safeguards continue"
                );
            }
        }
        // Durable flow-history (trained smart/co-entry/lookback state) is restored and validated BEFORE inference. The resume
        // time is the wall clock now: any interval between the checkpoint's cursors and it is a NAMED gap, never assumed covered.
        {
            let fh_file = std::env::var("PQ_FLOW_HISTORY_FILE")
                .unwrap_or_else(|_| "data/flow_history.ckpt".to_string());
            // The resume time is the wall clock. An operator-declared replay clock is accepted ONLY in an explicitly declared
            // offline paper replay (see `resolve_flow_resume_clock`); anywhere else it is a startup refusal, never ignored.
            let wall_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let resume_ms = match pump_quant_junction::model_lifecycle::resolve_flow_resume_clock(
                args.live_mode,
                model_armed,
                std::env::var("PQ_OFFLINE_PAPER_REPLAY").ok().as_deref(),
                std::env::var("PQ_FLOW_RESUME_MS").ok().as_deref(),
                wall_ms,
            ) {
                Ok((ms, declared)) => {
                    if declared {
                        eprintln!("[pq-daemon] OFFLINE PAPER REPLAY: flow-history resume clock DECLARED = {ms} (wall clock is {wall_ms}); no other clock is affected");
                    }
                    ms
                }
                Err(why) => {
                    eprintln!("[pq-daemon] FATAL: PQ_FLOW_RESUME_MS refused ({why:?}). It is honoured only with PQ_OFFLINE_PAPER_REPLAY=1, no --live, and the paper model lane armed; refusing to start.");
                    return ExitCode::from(EXIT_RESUME_CLOCK_REFUSED);
                }
            };
            let prov = pump_quant_app::flow_checkpoint::Provenance {
                seed_source: std::env::var("PQ_FLOW_SEED_SOURCE").unwrap_or_else(|_| "none".into()),
                seed_sha256: std::env::var("PQ_FLOW_SEED_SHA256").unwrap_or_else(|_| "none".into()),
                seed_before_ms: std::env::var("PQ_FLOW_SEED_BEFORE_MS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0),
                producer: "pq-daemon/corpus-flow-rows".into(),
            };
            let a = engine.model_flow_attach(
                std::path::Path::new(&fh_file),
                pump_quant_market_state::flow_reducer::FlowParams::default(),
                prov,
                resume_ms,
            );
            eprintln!("[pq-daemon] flow-history {fh_file}: {a:?}");
            // READINESS CLASS (reporting only; no refusal or trading behaviour changes here). An intentional cold start is
            // DECLARED (`PQ_FLOW_SEED_SOURCE=cold_start:<label>`) and its zeros mean "none observed in the declared history".
            // A fresh history with no declaration is a MISSING expected bootstrap; an untrusted file is a FAILED one.
            {
                let declared = std::env::var("PQ_FLOW_SEED_SOURCE").unwrap_or_default();
                let class = match &a {
                    pump_quant_app::engine::model_admit::FlowAttach::Fresh
                        if declared.starts_with("cold_start:") =>
                    {
                        "cold_start_declared_history_limited"
                    }
                    pump_quant_app::engine::model_admit::FlowAttach::Fresh => {
                        "bootstrap_missing_undeclared"
                    }
                    pump_quant_app::engine::model_admit::FlowAttach::Untrusted(_) => {
                        "bootstrap_failed_untrusted"
                    }
                    pump_quant_app::engine::model_admit::FlowAttach::Restored {
                        complete: true,
                        ..
                    } => "restored_complete",
                    pump_quant_app::engine::model_admit::FlowAttach::Restored { .. } => {
                        "restored_with_unavailable_interval"
                    }
                };
                if class == "bootstrap_missing_undeclared" {
                    eprintln!("[pq-daemon] ALERT: flow-history has NO checkpoint and NO declared cold start: the expected bootstrap is MISSING. Smart-wallet/co-entry/lookback fields are 'none observed', not 'none exist'.");
                }
                let _ = std::fs::write(
                    "data/flow_readiness.json",
                    format!("{{\"class\":\"{class}\",\"seed_source\":\"{}\",\"resume_clock_declared\":{},\"note\":\"history-limited classes: zeros mean none observed in this declared history, not none globally\"}}", declared.replace('"', "'"), std::env::var("PQ_FLOW_RESUME_MS").is_ok()),
                );
            }
            match &a {
                pump_quant_app::engine::model_admit::FlowAttach::Untrusted(why) => eprintln!(
                    "[pq-daemon] ALERT: flow-history UNTRUSTED ({why}) - Qwen entry and management refuse by name; monitoring, reconciliation and hard safeguards continue; the file is left untouched"
                ),
                pump_quant_app::engine::model_admit::FlowAttach::Restored { unavailable_ms, complete: false, late } => eprintln!(
                    "[pq-daemon] ALERT: flow-history restored across an UNAVAILABLE interval of {unavailable_ms} ms (late records: {late}) - every Qwen prompt after the gap refuses by scope until the interval is reconstructed into the state; an acknowledgement does not clear it; coverage is NOT complete"
                ),
                pump_quant_app::engine::model_admit::FlowAttach::Restored { late, .. } if *late > 0 => eprintln!(
                    "[pq-daemon] ALERT: flow-history restored with {late} durable late-event records - affected windows refuse by scope"
                ),
                _ => {}
            }
        }
        match restore {
            pump_quant_junction::model_lifecycle::StartupRestore::Clean => {
                eprintln!("[pq-daemon] held-state: no ledger at {held_file} - clean start");
            }
            pump_quant_junction::model_lifecycle::StartupRestore::Restored(r) => {
                eprintln!(
                    "[pq-daemon] held-state RESTORED from {held_file}: positions={} pending_orders_uncertain={} \
                     committed_lamports={} realized_lamports={} inventory_unknown={} - management stays DEGRADED \
                     until history + reserves are recovered; uncertain orders need a reconciled report",
                    r.positions, r.pending_uncertain, r.committed_lamports, r.realized_lamports, r.inventory_unknown
                );
            }
            pump_quant_junction::model_lifecycle::StartupRestore::Refused(why) => {
                eprintln!(
                    "[pq-daemon] ALERT: HELD-STATE RESTORE REFUSED ({why}) for {held_file}. The ledger is untouched. \
                     Starting would orphan recorded exposure, so the daemon exits without trading."
                );
                return ExitCode::from(EXIT_HELD_STATE_REFUSED);
            }
        }
    }
    if model_armed {
        let sf = std::env::var("PQ_MODEL_SAFETY_FILE").unwrap_or_else(|_| {
            pump_quant_junction::model_lifecycle::DEFAULT_SAFETY_FILE.to_string()
        });
        let h = pump_quant_junction::model_lifecycle::check_headroom(
            std::path::Path::new(&sf),
            pump_quant_junction::model_lifecycle::MIN_FREE_BYTES,
        );
        eprintln!("[pq-daemon] durable-state headroom at start: {h:?}");
    }
    let mut model_stop_last_alert = Instant::now() - Duration::from_secs(3600);
    let mut model_stop_session = pump_quant_junction::model_lifecycle::StopSession::new();
    let mut stale_callout = pump_quant_junction::model_lifecycle::StaleCallout::default();
    let replay_harness =
        !args.live_mode && std::env::var("PQ_OFFLINE_PAPER_REPLAY").as_deref() == Ok("1");

    // Run-mode tag for tape/journal exports — derived from the ENGINE's actual
    // RunMode, NOT the --live CLI flag. This prevents paper-mode fallback from
    // being mislabeled as "live" in the tape. When construct_live_engine()
    // succeeds, the engine is RunMode::Live; when it fails and we fall back,
    // the engine is RunMode::Paper. The tape must reflect what actually
    // happened, not what was requested.
    let engine_mode = engine.mode();
    let run_mode_tag = match engine_mode {
        RunMode::Live => "live",
        RunMode::Paper | RunMode::Replay => "paper",
    };
    let journal_run_mode = match engine_mode {
        RunMode::Live => JournalRunMode::Live,
        RunMode::Paper | RunMode::Replay => JournalRunMode::Paper,
    };
    eprintln!(
        "[pq-daemon] run_mode: {} (engine mode {:?}, --live-mode flag: {})",
        run_mode_tag, engine_mode, args.live_mode
    );

    // G3 fix: restore the creator ledger from disk (cross-session persistence).
    // Without this, every daemon restart wipes the creator track record to
    // "Unknown", starving the classifier (GAP-B) and discarding accumulated
    // rug/migration observations.
    if std::path::Path::new(LEDGER_PATH).exists() {
        match std::fs::read(LEDGER_PATH) {
            Ok(bytes) => {
                if engine.restore_creator_ledger(&bytes) {
                    let len = engine.measured().creator_ledger_len();
                    eprintln!(
                        "[pq-daemon] creator ledger restored: {len} entries from {LEDGER_PATH}"
                    );
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
                if report.saw_damage() {
                    " [DAMAGE SEEN]"
                } else {
                    ""
                }
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

    // ── Rev-30: LS-primary / WS-fallback gating ─────────────────────────
    // LaserStream gRPC is the PRIMARY data source. Helius WS accountSubscribe
    // is FALLBACK ONLY — activated when LS dies, deactivated when LS recovers.
    // This prevents redundant simultaneous account data streams.
    //
    // ls_active: true when LS child is spawned and delivering data. When true,
    //   the daemon suppresses new WS accountSubscribe requests and processes
    //   LS Account updates instead. When LS dies (respawn limit reached),
    //   ls_active → false and WS accountSubscribe resumes as fallback.
    //
    // pda_to_mint: reverse map of bonding_curve_pda(mint) → mint. LS Account
    //   updates carry the bonding curve PDA as `pubkey` but not the mint. We
    //   populate this map whenever we see a new mint (PumpPortal create, LS
    //   transactions) so we can resolve PDA → mint for LS account decoding.
    let mut ls_active: bool = false;
    let mut pda_to_mint: std::collections::HashMap<[u8; 32], [u8; 32]> =
        std::collections::HashMap::new();

    let mut reserve_tracker: HashMap<[u8; 32], ReserveSnapshot> = HashMap::new();
    // Who owns pump.fun CURVE trade history. `events` (default): verified-successful TradeEvents
    // only; snapshot deltas then supply reserve state and reconciliation, never trades, and the
    // instruction-arg prints (no price, net quantities) are not queued as curve trades. A feed that
    // cannot supply `meta.tx_ok` therefore yields named gaps, not silent snapshot-fed history.
    let curve_trade_source = match std::env::var("PQ_CURVE_TRADE_SOURCE").as_deref() {
        Ok("snapshot_delta") => {
            pump_quant_junction::curve_trade_events::CurveTradeSource::SnapshotDelta
        }
        _ => pump_quant_junction::curve_trade_events::CurveTradeSource::Events,
    };
    let mut curve_dedup = pump_quant_junction::curve_trade_events::EventDedup::new(65_536);
    let mut amm_row_stats = pump_quant_junction::curve_trade_events::AmmRowStats::default();
    let mut amm_row_status_unknown: u64 = 0;
    let mut curve_ev_produced: u64 = 0;
    let mut curve_ev_duplicates: u64 = 0;
    let mut curve_ev_incomplete: u64 = 0;
    let mut curve_ev_incomplete_unnamed: u64 = 0;
    let mut curve_compat = pump_quant_junction::curve_trade_events::ProducerCompat::default();
    let mut curve_compat_announced = false;
    let events_mode_health =
        curve_trade_source == pump_quant_junction::curve_trade_events::CurveTradeSource::Events;
    // The instruction prints' wallets, waiting for their reserve prints (see `trade_join`).
    let mut trade_join = TradeJoin::new(TRADE_JOIN_CAP, TRADE_JOIN_HORIZON_SLOTS);
    // Wangr Rev-14: tracks which mints we've already emitted MarketAuxiliary
    // for (prevents duplicate aux events on re-subscribe/reconnect).
    // Note: creator_launches is tracked internally by the engine via its own
    // mint_creator/creator_launches maps — no daemon-side tracker needed.
    let mut aux_emitted: std::collections::HashSet<[u8; 32]> = std::collections::HashSet::new();
    // Narrative precondition (operator ruling 2026-09-26): the rotating lexicon.
    // Absent or malformed => `None`, and then NO narrative verdicts are emitted at
    // all. That is a COVERAGE GAP: the gate sees the zero sentinel and admits.
    // Never a silent refusal, and never a fabricated family.
    let narrative_lexicon_path = std::env::var("PQ_NARRATIVE_LEXICON")
        .unwrap_or_else(|_| "tools/data-pipeline/output/dynamic_lexicon_v1.json".to_string());
    let mut narrative_lex =
        pump_quant_junction::narrative_lexicon::NarrativeLexicon::load(&narrative_lexicon_path);
    match &narrative_lex {
        Some(l) => eprintln!(
            "[pq-daemon] narrative lexicon loaded: v{} {} entries from {}",
            l.version(),
            l.len(),
            narrative_lexicon_path
        ),
        None => eprintln!(
            "[pq-daemon] narrative lexicon UNAVAILABLE at {narrative_lexicon_path} -> no narrative \
             verdicts will be emitted (coverage gap; the gate admits on the sentinel)"
        ),
    }
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
    // ── Restored held positions: re-establish their feeds INDEPENDENTLY of discovery ──
    // A restart that rebuilt held positions (model_lifecycle::restore_held_state) has no live reserve or
    // print feed for them until something subscribes. Do it now: trade prints via PumpPortal, the PDA map
    // for LaserStream account decoding, and the Helius account subscription as the fallback plane. Status
    // stays DEGRADED (named) until a fresh reserve actually arrives; a subscription is not readiness.
    if model_armed {
        for mint_bytes in pump_quant_junction::model_lifecycle::mints_needing_feeds(&engine) {
            let mint_b58 = Pubkey::from(mint_bytes).to_string();
            let pda = bonding_curve_pda(&mint_bytes);
            pda_to_mint.insert(pda.to_bytes(), mint_bytes);
            if trade_sub_tracker.add(&mint_b58) {
                let sub_msg = pumpportal_ws::subscribe_token_trade(std::slice::from_ref(&mint_b58));
                match pp_conn.send_text(&sub_msg) {
                    Ok(()) => {
                        stats.pp_trade_subs_sent += 1;
                        eprintln!(
                            "[pq-daemon] restored held mint {mint_b58}: trade feed subscribed"
                        );
                    }
                    Err(e) => {
                        eprintln!("[pq-daemon] ALERT: restored held mint {mint_b58}: trade subscribe FAILED: {e}");
                        stats.ws_errors += 1;
                    }
                }
            }
            if !ls_active {
                let req_id = next_req_id;
                next_req_id += 1;
                let req = helius_ws::account_subscribe_request(
                    req_id,
                    &pda.to_string(),
                    &args.commitment,
                );
                if helius_conn.send_text(&req).is_ok() {
                    sub_tracker.record_request(req_id, mint_bytes);
                }
            }
        }
    }

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
            if p.exists() {
                p.to_str().map(|s| s.to_string())
            } else {
                None
            }
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
        cmd.env(
            "WSLENV",
            "HELIUS_API_KEY/u:LASERSTREAM_ENDPOINT/u:HELIUS_WS_URL/u",
        );
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
                ls_active = true; // Rev-30: LS is primary — suppress WS accountSubscribe
                ls_child = Some(child);
            }
            None => {
                eprintln!("[pq-daemon] Proceeding with Helius WS as secondary lane");
                stats
                    .stubbed_or_assumed
                    .push("LaserStream spawn failed — Helius WS as fallback".to_string());
            }
        }
    } else {
        eprintln!("[pq-daemon] LaserStream binary not found — set PQ_LASERSTREAM_BIN");
        stats
            .stubbed_or_assumed
            .push("LaserStream binary not found — Helius WS as fallback".to_string());
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
    let fc_bin: Option<String> = std::env::var("PQ_FIRECRAWL_BIN")
        .ok()
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
                "../../../tools/firecrawl-bridge-rs/target/release/pq-firecrawl-bridge.exe",
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
                                if !text.is_empty() && fc_tx_clone.send(text.into_bytes()).is_err()
                                {
                                    break;
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
                stats
                    .stubbed_or_assumed
                    .push("Firecrawl bridge spawn failed — no social intelligence".to_string());
            }
        }
    } else {
        eprintln!("[pq-daemon] Firecrawl bridge binary not found — set PQ_FIRECRAWL_BIN");
        stats
            .stubbed_or_assumed
            .push("Firecrawl bridge not found — no social intelligence".to_string());
    }

    let tick_period = Duration::from_millis(tick_period_ms);
    let mut next_tick = Instant::now() + tick_period;
    // OFFLINE PAPER REPLAY decision barriers (inert unless PQ_OFFLINE_PAPER_REPLAY=1 with the model lane armed AND an
    // explicit barrier list). Production cadence, timestamp checks and monotonic deadlines are untouched.
    let barrier_clocks: Vec<i64> = if replay_harness && model_armed {
        std::env::var("PQ_REPLAY_BARRIERS")
            .ok()
            .map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let barrier_mode = !barrier_clocks.is_empty();
    let mut barrier_idx: usize = 0;
    let mut barrier_ready = false;
    let mut barrier_hold: Option<LaserStreamUpdate> = None;
    let mut barrier_seen_count: u64 = 0;
    let mut barrier_last_input = Instant::now();
    if barrier_mode {
        engine.barrier_enable();
        eprintln!(
            "[pq-daemon] OFFLINE PAPER REPLAY: {} decision barriers at fixed source-time clocks; the timed tick is replaced by barrier ticks",
            barrier_clocks.len()
        );
    }
    let status_path = std::path::Path::new(STATUS_PATH);
    let mut tick_counter: u64 = 0;
    let mut last_status_write_tick: u64 = 0;
    let mut last_status_write_wallclock: Instant = Instant::now();
    // Counter for periodic session_history.jsonl writes (crash resilience).
    // Every 20 heartbeat writes (~5 min), a final=false checkpoint is appended.
    let mut session_history_write_counter: u32 = 0;
    let mut last_brain_snap_tick: u64 = 0;
    let mut last_tape_flush_tick: u64 = 0;

    // ─── Rev-19: on-chain confirmation feedback poller ─────────────────
    // Every CONFIRM_POLL_TICKS ticks, we poll getSignaturesForAddress for
    // our wallet and match the results against the engine's pending_buys /
    // pending_sells. Matched txs are fed back as AppEvent::OurBuyConfirmed /
    // OurBuyFailed / OurSellConfirmed / OurSellFailed. This closes the
    // architecture gap where paper PnL was recorded as live without verifying
    // the on-chain outcome. Only active in live mode.
    let confirm_poll_interval_secs: u64 = 5;
    let mut next_confirm_poll: Instant =
        Instant::now() + Duration::from_secs(confirm_poll_interval_secs);
    // Extract the Helius RPC URL from creds file or env (same logic as
    // construct_live_engine, but we need it in main's scope for the poller).
    let rpc_url_for_confirm = if args.live_mode {
        extract_helius_rpc_url(&args)
    } else {
        String::new()
    };
    let wallet_for_confirm = args.wallet_address.clone();

    // ─── Autonomous bridge: config hot-reload + defense-in-depth ────────
    // The bridge connects the evaluator/refiner framework to the live daemon.
    // G2: hot-reload CONFIG_PROMOTION.json (written by pq-refiner)
    // G4: defense-in-depth — cliff veto, circuit breaker, kill switch
    // RETIRED: G1 automatic refiner spawn. The daemon no longer spawns the
    // evaluator/refiner promotion loop; strategy promotion is operator-gated
    // (a promotion file must be placed deliberately) so the obsolete strategy
    // authority is isolated from the production runtime.
    let mut defense_state = DefenseState::default();
    let mut config_mtime: Option<u64> = None;
    // ── GAP B: auto-revert state ─────────────────────────────────────────
    // Tracks the config fingerprint + PnL at the moment of each promotion.
    // If post-promotion PnL deteriorates beyond a threshold within the grace
    // period, the daemon auto-reverts to the archived champion config.
    let mut auto_revert_state = AutoRevertState::default();
    let mut promotion_tick: u64 = 0; // tick at which the last promotion was applied
    let mut pre_promotion_fingerprint: u64 = 0; // fingerprint before the promotion
    let mut trades_at_promotion: u64 = 0; // cumulative trade count at promotion time
    eprintln!(
        "[pq-daemon] autonomous bridge: defense-in-depth + config hot-reload + auto-revert ACTIVE; \
         automatic refiner promotion RETIRED (operator-gated){}",
        if args.refiner_every_ticks > 0 {
            " [--refiner-every-ticks ignored]"
        } else {
            ""
        }
    );

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
            .as_secs())
            << 32);
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
            eprintln!(
                "[pq-daemon] EMERGENCY: ticks={} promoted={} admitted={} net={}lamports",
                st.info_time_tick, st.promoted, st.admitted, st.net_realized_lamports
            );
            // Kill LaserStream child if present — use kill_process_tree to
            // kill the entire process tree (prevents orphaned gRPC in WSL2).
            // GAP #13: bare child.kill() leaves wsl.exe grandchildren alive.
            if let Some(ref mut child) = ls_child {
                kill_process_tree(child);
            }
            // Kill Firecrawl bridge child if present — same tree-kill logic.
            if let Some(ref mut child) = fc_child {
                kill_process_tree(child);
            }
            return ExitCode::from(EXIT_EMERGENCY);
        }

        // ── Graceful shutdown check (every iteration) ────────────────────
        if daemon_stop_requested() {
            // A model-armed engine is the SOLE protector of whatever it holds or has outstanding: a stop
            // file alone never terminates it. Complete only when flat+reconciled or after an
            // acknowledged protective handoff; otherwise stay up, blocked, and alert.
            if model_armed {
                use pump_quant_junction::model_lifecycle::{
                    handle_stop_request, StopGate, HANDOFF_REQUEST_FILE,
                    PROTECTIVE_HANDOFF_ACK_FILE,
                };
                match handle_stop_request(
                    &mut engine,
                    &mut model_stop_session,
                    std::path::Path::new(HANDOFF_REQUEST_FILE),
                    std::path::Path::new(PROTECTIVE_HANDOFF_ACK_FILE),
                ) {
                    StopGate::CompleteFlat => {
                        eprintln!("[pq-daemon] DAEMON_STOP accepted: flat and reconciled");
                        clean_stop_sentinel();
                        break;
                    }
                    StopGate::CompleteHandedOff { recipient } => {
                        eprintln!("[pq-daemon] DAEMON_STOP accepted: protective handoff accepted by '{recipient}' (session/request/exposure bound)");
                        clean_stop_sentinel();
                        break;
                    }
                    StopGate::Incomplete {
                        assessment: a,
                        rejection,
                        request_id,
                        exposure_digest,
                    } => {
                        if model_stop_last_alert.elapsed() >= Duration::from_secs(30) {
                            eprintln!(
                                "[pq-daemon] ALERT: INCOMPLETE SHUTDOWN - held={} pending_orders={} uncertain={}; entries BLOCKED, protection continues, process NOT terminated. \
                                 No valid protective-handoff acknowledgement ({rejection:?}). Recipient must write {} echoing request_id={} exposure_digest={} (see {})",
                                a.held, a.pending_orders, a.uncertain_orders,
                                PROTECTIVE_HANDOFF_ACK_FILE, request_id, exposure_digest, HANDOFF_REQUEST_FILE
                            );
                            model_stop_last_alert = Instant::now();
                        }
                    }
                }
            } else {
                eprintln!("[pq-daemon] DAEMON_STOP detected — initiating graceful shutdown");
                clean_stop_sentinel();
                break;
            }
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
            let active_mints_vec = sub_tracker.clear_server_subs();
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
                            for mint in &active_mints_vec {
                                let pda = bonding_curve_pda(mint);
                                let pda_str = pda.to_string();
                                let req_id = next_req_id;
                                next_req_id += 1;
                                let req = helius_ws::account_subscribe_request(
                                    req_id,
                                    &pda_str,
                                    &args.commitment,
                                );
                                let _ = c.send_text(&req);
                                sub_tracker.record_request(req_id, *mint);
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
            // Best-effort open-positions telemetry dump.
            {
                let snaps = engine.open_positions_snapshot();
                let open_path = std::path::Path::new("data/open_positions.json");
                let _ = pump_quant_app::live_status::OpenPositionSnapshot::write_to_path(
                    &snaps, open_path,
                );
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
            // Low-frequency (status-heartbeat) dropped-print health: lets an operator tell an
            // honestly QUIET market from one whose data is INCOMPLETE. Never on the hot path.
            let flow_drop = engine.model_flow_drop_summary();
            let (mh_unflushed, _mh_fail_now, mh_fail_total) = engine.model_missing_persist_health();
            let (
                fh_durable,
                fh_sub,
                fh_fail_now,
                fh_fail_total,
                fh_clone_us,
                fh_clone_max_us,
                fh_bytes,
                fh_enc_us,
                fh_write_us,
            ) = engine.model_flow_health();
            let health_json = format!(
                concat!(
                    "{{",
                    "\"onchain_confirms_decoded\":{},",
                    "\"helius_account_notifications\":{},",
                    "\"helius_reconnects\":{},",
                    "\"ls_transactions_received\":{},",
                    "\"ls_spawned\":{},",
                    "\"ls_active\":{},",
                    "\"ls_account_received\":{},",
                    "\"ls_onchain_confirms_decoded\":{},",
                    "\"ls_account_unresolved\":{},\"pda_installed_from_tx\":{},\"pda_map_full_refused\":{},",
                    "\"delta_trades_derived\":{},",
                    "\"delta_no_trade\":{},",
                    "\"delta_out_of_range\":{},",
                    "\"flow_upstream_drops\":{},",
                    "\"flow_windows_incomplete\":{},",
                    "\"flow_missing_observations\":{},",
                    "\"missing_history_unflushed\":{},",
                    "\"missing_history_persist_failures\":{},",
                    "\"missing_history_continuity_unknown\":{},",
                    "\"flow_ckpt_durable_seq\":{},",
                    "\"flow_ckpt_submitted_seq\":{},",
                    "\"flow_ckpt_failures_now\":{},",
                    "\"flow_ckpt_failures_total\":{},",
                    "\"flow_ckpt_clone_us\":{},",
                    "\"flow_ckpt_clone_max_us\":{},",
                    "\"flow_ckpt_bytes\":{},",
                    "\"flow_ckpt_encode_us\":{},",
                    "\"flow_ckpt_write_us\":{},",
                    "\"curve_trade_source\":\"{}\",",
                    "\"curve_events_produced\":{},",
                    "\"curve_events_duplicates\":{},",
                    "\"curve_events_incomplete\":{},",
                    "\"curve_events_incomplete_unnamed\":{},",
                    "\"amm_rows_emitted\":{},",
                    "\"amm_rows_resolver_rejects\":{},",
                    "\"amm_rows_duplicates\":{},",
                    "\"amm_rows_status_unknown\":{},",
                    "\"curve_producer_ready\":{},",
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
                ls_active,
                stats.ls_account_received,
                stats.ls_onchain_confirms_decoded,
                stats.ls_account_unresolved,
                stats.pda_installed_from_tx,
                stats.pda_map_full_refused,
                stats.delta_trades_derived,
                stats.delta_no_trade,
                stats.delta_out_of_range,
                flow_drop.drops_total,
                flow_drop.mints_incomplete_now,
                flow_drop.mints_history_unreconstructed,
                mh_unflushed,
                mh_fail_total,
                engine.model_history_continuity_unknown(),
                fh_durable,
                fh_sub,
                fh_fail_now,
                fh_fail_total,
                fh_clone_us,
                fh_clone_max_us,
                fh_bytes,
                fh_enc_us,
                fh_write_us,
                if events_mode_health {
                    "events"
                } else {
                    "snapshot_delta"
                },
                curve_ev_produced,
                curve_ev_duplicates,
                curve_ev_incomplete,
                curve_ev_incomplete_unnamed,
                amm_row_stats.emitted,
                amm_row_stats.resolver_rejects,
                amm_row_stats.duplicates,
                amm_row_status_unknown,
                // Snapshot-delta mode has no producer-status requirement.
                !events_mode_health || curve_compat.ready(),
                uptime_secs,
                tick_counter,
                sub_tracker.len(),
                stats.account_subs_evicted,
                stats.subs_leaked_no_ack,
                stats.sub_cap_errors,
            );
            let _ = std::fs::write("data/daemon_health.json", health_json);
            // The model lane's own funnel counters (discovery / refusal reasons / dispatch), written next to the health
            // file so an operator or test can see WHERE a market stopped, with exact counts.
            if model_armed {
                let rep: std::collections::BTreeMap<&String, &u64> =
                    engine.model_lane_report().iter().collect();
                if let Ok(j) = serde_json::to_string(&rep) {
                    let _ = std::fs::write("data/model_lane_report.json", j);
                }
            }

            last_status_write_tick = tick_counter;
            last_status_write_wallclock = Instant::now();
        }

        // ── Poll LaserStream gRPC (PRIMARY ingest lane) ──────────────────
        loop {
            match next_ls_update(
                &ls_rx,
                &mut barrier_hold,
                barrier_clocks.get(barrier_idx).copied(),
                &mut barrier_ready,
            ) {
                Ok(LaserStreamUpdate::Transaction(tx)) => {
                    did_work = true;
                    stats.ls_transactions_received += 1;
                    let classified = classify_pump_instructions(&tx);
                    stats.ls_instructions_classified += classified.len() as u64;
                    // CURVE IDENTITY from verified pump.fun instructions. The curve PDA is a pure function of
                    // (pump program, mint), so deriving it from the mint named by a decoded pump.fun buy / sell /
                    // create instruction is itself the ownership check: an account update whose pubkey equals this
                    // derivation IS this mint's bonding curve. Required for markets first seen mid-life: no launch
                    // or PumpPortal create event is needed, and none is pretended (launch history stays a separate,
                    // named input). Bounded; a full map is counted, never silently grown.
                    for c in &classified {
                        let m = match c {
                            pump_quant_junction::laserstream::PumpInstruction::Buy {
                                mint, ..
                            }
                            | pump_quant_junction::laserstream::PumpInstruction::Sell {
                                mint,
                                ..
                            }
                            | pump_quant_junction::laserstream::PumpInstruction::Launch {
                                mint,
                                ..
                            } => Some(*mint),
                            _ => None,
                        };
                        if let Some(m) = m {
                            let pda = bonding_curve_pda(&m).to_bytes();
                            if !pda_to_mint.contains_key(&pda) {
                                if pda_to_mint.len() >= PDA_MAP_CAP {
                                    stats.pda_map_full_refused += 1;
                                } else {
                                    pda_to_mint.insert(pda, m);
                                    stats.pda_installed_from_tx += 1;
                                }
                            }
                        }
                    }
                    let events = instructions_to_events_with_meta(
                        &classified,
                        tx.slot,
                        tx.is_live,
                        tx.recv_unix_ms,
                        tx.fee_lamports,
                        tx.cu_consumed,
                    );
                    use pump_quant_junction::curve_trade_events::{ingest_curve_tx, EventIngest};
                    let events_mode = curve_trade_source
                        == pump_quant_junction::curve_trade_events::CurveTradeSource::Events;
                    let events: Vec<_> = if events_mode {
                        // Curve instruction-arg prints are NOT curve trades in event mode.
                        events
                            .into_iter()
                            .filter(|e| {
                                !(e.source == ProvenanceSource::LaserStream
                                    && matches!(
                                        e.event,
                                        AppEvent::MarketTrade {
                                            venue: Some(pump_quant_app::event::TradeVenue::PumpFun),
                                            ..
                                        }
                                    ))
                            })
                            .collect()
                    } else {
                        events
                    };
                    if events_mode {
                        curve_compat.note(&tx);
                        if !curve_compat_announced {
                            if let Some(r) = curve_compat.reason() {
                                curve_compat_announced = true;
                                eprintln!("FATAL-READINESS: curve trade source = events, but {r}");
                            }
                        }
                        let mut ev_out = Vec::new();
                        // PumpSwap corpus-definition FEATURE rows (wallet history; independent of execution scope).
                        // Unknown tx status on a PumpSwap swap line is counted, never treated as success.
                        if tx.instructions.iter().any(|i| {
                            i.program_id == pump_quant_junction::laserstream::PUMP_SWAP_PROGRAM
                        }) {
                            if tx.tx_ok.is_none() {
                                amm_row_status_unknown += 1;
                            }
                            pump_quant_junction::curve_trade_events::ingest_amm_rows(
                                &tx,
                                &mut curve_dedup,
                                &mut amm_row_stats,
                                &mut ev_out,
                            );
                        }
                        match ingest_curve_tx(&tx, &mut curve_dedup, &mut ev_out) {
                            EventIngest::Nothing => {}
                            EventIngest::Produced {
                                events: n,
                                duplicates: d,
                            } => {
                                curve_ev_produced += n as u64;
                                curve_ev_duplicates += d as u64;
                            }
                            EventIngest::Incomplete(reason) => {
                                curve_ev_incomplete += 1;
                                // Named gap per affected mint; no snapshot fallback.
                                let mut named = false;
                                for c in &classified {
                                    let m = match c {
                                        pump_quant_junction::laserstream::PumpInstruction::Buy { mint, .. }
                                        | pump_quant_junction::laserstream::PumpInstruction::Sell { mint, .. } => Some(*mint),
                                        _ => None,
                                    };
                                    if let (Some(m), Some(ms)) = (m, tx.recv_unix_ms) {
                                        engine.note_missing_observation(
                                            m,
                                            ms,
                                            pump_quant_app::decision_join::MissingKind::PossibleTrade,
                                            format!("tx_event:{reason}:{}", tx.slot),
                                        );
                                        named = true;
                                    }
                                }
                                if !named {
                                    curve_ev_incomplete_unnamed += 1;
                                }
                            }
                        }
                        for pe in ev_out {
                            if !queue.push(pe, tx.slot) {
                                stats.junction_overflow_dropped += 1;
                            }
                        }
                    }
                    // The instruction print is the ONLY one that knows the wallet. Note it
                    // against (mint, slot) so the reserve-delta print — which knows the price —
                    // can claim it when it is derived.
                    for ev in &events {
                        if let AppEvent::MarketTrade {
                            mint,
                            signed_base,
                            buyer_entity,
                            // Only a known address is worth noting: the join exists to supply
                            // the address, so a hash-only print has nothing to contribute.
                            trader_pubkey: Some(pk),
                            recv_unix_ms,
                            ..
                        } = &ev.event
                        {
                            trade_join.note_instruction_with_meta(
                                mint.as_bytes(),
                                ev.slot,
                                *buyer_entity,
                                *pk,
                                *signed_base > 0,
                                *recv_unix_ms,
                                tx.fee_lamports,
                                tx.cu_consumed,
                            );
                        }
                    }
                    for ev in &events {
                        stats.ls_events_emitted += 1;
                        if !queue.push(*ev, tx.slot) {
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
                Ok(LaserStreamUpdate::Account {
                    pubkey,
                    owner: _,
                    data,
                    slot,
                    recv_unix_ms,
                }) => {
                    did_work = true;
                    stats.ls_account_received += 1;

                    // ── Rev-30: Process LS account updates as PRIMARY bonding
                    // curve data source. The gRPC server subscribes to accounts
                    // owned by PUMP_PROGRAM (bonding curves) and PUMPSWAP_PROGRAM
                    // (AMM pools). When LS is active, this replaces Helius WS
                    // accountSubscribe entirely — no redundant WS data stream.
                    //
                    // The `pubkey` is the bonding curve PDA. We resolve it to
                    // a mint via the pda_to_mint reverse map, then decode the
                    // account data the same way the WS handler does.
                    if let Some(mint_bytes) = pda_to_mint.get(&pubkey) {
                        let mb = *mint_bytes;
                        if let Some((provenanced, curve)) =
                            decode_onchain_confirm_with_curve(&mb, &data, slot)
                        {
                            queue.push(provenanced, slot);
                            // Model lane: the full four-reserve observation with its wire clock.
                            // Additive; an update with no clock emits nothing.
                            if let Some(obs) =
                                pump_quant_junction::decode::curve_observed_from_curve(
                                    &mb,
                                    &curve,
                                    slot,
                                    recv_unix_ms,
                                )
                            {
                                queue.push(obs, slot);
                            }
                            stats.ls_onchain_confirms_decoded += 1;
                            stats.last_confirm_tick = tick_counter;
                            stats.pda_venue_matches += 1;

                            // Wangr Rev-14: decode curve tail for token_standard
                            if aux_emitted.contains(&mb) {
                                let token_standard = match decode_pump_curve_tail(&data) {
                                    Some(tail) => match tail.is_mayhem_mode {
                                        Some(true) => 2,  // mayhem/token2022
                                        Some(false) => 1, // legacy SPL
                                        None => 0,        // unknown
                                    },
                                    None => 0,
                                };
                                queue.push(
                                    ProvenancedEvent {
                                        event: AppEvent::MarketAuxiliary {
                                            mint: Mint(mb),
                                            token_standard,
                                            symbol_len: 0,
                                        },
                                        source: ProvenanceSource::LaserStreamAccount,
                                        slot,
                                        is_live: true,
                                    },
                                    slot,
                                );
                            }

                            // Derive market trade from reserve delta — same
                            // logic as the WS account notification handler.
                            let prev = reserve_tracker.get(&mb).copied();
                            // The account notification's wire receive time is the print's
                            // clock: this is the only producer with a real `price_fp`, so it
                            // is the feed the live state ledger's windows key on.
                            // In event mode the snapshot is state/reconciliation ONLY: it neither
                            // derives a trade nor records a drop (a net delta over several trades is
                            // not a gap — the events own the history).
                            let snapshot_trades = curve_trade_source.snapshot_may_feed_trades();
                            let derived = if snapshot_trades {
                                derive_market_trade_from_delta(
                                    &mb,
                                    prev,
                                    &curve,
                                    slot,
                                    true,
                                    recv_unix_ms,
                                )
                            } else {
                                None
                            };
                            if let Some(mut trade_pe) = derived {
                                // Join the two halves: this producer knows the price and both
                                // legs, the instruction print knows the trader. An ambiguous or
                                // missing match leaves `buyer_entity: 0` — the ledger reports
                                // `identity_known: false` rather than a guessed concentration.
                                let side_is_buy = match &trade_pe.event {
                                    AppEvent::MarketTrade { signed_base, .. } => *signed_base > 0,
                                    _ => false,
                                };
                                match trade_join.take_identity(&mb, slot, side_is_buy) {
                                    JoinOutcome::Identity {
                                        entity,
                                        pubkey,
                                        fee_lamports: j_fee,
                                        cu_consumed: j_cu,
                                    } => {
                                        if let AppEvent::MarketTrade {
                                            buyer_entity,
                                            trader_pubkey,
                                            fee_lamports,
                                            cu_consumed,
                                            ..
                                        } = &mut trade_pe.event
                                        {
                                            // The reserve print has the price but not the
                                            // transaction; the matched instruction print does.
                                            // Stamped only on a unique (mint, slot, side) match
                                            // -- an ambiguous or missing match leaves None.
                                            *fee_lamports = j_fee;
                                            *cu_consumed = j_cu;
                                            *buyer_entity = entity;
                                            // The address is what the flow reducer's
                                            // freshness / smart-wallet / co-entry rules key on;
                                            // the engine's hashed id cannot stand in for it.
                                            *trader_pubkey = Some(pubkey);
                                        }
                                    }
                                    JoinOutcome::Unknown | JoinOutcome::Ambiguous => {}
                                }
                                queue.push(trade_pe, slot);
                                stats.delta_trades_derived += 1;
                            } else if snapshot_trades {
                                stats.delta_no_trade += 1;

                                if !pump_quant_junction::reserve_delta::delta_representable(
                                    prev.as_ref(),
                                    &curve,
                                ) {
                                    stats.delta_out_of_range += 1;
                                }
                                // A print that moved the curve but the derivation refused never
                                // reaches the flow reducer, so no received-print check can see
                                // it. The shared producer->engine step classifies the miss and
                                // records the drop (with its wire receive time) so the decision
                                // join refuses any 300 s flow window that would otherwise be
                                // served as complete or quietly idle; an ordinary no-trade
                                // (`NoPrint`) is left alone.
                                let _ =
                                    pump_quant_junction::reserve_delta::note_curve_snapshot_outcome(
                                        &mut engine,
                                        mb,
                                        prev.as_ref(),
                                        &curve,
                                        slot,
                                        recv_unix_ms,
                                    );
                            }
                            reserve_tracker.insert(
                                mb,
                                ReserveSnapshot {
                                    virtual_sol: curve.virtual_sol,
                                    virtual_token: curve.virtual_token,
                                    slot,
                                },
                            );
                            // C1: publish these reserves into the curve cache. The hot path (and the
                            // outbound sink's state fetch) then answers from the stream instead of paying a
                            // cold 4-RTT fetch. Returns false for a mint whose ctx was never learned — the
                            // stream carries no fee_recipient, so nothing is fabricated here.
                            if let Some(fetcher) = live_state_fetcher.as_ref() {
                                fetcher.note_stream_reserves(
                                    &mb,
                                    curve.virtual_sol,
                                    curve.virtual_token,
                                    curve.complete,
                                    slot,
                                );
                            }
                        }
                    } else {
                        // PDA not in reverse map — this happens for bonding
                        // curves of mints we haven't seen yet (e.g. LS catches
                        // a curve update before PumpPortal create arrives).
                        // This is expected and non-fatal; the WS fallback would
                        // also miss these without a prior create event.
                        stats.ls_account_unresolved += 1;
                    }
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
                                "LaserStream exhausted respawns — Helius WS fallback".to_string(),
                            );
                            // Mark ls_spawned false so we don't keep trying
                            stats.ls_spawned = false;
                            // Rev-30: LS is dead — activate WS fallback.
                            ls_active = false;
                            eprintln!(
                                "[pq-daemon] ls_active=false — WS accountSubscribe now active as fallback data source"
                            );
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
                                    // Rev-30: LS recovered — re-activate as primary.
                                    ls_active = true;
                                    // Close Helius WS account subscriptions
                                    // to stop redundant WS data stream. The
                                    // WS connection itself stays open for slot
                                    // notifications (cheap, non-redundant).
                                    let active_mints_vec = sub_tracker.clear_server_subs();
                                    if !active_mints_vec.is_empty() {
                                        eprintln!(
                                            "[pq-daemon] LS recovered — cleared {} WS accountSubs (redundant with LS gRPC)",
                                            active_mints_vec.len()
                                        );
                                    }
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
                let is_migration = text.contains("\"txType\":\"migrate\"")
                    || text.contains("\"txType\":\"migration\"");

                if is_create {
                    stats.pp_creates_received += 1;
                    if handle_create_payload(text.as_bytes(), 0, &queue) {
                        stats.pp_creates_parsed += 1;
                    }
                    if let Some(meta) = pump_quant_ingest::pumpportal_parse::parse_pumpportal_create(
                        text.as_bytes(),
                    ) {
                        let mint_bytes = meta.mint;
                        let mint_b58 = Pubkey::from(mint_bytes).to_string();

                        // ── R-3: populate creator→mint pubkey map ──────────────
                        // The creator's raw wallet pubkey is captured from the
                        // PumpPortal create event's traderPublicKey field. This
                        // map is consulted by CreatorHistoryVetoSink on buy
                        // admits to query getSignaturesForAddress.
                        if let Some(cp) = meta.creator_pubkey {
                            if let Ok(mut guard) = creator_pubkey_map.lock() {
                                guard.insert(mint_bytes, cp);
                            }
                        }

                        // ── Rev-30: Populate PDA→mint reverse map for LS Account ──
                        // When LaserStream delivers bonding-curve account updates,
                        // the pubkey is the PDA — we need this reverse map to
                        // resolve PDA → mint for decoding.
                        let pda = bonding_curve_pda(&mint_bytes);
                        pda_to_mint.insert(pda.to_bytes(), mint_bytes);

                        // ── Wangr Rev-14: emit MarketAuxiliary with symbol_len ──
                        // The PumpPortal create event carries the token symbol,
                        // which the wangr symbol-length filter needs. We also
                        // track the creator's cumulative launch count here.
                        // token_standard = 0 means "unknown" (will be updated
                        // when the OnchainConfirm decodes the curve tail).
                        if aux_emitted.insert(mint_bytes) {
                            let symbol_len = meta.symbol.len().min(255) as u8;
                            queue.push(
                                ProvenancedEvent {
                                    event: AppEvent::MarketAuxiliary {
                                        mint: Mint(mint_bytes),
                                        token_standard: 0, // unknown until OnchainConfirm
                                        symbol_len,
                                    },
                                    source: ProvenanceSource::PumpPortalTrade,
                                    slot: 0,
                                    is_live: true,
                                },
                                0,
                            );

                            // ── Narrative precondition: resolve the NAME ──────────
                            // Emitted at the same instant as the auxiliary data,
                            // because the name is known here and the gate must hold
                            // the verdict before it can evaluate this market. Only
                            // the LEXICAL lane can fire at launch time — the
                            // attention plane has no history yet and the model lane
                            // is offline — so an unmatched name resolves
                            // `Unresolved`. That is a refusal under ENFORCE and a
                            // recorded fact under the default OBSERVE mode; absence
                            // is never fabricated into `false`.
                            if let Some(lex) = narrative_lex.as_mut() {
                                let now_ms = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis() as u64)
                                    .unwrap_or(0);
                                let (verdict, stage, family, lexicon_version) =
                                    lex.resolve(&meta.name, &meta.symbol, now_ms);
                                queue.push(
                                    ProvenancedEvent {
                                        event: AppEvent::NarrativeResolved {
                                            mint: Mint(mint_bytes),
                                            verdict,
                                            stage,
                                            family,
                                            lexicon_version,
                                        },
                                        source: ProvenanceSource::PumpPortalTrade,
                                        slot: 0,
                                        is_live: true,
                                    },
                                    0,
                                );
                            }
                        }

                        if trade_sub_tracker.add(&mint_b58) {
                            let sub_msg = pumpportal_ws::subscribe_token_trade(
                                std::slice::from_ref(&mint_b58),
                            );
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
                            // ── Rev-30: LS-primary gating ──────────────────────
                            // When LaserStream is active (primary data source),
                            // SKIP the Helius WS accountSubscribe. LS already
                            // delivers bonding-curve account updates via gRPC,
                            // so a WS accountSubscribe would be redundant. We
                            // still populate pda_to_mint (done above) so LS
                            // Account updates can resolve PDA → mint.
                            //
                            // When LS is down (ls_active = false), WS
                            // accountSubscribe activates as fallback — the
                            // original subscription logic runs unchanged.
                            if ls_active {
                                eprintln!(
                                    "[pq-daemon] LS active — skipping WS accountSubscribe for mint {:.8} (LS covers bonding curves via gRPC)",
                                    hex_short(&mint_bytes)
                                );
                            } else {
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
                                let estimated_server_subs = sub_tracker.server_visible_count()
                                    as u64
                                    + stats.subs_leaked_no_ack;
                                let threshold =
                                    (HELIUS_SUB_CAP as f64 * SUB_CAP_RECONNECT_THRESHOLD) as u64;
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
                                    let active_mints_vec = sub_tracker.clear_server_subs();
                                    match WsConn::connect(&helius_url) {
                                        Ok(mut c) => {
                                            let _ = c.set_read_timeout(Duration::from_millis(
                                                WS_READ_TIMEOUT_MS,
                                            ));
                                            let _ =
                                                c.send_text(&helius_ws::slot_subscribe_request());
                                            for mint in &active_mints_vec {
                                                let pda = bonding_curve_pda(mint);
                                                let pda_str = pda.to_string();
                                                let req_id = next_req_id;
                                                next_req_id += 1;
                                                let req = helius_ws::account_subscribe_request(
                                                    req_id,
                                                    &pda_str,
                                                    &args.commitment,
                                                );
                                                let _ = c.send_text(&req);
                                                sub_tracker.record_request(req_id, *mint);
                                            }
                                            helius_conn_established_at = Instant::now();
                                            last_slot_time = Instant::now();
                                            helius_conn = c;
                                            eprintln!("[pq-daemon] PROACTIVE reconnect succeeded");
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
                                    // Held positions keep their reserve feed regardless of discovery pressure.
                                    let protected: std::collections::HashSet<[u8; 32]> =
                                        if model_armed {
                                            engine.model_held_mints().into_iter().collect()
                                        } else {
                                            std::collections::HashSet::new()
                                        };
                                    if let Some((evicted_req, evicted_mint, evicted_server_sub)) =
                                        sub_tracker.evict_oldest_protecting(&protected)
                                    {
                                        stats.account_subs_evicted += 1;
                                        reserve_tracker.remove(&evicted_mint);
                                        // Track leaked subscriptions: if the ACK
                                        // never arrived (server_sub_id is None),
                                        // we CANNOT send accountUnsubscribe, and
                                        // the server-side slot leaks permanently
                                        // until TCP timeout. This is the root cause
                                        // of the 1000-sub cap death spiral.
                                        if let Some(ssid) = evicted_server_sub {
                                            // Send accountUnsubscribe to release the Helius
                                            // server-side subscription slot. Without this the
                                            // connection leaks subscriptions until Helius caps
                                            // at 1000, after which no new accountSubscribe
                                            // succeeds and ALL new candidates fail with
                                            // NeedsOnchainConfirmation.
                                            let unsub_id = next_req_id;
                                            next_req_id += 1;
                                            let unsub = helius_ws::account_unsubscribe_request(
                                                unsub_id, ssid,
                                            );
                                            if let Err(e) = helius_conn.send_text(&unsub) {
                                                eprintln!(
                                                "[pq-daemon] accountUnsubscribe send error (server_sub={ssid}): {e}"
                                            );
                                                stats.ws_errors += 1;
                                            }
                                        } else {
                                            stats.subs_leaked_no_ack += 1;
                                            eprintln!(
                                                "[pq-daemon] EVICT LEAK: req={evicted_req} \
                                             mint={:.8} — ACK never arrived, server slot \
                                             leaked (total leaked: {})",
                                                hex_short(&evicted_mint),
                                                stats.subs_leaked_no_ack
                                            );
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
                                    req_id,
                                    &pda_str,
                                    &args.commitment,
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
                            } // Rev-30: close else (WS fallback) block
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
                    if let Some(tx) =
                        pump_quant_ingest::pumpportal_parse::parse_pumpportal(text.as_bytes())
                    {
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
                                let _ =
                                    c.set_read_timeout(Duration::from_millis(WS_READ_TIMEOUT_MS));
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
            Ok(Some(WsEvent::Binary(_))) => {
                stats.ws_errors += 1;
            }
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
                    Err(_) => {
                        stats.ws_errors += 1;
                        continue;
                    }
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
                                            decode_onchain_confirm_with_curve(
                                                &mb,
                                                &account_data,
                                                slot,
                                            )
                                        {
                                            queue.push(provenanced, slot);
                                            stats.helius_onchain_confirms_decoded += 1;
                                            stats.last_confirm_tick = tick_counter;
                                            stats.pda_venue_matches += 1;

                                            // ── Wangr Rev-14: decode curve tail for token_standard ──
                                            // is_mayhem_mode: None=unknown, Some(false)=legacy SPL,
                                            // Some(true)=mayhem/token2022. Wangr study found legacy
                                            // tokens graduate 5× more often.
                                            if aux_emitted.contains(&mb) {
                                                let token_standard =
                                                    match decode_pump_curve_tail(&account_data) {
                                                        Some(tail) => match tail.is_mayhem_mode {
                                                            Some(true) => 2,  // mayhem/token2022
                                                            Some(false) => 1, // legacy SPL
                                                            None => 0,        // unknown
                                                        },
                                                        None => 0,
                                                    };
                                                queue.push(ProvenancedEvent {
                                                    event: AppEvent::MarketAuxiliary {
                                                        mint: Mint(mb),
                                                        token_standard,
                                                        symbol_len: 0, // already sent from create
                                                    },
                                                    source: ProvenanceSource::HeliusAccountSubscribe,
                                                    slot,
                                                    is_live: true,
                                                }, slot);
                                            }

                                            let prev = reserve_tracker.get(&mb).copied();
                                            // The WS feed records no receive time, so this
                                            // print cannot be windowed on a clock it does not
                                            // have. `None` is the honest value.
                                            if let Some(trade_pe) = derive_market_trade_from_delta(
                                                &mb, prev, &curve, slot, true, None,
                                            ) {
                                                queue.push(trade_pe, slot);
                                                stats.delta_trades_derived += 1;
                                            } else {
                                                stats.delta_no_trade += 1;

                                                if !pump_quant_junction::reserve_delta::delta_representable(prev.as_ref(), &curve) {

                                                    stats.delta_out_of_range += 1;

                                                }
                                            }
                                            reserve_tracker.insert(
                                                mb,
                                                ReserveSnapshot {
                                                    virtual_sol: curve.virtual_sol,
                                                    virtual_token: curve.virtual_token,
                                                    slot,
                                                },
                                            );
                                            // C1: publish these reserves into the curve cache. The hot path (and the
                                            // outbound sink's state fetch) then answers from the stream instead of paying a
                                            // cold 4-RTT fetch. Returns false for a mint whose ctx was never learned — the
                                            // stream carries no fee_recipient, so nothing is fabricated here.
                                            if let Some(fetcher) = live_state_fetcher.as_ref() {
                                                fetcher.note_stream_reserves(
                                                    &mb,
                                                    curve.virtual_sol,
                                                    curve.virtual_token,
                                                    curve.complete,
                                                    slot,
                                                );
                                            }
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
                                let server_sub =
                                    params.and_then(extract_server_sub_id).unwrap_or(0);

                                let mint_bytes = sub_tracker.mint_for_server_sub(server_sub);

                                if let Some(mb) = mint_bytes {
                                    let (data_b64, slot_opt) = extract_account_data(result);
                                    let slot = slot_opt.unwrap_or(0);
                                    if let Some(data_str) = data_b64 {
                                        if let Ok(account_data) = B64.decode(data_str.as_bytes()) {
                                            if let Some((provenanced, curve)) =
                                                decode_onchain_confirm_with_curve(
                                                    &mb,
                                                    &account_data,
                                                    slot,
                                                )
                                            {
                                                queue.push(provenanced, slot);
                                                stats.helius_onchain_confirms_decoded += 1;
                                                stats.last_confirm_tick = tick_counter;
                                                stats.pda_venue_matches += 1;

                                                // ── Wangr Rev-14: decode curve tail for token_standard ──
                                                // is_mayhem_mode: None=unknown, Some(false)=legacy SPL,
                                                // Some(true)=mayhem/token2022. Wangr study found legacy
                                                // tokens graduate 5× more often.
                                                if aux_emitted.contains(&mb) {
                                                    let token_standard =
                                                        match decode_pump_curve_tail(&account_data)
                                                        {
                                                            Some(tail) => match tail.is_mayhem_mode
                                                            {
                                                                Some(true) => 2,  // mayhem/token2022
                                                                Some(false) => 1, // legacy SPL
                                                                None => 0,        // unknown
                                                            },
                                                            None => 0,
                                                        };
                                                    queue.push(ProvenancedEvent {
                                                        event: AppEvent::MarketAuxiliary {
                                                            mint: Mint(mb),
                                                            token_standard,
                                                            symbol_len: 0, // already sent from create
                                                        },
                                                        source: ProvenanceSource::HeliusAccountSubscribe,
                                                        slot,
                                                        is_live: true,
                                                    }, slot);
                                                }

                                                let prev = reserve_tracker.get(&mb).copied();
                                                if let Some(trade_pe) =
                                                    derive_market_trade_from_delta(
                                                        &mb, prev, &curve, slot, true, None,
                                                    )
                                                {
                                                    queue.push(trade_pe, slot);
                                                    stats.delta_trades_derived += 1;
                                                } else {
                                                    stats.delta_no_trade += 1;

                                                    if !pump_quant_junction::reserve_delta::delta_representable(prev.as_ref(), &curve) {

                                                        stats.delta_out_of_range += 1;

                                                    }
                                                }
                                                reserve_tracker.insert(
                                                    mb,
                                                    ReserveSnapshot {
                                                        virtual_sol: curve.virtual_sol,
                                                        virtual_token: curve.virtual_token,
                                                        slot,
                                                    },
                                                );
                                                // C1: publish these reserves into the curve cache. The hot path (and the
                                                // outbound sink's state fetch) then answers from the stream instead of paying a
                                                // cold 4-RTT fetch. Returns false for a mint whose ctx was never learned — the
                                                // stream carries no fee_recipient, so nothing is fabricated here.
                                                if let Some(fetcher) = live_state_fetcher.as_ref() {
                                                    fetcher.note_stream_reserves(
                                                        &mb,
                                                        curve.virtual_sol,
                                                        curve.virtual_token,
                                                        curve.complete,
                                                        slot,
                                                    );
                                                }
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
                            _ => {
                                stats.ws_errors += 1;
                            }
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
                let active_mints_vec = sub_tracker.clear_server_subs();
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
                                let _ =
                                    c.set_read_timeout(Duration::from_millis(WS_READ_TIMEOUT_MS));
                                let _ = c.send_text(&helius_ws::slot_subscribe_request());
                                // GAP #8: record_request() MUST be called for
                                // each re-subscription. Without it, the ACK
                                // from Helius arrives but record_ack() can't
                                // map req_id → mint because record_request
                                // never stored it. server_sub_to_mint stays
                                // empty → all notifications are silently
                                // dropped → 0 OnchainConfirms after reconnect.
                                for mint in &active_mints_vec {
                                    let pda = bonding_curve_pda(mint);
                                    let pda_str = pda.to_string();
                                    let req_id = next_req_id;
                                    next_req_id += 1;
                                    let req = helius_ws::account_subscribe_request(
                                        req_id,
                                        &pda_str,
                                        &args.commitment,
                                    );
                                    let _ = c.send_text(&req);
                                    // CRITICAL: register the req_id → mint
                                    // mapping so the ACK can be resolved.
                                    sub_tracker.record_request(req_id, *mint);
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
            Ok(Some(WsEvent::Binary(_))) => {
                stats.ws_errors += 1;
            }
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
                let active_mints_vec = sub_tracker.clear_server_subs();
                let mut backoff_ms = WS_RECONNECT_SLEEP_MS;
                let mut reconnected = false;
                for _ in 0..MAX_RECONNECT_ATTEMPTS {
                    if !reconnected {
                        match WsConn::connect(&helius_url) {
                            Ok(mut c) => {
                                let _ =
                                    c.set_read_timeout(Duration::from_millis(WS_READ_TIMEOUT_MS));
                                let _ = c.send_text(&helius_ws::slot_subscribe_request());
                                for mint in &active_mints_vec {
                                    let pda = bonding_curve_pda(mint);
                                    let pda_str = pda.to_string();
                                    let req_id = next_req_id;
                                    next_req_id += 1;
                                    let req = helius_ws::account_subscribe_request(
                                        req_id,
                                        &pda_str,
                                        &args.commitment,
                                    );
                                    let _ = c.send_text(&req);
                                    sub_tracker.record_request(req_id, *mint);
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
            let active_mints_vec = sub_tracker.clear_server_subs();
            // GAP #11: Same exponential backoff ladder as the Closed/Err paths.
            let mut backoff_ms = WS_RECONNECT_SLEEP_MS;
            let mut reconnected = false;
            for _ in 0..MAX_RECONNECT_ATTEMPTS {
                if !reconnected {
                    match WsConn::connect(&helius_url) {
                        Ok(mut c) => {
                            let _ = c.set_read_timeout(Duration::from_millis(WS_READ_TIMEOUT_MS));
                            let _ = c.send_text(&helius_ws::slot_subscribe_request());
                            for mint in &active_mints_vec {
                                let pda = bonding_curve_pda(mint);
                                let pda_str = pda.to_string();
                                let req_id = next_req_id;
                                next_req_id += 1;
                                let req = helius_ws::account_subscribe_request(
                                    req_id,
                                    &pda_str,
                                    &args.commitment,
                                );
                                let _ = c.send_text(&req);
                                sub_tracker.record_request(req_id, *mint);
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
                        batch.push(pump_quant_ingest::social_source::RawSocialPayload::new(
                            json_bytes, now_ns,
                        ));
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        if stats.fc_spawned {
                            eprintln!("[pq-daemon] Firecrawl bridge disconnected — social intelligence degraded");
                            stats
                                .stubbed_or_assumed
                                .push("Firecrawl bridge disconnected mid-session".to_string());
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
                    stats
                        .stubbed_or_assumed
                        .push("Firecrawl bridge exited — social intelligence disabled".to_string());
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
                    } // close if let Some(ref mut stdin)
                } // close Ok(None) => arm
                Err(e) => {
                    eprintln!("[pq-daemon] Firecrawl bridge try_wait error: {e}");
                    drop(child.stdin.take());
                    fc_child = None;
                }
            } // close match child.try_wait()
        } // close if let Some(ref mut child) = fc_child

        // ── Periodic Tick (engine evaluate) ──────────────────────────────
        // The FINAL barrier has no later update to trigger it: it fires when the (finite, captured) input has been idle
        // for 3 s of wall time and nothing is held back. Offline replay only.
        if barrier_mode {
            let seen = stats.ls_transactions_received + stats.ls_account_received;
            if seen != barrier_seen_count {
                barrier_seen_count = seen;
                barrier_last_input = Instant::now();
            }
            if !barrier_ready
                && barrier_idx + 1 == barrier_clocks.len()
                && barrier_hold.is_none()
                && barrier_last_input.elapsed() > Duration::from_secs(3)
            {
                barrier_ready = true;
            }
        }
        let barrier_fire = barrier_mode && barrier_ready;
        if barrier_fire || (!barrier_mode && Instant::now() >= next_tick) {
            // ── Wangr Rev-14: inject TimeSignal before each Tick ──
            // The engine stores (dow, hour_utc) and enriches Features at
            // gate-evaluate time, enabling the wangr day-of-week and hour-of-day
            // entry filters. Computed from wall-clock UTC (no chrono dep).
            // Barrier mode derives the time signal from the SOURCE clock, not the wall clock.
            let (dow, hour_utc) = if barrier_fire {
                utc_dow_hour(
                    UNIX_EPOCH + Duration::from_millis(engine.model_clock_ms_now().max(0) as u64),
                )
            } else {
                utc_dow_hour(SystemTime::now())
            };
            let ts_event = AppEvent::TimeSignal { dow, hour_utc };
            engine.tick(ts_event);
            if let Some(ref mut writer) = event_stream_writer {
                if let Err(e) = writer.write_event(&ts_event, last_slot_seen) {
                    eprintln!("[pq-daemon] event_stream TimeSignal write error: {}", e);
                }
            }

            engine.tick(pump_quant_app::event::AppEvent::Tick);
            // Phase 3: write the Tick to the event stream so the replay engine
            // can reproduce the evaluate() calls. Without Ticks in the stream,
            // the engine replay never triggers admission decisions — making
            // the refiner's engine-replay subprocess useless.
            if let Some(ref mut writer) = event_stream_writer {
                if let Err(e) =
                    writer.write_event(&pump_quant_app::event::AppEvent::Tick, last_slot_seen)
                {
                    eprintln!("[pq-daemon] event_stream tick write error: {}", e);
                }
            }
            next_tick = Instant::now() + tick_period;
            tick_counter += 1;
            // ── OFFLINE PAPER REPLAY barrier: settle the verdicts for the state cut at this source-time clock, apply
            // them (in logical id order) through further PRODUCTION ticks at the SAME clock, then record the logical
            // state. Nothing else about the tick path changes.
            if barrier_fire {
                let mut missing = engine.barrier_settle(Duration::from_secs(20));
                for _ in 0..3 {
                    engine.tick(pump_quant_app::event::AppEvent::Tick);
                    let again = engine.barrier_settle(Duration::from_secs(20));
                    missing = again;
                    if again == 0 && engine.barrier_staged() == 0 {
                        break;
                    }
                }
                if std::env::var("PQ_REPLAY_PERSIST_AT_BARRIERS").as_deref() == Ok("1") {
                    let f = engine.model_flow_flush(Duration::from_secs(20));
                    let h = engine.model_held_persist_now();
                    eprintln!("[pq-daemon] replay barrier {barrier_idx}: flow flush durable={f} held persisted={h}");
                }
                // Barrier mode only: the lane's funnel counters as of THIS barrier (the timed health writer is not run
                // on a barrier schedule, so its copy would be stale).
                if let Ok(j) = serde_json::to_string(&engine.model_lane_report()) {
                    let _ = std::fs::write("data/model_lane_report.json", j);
                }
                let lines = engine.barrier_state_lines();
                let digest = engine.barrier_state_digest();
                let log = engine.barrier_take_log();
                let rec = serde_json::json!({
                    "barrier": barrier_idx,
                    "clock": barrier_clocks[barrier_idx],
                    "engine_clock_ms": engine.model_clock_ms_now(),
                    "state_digest": digest,
                    "state": lines,
                    "dispatches": log.iter().map(|d| serde_json::json!({
                        "id": d.id, "kind": d.kind,
                        "mint": d.mint.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                        "prompt_sha256": d.prompt_sha256,
                    })).collect::<Vec<_>>(),
                    "foreign_verdicts": engine.barrier_foreign(),
                    "unsettled": missing,
                });
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open("data/barrier_log.jsonl")
                {
                    use std::io::Write as _;
                    let _ = writeln!(f, "{rec}");
                }
                if let Some(pause) = std::env::var("PQ_REPLAY_PAUSE_AFTER_BARRIER")
                    .ok()
                    .and_then(|v| v.parse::<usize>().ok())
                {
                    if barrier_idx == pause {
                        let _ = std::fs::write("data/BARRIER_PAUSED", barrier_idx.to_string());
                        while std::path::Path::new("data/BARRIER_PAUSED").exists() {
                            std::thread::sleep(Duration::from_millis(50));
                        }
                    }
                }
                barrier_idx += 1;
                barrier_ready = false;
            }

            // ── E3: drain finished async submissions ────────────────────
            // The decision thread handed these off without waiting. Reporting the
            // verdict back is where the accounting happens: acceptance registers the
            // pending tx for the confirmation poller below, refusal reverses a
            // phantom buy position. Ordering matters — this runs BEFORE the poll so a
            // just-accepted signature is eligible on the same pass.
            if let Some(sink) = async_outbound {
                for r in sink.drain_results() {
                    let (accepted, submit_rpc_us) = match &r.outcome {
                        pump_quant_execution::ex_outbound_sink::OutboundOutcome::Accepted {
                            signature,
                            submit_rpc_us,
                        } => (Some(*signature), *submit_rpc_us),
                        _ => (None, 0),
                    };
                    match accepted {
                        Some(sig) if sig != [0u8; 64] => {
                            if engine.complete_async_outbound(
                                r.ticket,
                                sig,
                                r.worker_us,
                                submit_rpc_us,
                            ) {
                                eprintln!(
                                    "[pq-daemon] outbound ticket={} {:?} ACCEPTED in {}µs (worker)",
                                    r.ticket,
                                    if r.record.is_buy { "buy" } else { "sell" },
                                    r.worker_us
                                );
                            } else {
                                eprintln!(
                                    "[pq-daemon] outbound ticket={} verdict for an evicted ticket — ignored (never treated as a confirmation)",
                                    r.ticket
                                );
                            }
                        }
                        _ => {
                            if engine.fail_async_outbound(r.ticket, r.worker_us) {
                                eprintln!(
                                    "[pq-daemon] outbound ticket={} {:?} NOT SUBMITTED ({:?}) after {}µs",
                                    r.ticket,
                                    if r.record.is_buy { "buy" } else { "sell" },
                                    r.outcome,
                                    r.worker_us
                                );
                            }
                        }
                    }
                    sink.note_delivered();
                }
            }

            // ── Rev-19: On-chain confirmation feedback poll ──────────────
            // Poll getSignaturesForAddress for our wallet every
            // confirm_poll_interval_secs to check if pending buy/sell txs
            // landed on-chain. Feed the results back into the engine as
            // OurBuyConfirmed/OurBuyFailed/OurSellConfirmed/OurSellFailed.
            if args.live_mode && Instant::now() >= next_confirm_poll {
                next_confirm_poll =
                    Instant::now() + Duration::from_secs(confirm_poll_interval_secs);
                let pending_buys = engine.pending_buy_signatures();
                let pending_sells = engine.pending_sell_signatures();
                if !pending_buys.is_empty() || !pending_sells.is_empty() {
                    let results = poll_signature_confirmations(
                        &rpc_url_for_confirm,
                        &wallet_for_confirm,
                        &pending_buys,
                        &pending_sells,
                    );
                    for r in &results {
                        let mint_bytes = if r.kind == "buy" {
                            pending_buys
                                .iter()
                                .find(|(_, s)| s == &r.signature)
                                .map(|(m, _)| *m)
                        } else {
                            pending_sells
                                .iter()
                                .find(|(_, s)| s == &r.signature)
                                .map(|(m, _)| *m)
                        };
                        let mint_bytes = match mint_bytes {
                            Some(m) => m,
                            None => continue,
                        };
                        let mint = pump_quant_domain::ids::Mint::from_bytes(mint_bytes);
                        let event = match (r.kind, r.confirmed) {
                            ("buy", true) => pump_quant_app::event::AppEvent::OurBuyConfirmed {
                                mint,
                                signature: r.signature,
                                slot: r.slot,
                            },
                            ("buy", false) => pump_quant_app::event::AppEvent::OurBuyFailed {
                                mint,
                                signature: r.signature,
                                err_code: 0, // unknown — RPC doesn't classify error types
                                slot: r.slot,
                            },
                            ("sell", true) => pump_quant_app::event::AppEvent::OurSellConfirmed {
                                mint,
                                signature: r.signature,
                                slot: r.slot,
                            },
                            ("sell", false) => pump_quant_app::event::AppEvent::OurSellFailed {
                                mint,
                                signature: r.signature,
                                err_code: 0, // unknown — RPC doesn't classify error types
                                slot: r.slot,
                            },
                            _ => continue,
                        };
                        engine.tick(event);
                    }
                    if !results.is_empty() {
                        eprintln!(
                            "[pq-daemon] confirmation poll: {}/{} pending txs resolved",
                            results.len(),
                            pending_buys.len() + pending_sells.len()
                        );
                    }
                }

                // Rev-31 (2026-08-21): Evict stale pending txs that the poll
                // never resolved. A pending buy/sell older than 30s (120 ticks
                // at 250ms/tick) is assumed to have failed or never landed.
                // For stale buys: reverse the paper position (phantom fix).
                // For stale sells: reverse the paper exit (ladder can retry).
                // This is the safety net that prevents permanent phantom
                // positions when the poll misses a tx.
                {
                    let stale_threshold_ticks: u64 = 120; // ~30s at 250ms/tick
                    let evicted = engine.evict_stale_pending(stale_threshold_ticks);
                    if evicted > 0 {
                        eprintln!(
                            "[pq-daemon] stale pending eviction: {evicted} txs evicted (phantom positions reversed)"
                        );
                    }
                }
            }

            // ── Periodic status write ────────────────────────────────────
            // Tick-count based status write. The wall-clock heartbeat at
            // the top of the loop handles the event-starvation case.
            // Held-position data readiness: MEASURED (reserve age vs the 60 s pricing bound, management
            // prompt cuttable now), reported on the status cadence. A degraded held position is stated
            // loudly; the 60 s bound is never loosened to make refusals disappear.
            // Durable state must never fail silently: check headroom on the safety file's filesystem and say
            // so loudly before a write can fail. (A failed persist is already fail-closed in the engine.)
            #[allow(clippy::manual_is_multiple_of)] // MSRV 1.85: is_multiple_of stabilised in 1.87
            if model_armed && tick_counter % 6000 == 0 {
                let sf = std::env::var("PQ_MODEL_SAFETY_FILE").unwrap_or_else(|_| {
                    pump_quant_junction::model_lifecycle::DEFAULT_SAFETY_FILE.to_string()
                });
                match pump_quant_junction::model_lifecycle::check_headroom(
                    std::path::Path::new(&sf),
                    pump_quant_junction::model_lifecycle::MIN_FREE_BYTES,
                ) {
                    pump_quant_junction::model_lifecycle::Headroom::Low { free, floor } => eprintln!(
                        "[pq-daemon] ALERT: DISK HEADROOM LOW free={free} < floor={floor} on the durable-state filesystem; \
                         journal/safety writes may fail (persist failures count: {})",
                        engine.model_safety_persist_failures()
                    ),
                    pump_quant_junction::model_lifecycle::Headroom::Unknown => {
                        eprintln!("[pq-daemon] WARN: disk headroom could not be measured for {sf}");
                    }
                    pump_quant_junction::model_lifecycle::Headroom::Ok { .. } => {}
                }
            }
            // OFFLINE PAPER REPLAY HARNESS ONLY: a sentinel file asks for a blocking durable flow-history flush, so a test can
            // compare checkpoints at a fixed cursor (graceful stop with held positions deliberately does not terminate, so the
            // shutdown flush is unreachable there). Inert unless PQ_OFFLINE_PAPER_REPLAY=1; never reachable in a normal daemon.
            if model_armed && replay_harness && std::path::Path::new("data/FLOW_FLUSH").exists() {
                let ok = engine.model_flow_flush(std::time::Duration::from_secs(20));
                let _ = std::fs::remove_file("data/FLOW_FLUSH");
                let _ = std::fs::write("data/FLOW_FLUSHED", if ok { "ok" } else { "timeout" });
                eprintln!("[pq-daemon] replay-harness flow flush: durable={ok}");
            }
            #[allow(clippy::manual_is_multiple_of)] // MSRV 1.85: is_multiple_of stabilised in 1.87
            if model_armed && (tick_counter % 20 == 0 || barrier_fire) {
                let now_ms = engine.model_clock_ms_now();
                for l in stale_callout.evaluate(&engine, now_ms, 60_000) {
                    eprintln!(
                        "[pq-daemon] {}HELD-DATA {}",
                        if l.alert { "ALERT: " } else { "" },
                        l.text
                    );
                }
                #[allow(clippy::manual_is_multiple_of)]
                // MSRV 1.85: is_multiple_of stabilised in 1.87
                if tick_counter % args.status_every_ticks.max(1) == 0 {
                    let (report, _) =
                        pump_quant_junction::model_lifecycle::held_data_report(&engine);
                    if !report.is_empty() {
                        eprintln!("[pq-daemon] HELD-DATA status\n{report}");
                    }
                }
            }
            if tick_counter - last_status_write_tick >= args.status_every_ticks {
                let st = engine.live_status();
                match st.write_to_path(status_path) {
                    Ok(()) => {}
                    Err(e) => eprintln!("[pq-daemon] live_status write failed: {e}"),
                }
                // Best-effort open-positions telemetry dump.
                {
                    let snaps = engine.open_positions_snapshot();
                    let open_path = std::path::Path::new("data/open_positions.json");
                    let _ = pump_quant_app::live_status::OpenPositionSnapshot::write_to_path(
                        &snaps, open_path,
                    );
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
                    let lane = if t.scalp {
                        TapeLane::Scalp
                    } else {
                        TapeLane::Early
                    };
                    let net = t.gross as i64 - t.fees as i64 - t.tips as i64 - t.failed as i64;
                    // Emit enriched TradeFull record (16-field format for replay).
                    // Fields not yet available from engine.take_tape_trades() are
                    // zeroed — future enrichment will populate them from the
                    // decision journal and position exit context.
                    let mint_b58 = Pubkey::from(t.mint).to_string();
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
                        decision_latency_us: t.decision_to_submit_us,
                        confirm_latency_us: t.submit_to_confirm_us,
                        run_mode_tag,
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
                    let trade_lane = if t.scalp {
                        TradeLane::Scalp
                    } else {
                        TradeLane::Early
                    };
                    let rec = TradeRecord {
                        slot: last_slot_seen,
                        mint_b58: Pubkey::from(t.mint).to_string(),
                        side: TradeSide::Buy,
                        entry_price_fp: t.entry_price_fp as i128,
                        exit_price_fp: t.exit_price_fp as i128,
                        size_lamports: t.size_lamports,
                        strategy_id: t.archetype as u64,
                        source: if t.scalp {
                            ProvenanceSource::HeliusAccountSubscribe
                        } else {
                            ProvenanceSource::PumpPortalTrade
                        },
                        outcome: if net >= 0 {
                            TradeOutcome::Filled
                        } else {
                            TradeOutcome::FilledWithSlippage
                        },
                        realized_pnl_lamports: net,
                        fees_lamports: (t.fees + t.tips) as u64,
                        slippage_lamports: t.failed as u64,
                        decision_latency_us: t.decision_to_submit_us,
                        submit_call_us: t.submit_call_us,
                        submit_rpc_us: t.submit_rpc_us,
                        exit_submit_rpc_us: t.exit_submit_rpc_us,
                        confirm_latency_us: t.submit_to_confirm_us,
                        exit_submit_call_us: t.exit_submit_call_us,
                        run_mode: journal_run_mode,
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
                        let cumulative_pnl =
                            prior_tape_pnl.saturating_add(st.net_realized_lamports as i64);
                        // Snapshot the cumulative trade count at promotion time
                        // so we can compute trades-since-promotion for the
                        // variance-based auto-revert threshold.
                        let cumulative_trades =
                            prior_tape_trades.saturating_add(tape_exporter.total_exported());
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
                if auto_revert_state.promoted_fingerprint != 0 && !auto_revert_state.reverted {
                    let ticks_since = tick_counter.saturating_sub(promotion_tick);
                    let st = engine.live_status();
                    let cumulative_pnl =
                        prior_tape_pnl.saturating_add(st.net_realized_lamports as i64);
                    // Compute trades-since-promotion for the variance-based
                    // auto-revert threshold.
                    let cumulative_trades =
                        prior_tape_trades.saturating_add(tape_exporter.total_exported());
                    let trades_since = cumulative_trades.saturating_sub(trades_at_promotion);
                    let current_fp = config_fingerprint(&cfg.dump_to_text());
                    if let Some(revert_config_text) = check_auto_revert(
                        current_fp,
                        cumulative_pnl as i128,
                        ticks_since,
                        trades_since,
                    ) {
                        // Revert: parse the archived champion config back
                        eprintln!("[pq-daemon] AUTO-REVERT: reverting config to archived champion");
                        if let Ok(reverted_cfg) =
                            pump_quant_app::config::Config::from_str_over_default(
                                &revert_config_text,
                            )
                        {
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
                defense_state.update_drawdown(st.net_realized_lamports.max(0) as i64);
                if !defense_state.trading_allowed() {
                    eprintln!(
                        "[pq-daemon] DEFENSE-IN-DEPTH: TRADING HALTED — reason: {:?}",
                        defense_state.kill_reason()
                    );
                    // Write an EMERGENCY_STOP sentinel so the operator sees it.
                    let _ = std::fs::write(EMERGENCY_STOP_FILE, "defense-in-depth automatic halt");
                    // Kill LaserStream child if present — tree-kill to prevent
                    // orphaned gRPC processes burning Helius credits. GAP #13.
                    if let Some(ref mut child) = ls_child {
                        kill_process_tree(child);
                    }
                    if let Some(ref mut child) = fc_child {
                        kill_process_tree(child);
                    }
                    return ExitCode::from(EXIT_EMERGENCY);
                }
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
        let p99_idx = ((n as f64 * 0.99).ceil() as u64)
            .saturating_sub(1)
            .min(n - 1);
        stats.dwell_p99_ms = dwell_samples[p99_idx as usize];
    }

    // Durable flow history: force one consistent snapshot and wait (bounded) until it is on disk. A failure is reported and
    // the previous checkpoint stays authoritative (the durable cursor does not advance).
    if engine.paper_model_enabled() {
        let ok = engine.model_flow_flush(std::time::Duration::from_secs(20));
        eprintln!(
            "[pq-daemon] flow-history final flush: {}",
            if ok {
                "durable"
            } else {
                "NOT durable (previous checkpoint remains authoritative)"
            }
        );
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
        let _lane = if t.scalp {
            TapeLane::Scalp
        } else {
            TapeLane::Early
        };
        let net = t.gross as i64 - t.fees as i64 - t.tips as i64 - t.failed as i64;
        let mint_b58 = Pubkey::from(t.mint).to_string();
        // Feed final trades to memory bank too
        let trade_lane = if t.scalp {
            TradeLane::Scalp
        } else {
            TradeLane::Early
        };
        let rec = TradeRecord {
            slot: last_slot_seen,
            mint_b58: mint_b58.clone(),
            side: TradeSide::Buy,
            entry_price_fp: t.entry_price_fp as i128,
            exit_price_fp: t.exit_price_fp as i128,
            size_lamports: t.size_lamports,
            strategy_id: t.archetype as u64,
            source: if t.scalp {
                ProvenanceSource::HeliusAccountSubscribe
            } else {
                ProvenanceSource::PumpPortalTrade
            },
            outcome: if net >= 0 {
                TradeOutcome::Filled
            } else {
                TradeOutcome::FilledWithSlippage
            },
            realized_pnl_lamports: net,
            fees_lamports: (t.fees + t.tips) as u64,
            slippage_lamports: t.failed as u64,
            decision_latency_us: t.decision_to_submit_us,
            submit_call_us: t.submit_call_us,
            submit_rpc_us: t.submit_rpc_us,
            exit_submit_rpc_us: t.exit_submit_rpc_us,
            confirm_latency_us: t.submit_to_confirm_us,
            exit_submit_call_us: t.exit_submit_call_us,
            run_mode: journal_run_mode,
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
            decision_latency_us: t.decision_to_submit_us,
            confirm_latency_us: t.submit_to_confirm_us,
            run_mode_tag,
            error_code: t.exit_reason_code as u32,
            seq: 0,
            mfe_bps: t.mfe_bps,
            mae_bps: t.mae_bps,
        });
    }
    // ── E3: outbound worker totals ──────────────────────────────────────
    // Nothing here was surfaced while the daemon ran except per-verdict lines; the
    // summary makes a saturated/refusing queue auditable after the fact.
    if let Some(sink) = async_outbound {
        eprintln!(
            "[pq-daemon] outbound lanes: {} verdicts delivered, {} refused, {} still in flight, per-lane refusals {:?}",
            sink.delivered(),
            sink.refused(),
            sink.in_flight(),
            sink.lane_stats()
        );
        if sink.in_flight() > 0 {
            eprintln!(
                "[pq-daemon] WARNING: {} submission(s) never reported a verdict — the chain state of those txs is unknown",
                sink.in_flight()
            );
        }
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
    println!(
        "  slot_notifications:        {}",
        stats.helius_slot_notifications
    );
    println!(
        "  account_notifications:     {}",
        stats.helius_account_notifications
    );
    println!(
        "  onchain_confirms_decoded:  {}",
        stats.helius_onchain_confirms_decoded
    );
    println!("  reconnects:                {}", stats.helius_reconnects);
    println!();
    println!("-- LaserStream gRPC --");
    println!(
        "  transactions_received:  {}",
        stats.ls_transactions_received
    );
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
    println!(
        "  overflow_dropped:      {}",
        stats.junction_overflow_dropped
    );
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
            println!(
                "  mint={} entry={:.6} unrealized_pnl={:.6} remaining={}bps",
                Pubkey::from(pos.mint),
                entry_sol,
                pnl_sol,
                pos.remaining_bps
            );
        }
    }
    println!();
    println!("-- Errors --");
    println!("  ws_errors:             {}", stats.ws_errors);
    println!();
    println!("[pq-daemon] shutdown complete — exit 0");

    ExitCode::SUCCESS
}

#[cfg(test)]
mod held_feed_tests {
    use super::*;

    fn m(i: u8) -> [u8; 32] {
        [i; 32]
    }

    #[test]
    fn eviction_never_removes_a_held_positions_reserve_subscription() {
        let mut t = SubTracker::new();
        // oldest first: 1 (held), 2, 3
        for (i, id) in [(1u8, 10u64), (2, 11), (3, 12)] {
            t.record_request(id, m(i));
        }
        let protected: std::collections::HashSet<[u8; 32]> = [m(1)].into_iter().collect();
        let (_, evicted, _) = t
            .evict_oldest_protecting(&protected)
            .expect("an unprotected one exists");
        assert_eq!(
            evicted,
            m(2),
            "the oldest UNPROTECTED subscription goes; the held one stays"
        );
        assert!(t.active_mints().iter().any(|(_, mm)| *mm == m(1)));
        // Control: the legacy path would have evicted the held one (oldest).
        let mut t2 = SubTracker::new();
        for (i, id) in [(1u8, 10u64), (2, 11), (3, 12)] {
            t2.record_request(id, m(i));
        }
        assert_eq!(t2.evict_oldest().unwrap().1, m(1));
    }

    #[test]
    fn when_every_subscription_is_held_nothing_is_evicted_and_the_caller_declines_the_new_one() {
        let mut t = SubTracker::new();
        for (i, id) in [(1u8, 10u64), (2, 11)] {
            t.record_request(id, m(i));
        }
        let protected: std::collections::HashSet<[u8; 32]> = [m(1), m(2)].into_iter().collect();
        assert!(t.evict_oldest_protecting(&protected).is_none());
        assert_eq!(t.len(), 2);
    }
}
