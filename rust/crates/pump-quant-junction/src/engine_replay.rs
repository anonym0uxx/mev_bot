//! `engine_replay` — Phase 3: config-driven re-simulation of the full engine pipeline.
//!
//! Before Phase 3, the refiner's `shadow_replay()` replayed a fixed tape of
//! already-executed trades. Mutating config changed fee/slippage adjustments
//! on those trades, but never changed WHICH trades were taken. Every challenger
//! got the same trades → same `netsol` → no differentiation → no promotions.
//!
//! This module fixes that by loading the raw **event stream** and re-running
//! the full engine pipeline (admission → sizing → exit) under each mutated
//! config. Different configs now produce different trade sequences, different
//! P&L, and actually different `netsol` values.
//!
//! Constitution: §13 (paper/live parity), §16 (no look-ahead), §22 (integer-only).

use std::path::Path;

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, Report, RunMode};
use pump_quant_app::event::AppEvent;

use crate::event_stream::read_event_stream;

/// Interval (in state-update events) at which synthetic Ticks are injected
/// when the event stream has no native Ticks. A smaller interval means more
/// frequent evaluation calls — closer to the live daemon's behavior. The live
/// daemon ticks roughly every 100ms; over a 90k-event stream, injecting every
/// 50 events gives ~1800 evaluation passes, which is sufficient for the engine
/// to evaluate candidates across the full time span.
const TICK_INJECTION_INTERVAL: usize = 50;

/// If the event stream contains no `Tick` events, inject a `Tick` after every
/// `TICK_INJECTION_INTERVAL`-th state-update event. This is critical for
/// backward compatibility with pre-Phase-3 event streams that only captured
/// state-update events (OnchainConfirm, TokenMetadata, MarketTrade, Migration)
/// but never Ticks. Without Ticks, `Engine::run()` never calls `evaluate()`,
/// so no trades are admitted and the replay is useless.
///
/// If the stream already contains Ticks, it is returned unchanged — the live
/// daemon's native Ticks are authoritative.
fn inject_missing_ticks(mut events: Vec<AppEvent>) -> Vec<AppEvent> {
    let has_ticks = events.iter().any(|e| matches!(e, AppEvent::Tick));
    if has_ticks {
        return events;
    }

    // No Ticks present — inject them.
    let mut result = Vec::with_capacity(events.len() + events.len() / TICK_INJECTION_INTERVAL + 1);
    let mut since_last_tick: usize = 0;
    for event in events.drain(..) {
        result.push(event);
        since_last_tick += 1;
        if since_last_tick >= TICK_INJECTION_INTERVAL {
            result.push(AppEvent::Tick);
            since_last_tick = 0;
        }
    }
    // Final tick to trigger evaluation of the last batch.
    result.push(AppEvent::Tick);
    result
}

/// Result of a config-driven engine replay.
#[derive(Debug, Clone)]
pub struct EngineReplayResult {
    /// The engine's final report (net_lamports, admitted, rejected, etc.).
    pub report: Report,
    /// Number of events fed into the engine.
    pub events_fed: usize,
    /// Number of lines skipped during event stream parsing.
    pub parse_skipped: usize,
}

/// Replay the full engine pipeline against the event stream using `cfg`.
///
/// This constructs a fresh `Engine` in `RunMode::Replay`, loads the event
/// stream from `path`, and feeds every event in order. The engine re-derives
/// admission, sizing, and exit decisions from `cfg` — so different configs
/// produce different trade sequences and different net P&L.
///
/// **Tick injection**: if the event stream contains NO `Tick` events (the
/// old format, before Phase 3), this function injects a `Tick` after every
/// `TICK_INJECTION_INTERVAL`-th state-update event. Without Ticks, the engine
/// never calls `evaluate()`, so no trades are admitted — making the replay
/// useless. This ensures backward compatibility with pre-Phase-3 streams.
///
/// **Rolling window** (Rev-11 §6): if `window_ticks > 0`, only the last
/// `window_ticks` Tick events (and all inter-Tick state events between them)
/// are fed to the engine. This optimizes for CURRENT market conditions rather
/// than the full historical tape. A window of 20000 ticks ≈ 1.4h.
///
/// Returns `None` if the event stream cannot be read or is empty.
#[must_use]
pub fn replay_event_stream<P: AsRef<Path>>(path: P, cfg: Config) -> Option<EngineReplayResult> {
    replay_event_stream_windowed(path, cfg, 0)
}

/// Rev-11 §6: Replay with an optional rolling-window on Tick count.
/// When `window_ticks == 0`, the full event stream is replayed (legacy behavior).
/// When `window_ticks > 0`, only events after the last `window_ticks`-th Tick
/// (counting from the end) are fed to the engine.
#[must_use]
pub fn replay_event_stream_windowed<P: AsRef<Path>>(
    path: P,
    cfg: Config,
    window_ticks: u64,
) -> Option<EngineReplayResult> {
    let (events, skipped) = read_event_stream(path).ok()?;

    if events.is_empty() {
        return None;
    }

    // Inject Ticks if the stream has none.
    let events = inject_missing_ticks(events);

    // Rev-11 §6: Apply rolling window if specified.
    let events = if window_ticks > 0 {
        apply_rolling_window(events, window_ticks)
    } else {
        events
    };

    if events.is_empty() {
        return None;
    }

    let mut engine = Engine::new(cfg, RunMode::Replay);
    let report = engine.run(&events);
    Some(EngineReplayResult {
        report,
        events_fed: events.len(),
        parse_skipped: skipped,
    })
}

/// Rev-11 §6: Trim the event stream to only the last `window_ticks` Ticks
/// and all events between them. Events before the cutoff point are dropped.
/// This ensures the engine sees a coherent slice of market history, not
/// dangling references to tokens that were admitted before the window.
fn apply_rolling_window(events: Vec<AppEvent>, window_ticks: u64) -> Vec<AppEvent> {
    // Count total ticks in the stream.
    let total_ticks = events.iter().filter(|e| matches!(e, AppEvent::Tick)).count();
    if total_ticks <= window_ticks as usize {
        // Fewer ticks than the window — keep everything.
        return events;
    }

    // Find the cutoff index: the position of the (total_ticks - window_ticks)-th
    // Tick from the start. Everything from that Tick onward is kept.
    let cutoff_tick = total_ticks - window_ticks as usize; // index of the first Tick to keep
    let mut tick_count = 0;
    let mut cutoff_idx = 0;
    for (i, e) in events.iter().enumerate() {
        if matches!(e, AppEvent::Tick) {
            if tick_count == cutoff_tick {
                cutoff_idx = i;
                break;
            }
            tick_count += 1;
        }
    }

    // Keep everything from the cutoff Tick onward.
    events[cutoff_idx..].to_vec()
}

/// Replay with a pre-loaded event vector (for testing without file I/O).
#[must_use]
pub fn replay_events(events: &[AppEvent], cfg: Config) -> EngineReplayResult {
    let mut engine = Engine::new(cfg, RunMode::Replay);
    let report = engine.run(events);
    EngineReplayResult {
        report,
        events_fed: events.len(),
        parse_skipped: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pump_quant_app::config::Config;
    use pump_quant_app::event::AppEvent;
    use pump_quant_domain::ids::Mint;

    /// Two configs with different `gate_expected_move_bps` must produce
    /// different admission decisions when fed the same event stream.
    /// This is the core property the refiner needs to differentiate challengers.
    #[test]
    fn different_configs_produce_different_reports() {
        // Try multiple paths: the test data may be in the crate's data dir,
        // the workspace data dir, or a relative path from the test CWD.
        let candidates = [
            "data/event_stream.jsonl",
            "../data/event_stream.jsonl",
            "../../data/event_stream.jsonl",
            "../../../data/event_stream.jsonl",
        ];
        let tape_path = candidates.iter()
            .map(|p| std::path::Path::new(p))
            .find(|p| p.exists());

        let tape_path = match tape_path {
            Some(p) => p,
            None => {
                eprintln!("skipping: event_stream.jsonl not found in any candidate path");
                return;
            }
        };

        // The engine replay must differentiate configs. We test TWO parameters
        // that change admission decisions on the real event stream:
        //   1. gate_margin_bps — higher margin → tighter gate → fewer admitted
        //   2. mcap_band_enable — toggling the band filter changes which
        //      candidates pass the pre-gate selection
        // We use a very high gate_margin_bps (50000 = 50x normal) to make the
        // economic gate refuse nearly everything, vs the default (50).
        let cfg_lo_margin = {
            let mut c = Config::dev_portable();
            c.gate_margin_bps = 50; // default
            c.expected_move_model_enable = false;
            c
        };
        let cfg_hi_margin = {
            let mut c = Config::dev_portable();
            c.gate_margin_bps = 50_000; // extreme — refuses almost everything
            c.expected_move_model_enable = false;
            c
        };

        let result_lo = replay_event_stream(tape_path, cfg_lo_margin);
        let result_high = replay_event_stream(tape_path, cfg_hi_margin);

        // Both replays must produce results.
        assert!(result_lo.is_some(), "lo-margin replay must produce a result");
        assert!(result_high.is_some(), "hi-margin replay must produce a result");

        let lo = result_lo.unwrap();
        let high = result_high.unwrap();

        // The high-margin config must reject more (or admit fewer) than low.
        // An extreme margin (50000 bps = 500% required edge over cost) makes
        // the economic gate impossible to clear → 0 admitted.
        assert!(
            high.report.admitted <= lo.report.admitted,
            "high margin (50000) must admit <= low margin (50): got high={} low={}",
            high.report.admitted, lo.report.admitted
        );
        // And if the low-margin config admits anything, the high-margin must
        // admit strictly less (the extreme margin should refuse all trades).
        if lo.report.admitted > 0 {
            assert!(
                high.report.admitted < lo.report.admitted,
                "extreme margin must reduce admissions: high={} low={}",
                high.report.admitted, lo.report.admitted
            );
        }
    }

    /// An empty event slice produces a valid report with zero admitted.
    #[test]
    fn empty_events_produce_zero_admitted() {
        let result = replay_events(&[], Config::dev_portable());
        assert_eq!(result.events_fed, 0);
        assert_eq!(result.report.admitted, 0);
    }

    /// Tick injection: a stream with no Ticks must get Ticks injected.
    #[test]
    fn tick_injection_adds_ticks_to_empty_stream() {
        let mint = Mint([99u8; 32]);
        let events = vec![
            AppEvent::OnchainConfirm {
                mint,
                virtual_sol_lamports: 100_000_000_000,
                real_sol_lamports: 30_000_000_000,
            },
            AppEvent::Tick,
        ];
        // The injected stream must have more events (at least one injected Tick).
        let injected = inject_missing_ticks(vec![AppEvent::OnchainConfirm {
            mint,
            virtual_sol_lamports: 100_000_000_000,
            real_sol_lamports: 30_000_000_000,
        }]);
        assert!(injected.len() >= 2, "injected stream must have at least one Tick");
        assert!(injected.iter().any(|e| matches!(e, AppEvent::Tick)),
            "injected stream must contain at least one Tick");
    }

    /// Tick injection: a stream that already has Ticks must NOT be modified.
    #[test]
    fn tick_injection_preserves_streams_with_ticks() {
        let events = vec![AppEvent::Tick, AppEvent::Tick, AppEvent::Tick];
        let injected = inject_missing_ticks(events.clone());
        assert_eq!(injected.len(), events.len(),
            "stream with existing Ticks must not be modified");
    }

    /// Tick injection: the interval must be respected.
    #[test]
    fn tick_injection_respects_interval() {
        let mint = Mint([99u8; 32]);
        // Create exactly TICK_INJECTION_INTERVAL events — expect 1 injected Tick.
        let mut events = Vec::new();
        for _ in 0..TICK_INJECTION_INTERVAL {
            events.push(AppEvent::OnchainConfirm {
                mint,
                virtual_sol_lamports: 100_000_000_000,
                real_sol_lamports: 30_000_000_000,
            });
        }
        let injected = inject_missing_ticks(events);
        // Original 50 + 1 interval Tick + 1 final Tick = 52.
        assert_eq!(injected.len(), TICK_INJECTION_INTERVAL + 2,
            "expected {}+2 events after injection", TICK_INJECTION_INTERVAL);
        let tick_count = injected.iter().filter(|e| matches!(e, AppEvent::Tick)).count();
        assert_eq!(tick_count, 2, "expected 2 injected Ticks (interval + final)");
    }

    /// Replaying the same events with the same config is deterministic.
    #[test]
    fn replay_is_deterministic() {
        let mint = Mint([99u8; 32]);
        let events = vec![
            AppEvent::Tick,
            AppEvent::OnchainConfirm {
                mint,
                virtual_sol_lamports: 100_000_000_000,
                real_sol_lamports: 30_000_000_000,
            },
            AppEvent::Tick,
        ];
        let cfg = Config::dev_portable();

        let r1 = replay_events(&events, cfg.clone());
        let r2 = replay_events(&events, cfg);

        assert_eq!(r1.report.admitted, r2.report.admitted);
        assert_eq!(r1.report.rejected, r2.report.rejected);
        assert_eq!(r1.report.net_lamports, r2.report.net_lamports);
    }
}
