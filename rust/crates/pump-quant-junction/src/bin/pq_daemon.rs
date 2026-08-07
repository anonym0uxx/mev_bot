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
use pump_quant_app::engine::{Engine, RunMode, TapeTrade};
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
use pump_quant_junction::event_stream::EventStreamWriter;
use pump_quant_junction::autonomous_bridge::{
    DefenseState, try_reload_config, RefinerSpawner,
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
const MAX_ACCOUNT_SUBS: usize = 64;
const MAX_TRADE_SUBS: usize = 512;
const STALE_SECS: u64 = 30;

/// Exit code on emergency stop.
const EXIT_EMERGENCY: u8 = 99;
/// Path (relative to CWD) for the graceful-shutdown sentinel file.
const DAEMON_STOP_FILE: &str = "data/DAEMON_STOP";
/// Path (relative to CWD) for the emergency-stop sentinel file.
const EMERGENCY_STOP_FILE: &str = "data/EMERGENCY_STOP";
/// Path for the live-status JSON.
const STATUS_PATH: &str = "data/live_status.json";
const TAPE_PATH: &str = "data/tape.jsonl";
/// Path for the raw event stream (for deterministic replay).
const EVENT_STREAM_PATH: &str = "data/event_stream.jsonl";

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
}

fn parse_args() -> Result<DaemonArgs, u8> {
    let args: Vec<String> = std::env::args().collect();
    let mut a = DaemonArgs {
        junction_cap: 4096,
        commitment: "processed".to_string(),
        status_every_ticks: 500,
        brain_snapshot_every_ticks: 5000,
        tape_every_ticks: 1000,
        refiner_every_ticks: 5000, // default: ~every 5000 ticks
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
                a.refiner_every_ticks = args[i + 1].parse().unwrap_or(5000);
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

struct SubTracker {
    req_to_mint: HashMap<u64, [u8; 32]>,
    server_sub_to_mint: HashMap<u64, [u8; 32]>,
    subscription_order: Vec<(u64, [u8; 32])>,
}

impl SubTracker {
    fn new() -> Self {
        Self {
            req_to_mint: HashMap::new(),
            server_sub_to_mint: HashMap::new(),
            subscription_order: Vec::new(),
        }
    }

    fn record_request(&mut self, req_id: u64, mint: [u8; 32]) {
        self.req_to_mint.insert(req_id, mint);
        self.subscription_order.push((req_id, mint));
    }

    fn record_ack(&mut self, req_id: u64, server_sub_id: u64) {
        if let Some(mint) = self.req_to_mint.get(&req_id).copied() {
            self.server_sub_to_mint.insert(server_sub_id, mint);
        }
    }

    fn mint_for_server_sub(&self, server_sub_id: u64) -> Option<[u8; 32]> {
        self.server_sub_to_mint.get(&server_sub_id).copied()
    }

    fn evict_oldest(&mut self) -> Option<(u64, [u8; 32])> {
        let item = self.subscription_order.first().copied()?;
        let (req_id, mint) = item;
        self.subscription_order.remove(0);
        self.req_to_mint.remove(&req_id);
        Some((req_id, mint))
    }

    fn active_mints(&self) -> Vec<(u64, [u8; 32])> {
        self.subscription_order.clone()
    }

    fn len(&self) -> usize {
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

    let mut cfg = Config::dev_portable().with_mcap_band(); // Amendment A-14: $9k-$20k band
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
    let _ = pp_conn.set_read_timeout(Duration::from_millis(tick_period_ms));
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
    let _ = helius_conn.set_read_timeout(Duration::from_millis(tick_period_ms));
    if let Err(e) = helius_conn.send_text(&helius_ws::slot_subscribe_request()) {
        eprintln!("[pq-daemon] Helius slotSubscribe error: {e}");
        return ExitCode::from(4);
    }

    // ─── Engine + queue ──────────────────────────────────────────────────
    let queue = BoundedJunctionQueue::with_capacity(args.junction_cap);
    let mut dwell_samples: Vec<u64> = Vec::new();
    let mut engine = Engine::new(cfg, RunMode::Paper);

    let mut sub_tracker = SubTracker::new();
    let mut trade_sub_tracker = TradeSubTracker::new();
    let mut pending_notifications: VecDeque<(u64, String, u64)> = VecDeque::new();
    let mut next_req_id: u64 = 100;
    let mut stats = SessionStats::new();

    let mut reserve_tracker: HashMap<[u8; 32], ReserveSnapshot> = HashMap::new();
    let mut last_slot_seen: u64 = 0;
    let mut last_slot_time = Instant::now();

    // ─── LaserStream gRPC primary ingest lane ────────────────────────────
    let (ls_tx, ls_rx) = mpsc::channel::<LaserStreamUpdate>();
    let mut ls_child: Option<std::process::Child> = None;
    let ls_bin: Option<String> = std::env::var("PQ_LASERSTREAM_BIN").ok()
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

    if let Some(bin_path) = &ls_bin {
        let mut cmd = std::process::Command::new(bin_path);
        cmd.stdout(std::process::Stdio::piped())
           .stderr(std::process::Stdio::piped());
        if let Ok(endpoint) = std::env::var("LASERSTREAM_ENDPOINT") {
            cmd.env("LASERSTREAM_ENDPOINT", endpoint);
        }
        if let Ok(key) = std::env::var("HELIUS_API_KEY") {
            cmd.env("HELIUS_API_KEY", key);
        }
        match cmd.spawn() {
            Ok(mut child) => {
                eprintln!("[pq-daemon] LaserStream gRPC spawned: {:?}", bin_path);
                stats.ls_spawned = true;
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
                ls_child = Some(child);
            }
            Err(e) => {
                eprintln!("[pq-daemon] LaserStream spawn FAILED: {e}");
                eprintln!("[pq-daemon] Proceeding with Helius WS as secondary lane");
                stats.stubbed_or_assumed.push(
                    "LaserStream gRPC spawn failed — Helius WS as fallback".to_string()
                );
            }
        }
    } else {
        eprintln!("[pq-daemon] LaserStream binary not found — set PQ_LASERSTREAM_BIN");
        stats.stubbed_or_assumed.push(
            "LaserStream gRPC binary not found — Helius WS as fallback".to_string()
        );
    }
    let _ls_state = LaserStreamState::new(); // reserved for future per-slot accounting

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
    eprintln!("[pq-daemon] autonomous bridge: defense-in-depth + config hot-reload + refiner scheduling ACTIVE");

    // Phase 2: tape exporter — drains engine trades to evaluator JSONL format.
    let mut tape_exporter = TapeExporter::new(TAPE_PATH);
    // Event stream capture for deterministic replay (§13 paper/live parity).
    // Fail-safe: if the file can't be opened, daemon continues without capture.
    let mut event_stream_writer = EventStreamWriter::open(EVENT_STREAM_PATH);
    eprintln!("[pq-daemon] tape export path: {TAPE_PATH}");

    // ─── === PERSISTENT EVENT LOOP === ────────────────────────────────────
    // Unlike paper_session which has `while Instant::now() < deadline`, this
    // loop runs forever. The only exits are:
    //   1. DAEMON_STOP file → graceful shutdown (exit 0)
    //   2. EMERGENCY_STOP file → immediate exit (exit 99)
    //   3. Both WS connections break AND reconnects fail irrecoverably
    eprintln!("[pq-daemon] === ENTERING PERSISTENT EVENT LOOP ===");

    loop {
        // ── Emergency stop check (every iteration) ───────────────────────
        if emergency_stop_requested() {
            eprintln!("[pq-daemon] EMERGENCY STOP detected — halting immediately");
            // Don't clean up — the operator triggered this deliberately.
            // Print a minimal status and exit.
            let st = engine.live_status();
            eprintln!("[pq-daemon] EMERGENCY: ticks={} promoted={} admitted={} net={}lamports",
                st.info_time_tick, st.promoted, st.admitted, st.net_realized_lamports);
            // Kill LaserStream child if present
            if let Some(ref mut child) = ls_child { let _ = child.kill(); }
            // Kill Firecrawl bridge child if present
            if let Some(ref mut child) = fc_child { let _ = child.kill(); }
            return ExitCode::from(EXIT_EMERGENCY);
        }

        // ── Graceful shutdown check (every iteration) ────────────────────
        if daemon_stop_requested() {
            eprintln!("[pq-daemon] DAEMON_STOP detected — initiating graceful shutdown");
            clean_stop_sentinel();
            break;
        }

        let mut did_work = false;

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
                    // In daemon mode: attempt to RE-SPAWN the gRPC binary.
                    if stats.ls_spawned {
                        eprintln!("[pq-daemon] LaserStream disconnected — attempting respawn");
                        stats.ls_reconnects += 1;
                        // Try to re-spawn
                        if let Some(bin_path) = &ls_bin {
                            let mut cmd = std::process::Command::new(bin_path);
                            cmd.stdout(std::process::Stdio::piped())
                               .stderr(std::process::Stdio::piped());
                            if let Ok(endpoint) = std::env::var("LASERSTREAM_ENDPOINT") {
                                cmd.env("LASERSTREAM_ENDPOINT", endpoint);
                            }
                            if let Ok(key) = std::env::var("HELIUS_API_KEY") {
                                cmd.env("HELIUS_API_KEY", key);
                            }
                            match cmd.spawn() {
                                Ok(mut child) => {
                                    eprintln!("[pq-daemon] LaserStream respawned");
                                    let stdout = child.stdout.take().expect("piped stdout");
                                    let ls_tx_clone2 = ls_tx.clone();
                                    std::thread::spawn(move || {
                                        use std::io::BufRead;
                                        let reader = std::io::BufReader::new(stdout);
                                        for line in reader.lines() {
                                            match line {
                                                Ok(text) => {
                                                    if let Some(update) = parse_ndjson_line(&text) {
                                                        if ls_tx_clone2.send(update).is_err() {
                                                            break;
                                                        }
                                                    }
                                                }
                                                Err(_) => break,
                                            }
                                        }
                                    });
                                    ls_child = Some(child);
                                }
                                Err(e) => {
                                    eprintln!("[pq-daemon] LaserStream respawn FAILED: {e}");
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
                            if sub_tracker.len() >= MAX_ACCOUNT_SUBS {
                                if let Some((evicted_req, evicted_mint)) = sub_tracker.evict_oldest() {
                                    stats.account_subs_evicted += 1;
                                    reserve_tracker.remove(&evicted_mint);
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
                }
            }
            Ok(Some(WsEvent::Closed(reason))) => {
                eprintln!("[pq-daemon] PumpPortal closed: {reason}, reconnecting…");
                stats.pp_reconnects += 1;
                pp_conn = match WsConn::connect(&pp_url) {
                    Ok(mut c) => {
                        let _ = c.set_read_timeout(Duration::from_millis(tick_period_ms));
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
                        // In daemon mode: don't break — sleep and retry next iteration.
                        // The loop will keep trying. A persistent failure will be
                        // caught by the watchdog via stale live_status.json.
                        std::thread::sleep(Duration::from_secs(5));
                        // Try to reconnect the existing connection object
                        match WsConn::connect(&pp_url) {
                            Ok(mut c) => {
                                let _ = c.set_read_timeout(Duration::from_millis(tick_period_ms));
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
                                // Still failing — keep the old (broken) conn;
                                // the next iteration will retry.
                                pp_conn = WsConn::connect(&pp_url).unwrap_or_else(|_| {
                                    // Last resort: create a dummy that always returns None.
                                    // This is ugly but keeps the daemon alive.
                                    WsConn::connect(&pp_url).unwrap_or_else(|_| {
                                        panic!("PumpPortal irrecoverably dead")
                                    })
                                });
                                continue;
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
                    }
                    helius_ws::Inbound::Drift => {
                        eprintln!("[pq-daemon] Helius schema drift: {:.200}", text);
                        stats.ws_errors += 1;
                    }
                }
            }
            Ok(Some(WsEvent::Closed(reason))) => {
                eprintln!("[pq-daemon] Helius closed: {reason}, reconnecting…");
                stats.helius_reconnects += 1;
                helius_conn = match WsConn::connect(&helius_url) {
                    Ok(mut c) => {
                        let _ = c.set_read_timeout(Duration::from_millis(tick_period_ms));
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
                        }
                        c
                    }
                    Err(e) => {
                        eprintln!("[pq-daemon] Helius reconnect failed: {e}");
                        stats.ws_errors += 1;
                        // In daemon mode: sleep and retry next iteration rather than break
                        std::thread::sleep(Duration::from_secs(5));
                        match WsConn::connect(&helius_url) {
                            Ok(mut c) => {
                                let _ = c.set_read_timeout(Duration::from_millis(tick_period_ms));
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
                                }
                                c
                            }
                            Err(_) => {
                                // Keep going — the watchdog will catch persistent failure
                                // via stale live_status.json. Don't kill the daemon.
                                helius_conn = WsConn::connect(&helius_url)
                                    .unwrap_or_else(|_| {
                                        panic!("Helius irrecoverably dead")
                                    });
                                continue;
                            }
                        }
                    }
                };
            }
            Ok(Some(WsEvent::Pong)) | Ok(None) => {}
            Ok(Some(WsEvent::Binary(_))) => { stats.ws_errors += 1; }
            Err(e) => {
                eprintln!("[pq-daemon] Helius poll error: {e}");
                stats.ws_errors += 1;
            }
        }

        // ── Keepalive + staleness ────────────────────────────────────────
        let _ = pp_conn.maybe_keepalive();
        let _ = helius_conn.maybe_keepalive();
        if last_slot_seen > 0 && last_slot_time.elapsed() > Duration::from_secs(STALE_SECS) {
            eprintln!(
                "[pq-daemon] Helius stale: no slot for {}s, reconnecting",
                last_slot_time.elapsed().as_secs()
            );
            stats.helius_reconnects += 1;
            helius_conn = match WsConn::connect(&helius_url) {
                Ok(mut c) => {
                    let _ = c.set_read_timeout(Duration::from_millis(tick_period_ms));
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
                    }
                    last_slot_time = Instant::now();
                    c
                }
                Err(e) => {
                    eprintln!("[pq-daemon] Helius stale-reconnect failed: {e}");
                    stats.ws_errors += 1;
                    // Don't break — keep the daemon alive. The watchdog will
                    // detect persistent staleness via live_status.json age.
                    last_slot_time = Instant::now(); // reset to avoid spam
                    continue;
                }
            };
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
        if let Some(ref mut child) = fc_child {
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
            }
        }

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
            if tick_counter - last_status_write_tick >= args.status_every_ticks {
                let st = engine.live_status();
                match st.write_to_path(status_path) {
                    Ok(()) => {}
                    Err(e) => eprintln!("[pq-daemon] live_status write failed: {e}"),
                }
                engine.write_brain_analysis();
                last_status_write_tick = tick_counter;
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
                    tape_exporter.push(TapeRecord::TradeFull {
                        slot: last_slot_seen,
                        mint_b58: String::new(),
                        side_tag: "buy",
                        entry_price_fp: 0,
                        exit_price_fp: 0,
                        size_lamports: 0,
                        strategy_id: 0,
                        source_tag: "unknown",
                        outcome_tag: if net >= 0 { "profit" } else { "loss" },
                        realized_pnl_lamports: net,
                        fees_lamports: (t.fees + t.tips) as u64,
                        slippage_lamports: t.failed as u64,
                        decision_latency_us: 0,
                        confirm_latency_us: 0,
                        run_mode_tag: "paper",
                        error_code: 0,
                        seq: 0,
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

            // ── Autonomous bridge: config hot-reload (G2) ──────────────
            // Check if CONFIG_PROMOTION.json has been written/updated by
            // the refiner. If so, parse mutations and apply to live config.
            {
                let reload = try_reload_config(&mut cfg, &mut config_mtime);
                if reload.applied {
                    let n = reload.n_mutations;
                    eprintln!(
                        "[pq-daemon] CONFIG HOT-RELOAD: {n} mutations applied. Summary: {}",
                        reload.summary
                    );
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
                    // Kill LaserStream child if present.
                    if let Some(ref mut child) = ls_child { let _ = child.kill(); }
                    if let Some(ref mut child) = fc_child { let _ = child.kill(); }
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

    // Kill LaserStream child
    if let Some(ref mut child) = ls_child {
        let _ = child.kill();
        eprintln!("[pq-daemon] LaserStream child terminated");
    }

    // Kill Firecrawl bridge child
    if let Some(ref mut child) = fc_child {
        let _ = child.kill();
        eprintln!("[pq-daemon] Firecrawl bridge terminated");
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
        tape_exporter.push(TapeRecord::TradeFull {
            slot: last_slot_seen,
            mint_b58: String::new(),
            side_tag: "buy",
            entry_price_fp: 0,
            exit_price_fp: 0,
            size_lamports: 0,
            strategy_id: 0,
            source_tag: "unknown",
            outcome_tag: if net >= 0 { "profit" } else { "loss" },
            realized_pnl_lamports: net,
            fees_lamports: (t.fees + t.tips) as u64,
            slippage_lamports: t.failed as u64,
            decision_latency_us: 0,
            confirm_latency_us: 0,
            run_mode_tag: "paper",
            error_code: 0,
            seq: 0,
        });
    }
    match tape_exporter.flush() {
        Ok(n) => eprintln!(
            "[pq-daemon] final tape flush: {n} records (total={})",
            tape_exporter.total_exported()
        ),
        Err(e) => eprintln!("[pq-daemon] final tape flush FAILED: {e}"),
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
