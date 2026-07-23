//! `pq-stream-capture` — the live-stream ingestion spine: push/WS `[S]`
//! capture lanes for the pump-quant system, in Rust.
//!
//! Sibling of the polling suites `../social-ingest-rs` (pure-std Twitch IRC)
//! and `../social-ingest-https-rs` (ureq HTTPS lanes), extending the same
//! discipline to PUSH transports:
//!
//! * [`ws`] — hand-rolled RFC 6455 WebSocket client over rustls (pure frame
//!   codec + reassembly, tested against the RFC vectors);
//! * [`helius_ws`] — Helius Enhanced WebSocket (`transactionSubscribe` +
//!   `accountSubscribe` + `slotSubscribe` heartbeat);
//! * [`pumpportal_ws`] — PumpPortal new-token / migration / trade stream;
//! * [`webhook_listener`] — pure-std HTTP listener for Helius enhanced
//!   webhooks (whale lane), raw + normalized emission;
//! * [`rpc`] — deterministic multi-provider JSON-RPC failover;
//! * [`fees`] — priority-fee calibration sampler (`fee_calibration_v1`).
//!
//! # Constitution discipline (binding)
//! * **§6.3 raw-bytes-first.** Every lane emits the vendor payload untouched
//!   (verbatim text, or the lossless [`json`] round trip) BEFORE any derived
//!   view. Normalized lines (whale, fee calibration) are additive, never a
//!   replacement.
//! * **§6.6/§28 auxiliary tier.** Helius-parsed webhooks and PumpPortal are
//!   third-party interpretation: DISCOVERY/CORROBORATION tier only, never
//!   canonical truth — canonical facts come from raw transactions.
//! * **§22 determinism boundary.** The wall clock is read in `main.rs` and
//!   injected; the frame codec, JSON, normalizers, failover and percentile
//!   logic are pure functions, fixture-tested without sockets.
//! * **§99 bounded state.** Every buffer has a named cap (WS message 8 MiB,
//!   webhook body 2 MiB, dedupe rings, handshake head); every loop a bound.
//! * **§102 named constants.** All tunables are named constants with their
//!   constitution citations at the definition site.
//! * **§18.8 loud degradation.** Missing credentials are fail-closed exit 3
//!   at arming; schema drift, slot gaps, staleness and oversize drops are
//!   loud stderr sentinels, never silence.

pub mod backoff;
pub mod dedupe;
pub mod emit;
pub mod fees;
pub mod helius_ws;
pub mod http;
pub mod json;
pub mod pumpportal_ws;
pub mod rpc;
pub mod webhook_listener;
pub mod ws;

/// Read a watchlist file: one entry per line, `#` comments and blank lines
/// skipped, entries trimmed. Shared by `--accounts-file` / `--watch-file`.
pub fn read_list_file(path: &str) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::read_list_file;

    #[test]
    fn list_file_skips_comments_and_blanks() {
        let dir = std::env::temp_dir().join("pq-stream-capture-listfile-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("accounts.txt");
        std::fs::write(&path, "# header\n\n  Acct1  \nAcct2\n#tail\n").unwrap();
        let list = read_list_file(path.to_str().unwrap()).unwrap();
        assert_eq!(list, vec!["Acct1".to_string(), "Acct2".to_string()]);
    }

    #[test]
    fn missing_list_file_is_an_error_not_a_panic() {
        assert!(read_list_file("/nonexistent/nope.txt").is_err());
    }
}
