//! `pq-social-capture` — the HTTPS `[S]` capture lanes, in Rust.
//!
//! Rust twins of the polling Python adapters in `../social-ingest/`:
//! `twitterapi` (twitterapi_stream.py), `tiktok` (tiktok_stream.py) and
//! `firecrawl` (firecrawl_stream.py). Each subcommand polls the SAME endpoint
//! with the SAME query parameters, watchlists, cursor semantics and cadence
//! defaults as its Python twin, and emits the IDENTICAL normalized NDJSON
//! (`normalize.py` schema, plus the Twitch lane's `observed_at_ns` capture
//! stamp) on stdout. Telegram is deliberately NOT here: MTProto needs a heavy
//! SDK (grammers), so that lane stays Python (see README).
//!
//! # Constitution discipline (binding)
//! * **§22 determinism boundary.** The wall clock is read only in `main.rs` at
//!   the capture edge; every module in this library is a pure function of its
//!   inputs. `--replay` mode is fully deterministic (synthetic monotone
//!   timestamps, zero network) so tests and replays are byte-stable.
//! * **§29 provenance.** Origin identity (author / community) is carried
//!   verbatim; trust is earned downstream in the D-ledger, never here.
//! * **§67 removable adapter.** One binary, one dependency (`ureq`), speaks
//!   only the shared NDJSON contract on stdout. Delete it and the system loses
//!   the Rust HTTPS lanes; the Python twins still work.
//! * **§83.** No sentiment, no opinion, no decision — capture only.

pub mod backoff;
pub mod dedupe;
pub mod emit;
pub mod firecrawl;
pub mod http;
pub mod json;
pub mod tiktok;
pub mod twitterapi;
pub mod urlenc;
pub mod yaml;

use std::io::Write as _;

/// Replay-mode synthetic clock: base + step per emitted event. Identical
/// constants to `pq-twitch-capture` so every Rust lane's replay output shares
/// one deterministic time base (§22 — no clock behind the boundary).
pub const REPLAY_BASE_NS: u64 = 1_000_000_000;
/// Per-event step of the replay clock (see [`REPLAY_BASE_NS`]).
pub const REPLAY_STEP_NS: u64 = 1_000_000;

/// Shared `--replay` driver: read a saved raw-API-response fixture (one JSON
/// value per poll/page, whitespace-separated), hand each response to the
/// adapter's `process` closure together with the cross-poll dedupe ring and an
/// emit sink that stamps the deterministic synthetic clock. Zero network, zero
/// wall clock — byte-identical run-to-run (§22); this is what the integration
/// tests drive.
pub fn replay_pages(
    path: &str,
    mut process: impl FnMut(&json::Value, &mut dedupe::DedupeRing, &mut dyn FnMut(&emit::Event<'_>)),
) -> u8 {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[pq-social-capture] replay failed: {path}: {e}");
            return 1;
        }
    };
    let values = match json::parse_stream(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[pq-social-capture] replay failed: bad JSON in {path}: {e}");
            return 1;
        }
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut ring = dedupe::DedupeRing::new(dedupe::DEFAULT_CAP);
    let mut emitted: u64 = 0;
    for value in &values {
        let mut sink = |ev: &emit::Event<'_>| {
            let ts = REPLAY_BASE_NS + emitted * REPLAY_STEP_NS;
            let line = emit::event_line(ev, ts);
            let _ = out.write_all(line.as_bytes());
            let _ = out.write_all(b"\n");
            let _ = out.flush();
            emitted += 1;
        };
        process(value, &mut ring, &mut sink);
    }
    eprintln!("[pq-social-capture] replay: emitted {emitted} events");
    0
}

/// Mirror of Python `str(float)` for the cadence diagnostics: `5.0` for whole
/// numbers, shortest form otherwise, so stderr matches the Python twins.
#[must_use]
pub fn py_float(f: f64) -> String {
    if f.fract() == 0.0 && f.is_finite() {
        format!("{f:.1}")
    } else {
        format!("{f}")
    }
}

#[cfg(test)]
mod tests {
    use super::py_float;

    #[test]
    fn py_float_matches_python_str() {
        assert_eq!(py_float(5.0), "5.0");
        assert_eq!(py_float(2.5), "2.5");
        assert_eq!(py_float(0.0), "0.0");
    }
}
