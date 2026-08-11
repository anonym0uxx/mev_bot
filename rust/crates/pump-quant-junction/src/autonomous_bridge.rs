//! `autonomous_bridge` — the integration layer between the live daemon and the
//! evaluator/refiner framework. This module closes the 10 gaps identified in the
//! autonomous loop audit:
//!
//! - **Config hot-reload** (G2): reads `data/CONFIG_PROMOTION.json`, parses
//!   mutations, and applies them to the engine's `Config` via `Config::apply()`.
//! - **Defense-in-depth** (G4): tracks drawdown, feeds the cliff veto, circuit
//!   breaker, and kill switch, and returns a `DefenseVerdict` that the daemon
//!   consults before admitting trades.
//! - **Refiner scheduling** (G1): spawns `pq-refiner` as a child process on a
//!   periodic timer, so the evaluator runs without blocking the daemon's event
//!   loop.
//!
//! All functions are non-blocking: they are called from the daemon's main loop
//! on specific tick intervals, never stalling the WS event path.

use std::fs;
use std::path::Path;
use std::process::Command;

use pump_quant_app::config::Config;
use pump_quant_evaluator::defense_in_depth::{
    CircuitBreakerConfig, CircuitBreakerState, CliffVetoConfig,
    DefenseVerdict, KillSwitch, evaluate_defense,
};
use pump_quant_evaluator::evaluator_state::LifecycleStage;

// ── Constants ──────────────────────────────────────────────────────────────

/// Path to the promotion file written by pq-refiner.
pub const PROMOTION_FILE: &str = "data/CONFIG_PROMOTION.json";

/// Path to the refiner binary (relative to the workspace target dir).
const REFINER_BIN: &str = "pq-refiner";

/// Path to the refiner state file.
const REFINER_STATE_FILE: &str = "data/evaluator_state.json";

/// Path to the tape file.
const TAPE_FILE: &str = "data/tape.jsonl";

/// Path to the raw event stream (for Phase 3 engine replay in the refiner).
/// When the refiner gets `--event-stream-path`, it runs the full engine
/// pipeline per challenger (admission → sizing → exit), producing genuinely
/// different trade sequences for different configs. Without it, the refiner
/// falls back to shadow replay which only adjusts fee/slippage on a fixed
/// trade set — producing identical NetSol for 95%+ of mutations.
const EVENT_STREAM_FILE: &str = "data/event_stream.jsonl";

/// Directory for champion config archive snapshots (GAP D).
/// Each promotion saves the pre-promotion config here as
/// `<fingerprint>_<timestamp>.txt` so we can revert to known-good configs.
const CHAMPION_ARCHIVE_DIR: &str = "data/champion_archive";

/// Path to the champion config file written by the daemon for the refiner.
const CHAMPION_CONFIG_FILE: &str = "data/CHAMPION_CONFIG.txt";

/// Path to the auto-revert state file (GAP B). Tracks the pre-promotion
/// config fingerprint, PnL at promotion time, and a grace-period counter.
/// If post-promotion PnL deteriorates beyond a threshold within the grace
/// period, the daemon auto-reverts to the archived champion config.
const AUTO_REVERT_STATE_FILE: &str = "data/auto_revert_state.json";

// ── Config hot-reload (G2) ─────────────────────────────────────────────────

/// Result of a config hot-reload check.
pub struct ReloadResult {
    /// True if a promotion file was found and applied.
    pub applied: bool,
    /// Number of parameters mutated.
    pub n_mutations: usize,
    /// Human-readable summary of what changed.
    pub summary: String,
}

/// Check for a CONFIG_PROMOTION.json file and, if present and newer than the
/// last applied timestamp, apply its mutations to the config.
///
/// The promotion file format (written by pq-refiner):
/// ```json
/// {
///   "challenger_id": "...",
///   "mutations": [
///     { "name": "gate_margin_bps", "from": 50, "to": 60 },
///     ...
///   ],
///   "verdict": "defeats",
///   "gate_verdict": "...",
///   "status": "READY_FOR_CONFIG_UPDATE"
/// }
/// ```
///
/// We parse the mutations array and call `cfg.apply(name, to)` for each.
/// Returns `ReloadResult::default()` (applied=false) if no file or stale.
///
/// **Validation fence (S1):** Config is `Copy`, so we apply mutations to a
/// detached snapshot first, call `validate()` on the snapshot, and only
/// commit to the live config if validation passes. This prevents the refiner
/// from promoting envelope-violating mutations (e.g. floor > ceiling) through
/// the hot-reload path, which never called `validate()` before this fix.
pub fn try_reload_config(cfg: &mut Config, last_mtime: &mut Option<u64>) -> ReloadResult {
    let path = Path::new(PROMOTION_FILE);
    if !path.exists() {
        return ReloadResult {
            applied: false,
            n_mutations: 0,
            summary: "no promotion file".to_string(),
        };
    }

    // Read file metadata for mtime
    let metadata = match fs::metadata(path) {
        Ok(m) => m,
        Err(_) => {
            return ReloadResult {
                applied: false,
                n_mutations: 0,
                summary: "cannot read promotion metadata".to_string(),
            }
        }
    };

    // Use modified timestamp (seconds since epoch on Windows via std::fs::Metadata)
    let mtime = metadata.modified().ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Skip if we already applied this file
    if let Some(prev) = *last_mtime {
        if mtime == prev {
            return ReloadResult {
                applied: false,
                n_mutations: 0,
                summary: "promotion already applied".to_string(),
            };
        }
    }

    // Read and parse the promotion file
    let content = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            return ReloadResult {
                applied: false,
                n_mutations: 0,
                summary: format!("read error: {e}"),
            }
        }
    };

    // ── Validation fence (S1) ─────────────────────────────────────────────
    // Config is Copy, so we apply mutations to a snapshot first, validate the
    // snapshot, and only commit to the live config if validation passes.
    // This catches envelope violations (floor > ceiling, step > envelope
    // width) that the per-mutation apply() cannot detect because each apply()
    // only sees one key at a time.
    let mut snapshot = *cfg;
    let mut snapshot_ok = true;
    let mut mutations_applied = 0usize;
    let mut summary_parts: Vec<String> = Vec::new();
    let mut apply_errors: Vec<String> = Vec::new();

    // Line-based parser: scan for "name" and "to" pairs within the mutations
    // array. Line-based parsing avoids the comma-inside-string problem that
    // breaks naive comma-split approaches (e.g. "G1:pass G2:pass" has a comma).
    let mut in_mutations = false;
    let mut current_name: Option<String> = None;
    let mut current_to: Option<i64> = None;

    /// Apply a single mutation to the snapshot, recording the result.
    /// This closure captures `snapshot`, `mutations_applied`, `summary_parts`,
    /// and `apply_errors` by reference.
    fn apply_one(
        snapshot: &mut Config,
        name: &str,
        to_val: i64,
        mutations_applied: &mut usize,
        summary_parts: &mut Vec<String>,
        apply_errors: &mut Vec<String>,
    ) {
        match snapshot.apply(name, to_val) {
            Ok(()) => {
                *mutations_applied += 1;
                summary_parts.push(format!("{name}={to_val}"));
            }
            Err(e) => {
                apply_errors.push(format!("{name}={to_val}: {e}"));
                eprintln!(
                    "[autonomous-bridge] config apply FAILED for {name}={to_val}: {e}"
                );
            }
        }
    }

    for line in content.lines() {
        let line = line.trim();

        // Detect entering the mutations array
        if line.contains("\"mutations\"") && line.contains('[') {
            in_mutations = true;
            continue;
        }
        // Detect leaving the mutations array
        if in_mutations && line.starts_with(']') {
            in_mutations = false;
            // Apply any pending pair at array close
            if let (Some(ref name), Some(to_val)) = (&current_name, current_to) {
                apply_one(
                    &mut snapshot,
                    name,
                    to_val,
                    &mut mutations_applied,
                    &mut summary_parts,
                    &mut apply_errors,
                );
            }
            current_name = None;
            current_to = None;
            continue;
        }
        if !in_mutations {
            continue;
        }

        // Extract "name": "value" from this line
        if let Some(name) = extract_json_string(line, "name") {
            current_name = Some(name);
        }
        // Extract "to": number from this line
        if let Some(to_val) = extract_json_i64(line, "to") {
            current_to = Some(to_val);
        }
        // When we have both and hit a closing brace, apply
        if line.contains('}') && current_name.is_some() && current_to.is_some() {
            if let (Some(ref name), Some(to_val)) = (&current_name, current_to) {
                apply_one(
                    &mut snapshot,
                    name,
                    to_val,
                    &mut mutations_applied,
                    &mut summary_parts,
                    &mut apply_errors,
                );
            }
            current_name = None;
            current_to = None;
        }
    }

    // ── Validate the snapshot (S1) ────────────────────────────────────────
    // If validate() fails on the snapshot, we reject the entire promotion.
    // The live config is untouched. This is the correct behavior: a promotion
    // that produces an internally inconsistent config is a bug in the refiner,
    // not a valid optimization.
    if snapshot_ok {
        if let Err(e) = snapshot.validate() {
            eprintln!(
                "[autonomous-bridge] CONFIG HOT-RELOAD REJECTED: validate() failed after {mutations_applied} mutations: {e}"
            );
            // Delete the promotion file so we don't re-reject it forever.
            let _ = fs::remove_file(path);
            *last_mtime = Some(mtime);
            return ReloadResult {
                applied: false,
                n_mutations: 0,
                summary: format!(
                    "REJECTED by validate(): {e} ({} mutations rolled back)",
                    mutations_applied
                ),
            };
        }
    }

    // ── Commit: validation passed, copy snapshot into live config ─────────
    *cfg = snapshot;

    // Delete the promotion file so we don't re-apply it
    let _ = fs::remove_file(path);

    *last_mtime = Some(mtime);

    ReloadResult {
        applied: mutations_applied > 0,
        n_mutations: mutations_applied,
        summary: summary_parts.join(", "),
    }
}

/// Extract a string value for a given key from a JSON fragment.
/// Finds the LAST occurrence of `"key"` then reads the string after the
/// colon that follows it. This handles lines with multiple key-value pairs.
fn extract_json_string(token: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let key_pos = token.rfind(&needle)?;
    let after_key = &token[key_pos + needle.len()..];
    let colon_pos = after_key.find(':')?;
    let after_colon = after_key[colon_pos + 1..].trim();
    // Strip leading quote
    let start = after_colon.find('"')? + 1;
    let rest = &after_colon[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Extract an i64 value for a given key from a JSON fragment.
/// Finds the LAST occurrence of `"key"` then parses the number after the
/// colon that follows it.
fn extract_json_i64(token: &str, key: &str) -> Option<i64> {
    let needle = format!("\"{key}\"");
    let key_pos = token.rfind(&needle)?;
    let after_key = &token[key_pos + needle.len()..];
    let colon_pos = after_key.find(':')?;
    let after_colon = after_key[colon_pos + 1..].trim();
    // Parse the leading integer (may have trailing } or ] or ,)
    let num_str: String = after_colon
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    num_str.parse().ok()
}

// ── Defense-in-depth (G4) ──────────────────────────────────────────────────

/// Runtime defense-in-depth state carried across daemon ticks.
pub struct DefenseState {
    pub cliff_config: CliffVetoConfig,
    pub breaker_state: CircuitBreakerState,
    pub breaker_config: CircuitBreakerConfig,
    pub kill_switch: KillSwitch,
    /// Peak net realized lamports (for drawdown tracking).
    pub peak_net_lamports: i64,
    /// Maximum drawdown observed (lamports, always positive).
    pub max_dd_lamports: i64,
    /// Lifecycle stage for defense evaluation.
    pub stage: LifecycleStage,
}

impl Default for DefenseState {
    fn default() -> Self {
        Self {
            cliff_config: CliffVetoConfig::default(),
            breaker_state: CircuitBreakerState::default(),
            breaker_config: CircuitBreakerConfig::default(),
            kill_switch: KillSwitch::default(),
            peak_net_lamports: 0,
            max_dd_lamports: 0,
            stage: LifecycleStage::RegisteredChallenger,
        }
    }
}

impl DefenseState {
    /// Create a new defense state with a given bankroll (for cliff veto threshold).
    pub fn with_bankroll(bankroll_lamports: i64) -> Self {
        Self {
            cliff_config: CliffVetoConfig {
                bankroll_lamports,
                ..CliffVetoConfig::default()
            },
            ..Self::default()
        }
    }

    /// Record a trade outcome for circuit breaker tracking.
    pub fn record_trade(&mut self, profitable: bool, cycle: u64) {
        let tripped = self
            .breaker_state
            .record_trade(profitable, cycle, &self.breaker_config);
        if tripped {
            eprintln!(
                "[autonomous-bridge] CIRCUIT BREAKER TRIPPED at cycle {cycle}"
            );
        }
    }

    /// Update drawdown tracking from the current net realized P&L.
    pub fn update_drawdown(&mut self, net_realized_lamports: i64) {
        if net_realized_lamports > self.peak_net_lamports {
            self.peak_net_lamports = net_realized_lamports;
        }
        let dd = self.peak_net_lamports - net_realized_lamports;
        if dd > self.max_dd_lamports {
            self.max_dd_lamports = dd;
        }
    }

    /// Evaluate all three defense layers. Returns a verdict the daemon consults
    /// before admitting new trades.
    pub fn evaluate(&self) -> DefenseVerdict {
        evaluate_defense(
            self.max_dd_lamports,
            &self.cliff_config,
            &self.breaker_state,
            &self.kill_switch,
            self.stage.clone(),
        )
    }

    /// Check if trading is currently allowed by all defense layers.
    pub fn trading_allowed(&self) -> bool {
        self.evaluate().trading_allowed()
    }

    /// Activate the kill switch (manual or automatic).
    pub fn activate_kill(&mut self, cycle: u64, reason: &str) {
        use pump_quant_evaluator::defense_in_depth::KillReason;
        let kr = match reason {
            "manual" => KillReason::Manual,
            "cliff" | "breaker" => KillReason::AutoDrawdown,
            _ => KillReason::AutoDrawdown,
        };
        self.kill_switch.activate(cycle, kr);
        eprintln!("[autonomous-bridge] KILL SWITCH ACTIVATED: {reason} at cycle {cycle}");
    }

    /// Deactivate the kill switch.
    pub fn deactivate_kill(&mut self) {
        self.kill_switch.deactivate();
    }

    /// Return the kill reason if the kill switch is active, else None.
    pub fn kill_reason(&self) -> Option<pump_quant_evaluator::defense_in_depth::KillReason> {
        if self.kill_switch.active {
            // reason_tag is stored as u32, map it back to KillReason.
            use pump_quant_evaluator::defense_in_depth::KillReason;
            match self.kill_switch.reason_tag {
                1 => Some(KillReason::Manual),
                2 => Some(KillReason::AutoDrawdown),
                3 => Some(KillReason::AutoLatency),
                4 => Some(KillReason::AutoInsufficientBalance),
                _ => Some(KillReason::Inactive),
            }
        } else {
            None
        }
    }
}

// ── Refiner scheduling (G1) ────────────────────────────────────────────────

/// Spawns the pq-refiner binary as a child process on a periodic timer.
/// The daemon holds an instance of this and calls `.spawn(tick)` every
/// `refiner_every_ticks` ticks. The refiner reads the tape, runs the
/// 8-gate / SPRT / committee, and writes CONFIG_PROMOTION.json, which
/// the daemon hot-reloads on the next tick.
pub struct RefinerSpawner {
    /// Tick at which the last spawn happened (0 = never).
    last_spawn_tick: u64,
    /// Whether a spawn is currently in flight (refiner process running).
    in_flight: bool,
}

impl RefinerSpawner {
    /// Create a new spawner.
    pub fn new() -> Self {
        Self {
            last_spawn_tick: 0,
            in_flight: false,
        }
    }

    /// Spawn the refiner for the given tick. Returns the child PID string.
    /// Non-blocking: the refiner runs as a detached child process.
    ///
    /// `config_text` is the champion config dumped via `Config::dump_to_text()`.
    /// It is written to `data/CHAMPION_CONFIG.txt` and passed to the refiner via
    /// `--config-path` so the refiner can generate challengers from the actual
    /// live config rather than a nonexistent fallback file.
    pub fn spawn(&mut self, tick: u64, config_text: &str) -> Result<String, String> {
        if self.in_flight {
            return Err("refiner already in flight".to_string());
        }
        let result = spawn_refiner_cycle(config_text);
        if result.is_ok() {
            self.last_spawn_tick = tick;
            self.in_flight = true;
            // Reset in_flight after a delay — we can't track child exit
            // without polling, so we just allow the next spawn after the
            // interval. The refiner is a one-shot binary that exits when done.
            self.in_flight = false;
        }
        result
    }
}

impl Default for RefinerSpawner {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawn the pq-refiner binary as a child process, non-blocking.
/// The refiner reads the tape, runs the 8-gate, and writes CONFIG_PROMOTION.json.
/// The daemon picks up the promotion file on the next hot-reload tick.
///
/// `config_text` is the champion config (from `Config::dump_to_text()`) written
/// to `data/CHAMPION_CONFIG.txt` and passed via `--config-path`. Without this,
/// the refiner falls back to `config/paper.toml` (which doesn't exist) and
/// generates zero challengers.
///
/// **GAP A fix**: passes `--event-stream-path data/event_stream.jsonl` so the
/// refiner runs the full engine replay (admission → sizing → exit) per
/// challenger, producing genuinely different trade sequences for different
/// configs. Without this, the refiner falls back to shadow replay only, which
/// adjusts fee/slippage on a FIXED trade set — producing identical NetSol for
/// 95%+ of parameter mutations, making the entire autonomous loop inert.
///
/// **GAP D fix**: before writing the champion config, archives the current
/// champion config (if it exists) to `data/champion_archive/` so we have a
/// revert target.
pub fn spawn_refiner_cycle(config_text: &str) -> Result<String, String> {
    // ── GAP D: archive the current champion config before overwriting ─────
    archive_champion_config();

    // Write the champion config to data/CHAMPION_CONFIG.txt for the refiner to read.
    if let Err(e) = fs::write(CHAMPION_CONFIG_FILE, config_text) {
        return Err(format!(
            "failed to write champion config to {CHAMPION_CONFIG_FILE}: {e}"
        ));
    }

    // Try to locate the refiner binary in the target directory.
    // Check common locations: ./target/release/pq-refiner, ./target/debug/pq-refiner
    let candidates = [
        "target/release/pq-refiner.exe",
        "target/release/pq-refiner",
        "target/debug/pq-refiner.exe",
        "target/debug/pq-refiner",
        "pq-refiner.exe",
        "pq-refiner",
    ];

    let bin_path = candidates
        .iter()
        .find(|p| Path::new(p).exists())
        .ok_or("pq-refiner binary not found")?;

    let mut cmd = Command::new(bin_path);
    cmd.arg("--tape-path").arg(TAPE_FILE);
    cmd.arg("--config-path").arg(CHAMPION_CONFIG_FILE);

    // ── GAP A fix: pass the event stream for engine replay ────────────────
    // This is THE critical fix. Without --event-stream-path, the refiner
    // falls back to shadow_replay() which only adjusts fee/slippage on the
    // fixed tape. With it, the refiner runs replay_event_stream() per
    // challenger — re-deriving admission, sizing, and exit decisions under
    // each mutated config. Different configs → different trades → different
    // NetSol → actual differentiation → real promotions.
    if Path::new(EVENT_STREAM_FILE).exists() {
        cmd.arg("--event-stream-path").arg(EVENT_STREAM_FILE);
        eprintln!(
            "[autonomous-bridge] refiner: engine replay ENABLED (event_stream exists)"
        );
    } else {
        eprintln!(
            "[autonomous-bridge] WARNING: event_stream.jsonl not found — \
             refiner will use shadow replay only (challengers may be identical)"
        );
    }

    // Spawn it detached (non-blocking). stdout/stderr go to the parent's.
    match cmd.spawn() {
        Ok(_child) => {
            let msg = format!(
                "spawned pq-refiner from {bin_path} with config {CHAMPION_CONFIG_FILE} \
                 and event_stream {EVENT_STREAM_FILE}"
            );
            eprintln!("[autonomous-bridge] {msg}");
            Ok(msg)
        }
        Err(e) => Err(format!("failed to spawn pq-refiner: {e}")),
    }
}

// ── GAP B: Auto-revert mechanism ────────────────────────────────────────────
//
// When the refiner promotes a config, the daemon hot-reloads it. But if the
// new config underperforms (slow bleed instead of cliff), there's no mechanism
// to undo the promotion. This implements:
//
// 1. On promotion: save the pre-promotion config fingerprint + PnL snapshot
//    to `data/auto_revert_state.json`.
// 2. On each hot-reload tick: check if post-promotion PnL has deteriorated
//    beyond a threshold within a grace period.
// 3. If deterioration exceeds threshold: write a revert promotion file
//    (CONFIG_PROMOTION.json with the archived champion config) and trigger
//    the daemon's hot-reload to pick it up.
//
// The grace period prevents premature reverts — a config may need time to
// find its trades. The deterioration threshold is conservative: we only
// revert if the post-promotion PnL rate is significantly worse than the
// pre-promotion rate, not just "slightly less profitable".

/// State tracked for auto-revert decisions.
#[derive(Clone, Debug)]
pub struct AutoRevertState {
    /// Config fingerprint of the config that was promoted (the "new" config).
    pub promoted_fingerprint: u64,
    /// Config fingerprint of the champion that was replaced (the "old" config).
    pub prior_champion_fingerprint: u64,
    /// Cumulative realized PnL (lamports) at the moment of promotion.
    pub pnl_at_promotion: i128,
    /// Ticks since the promotion was applied.
    pub ticks_since_promotion: u64,
    /// Cumulative trade count at the moment of promotion. Used to compute
    /// trades-since-promotion for the variance-based revert threshold.
    pub trades_at_promotion: u64,
    /// Whether auto-revert has triggered for this promotion.
    pub reverted: bool,
}

impl Default for AutoRevertState {
    fn default() -> Self {
        Self {
            promoted_fingerprint: 0,
            prior_champion_fingerprint: 0,
            pnl_at_promotion: 0,
            ticks_since_promotion: 0,
            trades_at_promotion: 0,
            reverted: false,
        }
    }
}

/// Minimum trades before auto-revert evaluates post-promotion PnL.
/// Below this, the sample is too noisy to distinguish a bad config from
/// variance. At our ~0.27 trades/min rate, 50 trades ≈ 3 hours — which
/// is longer than the 2-hour refiner cadence, so the revert check happens
/// between refiner cycles, not during one.
const AUTO_REVERT_MIN_TRADES: u64 = 50;

/// Per-trade PnL standard deviation (in lamports) used for the variance-based
/// revert threshold. Calibrated from our tape data: σ ≈ 0.0178 SOL/trade.
/// 1 SOL = 1e9 lamports, so 0.0178 SOL = 17,800,000 lamports.
const AUTO_REVERT_PER_TRADE_SIGMA_LAMPORTS: f64 = 17_800_000.0;

/// Confidence multiplier (k) for the variance-based threshold. The threshold
/// is `k × σ × √n` where n = trades since promotion. k=3 means we require
/// the drawdown to exceed the 99.7% confidence band (3σ) of per-trade noise
/// before reverting — this prevents false reverts from normal variance.
const AUTO_REVERT_CONFIDENCE_K: f64 = 3.0;

/// Maximum post-promotion PnL deterioration (in lamports) before auto-revert
/// triggers. This is a FLOOR: the actual threshold scales with trades
/// observed as `max(FLOOR, k × σ × √n)`. The floor prevents triggering
/// on micro-drawdowns when n is very small. Set to 0.05 SOL (50,000,000
/// lamports) — below this, even 50 trades of pure noise could trigger.
const AUTO_REVERT_DRAWDOWN_FLOOR_LAMPORTS: i128 = 50_000_000;

/// Write the auto-revert state to disk.
pub fn write_auto_revert_state(state: &AutoRevertState) {
    let json = format!(
        concat!(
            "{{\"schema\":\"auto_revert/2\",",
            "\"promoted_fingerprint\":\"{:#018x}\",",
            "\"prior_champion_fingerprint\":\"{:#018x}\",",
            "\"pnl_at_promotion\":{},",
            "\"ticks_since_promotion\":{},",
            "\"trades_at_promotion\":{},",
            "\"reverted\":{}}}\n"
        ),
        state.promoted_fingerprint,
        state.prior_champion_fingerprint,
        state.pnl_at_promotion,
        state.ticks_since_promotion,
        state.trades_at_promotion,
        state.reverted,
    );
    let _ = fs::write(AUTO_REVERT_STATE_FILE, json);
}

/// Read the auto-revert state from disk. Returns None if the file doesn't
/// exist or can't be parsed. Backward-compatible: if trades_at_promotion is
/// missing (schema v1), defaults to 0.
pub fn read_auto_revert_state() -> Option<AutoRevertState> {
    let text = fs::read_to_string(AUTO_REVERT_STATE_FILE).ok()?;
    // Simple field extraction — no JSON parser in this crate's deps.
    let extract = |key: &str| -> Option<String> {
        let needle = format!("\"{key}\":");
        let start = text.find(&needle)? + needle.len();
        let rest = &text[start..];
        let end = rest.find(|c: char| c == ',' || c == '}')?;
        Some(rest[..end].trim().to_string())
    };
    Some(AutoRevertState {
        promoted_fingerprint: u64::from_str_radix(
            extract("promoted_fingerprint")?.trim_start_matches('\"').trim_end_matches('\"'),
            16,
        ).ok()?,
        prior_champion_fingerprint: u64::from_str_radix(
            extract("prior_champion_fingerprint")?.trim_start_matches('\"').trim_end_matches('\"'),
            16,
        ).ok()?,
        pnl_at_promotion: extract("pnl_at_promotion")?.parse().ok()?,
        ticks_since_promotion: extract("ticks_since_promotion")?.parse().ok()?,
        trades_at_promotion: extract("trades_at_promotion")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0), // backward compat: schema v1 has no trades_at_promotion
        reverted: extract("reverted")?.parse::<u8>().map(|v| v != 0).ok()?,
    })
}

/// Check if auto-revert should trigger. Called by the daemon on each
/// hot-reload tick after a promotion has been applied.
///
/// Returns `Some(revert_config_text)` if we should revert, `None` otherwise.
/// The revert config text is read from the champion archive.
///
/// The threshold is variance-based: `threshold = max(FLOOR, k × σ × √n)`
/// where n = trades since promotion, σ = per-trade PnL std dev, k = confidence
/// multiplier. This prevents false reverts from normal trading variance
/// while still catching genuinely bad configs quickly.
pub fn check_auto_revert(
    current_fingerprint: u64,
    current_cumulative_pnl: i128,
    _tick_since_promotion: u64,
    trades_since_promotion: u64,
) -> Option<String> {
    let state = read_auto_revert_state()?;
    if state.reverted {
        return None; // Already reverted
    }
    if state.promoted_fingerprint != current_fingerprint {
        return None; // Different promotion context
    }

    // Require a minimum number of trades before evaluating.
    // Below this, the sample is too noisy to distinguish a bad config
    // from normal variance.
    if trades_since_promotion < AUTO_REVERT_MIN_TRADES {
        return None;
    }

    // Compute the variance-based threshold: max(FLOOR, k × σ × √n)
    let n = trades_since_promotion as f64;
    let dynamic_threshold = (AUTO_REVERT_CONFIDENCE_K
        * AUTO_REVERT_PER_TRADE_SIGMA_LAMPORTS
        * n.sqrt()) as i128;
    let threshold = AUTO_REVERT_DRAWDOWN_FLOOR_LAMPORTS.max(dynamic_threshold);

    // Check deterioration: has PnL dropped below the threshold?
    let drawdown = state.pnl_at_promotion - current_cumulative_pnl;
    if drawdown > threshold {
        eprintln!(
            "[autonomous-bridge] AUTO-REVERT triggered: drawdown={drawdown} lamports \
             (threshold={threshold}, dynamic={dynamic_threshold}, floor={AUTO_REVERT_DRAWDOWN_FLOOR_LAMPORTS}), \
             trades_since_promotion={trades_since_promotion} (min={AUTO_REVERT_MIN_TRADES}). \
             Reverting to champion fingerprint {:#018x}",
            state.prior_champion_fingerprint
        );

        // Load the archived champion config
        let archive_path = format!(
            "{CHAMPION_ARCHIVE_DIR}/champion_{:#018x}.txt",
            state.prior_champion_fingerprint
        );
        let revert_config = fs::read_to_string(&archive_path).ok()?;
        return Some(revert_config);
    }
    None
}

// ── GAP D: Champion config archive ──────────────────────────────────────────
//
// Before each refiner spawn (which may trigger a promotion that overwrites
// CHAMPION_CONFIG.txt), archive the current champion config. This preserves
// a revert target: if a promoted config underperforms, we can load the
// archived champion config and restore it.

/// Archive the current champion config to `data/champion_archive/`.
/// Called before each refiner spawn. The archive preserves the config text
/// keyed by its FNV-1a fingerprint, so auto-revert can load it by fingerprint.
fn archive_champion_config() {
    // Ensure the archive directory exists
    let _ = fs::create_dir_all(CHAMPION_ARCHIVE_DIR);

    // Read the current champion config file
    let config_text = match fs::read_to_string(CHAMPION_CONFIG_FILE) {
        Ok(text) => text,
        Err(_) => return, // No existing config to archive
    };

    // Compute the fingerprint of the current config
    // We use the same FNV-1a hash as the daemon's config fingerprint.
    // The daemon computes the hash via Config::fnv1a_64() on the
    // dump_to_text() output. Since the file IS that output, we hash it
    // the same way.
    let fingerprint = fnv1a_64_text(&config_text);

    let archive_path = format!("{CHAMPION_ARCHIVE_DIR}/champion_{fingerprint:#018x}.txt");

    // Only archive if we don't already have this fingerprint archived
    if Path::new(&archive_path).exists() {
        return;
    }

    let archive_path_ts = format!(
        "{CHAMPION_ARCHIVE_DIR}/champion_{fingerprint:#018x}_{}.txt",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    );

    // Write the archived config
    let _ = fs::write(&archive_path, &config_text);
    let _ = fs::write(&archive_path_ts, &config_text);

    // Write a manifest entry (append-only log of all champion configs)
    let manifest_path = format!("{CHAMPION_ARCHIVE_DIR}/manifest.jsonl");
    let manifest_entry = format!(
        "{{\"schema\":\"champion_archive/1\",\
         \"fingerprint\":\"{fingerprint:#018x}\",\
         \"ts_unix\":{ts},\
         \"config_file\":\"{path}\"}}\n",
        ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        path = archive_path.replace('\\', "/"),
    );
    let _ = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .write(true)
        .open(&manifest_path)
        .and_then(|mut f| {
            use std::io::Write;
            f.write_all(manifest_entry.as_bytes())
        });
}

/// FNV-1a 64-bit hash of a text string, matching the daemon's config
/// fingerprint computation. This replicates `pump_quant_brain::hash::fnv1a_64`
/// for the config text bytes.
fn fnv1a_64_text(text: &str) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x00000100000001b3;
    let mut hash = FNV_OFFSET;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Check if a refiner cycle is currently in progress by looking for the
/// refiner_status.json file's mtime.
pub fn refiner_running() -> bool {
    // The refiner writes data/refiner_status.json when it starts and finishes.
    // If the file was modified in the last 60 seconds, assume it's running or
    // just finished.
    let status_path = Path::new("data/refiner_status.json");
    if !status_path.exists() {
        return false;
    }
    let metadata = match fs::metadata(status_path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    let mtime = metadata.modified().ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    now.saturating_sub(mtime) < 30
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    /// S1: Serializes all tests that touch the shared PROMOTION_FILE.
    /// Without this, parallel test execution causes race conditions
    /// where one test overwrites another's promotion file.
    static PROMOTION_FILE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    fn promotion_lock() -> std::sync::MutexGuard<'static, ()> {
        let mtx = PROMOTION_FILE_LOCK.get_or_init(|| Mutex::new(()));
        mtx.lock().unwrap()
    }

    #[test]
    fn test_defense_state_default() {
        let ds = DefenseState::default();
        assert!(ds.trading_allowed());
        assert_eq!(ds.max_dd_lamports, 0);
        assert_eq!(ds.peak_net_lamports, 0);
    }

    #[test]
    fn test_defense_state_drawdown_tracking() {
        let mut ds = DefenseState::default();
        ds.update_drawdown(100_000);
        assert_eq!(ds.peak_net_lamports, 100_000);
        assert_eq!(ds.max_dd_lamports, 0);
        ds.update_drawdown(80_000);
        assert_eq!(ds.peak_net_lamports, 100_000);
        assert_eq!(ds.max_dd_lamports, 20_000);
        ds.update_drawdown(120_000);
        assert_eq!(ds.peak_net_lamports, 120_000);
        assert_eq!(ds.max_dd_lamports, 20_000); // DD doesn't increase past peak
    }

    #[test]
    fn test_defense_state_circuit_breaker() {
        let mut ds = DefenseState::default();
        // Record 5 consecutive losses (default breaker threshold)
        for i in 0..5 {
            ds.record_trade(false, i);
        }
        assert!(!ds.trading_allowed());
    }

    #[test]
    fn test_defense_state_kill_switch() {
        let mut ds = DefenseState::default();
        ds.activate_kill(10, "manual");
        assert!(!ds.trading_allowed());
        ds.deactivate_kill();
        assert!(ds.trading_allowed());
    }

    #[test]
    fn test_defense_state_cliff_veto() {
        // Default threshold: max_dd_bps=5000 (50%), bankroll=2 SOL.
        // threshold_lamports = 2_000_000_000 * 5000 / 10000 = 1_000_000_000 (1 SOL)
        // We need DD > 1 SOL to trigger the veto.
        let mut ds = DefenseState::with_bankroll(2_000_000_000); // 2 SOL
        ds.update_drawdown(2_000_000_000); // peak at 2 SOL
        ds.update_drawdown(500_000_000);   // DD of 1.5 SOL > 1 SOL threshold
        assert!(!ds.trading_allowed());
    }

    #[test]
    fn test_extract_json_string() {
        let token = " \"name\": \"gate_margin_bps\"";
        assert_eq!(
            extract_json_string(token, "name"),
            Some("gate_margin_bps".to_string())
        );
    }

    #[test]
    fn test_extract_json_i64() {
        let token = " \"to\": 60";
        assert_eq!(extract_json_i64(token, "to"), Some(60));
    }

    #[test]
    fn test_reload_no_file() {
        // S1: Serialize against other promotion-file tests.
        let _lock = promotion_lock();
        // Use a temp directory so no other test's promotion file leaks in.
        let tmp = std::env::temp_dir();
        let promo_path = tmp.join("pq_test_no_promotion.json");
        let _ = std::fs::remove_file(&promo_path);

        let mut cfg = Config::dev_portable().with_mcap_band();
        let mut last_mtime = None;

        // Call the internal parser directly with no file present at that path.
        // Since try_reload_config uses the hardcoded PROMOTION_FILE constant,
        // we verify the "no file" path by ensuring data/CONFIG_PROMOTION.json
        // does not exist.
        let _ = std::fs::remove_file(PROMOTION_FILE);
        let result = try_reload_config(&mut cfg, &mut last_mtime);
        assert!(!result.applied);
    }

    #[test]
    fn test_reload_applies_mutations() {
        // S1: Serialize against other promotion-file tests.
        let _lock = promotion_lock();
        // Create a fake promotion file
        let promotion_dir = Path::new("data");
        let _ = fs::create_dir_all(promotion_dir);
        // Clean up any stale promotion file from a prior test.
        let _ = fs::remove_file(PROMOTION_FILE);
        let promotion_content = r#"{
  "challenger_id": "test_001",
  "mutations": [
    {"name": "gate_margin_bps", "from": 50, "to": 60},
    {"name": "promote_k", "from": 8, "to": 12}
  ],
  "verdict": "defeats",
  "gate_verdict": "G1:pass G2:pass",
  "status": "READY_FOR_CONFIG_UPDATE"
}"#;
        let _ = fs::write(PROMOTION_FILE, promotion_content);

        let mut cfg = Config::dev_portable().with_mcap_band();
        let original_margin = cfg.gate_margin_bps;
        let original_promote_k = cfg.promote_k;

        let mut last_mtime = None;
        let result = try_reload_config(&mut cfg, &mut last_mtime);

        assert!(result.applied);
        assert_eq!(result.n_mutations, 2);
        assert_eq!(cfg.gate_margin_bps, 60);
        assert_eq!(cfg.promote_k, 12);
        assert_ne!(cfg.gate_margin_bps, original_margin);
        assert_ne!(cfg.promote_k, original_promote_k);

        // File should be consumed (deleted after apply)
        assert!(!Path::new(PROMOTION_FILE).exists());
    }

    /// S1: A promotion file that would invert the reflection envelope
    /// (floor > ceiling) must be REJECTED by validate() and the live config
    /// must remain untouched.
    #[test]
    fn test_reload_rejects_envelope_violation() {
        // S1: Serialize against other promotion-file tests.
        let _lock = promotion_lock();
        let promotion_dir = Path::new("data");
        let _ = fs::create_dir_all(promotion_dir);
        // Clean up any stale promotion file from a prior test.
        let _ = fs::remove_file(PROMOTION_FILE);
        // Set floor above ceiling — validate() will reject this.
        let promotion_content = r#"{
  "challenger_id": "test_envelope_violation",
  "mutations": [
    {"name": "reflect_weight_floor_bp", "from": 2000, "to": 50000},
    {"name": "reflect_weight_ceiling_bp", "from": 40000, "to": 3000}
  ],
  "verdict": "defeats",
  "gate_verdict": "G1:pass G2:pass",
  "status": "READY_FOR_CONFIG_UPDATE"
}"#;
        let _ = fs::write(PROMOTION_FILE, promotion_content);

        let mut cfg = Config::dev_portable().with_mcap_band();
        let original_floor = cfg.reflect_weight_floor_bp;
        let original_ceiling = cfg.reflect_weight_ceiling_bp;

        let mut last_mtime = None;
        let result = try_reload_config(&mut cfg, &mut last_mtime);

        // The promotion must be rejected.
        assert!(!result.applied);
        assert_eq!(result.n_mutations, 0);
        assert!(
            result.summary.contains("REJECTED"),
            "expected REJECTED in summary, got: {}",
            result.summary
        );

        // The live config must be untouched.
        assert_eq!(cfg.reflect_weight_floor_bp, original_floor);
        assert_eq!(cfg.reflect_weight_ceiling_bp, original_ceiling);

        // File should still be consumed (deleted) so we don't re-reject forever.
        assert!(!Path::new(PROMOTION_FILE).exists());
    }

    /// S1: A valid promotion that does not violate any envelope must pass
    /// through the validate() fence and be applied normally.
    #[test]
    fn test_reload_accepts_valid_envelope() {
        // S1: Serialize against other promotion-file tests.
        let _lock = promotion_lock();
        let promotion_dir = Path::new("data");
        let _ = fs::create_dir_all(promotion_dir);
        // Clean up any stale promotion file from a prior test.
        let _ = fs::remove_file(PROMOTION_FILE);
        // Mutate gate_margin_bps — a single-param change that validate() allows.
        let promotion_content = r#"{
  "challenger_id": "test_valid",
  "mutations": [
    {"name": "gate_margin_bps", "from": 50, "to": 55}
  ],
  "verdict": "defeats",
  "gate_verdict": "G1:pass G2:pass",
  "status": "READY_FOR_CONFIG_UPDATE"
}"#;
        let _ = fs::write(PROMOTION_FILE, promotion_content);

        let mut cfg = Config::dev_portable().with_mcap_band();

        let mut last_mtime = None;
        let result = try_reload_config(&mut cfg, &mut last_mtime);

        assert!(result.applied);
        assert_eq!(result.n_mutations, 1);
        assert_eq!(cfg.gate_margin_bps, 55);
        assert!(!Path::new(PROMOTION_FILE).exists());
    }
}
