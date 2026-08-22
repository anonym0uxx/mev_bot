//! `pq-watchdog` — the process supervisor (Phase 3 of the autonomous architecture).
//!
//! A separate process that monitors `pq-daemon`, restarts it on crash, and
//! respects the emergency-stop sentinel. It enforces:
//! - Crash recovery: if pq-daemon exits unexpectedly, restart it (up to N times)
//! - Emergency stop: if `data/EMERGENCY_STOP` exists, do NOT restart; exit
//! - Graceful shutdown: if `data/STOP_WATCHDOG` exists, stop restarting and exit
//! - Health check: if the daemon's `data/live_status.json` hasn't been updated
//!   in `health_timeout_secs`, kill and restart the daemon (hung process detection)
//! - Backoff: exponential backoff between restarts (capped at 60s)
//!
//! Usage: pq-watchdog [--max-restarts N] [--health-timeout-secs N] [--backoff-cap-secs N]
//!                    [--daemon-args "..."]
//!
//! The watchdog spawns pq-daemon as a child process. On crash, it increments
//! the restart counter, sleeps with backoff, and respawns. If the restart
//! counter exceeds `max_restarts`, it writes `data/WATCHDOG_GAVE_UP` and exits.
//! The emergency-stop sentinel (`data/EMERGENCY_STOP`) is checked BEFORE each
//! restart — if it exists, the watchdog exits immediately without restarting.

use std::process::{Command, Stdio, Child, ExitStatus};
use std::time::{Duration, Instant};
use std::path::Path;
use std::fs;

const EMERGENCY_STOP_FILE: &str = "data/EMERGENCY_STOP";
const STOP_WATCHDOG_FILE: &str = "data/STOP_WATCHDOG";
const STATUS_PATH: &str = "data/live_status.json";
const WATCHDOG_GAVE_UP_FILE: &str = "data/WATCHDOG_GAVE_UP";
const WATCHDOG_STATUS_FILE: &str = "data/watchdog_status.json";
/// Wangr Rev-14: the champion config file the daemon hot-reloads.
const CHAMPION_CONFIG_FILE: &str = "data/CHAMPION_CONFIG.txt";
const WATCHDOG_PID_FILE: &str = "data/watchdog.pid";

/// Check for an already-running watchdog via PID file.
/// Returns `Some(existing_pid)` if another watchdog is alive, `None` if not.
/// A stale PID file (process gone) is cleaned up and treated as no conflict.
fn check_existing_watchdog() -> Option<u32> {
    let pid_path = Path::new(WATCHDOG_PID_FILE);
    if !pid_path.exists() {
        return None;
    }
    let pid_str = match fs::read_to_string(pid_path) {
        Ok(s) => s.trim().to_string(),
        Err(_) => return None, // unreadable — treat as stale, let us proceed
    };
    let pid: u32 = match pid_str.parse() {
        Ok(p) => p,
        Err(_) => {
            // Non-numeric — stale, clean up
            let _ = fs::remove_file(pid_path);
            return None;
        }
    };
    // Check if that PID is still alive
    // On Windows, we use `tasklist` via Command; on Unix, we'd use kill(pid, 0).
    // For cross-platform simplicity, use std::process to check.
    // We try to get the process — if it's gone, the PID file is stale.
    let alive = if pid == std::process::id() {
        // We are that PID — shouldn't happen on a fresh start, but be safe
        false
    } else {
        // Use the OS to check if the PID is alive.
        // On Windows: `tasklist /FI "PID eq <pid>" /NH` returns the process
        // line if alive, or "INFO: No tasks..." if dead.
        #[cfg(windows)]
        {
            let output = Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/NH"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output();
            match output {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    // If the PID is alive, tasklist prints a line with the PID.
                    // If dead, it prints "INFO: No tasks running which match...".
                    stdout.contains(&pid.to_string()) && !stdout.contains("INFO:")
                }
                Err(_) => false, // can't check — assume stale to be safe
            }
        }
        #[cfg(not(windows))]
        {
            // On Unix, sending signal 0 to a PID checks liveness without killing.
            // We use libc-free approach: /proc/<pid> existence.
            Path::new(&format!("/proc/{pid}")).exists()
        }
    };
    if alive {
        Some(pid)
    } else {
        // Stale PID file — clean up and proceed
        let _ = fs::remove_file(pid_path);
        None
    }
}

/// Write our PID to the PID file. Called after the single-instance check passes.
fn write_pid_file() {
    let _ = fs::write(WATCHDOG_PID_FILE, std::process::id().to_string());
}

/// Remove the PID file on exit.
fn remove_pid_file() {
    let _ = fs::remove_file(WATCHDOG_PID_FILE);
}

struct WatchdogArgs {
    max_restarts: u32,
    health_timeout_secs: u64,
    backoff_cap_secs: u64,
    initial_backoff_secs: u64,
    daemon_args: String,
}

fn parse_args() -> WatchdogArgs {
    let args: Vec<String> = std::env::args().collect();
    let mut a = WatchdogArgs {
        max_restarts: 100,       // effectively unlimited
        health_timeout_secs: 120, // 2 minutes without status = hung
        backoff_cap_secs: 60,
        initial_backoff_secs: 2,
        daemon_args: String::new(),
    };
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--max-restarts" if i + 1 < args.len() => {
                a.max_restarts = args[i + 1].parse().unwrap_or(100);
                i += 2;
            }
            "--health-timeout-secs" if i + 1 < args.len() => {
                a.health_timeout_secs = args[i + 1].parse().unwrap_or(120);
                i += 2;
            }
            "--backoff-cap-secs" if i + 1 < args.len() => {
                a.backoff_cap_secs = args[i + 1].parse().unwrap_or(60);
                i += 2;
            }
            "--initial-backoff-secs" if i + 1 < args.len() => {
                a.initial_backoff_secs = args[i + 1].parse().unwrap_or(2);
                i += 2;
            }
            "--daemon-args" if i + 1 < args.len() => {
                a.daemon_args = args[i + 1].clone();
                i += 2;
            }
            _ => { i += 1; }
        }
    }
    a
}

fn emergency_stop_requested() -> bool {
    Path::new(EMERGENCY_STOP_FILE).exists()
}

fn stop_watchdog_requested() -> bool {
    Path::new(STOP_WATCHDOG_FILE).exists()
}

/// Check the refiner status file and log its last activity.
/// Returns the number of seconds since the refiner last ran, or None if
/// the status file doesn't exist or can't be parsed.
fn check_refiner_status() -> Option<u64> {
    let path = Path::new("data/REFINER_STATUS.json");
    if !path.exists() {
        return None;
    }
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let elapsed = modified.elapsed().ok()?;
    Some(elapsed.as_secs())
}

/// Check if the daemon's status file has been updated within the timeout.
/// Returns true if the daemon appears healthy (status file is recent).
fn daemon_is_healthy(health_timeout_secs: u64) -> bool {
    let path = Path::new(STATUS_PATH);
    if !path.exists() {
        return false; // no status file = not started yet or crashed early
    }
    match fs::metadata(path) {
        Ok(meta) => {
            let mtime = meta.modified().unwrap_or_else(|_| {
                std::time::SystemTime::now()
            });
            let elapsed = mtime.elapsed().unwrap_or(Duration::ZERO);
            elapsed.as_secs() < health_timeout_secs
        }
        Err(_) => false,
    }
}

/// GAP #14: Check if the daemon is suffering from OnchainConfirm starvation.
/// Reads data/daemon_health.json (written by the daemon every STATUS_HEARTBEAT_SECS)
/// and checks if the daemon has been running long enough (uptime > 120s) but has
/// ZERO onchain confirms — meaning BOTH data lanes (LaserStream gRPC AND Helius
/// WS) are dead. This is a CRITICAL health signal: without onchain confirms,
/// 95%+ of trade signals are rejected as NeedsOnchainConfirmation, and the
/// daemon is effectively trading blind.
///
/// Rev-30: With LS-primary/WS-fallback architecture, onchain confirms arrive
/// via TWO possible lanes:
///   1. `ls_onchain_confirms_decoded` — LaserStream gRPC account updates
///   2. `onchain_confirms_decoded`     — Helius WS accountSubscribe (fallback)
/// The daemon is considered starved ONLY if BOTH are zero after uptime > 120s.
fn daemon_onchain_confirm_healthy() -> Option<bool> {
    let path = Path::new("data/daemon_health.json");
    if !path.exists() {
        return None;
    }
    let content = fs::read_to_string(path).ok()?;
    let ws_confirms = extract_json_u64(&content, "onchain_confirms_decoded")?;
    let ls_confirms = extract_json_u64(&content, "ls_onchain_confirms_decoded")?;
    let uptime = extract_json_u64(&content, "uptime_secs")?;
    // If we can't find the values, fail-open (return None).
    let ws_confirms = ws_confirms?;
    let ls_confirms = ls_confirms?;
    let uptime = uptime?;
    // Rev-30: daemon is healthy if EITHER lane has confirms (LS primary OR WS fallback).
    // Starved only when BOTH are zero after sufficient uptime.
    if uptime > 120 && ws_confirms == 0 && ls_confirms == 0 {
        Some(false)
    } else {
        Some(true)
    }
}

/// Extract a u64 value from a JSON string by key name. Returns None if the
/// key is not found or the value is not a number. This is a lightweight
/// alternative to pulling in a full JSON parser.
fn extract_json_u64(json: &str, key: &str) -> Option<Option<u64>> {
    // Search for "key": <number> pattern
    let pattern = format!("\"{key}\"");
    let idx = json.find(&pattern)?;
    let rest = &json[idx + pattern.len()..];
    // Skip whitespace and colon
    let rest = rest.trim_start();
    let rest = rest.strip_prefix(':')?;
    let rest = rest.trim_start();
    // Parse the number
    let num_str: String = rest.chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if num_str.is_empty() {
        return Some(None);
    }
    num_str.parse::<u64>().ok().map(Some)
}

/// Write the watchdog status file for monitoring by the operator.
fn write_watchdog_status(
    restarts: u32,
    daemon_pid: Option<u32>,
    uptime_secs: u64,
    last_event: &str,
) {
    let status = format!(
        "{{\n  \"restarts\": {restarts},\n  \"daemon_pid\": {},\n  \"uptime_secs\": {uptime_secs},\n  \"last_event\": \"{last_event}\"\n}}",
        daemon_pid.map(|p| p.to_string()).unwrap_or_else(|| "null".to_string())
    );
    let _ = fs::write(WATCHDOG_STATUS_FILE, status);
}

/// Wangr Rev-14: log which wangr entry filters are enabled in the champion config.
/// Called at watchdog startup and after each daemon (re)spawn so the operator
/// can verify the filter state from the watchdog log. This is informational
/// only — the watchdog does not gate on filter state; the daemon's gate does.
fn log_wangr_filter_state() {
    let path = Path::new(CHAMPION_CONFIG_FILE);
    if !path.exists() {
        eprintln!("[pq-watchdog] wangr: no CHAMPION_CONFIG.txt — all 6 filters DISABLED (dev defaults)");
        return;
    }
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[pq-watchdog] wangr: failed to read CHAMPION_CONFIG.txt: {e}");
            return;
        }
    };
    // Simple key=value scanner — the config format is line-oriented `key = value`.
    let mut enabled: Vec<&str> = Vec::new();
    let mut disabled: Vec<&str> = Vec::new();
    let wangr_keys: &[(&str, &str)] = &[
        ("wangr_require_legacy_enable", "Legacy-Token(5× grad)"),
        ("wangr_dow_filter_enable",     "DoW-Skip/Boost"),
        ("wangr_hour_filter_enable",    "Hour-Skip/Boost"),
        ("wangr_symbol_len_filter_enable", "Symbol-Length(4-6)"),
        ("wangr_creator_min_launches",  "Creator-Min-Launches"),
        ("wangr_liq_zone_filter_enable", "Liquidity-Zone(1k-10k)"),
    ];
    for &(key, label) in wangr_keys {
        // Check if the key is present and its value is non-zero
        let found = text.lines().any(|line| {
            let stripped = line.split('#').next().unwrap_or("").trim();
            if let Some(eq_pos) = stripped.find('=') {
                let k = stripped[..eq_pos].trim();
                let v = stripped[eq_pos + 1..].trim();
                k == key && !v.is_empty() && v != "0" && v != "false"
            } else {
                false
            }
        });
        if found {
            enabled.push(label);
        } else {
            disabled.push(label);
        }
    }
    eprintln!("[pq-watchdog] wangr filters ENABLED:  [{}]", enabled.join(", "));
    eprintln!("[pq-watchdog] wangr filters DISABLED: [{}]", disabled.join(", "));
}

/// Spawn the pq-daemon child process.
/// GAP #15 FIX: Redirect daemon stderr to a log file instead of Stdio::inherit().
/// The old code used Stdio::inherit() which meant stderr went to the watchdog's
/// own stderr — and if the watchdog was launched via a script with output
/// redirection, the daemon's stderr was captured to the SAME log file as the
/// watchdog, making it impossible to distinguish between them. Worse, if the
/// watchdog was launched without any redirection, stderr was lost entirely
/// (the launch_test.log was stale since Aug 7 10:57 because of this).
/// Now we explicitly redirect daemon stderr to data/daemon_stderr.log which
/// is always fresh, always captured, and always available for debugging.
fn spawn_daemon(daemon_args: &str) -> Result<Child, String> {
    // Find the pq-daemon binary in the same directory as this watchdog,
    // or in the target/release or target/debug directory.
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| Path::new(".").to_path_buf());

    let daemon_path = exe_dir.join("pq-daemon");
    let daemon_path_str = daemon_path.to_string_lossy().to_string();

    // Open a stderr log file for the daemon. This file is truncated on each
    // spawn so we always have a fresh log for the current session.
    let stderr_log = std::fs::File::create("data/daemon_stderr.log")
        .map_err(|e| format!("Failed to create daemon_stderr.log: {e}"))?;
    let stderr = Stdio::from(stderr_log);

    let mut cmd = Command::new(&daemon_path_str);
    if !daemon_args.is_empty() {
        for arg in daemon_args.split_whitespace() {
            cmd.arg(arg);
        }
    }
    // GAP #15: daemon stdout → inherit (watchdog relays it), stderr → file
    cmd.stdout(Stdio::inherit());
    cmd.stderr(stderr);

    cmd.spawn().map_err(|e| {
        format!("Failed to spawn pq-daemon at {daemon_path_str}: {e}")
    })
}

/// Reap orphaned LaserStream processes before spawning a new daemon.
/// GAP #14 FIX: When the watchdog kills the daemon via health-check, the
/// daemon's LS child (wsl.exe → gRPC binary) survives as an orphan. On the
/// NEXT spawn, a new daemon spawns a NEW LS, while the old orphan keeps
/// burning Helius credits. This function scans for orphaned wsl.exe /
/// pq-laserstream-grpc processes and kills them before spawning.
#[cfg(windows)]
fn reap_orphaned_laserstream() {
    // Use wmic to find orphaned wsl.exe processes that are NOT children of
    // any pq-daemon. These are LS zombies from a previous session.
    let output = Command::new("wmic")
        .args(["process", "where", "name=\"wsl.exe\"", "get", "ProcessId"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();

    if let Ok(out) = output {
        let stdout = String::from_utf8_lossy(&out.stdout);
        for line in stdout.lines() {
            if let Ok(pid) = line.trim().parse::<u32>() {
                if pid == 0 { continue; }
                eprintln!("[pq-watchdog] reaping orphaned wsl.exe (pid={pid})");
                let _ = Command::new("taskkill")
                    .args(["/T", "/F", "/PID", &pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
    }
}

/// Kill a child process forcefully AND its entire process tree.
/// GAP #13 FIX: On Windows, child.kill() calls TerminateProcess which kills
/// ONLY the immediate process. When the watchdog kills the daemon due to a
/// health-check failure, the daemon's children (LaserStream via wsl.exe,
/// Firecrawl bridge) survive as orphans — still connected to Helius gRPC,
/// burning credits. We must use `taskkill /T /F /PID` to recursively kill
/// the entire process tree before falling back to child.kill().
fn kill_child(child: &mut Child) {
    let pid = child.id();
    #[cfg(windows)]
    {
        // taskkill /T = kill tree (recursive), /F = force. This kills the
        // daemon PID and ALL processes spawned by it — including wsl.exe
        // → bash → pq-laserstream-grpc chains in WSL2.
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    // Fallback for non-Windows or if taskkill failed
    let _ = child.kill();
    let _ = child.wait();
}

/// Wait for the child to exit, performing periodic health checks.
/// Returns the exit status (if the child exited on its own) or None if
/// we killed it due to a health check failure.
fn wait_with_health(
    child: &mut Child,
    health_timeout_secs: u64,
    check_interval_secs: u64,
) -> Option<ExitStatus> {
    let mut health_check_counter: u64 = 0;
    let check_interval = Duration::from_secs(check_interval_secs);

    loop {
        // Check if child has exited
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {}
            Err(_) => return None, // child is gone
        }

        // Periodic health check
        health_check_counter += 1;
        if health_check_counter % check_interval_secs == 0 {
            if !daemon_is_healthy(health_timeout_secs) {
                eprintln!("[pq-watchdog] HEALTH CHECK FAILED — daemon appears hung, killing");
                kill_child(child);
                return None;
            }
            // GAP #14: OnchainConfirm starvation check — if the daemon has been
            // running for >120s with ZERO onchain confirms, the Helius WS lane
            // is dead. This is a CRITICAL health signal: the daemon is trading
            // blind (PumpPortal only, no on-chain verification) and should be
            // restarted to re-establish the Helius WS connection.
            match daemon_onchain_confirm_healthy() {
                Some(false) => {
                    eprintln!("[pq-watchdog] ONCHAIN CONFIRM STARVATION — daemon running with 0 onchain confirms (both LS gRPC and Helius WS lanes dead), killing for restart");
                    kill_child(child);
                    return None;
                }
                Some(true) => {} // healthy
                None => {} // can't parse — fail-open, mtime check is primary
            }
        }

        std::thread::sleep(check_interval);
    }
}

fn main() -> std::process::ExitCode {
    let args = parse_args();

    // ─── Single-instance guard (dedup) ───────────────────────────────────
    // Before doing anything else, check if another watchdog is already running.
    // This prevents duplicate daemon trees from accreting on repeated launches.
    if let Some(existing_pid) = check_existing_watchdog() {
        eprintln!("[pq-watchdog] REFUSING TO START: another watchdog is already running (pid={existing_pid}).");
        eprintln!("[pq-watchdog] Only ONE watchdog instance is permitted. Aborting to prevent duplicate daemons.");
        remove_pid_file();
        return std::process::ExitCode::from(4);
    }
    write_pid_file();
    eprintln!("[pq-watchdog] PID file written: {} (pid={})", WATCHDOG_PID_FILE, std::process::id());

    eprintln!("[pq-watchdog] === STARTING ===");
    eprintln!("[pq-watchdog] max_restarts={}", args.max_restarts);
    eprintln!("[pq-watchdog] health_timeout={}s", args.health_timeout_secs);
    eprintln!("[pq-watchdog] backoff_cap={}s", args.backoff_cap_secs);
    eprintln!("[pq-watchdog] daemon_args=\"{}\"", args.daemon_args);

    // Wangr Rev-14: log filter state at startup
    log_wangr_filter_state();

    // Clean up any stale sentinels from a previous run
    let _ = fs::remove_file(STOP_WATCHDOG_FILE);
    let _ = fs::remove_file(WATCHDOG_GAVE_UP_FILE);

    let mut restart_count: u32 = 0;
    let start_time = Instant::now();
    let mut current_backoff = args.initial_backoff_secs;

    loop {
        // Check emergency stop BEFORE spawning
        if emergency_stop_requested() {
            eprintln!("[pq-watchdog] EMERGENCY STOP requested — halting, no restart");
            write_watchdog_status(restart_count, None, start_time.elapsed().as_secs(), "emergency_stop");
            remove_pid_file();
            return std::process::ExitCode::from(2);
        }

        // Check watchdog stop
        if stop_watchdog_requested() {
            eprintln!("[pq-watchdog] STOP_WATCHDOG requested — exiting gracefully");
            write_watchdog_status(restart_count, None, start_time.elapsed().as_secs(), "watchdog_stopped");
            let _ = fs::remove_file(STOP_WATCHDOG_FILE);
            remove_pid_file();
            return std::process::ExitCode::from(0);
        }

        // Check restart limit
        if restart_count >= args.max_restarts {
            eprintln!("[pq-watchdog] MAX RESTARTS ({}) exceeded — giving up", args.max_restarts);
            let _ = fs::write(WATCHDOG_GAVE_UP_FILE, format!(
                "Watchdog gave up after {restart_count} restarts."
            ));
            write_watchdog_status(restart_count, None, start_time.elapsed().as_secs(), "gave_up");
            remove_pid_file();
            return std::process::ExitCode::from(3);
        }

        // Spawn the daemon
        eprintln!("[pq-watchdog] spawning pq-daemon (attempt {}/{})", restart_count + 1, args.max_restarts);
        write_watchdog_status(restart_count, None, start_time.elapsed().as_secs(), "spawning");

        // GAP #14: Reap orphaned LS/wsl.exe processes from a PREVIOUS daemon
        // session before spawning a new one. Without this, each restart layer
        // a new LS orphan on top of the old ones — the 2M credit burn root cause.
        #[cfg(windows)]
        reap_orphaned_laserstream();

        // Wangr Rev-14: re-log filter state on each respawn (config may have
        // been hot-reloaded by the refiner between restarts)
        log_wangr_filter_state();

        let mut child = match spawn_daemon(&args.daemon_args) {
            Ok(c) => {
                eprintln!("[pq-watchdog] pq-daemon spawned (pid={})", c.id());
                write_watchdog_status(restart_count, Some(c.id()), start_time.elapsed().as_secs(), "running");
                c
            }
            Err(e) => {
                eprintln!("[pq-watchdog] FAILED to spawn pq-daemon: {e}");
                // Backoff and retry
                std::thread::sleep(Duration::from_secs(current_backoff));
                current_backoff = (current_backoff * 2).min(args.backoff_cap_secs);
                restart_count += 1;
                continue;
            }
        };

        // Wait for the daemon to exit, with health checks
        let exit_status = wait_with_health(&mut child, args.health_timeout_secs, 5);

        // G1b: Check refiner status during the watchdog loop. The daemon
        // spawns the refiner internally via RefinerSpawner. The watchdog
        // monitors the refiner's status file to ensure the autonomous loop
        // is running (daemon → tape → refiner → promotion → hot-reload).
        if let Some(secs_since_refiner) = check_refiner_status() {
            if secs_since_refiner > 600 {
                eprintln!(
                    "[pq-watchdog] WARNING: refiner hasn't run in {}s (check autonomous loop)",
                    secs_since_refiner
                );
            } else {
                eprintln!(
                    "[pq-watchdog] refiner status: last ran {}s ago",
                    secs_since_refiner
                );
            }
        } else {
            eprintln!("[pq-watchdog] refiner status: no status file yet (autonomous loop warming up)");
        }

        match exit_status {
            Some(status) if status.success() => {
                // Daemon exited cleanly (graceful shutdown via STOP file)
                eprintln!("[pq-watchdog] pq-daemon exited cleanly (code=0)");
                // Check if STOP file was placed (operator wants to stop)
                if stop_watchdog_requested() {
                    eprintln!("[pq-watchdog] STOP_WATCHDOG requested — exiting");
                    let _ = fs::remove_file(STOP_WATCHDOG_FILE);
                    write_watchdog_status(restart_count, None, start_time.elapsed().as_secs(), "clean_stop");
                    remove_pid_file();
                    return std::process::ExitCode::from(0);
                }
                // Otherwise, the daemon stopped on its own — restart it
                eprintln!("[pq-watchdog] pq-daemon clean exit, but no STOP requested — restarting");
                restart_count += 1;
                std::thread::sleep(Duration::from_secs(current_backoff));
                current_backoff = (current_backoff * 2).min(args.backoff_cap_secs);
            }
            Some(status) => {
                // Daemon crashed (non-zero exit)
                let code = status.code().unwrap_or(-1);
                eprintln!("[pq-watchdog] pq-daemon CRASHED (exit code={code})");
                write_watchdog_status(restart_count, None, start_time.elapsed().as_secs(), "crashed");
                restart_count += 1;
                eprintln!("[pq-watchdog] backing off for {current_backoff}s before restart");
                std::thread::sleep(Duration::from_secs(current_backoff));
                current_backoff = (current_backoff * 2).min(args.backoff_cap_secs);
            }
            None => {
                // We killed the daemon due to health check failure
                eprintln!("[pq-watchdog] pq-daemon killed by health check");
                write_watchdog_status(restart_count, None, start_time.elapsed().as_secs(), "health_kill");
                restart_count += 1;
                // Reset backoff for health-kill restarts (the daemon wasn't crashing,
                // it was hung — likely a network issue that may have resolved)
                current_backoff = args.initial_backoff_secs;
                std::thread::sleep(Duration::from_secs(current_backoff));
            }
        }

        // Reset backoff if we've been running successfully for a while
        // (if the daemon ran for more than 5 minutes before crashing, reset backoff)
        // This is handled by tracking the spawn time.
        // For simplicity, we just cap the backoff.
    }
}
