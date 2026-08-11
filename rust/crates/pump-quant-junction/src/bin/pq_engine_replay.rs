//! `pq-engine-replay` — Phase 3: real engine re-simulation via subprocess.
//!
//! This binary is the subprocess the refiner spawns per challenger. It accepts
//! an event-stream JSONL file and a config text file, feeds the events through
//! `Engine::new(cfg, RunMode::Replay).run(&events)`, and emits the resulting
//! `Report` as JSON on stdout. The refiner parses the JSON to score challengers
//! on their ACTUAL admission/sizing/exit decisions — not proxy heuristics.
//!
//! Usage:
//!   pq-engine-replay --event-stream <path> --config <path>
//!
//! Output (stdout, single JSON object):
//!   {"admitted":N,"rejected":N,"net_lamports":I,"promoted":N,"ticks":N,
//!    "events_fed":N,"parse_skipped":N}
//!
//! Exit codes: 0 = success, 1 = arg error, 2 = file not found,
//!             3 = config parse error, 4 = replay produced no result.

use std::env;
use std::fs;

use pump_quant_app::config::Config;
use pump_quant_junction::engine_replay::replay_event_stream_windowed;

const STATEMENT: &str = "\
pq-engine-replay — real engine re-simulation (Phase 3)\n\
Feeds an event stream through the full engine pipeline under a given config.\n\
The output is a genuine Report with admission/sizing/exit decisions.\n\n\
Usage:\n  pq-engine-replay --event-stream <path> --config <path>\n\n\
Options:\n  --event-stream <path>   JSONL event stream file (required)\n  --config <path>          Config text file (key=value format, required)\n  --replay-window-ticks N  Rev-11 §6: Only replay the last N Tick events\n\
                           (0 = full stream, default 0)\n  --help, -h               Show this help\n";

fn main() -> std::process::ExitCode {
    eprintln!("[pq-engine-replay] === ENGINE REPLAY START ===");

    let (event_stream_path, config_path, replay_window_ticks) = match parse_args() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[pq-engine-replay] ERROR: {e}");
            return std::process::ExitCode::from(1);
        }
    };

    eprintln!("[pq-engine-replay] event-stream: {event_stream_path}");
    eprintln!("[pq-engine-replay] config: {config_path}");
    eprintln!("[pq-engine-replay] replay-window-ticks: {replay_window_ticks}");

    // ─── Load config ──────────────────────────────────────────────────────
    let config_text = match fs::read_to_string(&config_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[pq-engine-replay] ERROR: cannot read config file: {e}");
            return std::process::ExitCode::from(2);
        }
    };

    let cfg = match Config::from_str_over_default(&config_text) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[pq-engine-replay] ERROR: config parse error: {e:?}");
            return std::process::ExitCode::from(3);
        }
    };

    eprintln!("[pq-engine-replay] config loaded: gate_expected_move_bps={}, tranches={}, em_model={}",
        cfg.gate_expected_move_bps, cfg.gate_exit_tranches, cfg.expected_move_model_enable);

    // ─── Run engine replay ────────────────────────────────────────────────
    let result = match replay_event_stream_windowed(&event_stream_path, cfg, replay_window_ticks) {
        Some(r) => r,
        None => {
            eprintln!("[pq-engine-replay] ERROR: replay produced no result (empty or unreadable stream)");
            return std::process::ExitCode::from(4);
        }
    };

    eprintln!("[pq-engine-replay] events_fed={} parse_skipped={}", result.events_fed, result.parse_skipped);
    eprintln!("[pq-engine-replay] admitted={} rejected={} net_lamports={}",
        result.report.admitted, result.report.rejected, result.report.net_lamports);

    // ─── Emit JSON on stdout ──────────────────────────────────────────────
    let json = format!(
        "{{\"admitted\":{},\"rejected\":{},\"net_lamports\":{},\"promoted\":{},\"ticks\":{},\"events_fed\":{},\"parse_skipped\":{}}}",
        result.report.admitted,
        result.report.rejected,
        result.report.net_lamports,
        result.report.promoted,
        result.report.ticks,
        result.events_fed,
        result.parse_skipped,
    );
    println!("{json}");

    eprintln!("[pq-engine-replay] === ENGINE REPLAY END ===");
    std::process::ExitCode::from(0)
}

fn parse_args() -> Result<(String, String, u64), String> {
    let mut event_stream_path = String::new();
    let mut config_path = String::new();
    let mut replay_window_ticks: u64 = 0;
    let raw: Vec<String> = env::args().collect();
    let mut idx = 1;
    while idx < raw.len() {
        let arg = &raw[idx];
        match arg.as_str() {
            "--event-stream" => {
                idx += 1;
                if idx >= raw.len() {
                    return Err("--event-stream requires a value".to_string());
                }
                event_stream_path = raw[idx].clone();
            }
            "--config" => {
                idx += 1;
                if idx >= raw.len() {
                    return Err("--config requires a value".to_string());
                }
                config_path = raw[idx].clone();
            }
            "--replay-window-ticks" => {
                idx += 1;
                if idx >= raw.len() {
                    return Err("--replay-window-ticks requires a value".to_string());
                }
                replay_window_ticks = raw[idx].parse().unwrap_or(0);
            }
            "--help" | "-h" => {
                print!("{STATEMENT}");
                std::process::exit(0);
            }
            _ => return Err(format!("unknown argument '{arg}' (try --help)")),
        }
        idx += 1;
    }
    if event_stream_path.is_empty() {
        return Err("--event-stream <path> is required".to_string());
    }
    if config_path.is_empty() {
        return Err("--config <path> is required".to_string());
    }
    Ok((event_stream_path, config_path, replay_window_ticks))
}
