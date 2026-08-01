//! `junction-run` — stage 4 wire-up binary.
//!
//! Connects the junction queue to the engine event loop ALONGSIDE the existing
//! `parse_events` text-file path. Both sources feed the same AppEvent stream
//! into `Engine::tick`, preserving the golden-digest certification path.
//!
//! Usage:
//!   junction-run paper <config-file> <events-file> [--junction-cap N]
//!
//! In `paper` mode:
//!   1. Text-file events are parsed via `pump_quant_app::parse::parse_events`
//!      (the EXACT same parser the main binary uses — golden digest is a
//!      property of that parser's output, §54).
//!   2. The junction queue is drained between batches. In this initial wire-up
//!      the queue is fed by a test harness (no live WS feed is available on the
//!      free lane — see Task 3 findings). The drain path is exercised by the
//!      overflow test (Task 2c).
//!   3. Both sources' AppEvents go through `Engine::tick` in order: text events
//!      first, then junction-queue events drained to empty.
//!
//! The golden digest invariant: if the junction queue is empty (no live feed),
//! the output MUST match the main binary's output exactly, because the same
//! events go through the same parser into the same engine. This is verified
//! by the `golden_digest_unchanged_with_empty_junction` test in the junction
//! crate's test suite.

use std::process::ExitCode;

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::parse::parse_events;
use pump_quant_junction::queue::BoundedJunctionQueue;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!(
            "usage: {} <paper|replay> <config-file> <events-file> [--junction-cap N]",
            args[0]
        );
        return ExitCode::from(2);
    }

    let mut junction_cap: usize = 4096;
    let mut i = 4;
    while i + 1 < args.len() {
        match args[i].as_str() {
            "--junction-cap" => {
                junction_cap = args[i + 1].parse().unwrap_or(4096);
            }
            other => {
                eprintln!("unknown flag '{other}'");
                return ExitCode::from(2);
            }
        }
        i += 2;
    }

    let mode = match args[1].as_str() {
        "paper" => RunMode::Paper,
        "replay" => RunMode::Replay,
        "live" => {
            eprintln!(
                "refused: live capital is Tier-0 human-gated and is not available from this binary"
            );
            return ExitCode::from(3);
        }
        other => {
            eprintln!("unknown mode '{other}' (expected paper|replay)");
            return ExitCode::from(2);
        }
    };

    let cfg_text = match std::fs::read_to_string(&args[2]) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot read config {}: {e}", args[2]);
            return ExitCode::from(1);
        }
    };
    let mut cfg = match Config::from_str_over_default(&cfg_text) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("bad config: {e}");
            return ExitCode::from(1);
        }
    };
    let _ = &mut cfg; // suppress unused_mut warning

    let events_text = match std::fs::read_to_string(&args[3]) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot read events {}: {e}", args[3]);
            return ExitCode::from(1);
        }
    };
    let text_events = match parse_events(&events_text) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("bad events: {e}");
            return ExitCode::from(1);
        }
    };

    // ─── Wire-up: junction queue alongside parse_events ───────────────────
    //
    // The junction queue is the live-feed drain point. In this initial wire-up
    // it is empty (no live WS feed on the free lane). The drain path is
    // exercised by the overflow test in the junction crate's test suite.
    // When a live feed is connected (Task 3 / future Developer key), the
    // queue is fed by PumpPortal WS trades + Helius accountSubscribe snapshots.
    let queue = BoundedJunctionQueue::with_capacity(junction_cap);

    let mut engine = Engine::new(cfg, mode);

    // Feed text-file events first (the golden-digest certification path).
    for ev in &text_events {
        engine.tick(*ev);
    }

    // Drain the junction queue — any ProvenancedEvent from live feeds.
    // Each event's .event field is the AppEvent; .source/.slot/.is_live are
    // provenance (criterion 65) logged but not consumed by the engine's tick.
    let mut junction_events_drained = 0u64;
    while let Some(provenanced) = queue.pop() {
        engine.tick(provenanced.event);
        junction_events_drained += 1;
    }

    let report = engine.run(&[]); // finalize (already ticked all events)

    // Suppress unused warnings for fields we report on but the binary doesn't
    // use for control flow.
    let _ = junction_events_drained;

    println!("mode              {:?}", engine.mode());
    println!("ticks             {}", report.ticks);
    println!("promoted          {}", report.promoted);
    println!("admitted          {}", report.admitted);
    println!("rejected          {}", report.rejected);
    println!("universe_filtered {}", report.universe_filtered);
    println!("net_lamports      {}", report.net_lamports);
    for (lane, net) in &report.per_lane_net {
        println!("  lane {lane:?}: net {net}");
    }
    for (lane, w) in &report.final_weights {
        println!("  weight {lane:?}: {w} bp");
    }
    println!("journal_digest    {:#018x}", report.journal_digest);
    println!("junction_drained  {}", junction_events_drained);
    let overflow = queue.overflow_stats();
    println!("junction_overflow {} (last_drop_slot {})", overflow.dropped, overflow.last_drop_slot);

    ExitCode::SUCCESS
}
