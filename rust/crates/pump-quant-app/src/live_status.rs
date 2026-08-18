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

/// One open position at session end (item 2c: pin open positions).
/// Report-plane only — never read by a gate, size, rank, or exit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenPositionSnapshot {
    /// The mint address of the open position.
    pub mint: [u8; 32],
    /// Entry tick (logical, from event stream).
    pub entry_tick: u64,
    /// Entry price (fixed-point, fp18).
    pub entry_price_fp: u64,
    /// Current/latest tick at snapshot time.
    pub current_tick: u64,
    /// Mark price at snapshot time (fixed-point, fp18).
    pub mark_price_fp: u64,
    /// Unrealized PnL in lamports (mark - entry) × remaining size.
    pub unrealized_pnl_lamports: i128,
    /// Remaining fraction in bps (10000 = full).
    pub remaining_bps: u32,
}

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
    /// Per-code reject histogram (index = reject code 1..18, 0 = unused slot).
    /// Exact population — every rejection increments both `rejected` and the
    /// matching slot here, so `sum(reject_counts) == rejected` is an invariant.
    pub reject_counts: [u64; 32],
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
    // ─── Rev-19 on-chain feedback counters ───────────────────────────────
    /// Live buy txs submitted to the sink (accepted).
    pub live_outbound_successes: u64,
    /// Live buy txs that failed at the sink (construction/signer/sender).
    pub live_outbound_failures: u64,
    /// Live sell txs submitted to the sink (accepted).
    pub live_sell_successes: u64,
    /// Live sell txs that failed at the sink.
    pub live_sell_failures: u64,
    /// Buy txs confirmed on-chain (landed successfully, tokens received).
    pub buy_confirmed_count: u64,
    /// Buy txs that failed on-chain (tokens NOT received, fee burned).
    pub buy_failed_count: u64,
    /// Sell txs confirmed on-chain (SOL recovered).
    pub sell_confirmed_count: u64,
    /// Sell txs that failed on-chain (SOL NOT recovered).
    pub sell_failed_count: u64,
    /// Pending buy txs awaiting on-chain confirmation.
    pub pending_buy_count: u64,
    /// Pending sell txs awaiting on-chain confirmation.
    pub pending_sell_count: u64,
}

impl OpenPositionSnapshot {
    /// Serialize a list of open positions to canonical JSON for telemetry.
    pub fn to_json_list(positions: &[Self]) -> String {
        let mut s = String::from("[");
        for (i, p) in positions.iter().enumerate() {
            if i > 0 { s.push(','); }
            // Convert fp18 prices to SOL for readability
            let entry_sol = p.entry_price_fp as f64 / 1e18;
            let mark_sol = p.mark_price_fp as f64 / 1e18;
            let pnl_sol = p.unrealized_pnl_lamports as f64 / 1e9;
            let ret_pct = if p.entry_price_fp > 0 {
                (p.mark_price_fp as f64 / p.entry_price_fp as f64 - 1.0) * 100.0
            } else { 0.0 };
            // Encode mint bytes as hex without external dependency
            let mint_hex: String = p.mint.iter().map(|b| format!("{:02x}", b)).collect();
            s.push_str(&format!(
                "{{\"mint\":\"{}\",\"entry_tick\":{},\"entry_price_sol\":{:.8},\"mark_price_sol\":{:.8},\"return_pct\":{:.2},\"unrealized_pnl_sol\":{:.6},\"remaining_bps\":{}}}",
                mint_hex,
                p.entry_tick,
                entry_sol,
                mark_sol,
                ret_pct,
                pnl_sol,
                p.remaining_bps,
            ));
        }
        s.push(']');
        s
    }

    /// Write the open positions JSON to a file (best-effort telemetry).
    pub fn write_to_path(positions: &[Self], path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut f = std::fs::File::create(path)?;
        f.write_all(Self::to_json_list(positions).as_bytes())?;
        f.write_all(b"\n")?;
        f.flush()
    }
}

impl LiveStatus {
    /// The status schema tag — bumped if the field set ever changes.
    pub const SCHEMA: &'static str = "live_status/2";

    /// Serialize to canonical JSON: a fixed key order, integer/decimal values, the
    /// digest as a fixed-width hex string. Two runs over the same tape produce the
    /// byte-identical string.
    #[must_use]
    pub fn to_canonical_json(&self) -> String {
        // Render reject_counts as a JSON array of 32 integers.
        let mut rc = String::from("[");
        for (i, &c) in self.reject_counts.iter().enumerate() {
            if i > 0 { rc.push(','); }
            rc.push_str(&c.to_string());
        }
        rc.push(']');
        format!(
            concat!(
                "{{\"schema\":\"{}\",",
                "\"info_time_tick\":{},",
                "\"promoted\":{},",
                "\"admitted\":{},",
                "\"rejected\":{},",
                "\"reject_counts\":{},",
                "\"open_positions\":{},",
                "\"net_realized_lamports\":{},",
                "\"universe_filtered\":{},",
                "\"probes_budgeted\":{},",
                "\"probe_spend_lamports\":{},",
                "\"journal_digest\":\"{:#018x}\",",
                "\"live_outbound_successes\":{},",
                "\"live_outbound_failures\":{},",
                "\"live_sell_successes\":{},",
                "\"live_sell_failures\":{},",
                "\"buy_confirmed_count\":{},",
                "\"buy_failed_count\":{},",
                "\"sell_confirmed_count\":{},",
                "\"sell_failed_count\":{},",
                "\"pending_buy_count\":{},",
                "\"pending_sell_count\":{}}}"
            ),
            Self::SCHEMA,
            self.info_time_tick,
            self.promoted,
            self.admitted,
            self.rejected,
            rc,
            self.open_positions,
            self.net_realized_lamports,
            self.universe_filtered,
            self.probes_budgeted,
            self.probe_spend_lamports,
            self.journal_digest,
            self.live_outbound_successes,
            self.live_outbound_failures,
            self.live_sell_successes,
            self.live_sell_failures,
            self.buy_confirmed_count,
            self.buy_failed_count,
            self.sell_confirmed_count,
            self.sell_failed_count,
            self.pending_buy_count,
            self.pending_sell_count,
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
            "\"schema\":\"live_status/2\"",
            "\"info_time_tick\":",
            "\"promoted\":",
            "\"admitted\":",
            "\"rejected\":",
            "\"reject_counts\":",
            "\"open_positions\":",
            "\"net_realized_lamports\":",
            "\"universe_filtered\":",
            "\"probes_budgeted\":",
            "\"probe_spend_lamports\":",
            "\"journal_digest\":\"0x",
        ] {
            assert!(json.contains(key), "missing key {key} in {json}");
        }
        // The reject histogram invariant: sum(reject_counts) == rejected.
        let sum: u64 = a.reject_counts.iter().sum();
        assert_eq!(sum, a.rejected, "histogram sum {} != rejected {}", sum, a.rejected);
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
