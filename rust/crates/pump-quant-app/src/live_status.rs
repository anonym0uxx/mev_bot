//! §60/§62 canonical live-status artifact (LAW 21): a periodic, bounded,
//! deterministic snapshot of the running engine written to `data/live_status.json`.
//!
//! The status is a FIXED set of scalar fields — no unbounded collections (§99) —
//! and every field is a deterministic function of the recorded event stream. In
//! particular the timestamp is **info-time**: the engine's logical tick taken from
//! the event stream, never a wall-clock read (a wall-clock would make two replays
//! of the same tape produce different artifacts, breaking determinism, §22/§54).
//! The JSON is emitted with a fixed key order so two runs over the same tape write
//! byte-identical files.

use std::io::Write;

/// A bounded, deterministic snapshot of the running engine (§60/§62).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveStatus {
    /// Info-time: the engine's logical tick from the event stream (never wall-clock).
    pub info_time_tick: u64,
    /// Candidates promoted to the gate so far.
    pub promoted: u64,
    /// Positions admitted so far.
    pub admitted: u64,
    /// Candidates rejected so far.
    pub rejected: u64,
    /// Currently-open positions.
    pub open_positions: u64,
    /// Running realized net-SOL, lamports (realized-only; marks never count).
    pub net_realized_lamports: i128,
    /// Mature-but-inactive candidates removed by the §21.5 universe screen.
    pub universe_filtered: u64,
    /// Sub-`x_min` paid-information probes budget-accounted so far (§33/§43).
    pub probes_budgeted: u64,
    /// Lifetime calibration/probe research spend, lamports.
    pub probe_spend_lamports: u64,
    /// Canonical decision-journal digest at this info-time.
    pub journal_digest: u64,
}

impl LiveStatus {
    /// The status schema tag — bumped if the field set ever changes.
    pub const SCHEMA: &'static str = "live_status/1";

    /// Serialize to canonical JSON: a fixed key order, integer/decimal values, the
    /// digest as a fixed-width hex string. Two runs over the same tape produce the
    /// byte-identical string.
    #[must_use]
    pub fn to_canonical_json(&self) -> String {
        format!(
            concat!(
                "{{\"schema\":\"{}\",",
                "\"info_time_tick\":{},",
                "\"promoted\":{},",
                "\"admitted\":{},",
                "\"rejected\":{},",
                "\"open_positions\":{},",
                "\"net_realized_lamports\":{},",
                "\"universe_filtered\":{},",
                "\"probes_budgeted\":{},",
                "\"probe_spend_lamports\":{},",
                "\"journal_digest\":\"{:#018x}\"}}"
            ),
            Self::SCHEMA,
            self.info_time_tick,
            self.promoted,
            self.admitted,
            self.rejected,
            self.open_positions,
            self.net_realized_lamports,
            self.universe_filtered,
            self.probes_budgeted,
            self.probe_spend_lamports,
            self.journal_digest,
        )
    }

    /// Write the canonical JSON to `path`, creating parent directories as needed.
    /// Atomic-ish: writes then flushes; a partial write surfaces as an `Err`.
    pub fn write_to_path(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut f = std::fs::File::create(path)?;
        f.write_all(self.to_canonical_json().as_bytes())?;
        f.write_all(b"\n")?;
        f.flush()
    }
}

#[cfg(test)]
mod tests {

    /// A curve that has been bought into by 0.2 SOL: the price reserve is the 30 SOL
    /// seed plus the raise, and the escrowed (extractable) SOL is the raise itself.
    /// `real_sol = virtual_sol - LAUNCH_VSOL_LAMPORTS` is the venue's identity, not a
    /// choice — see `crate::curve_state::real_sol_for`.
    const CURVE_REAL_SOL: u64 = 200_000_000;
    const CURVE_VSOL: u64 = crate::curve_state::LAUNCH_VSOL_LAMPORTS + CURVE_REAL_SOL;
    use crate::config::Config;
    use crate::engine::{Engine, RunMode};
    use crate::event::AppEvent;
    use pump_quant_domain::ids::Mint;

    fn mint(tag: u64) -> Mint {
        let mut b = [0u8; 32];
        b[..8].copy_from_slice(&tag.to_le_bytes());
        b[8] = 0xAB;
        Mint::from_bytes(b)
    }

    fn drive() -> Engine {
        let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
        for m in 0..6u64 {
            let mt = mint(m);
            for i in 0..3u64 {
                eng.tick(AppEvent::MarketTrade {
                    mint: mt,
                    price_fp: 1_000_000_000 + (i as i128) * 1_000_000 + (m as i128) * 1_000,
                    quote_lamports: 500_000,
                    liquidity_lamports: CURVE_VSOL,
                    signed_base: 600_000,
                    buyer_entity: m % 7,
                    age_slots: 12,
                });
            }
            // RE-EXPRESSED (2026-07-28): this harness used to declare a 0.2 SOL
            // "pool" and a 0.2 SOL sellable depth — a market that cannot exist on
            // this venue, where a curve is seeded with 30 SOL of VIRTUAL reserve and
            // escrows `virtual_sol - 30 SOL`. The reserve is now a real curve whose
            // extractable depth is still 0.2 SOL, so the report surface exercises the
            // same shape against a market the venue could actually produce.
            eng.tick(AppEvent::OnchainConfirm {
                mint: mt,
                virtual_sol_lamports: CURVE_VSOL,
                real_sol_lamports: CURVE_REAL_SOL,
            });
        }
        for _ in 0..8 {
            eng.tick(AppEvent::Tick);
        }
        eng
    }

    #[test]
    fn writer_emits_expected_keys_deterministically() {
        let a = drive().live_status();
        let b = drive().live_status();
        assert_eq!(a, b, "same tape -> identical status snapshot");
        let json = a.to_canonical_json();
        // Deterministic: identical JSON across two identical runs.
        assert_eq!(json, b.to_canonical_json());
        // The expected canonical keys are all present.
        for key in [
            "\"schema\":\"live_status/1\"",
            "\"info_time_tick\":",
            "\"promoted\":",
            "\"admitted\":",
            "\"rejected\":",
            "\"open_positions\":",
            "\"net_realized_lamports\":",
            "\"universe_filtered\":",
            "\"probes_budgeted\":",
            "\"probe_spend_lamports\":",
            "\"journal_digest\":\"0x",
        ] {
            assert!(json.contains(key), "missing key {key} in {json}");
        }
        // Info-time is the event-stream tick, not a wall-clock — non-zero after the
        // driven ticks and identical across replays (already asserted equal above).
        assert!(a.info_time_tick > 0);
    }

    #[test]
    fn write_to_path_roundtrips() {
        let st = drive().live_status();
        let dir = std::env::temp_dir().join(format!("pq_live_status_{}", std::process::id()));
        let path = dir.join("live_status.json");
        st.write_to_path(&path).expect("write status");
        let read = std::fs::read_to_string(&path).expect("read status");
        assert_eq!(read.trim_end(), st.to_canonical_json());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
