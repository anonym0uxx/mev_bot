//! `trade_journal` — the operational trade journal for the pump.fun scalping bot.
//!
//! Constitution reference: **§12** (append-only journals), **§43** (tables/journal
//! schema), **§65** (provenance), **§74** (net-SOL expectancy), **§111** (no
//! unrecorded experiment), **§105** (no fabricated observation).
//!
//! Every trade decision — paper or live — is recorded here with full provenance:
//! the mint, the side, the entry/exit price, the strategy that fired, the ingest
//! source that produced the signal, the outcome, the realized PnL in lamports,
//! the fees, the internal latency, and the run mode. This is the raw feed for the
//! [`crate::memory_bank`] which aggregates it into per-mint and per-strategy
//! performance summaries for continuous optimization toward maximum net SOL.
//!
//! ## Design constraints (§22 — deterministic, integer-only)
//!
//! * **No floating point.** Prices are `i128` fixed-point (the engine's `price_fp`
//!   convention), PnL is `i64` lamports, fees are `u64` lamports, latencies are
//!   `u64` microseconds. All arithmetic uses `checked_*` and `saturating_*`.
//! * **No unsafe.** `#![forbid(unsafe_code)]` is on the crate.
//! * **Deterministic.** No wall-clock, no RNG. The slot is the timebase.
//! * **Memory-bounded (§57).** The in-memory ring is capped; overflow rejects
//!   the oldest entry rather than growing without bound.
//! * **Paper/live parity.** `RunMode` is recorded per trade but does NOT change
//!   the data path — identical events produce identical records in both modes.

use crate::ProvenanceSource;
use std::collections::VecDeque;

// ─── Run mode ─────────────────────────────────────────────────────────────

/// Whether a trade was paper (simulated fill) or live (on-chain execution).
/// Recorded per trade for provenance; does NOT change the data path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunMode {
    Paper,
    Live,
}

impl RunMode {
    /// Compact single-char tag for JSONL output.
    pub fn tag(&self) -> &'static str {
        match self {
            RunMode::Paper => "P",
            RunMode::Live => "L",
        }
    }
}

/// S2: The evaluator lane tag carried in a `TradeRecord`.
/// Maps directly to the tape's `TapeLane` for correct per-lane attribution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TradeLane {
    Scalp,
    Early,
}

impl TradeLane {
    /// Returns the string tag used in JSON serialization.
    pub fn tag(&self) -> &'static str {
        match self {
            TradeLane::Scalp => "scalp",
            TradeLane::Early => "early",
        }
    }
}

// ─── Trade side ───────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TradeSide {
    Buy,
    Sell,
}

impl TradeSide {
    pub fn tag(&self) -> &'static str {
        match self {
            TradeSide::Buy => "BUY",
            TradeSide::Sell => "SELL",
        }
    }
}

// ─── Outcome ──────────────────────────────────────────────────────────────

/// The lifecycle outcome of a trade, recorded for the error taxonomy (§78) and
/// the exit-remediation ladder (§79).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TradeOutcome {
    /// Order filled at or better than the expected price.
    Filled,
    /// Order filled but at a worse price than expected (slippage > 0).
    FilledWithSlippage,
    /// Order rejected by the construction-validation gate (§77) pre-submission.
    RejectedPreSubmit,
    /// Order submitted on-chain but failed (see `error_code`).
    OnChainFailure,
    /// Order submitted, not yet confirmed. Resolved later by a follow-up record.
    Pending,
    /// Order timed out (slot-bounded freshness — §55/§104).
    Expired,
}

impl TradeOutcome {
    pub fn tag(&self) -> &'static str {
        match self {
            TradeOutcome::Filled => "FILLED",
            TradeOutcome::FilledWithSlippage => "FILLED_SLIP",
            TradeOutcome::RejectedPreSubmit => "REJECTED",
            TradeOutcome::OnChainFailure => "ONCHAIN_FAIL",
            TradeOutcome::Pending => "PENDING",
            TradeOutcome::Expired => "EXPIRED",
        }
    }

    /// True if the outcome is a terminal state (no further update expected).
    pub fn is_terminal(&self) -> bool {
        match self {
            TradeOutcome::Pending => false,
            _ => true,
        }
    }
}

// ─── Trade record ─────────────────────────────────────────────────────────

/// A single trade decision + outcome, with full provenance. This is the atom
/// of the trade journal — one `TradeRecord` per trade lifecycle.
///
/// All monetary values are in **lamports** (1 SOL = 1_000_000_000 lamports).
/// All prices are in the engine's `i128` fixed-point convention (`price_fp`).
/// All latencies are in microseconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TradeRecord {
    /// Slot at which the trade decision was made (the timebase).
    pub slot: u64,
    /// The mint that was traded, as a base58 string.
    pub mint_b58: String,
    /// Buy or sell.
    pub side: TradeSide,
    /// Entry price in fixed-point (`price_fp` from the MarketTrade event).
    pub entry_price_fp: i128,
    /// Exit price in fixed-point (0 if not yet exited).
    pub exit_price_fp: i128,
    /// Trade size in lamports.
    pub size_lamports: u64,
    /// The strategy ID that produced this trade (hashed strategy name).
    pub strategy_id: u64,
    /// The ingest source that produced the signal.
    pub source: ProvenanceSource,
    /// The outcome of the trade.
    pub outcome: TradeOutcome,
    /// Realized PnL in lamports (positive = profit, negative = loss, 0 = open).
    pub realized_pnl_lamports: i64,
    /// Total fees paid in lamports.
    pub fees_lamports: u64,
    /// Slippage in lamports (actual fill price minus expected, |signed|).
    pub slippage_lamports: u64,
    /// Internal decision latency: signal-received → order-decision, in microseconds.
    pub decision_latency_us: u64,
    /// On-chain confirmation latency: order-submitted → confirmed, in microseconds.
    pub confirm_latency_us: u64,
    /// Paper or live.
    pub run_mode: RunMode,
    /// Solana program error code if `outcome == OnChainFailure` (0 otherwise).
    pub error_code: u32,
    /// Monotonic sequence number (for crash-recovery ordering).
    pub seq: u64,
    /// S2: The evaluator lane this trade belongs to (scalp or early).
    /// Derived from the discovery lane via `eval_lane_of()` at trade-creation
    /// time. Carried in the record so the tape exporter can emit the correct
    /// `TapeLane` without hardcoding. `None` means the lane was not known at
    /// creation time (backward compat with older callers).
    pub lane: Option<TradeLane>,
    /// GAP C: Maximum Favorable Excursion in basis points.
    /// The highest unrealized profit (in bps) observed during the trade's
    /// lifetime, measured from entry price. 0 if never tracked.
    pub mfe_bps: i64,
    /// GAP C: Maximum Adverse Excursion in basis points.
    /// The deepest unrealized loss (in bps, positive = underwater) observed
    /// during the trade's lifetime. 0 if never tracked.
    pub mae_bps: i64,
}

impl TradeRecord {
    /// Serialize this record to a single JSONL line (no trailing newline).
    ///
    /// Uses manual JSON construction (no serde dep — §22/§113 portable discipline).
    /// All values are integers or quoted strings.
    pub fn to_jsonl(&self) -> String {
        // Build manually to avoid any float conversion and keep it deterministic.
        let mut s = String::with_capacity(256);
        s.push('{');
        s.push_str(&format!("\"slot\":{}", self.slot));
        s.push_str(&format!(",\"mint\":\"{}", self.mint_b58));
        s.push('"');
        s.push_str(&format!(",\"side\":\"{}\"", self.side.tag()));
        s.push_str(&format!(",\"entry_price_fp\":{}", self.entry_price_fp));
        s.push_str(&format!(",\"exit_price_fp\":{}", self.exit_price_fp));
        s.push_str(&format!(",\"size_lamports\":{}", self.size_lamports));
        s.push_str(&format!(",\"strategy_id\":{}", self.strategy_id));
        s.push_str(&format!(",\"source\":\"{}\"", self.source_tag()));
        s.push_str(&format!(",\"outcome\":\"{}\"", self.outcome.tag()));
        s.push_str(&format!(",\"pnl_lamports\":{}", self.realized_pnl_lamports));
        s.push_str(&format!(",\"fees_lamports\":{}", self.fees_lamports));
        s.push_str(&format!(",\"slippage_lamports\":{}", self.slippage_lamports));
        s.push_str(&format!(",\"decision_latency_us\":{}", self.decision_latency_us));
        s.push_str(&format!(",\"confirm_latency_us\":{}", self.confirm_latency_us));
        s.push_str(&format!(",\"run_mode\":\"{}\"", self.run_mode.tag()));
        s.push_str(&format!(",\"error_code\":{}", self.error_code));
        s.push_str(&format!(",\"seq\":{}", self.seq));
        // S2: Include the lane tag if present so the tape exporter and
        // downstream consumers can attribute trades to the correct lane.
        if let Some(ref lane) = self.lane {
            s.push_str(&format!(",\"lane\":\"{}\"", lane.tag()));
        }
        // GAP C: MFE/MAE excursion tracking
        s.push_str(&format!(",\"mfe_bps\":{}", self.mfe_bps));
        s.push_str(&format!(",\"mae_bps\":{}", self.mae_bps));
        s.push('}');
        s
    }

    /// The net SOL impact in lamports: `realized_pnl - fees`.
    /// This is the quantity the memory bank optimizes toward.
    pub fn net_lamports(&self) -> i64 {
        // saturating to avoid overflow on pathological sums
        let pnl = self.realized_pnl_lamports;
        let fees = self.fees_lamports as i64;
        pnl.saturating_sub(fees)
    }

    /// True if this trade was a win (net positive after fees).
    pub fn is_win(&self) -> bool {
        self.net_lamports() > 0
    }

    fn source_tag(&self) -> &'static str {
        match self.source {
            ProvenanceSource::PumpPortalTrade => "PumpPortal",
            ProvenanceSource::HeliusAccountSubscribe => "HeliusAcct",
            ProvenanceSource::HeliusTransactionSubscribe => "HeliusTx",
            ProvenanceSource::HeliusReserveDelta => "ReserveDelta",
            ProvenanceSource::LaserStream => "LaserStream",
        }
    }
}

// ─── Journal ──────────────────────────────────────────────────────────────

/// Configuration for the trade journal.
#[derive(Clone, Debug)]
pub struct JournalConfig {
    /// Maximum number of records to retain in the in-memory ring.
    /// Older records are evicted (§57 — memory-bounded).
    pub max_records: usize,
}

impl Default for JournalConfig {
    fn default() -> Self {
        // 10_000 trades is ~1.2 MB in-memory — generous for a scalp bot.
        Self { max_records: 10_000 }
    }
}

/// The trade journal — an append-only, bounded ring of `TradeRecord`s.
///
/// In Phase-A (laptop), the journal is in-memory only. The `flush_jsonl` method
/// writes the full ring to a file for human inspection and post-hoc analysis.
/// In Phase-B (server), the `pump-quant-journal` binary frame codec wraps each
/// record for crash-recovery durability (§12).
pub struct TradeJournal {
    records: VecDeque<TradeRecord>,
    config: JournalConfig,
    /// Monotonic sequence counter (never resets, even after eviction).
    next_seq: u64,
    /// Running tally of net lamports across all trades (for quick health check).
    net_lamports_running: i64,
    /// Running count of wins/losses.
    wins: u64,
    losses: u64,
}

impl TradeJournal {
    /// Create a new journal with the given config.
    pub fn new(config: JournalConfig) -> Self {
        Self {
            records: VecDeque::with_capacity(config.max_records),
            config,
            next_seq: 0,
            net_lamports_running: 0,
            wins: 0,
            losses: 0,
        }
    }

    /// Record a trade. Returns the sequence number assigned.
    /// If the ring is full, the oldest record is evicted (§57).
    pub fn record(&mut self, mut rec: TradeRecord) -> u64 {
        rec.seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);

        // Update running tallies BEFORE eviction so we never lose a data point.
        let net = rec.net_lamports();
        self.net_lamports_running = self.net_lamports_running.saturating_add(net);
        if rec.is_win() {
            self.wins = self.wins.saturating_add(1);
        } else if rec.outcome.is_terminal() {
            self.losses = self.losses.saturating_add(1);
        }

        // Evict oldest if at capacity.
        if self.records.len() >= self.config.max_records {
            self.records.pop_front();
        }
        self.records.push_back(rec);
        self.next_seq
    }

    /// Flush the entire in-memory ring to a JSONL file (one record per line).
    /// Overwrites the file if it exists. Returns the number of lines written.
    pub fn flush_jsonl(&self, path: &str) -> Result<usize, String> {
        use std::io::Write;
        let mut file = std::fs::File::create(path)
            .map_err(|e| format!("flush_jsonl: create {path}: {e}"))?;
        let mut count = 0usize;
        for rec in &self.records {
            let line = format!("{}\n", rec.to_jsonl());
            file.write_all(line.as_bytes())
                .map_err(|e| format!("flush_jsonl: write: {e}"))?;
            count += 1;
        }
        file.flush()
            .map_err(|e| format!("flush_jsonl: flush: {e}"))?;
        Ok(count)
    }

    /// Number of records currently in the ring.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// True if no records have been recorded.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Running net lamports across all trades ever recorded (not just the ring).
    pub fn net_lamports_total(&self) -> i64 {
        self.net_lamports_running
    }

    /// Total wins recorded.
    pub fn total_wins(&self) -> u64 {
        self.wins
    }

    /// Total losses recorded (terminal outcomes that were not wins).
    pub fn total_losses(&self) -> u64 {
        self.losses
    }

    /// Win rate in basis points (0..=10_000). Returns 0 if no trades.
    /// 5_000 = 50%, 10_000 = 100%.
    pub fn win_rate_bps(&self) -> u32 {
        let total = self.wins.saturating_add(self.losses);
        if total == 0 {
            return 0;
        }
        // wins * 10_000 / total, saturating
        let num = (self.wins as u128) * 10_000;
        let den = total as u128;
        ((num / den) as u32).min(10_000)
    }

    /// Iterate over all records in the ring (oldest first).
    pub fn iter(&self) -> impl Iterator<Item = &TradeRecord> {
        self.records.iter()
    }

    /// Get the last N records (most recent), for the memory bank's rolling window.
    pub fn last_n(&self, n: usize) -> Vec<&TradeRecord> {
        let n = n.min(self.records.len());
        self.records.iter().rev().take(n).collect::<Vec<_>>().into_iter().rev().collect()
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProvenanceSource;

    fn make_record(slot: u64, pnl: i64, fees: u64, mint: &str) -> TradeRecord {
        TradeRecord {
            slot,
            mint_b58: mint.to_string(),
            side: TradeSide::Buy,
            entry_price_fp: 1_000_000_000,
            exit_price_fp: 1_200_000_000,
            size_lamports: 50_000_000,
            strategy_id: 0xDEAD_BEEF,
            source: ProvenanceSource::LaserStream,
            outcome: TradeOutcome::Filled,
            realized_pnl_lamports: pnl,
            fees_lamports: fees,
            slippage_lamports: 0,
            decision_latency_us: 150,
            confirm_latency_us: 400,
            run_mode: RunMode::Paper,
            error_code: 0,
            seq: 0,
            lane: None,
            mfe_bps: 0,
            mae_bps: 0,
        }
    }

    #[test]
    fn test_record_and_basic_stats() {
        let mut j = TradeJournal::new(JournalConfig::default());
        assert!(j.is_empty());

        // Record 3 trades: win, win, loss.
        j.record(make_record(100, 10_000, 500, "MintA"));
        j.record(make_record(200, 5_000, 500, "MintB"));
        j.record(make_record(300, -3_000, 500, "MintC"));

        assert_eq!(j.len(), 3);
        assert_eq!(j.total_wins(), 2);
        assert_eq!(j.total_losses(), 1);
        // net = (10000-500) + (5000-500) + (-3000-500) = 9500 + 4500 - 3500 = 10500
        assert_eq!(j.net_lamports_total(), 10_500);
        // win rate = 2/3 = 6666 bps
        assert_eq!(j.win_rate_bps(), 6_666);
    }

    #[test]
    fn test_jsonl_serialization() {
        let rec = make_record(12345, 777, 100, "TestMint123");
        let jsonl = rec.to_jsonl();
        assert!(jsonl.contains("\"slot\":12345"));
        assert!(jsonl.contains("\"mint\":\"TestMint123\""));
        assert!(jsonl.contains("\"side\":\"BUY\""));
        assert!(jsonl.contains("\"source\":\"LaserStream\""));
        assert!(jsonl.contains("\"outcome\":\"FILLED\""));
        assert!(jsonl.contains("\"pnl_lamports\":777"));
        assert!(jsonl.contains("\"fees_lamports\":100"));
        assert!(jsonl.contains("\"run_mode\":\"P\""));
        // No floats anywhere.
        assert!(!jsonl.contains(".0") || jsonl.contains("price_fp"));
    }

    #[test]
    fn test_net_lamports() {
        let rec = make_record(1, 10_000, 2_000, "M");
        // net = 10000 - 2000 = 8000
        assert_eq!(rec.net_lamports(), 8_000);
        assert!(rec.is_win());

        let rec2 = TradeRecord {
            realized_pnl_lamports: -5_000,
            fees_lamports: 1_000,
            ..make_record(2, -5_000, 1_000, "M")
        };
        // net = -5000 - 1000 = -6000
        assert_eq!(rec2.net_lamports(), -6_000);
        assert!(!rec2.is_win());
    }

    #[test]
    fn test_ring_eviction() {
        let config = JournalConfig { max_records: 3 };
        let mut j = TradeJournal::new(config);

        j.record(make_record(1, 1, 0, "A"));
        j.record(make_record(2, 1, 0, "B"));
        j.record(make_record(3, 1, 0, "C"));
        assert_eq!(j.len(), 3);

        // This should evict "A" (oldest).
        j.record(make_record(4, 1, 0, "D"));
        assert_eq!(j.len(), 3);
        let mints: Vec<&str> = j.iter().map(|r| r.mint_b58.as_str()).collect();
        assert_eq!(mints, vec!["B", "C", "D"]);
    }

    #[test]
    fn test_eviction_preserves_running_totals() {
        let config = JournalConfig { max_records: 2 };
        let mut j = TradeJournal::new(config);

        // Record 4 trades, ring holds only 2.
        j.record(make_record(1, 10_000, 0, "A"));
        j.record(make_record(2, 20_000, 0, "B"));
        j.record(make_record(3, 30_000, 0, "C"));
        j.record(make_record(4, 40_000, 0, "D"));

        // Ring has only C, D — but running total should be 100_000.
        assert_eq!(j.len(), 2);
        assert_eq!(j.net_lamports_total(), 100_000);
        assert_eq!(j.total_wins(), 4);
    }

    #[test]
    fn test_outcome_terminal() {
        assert!(TradeOutcome::Filled.is_terminal());
        assert!(TradeOutcome::OnChainFailure.is_terminal());
        assert!(!TradeOutcome::Pending.is_terminal());
    }

    #[test]
    fn test_last_n() {
        let mut j = TradeJournal::new(JournalConfig::default());
        for i in 0..10u64 {
            j.record(make_record(i, 100, 0, &format!("M{i}")));
        }
        let last3 = j.last_n(3);
        assert_eq!(last3.len(), 3);
        // Should be the 3 most recent: M7, M8, M9.
        assert_eq!(last3[0].mint_b58, "M7");
        assert_eq!(last3[2].mint_b58, "M9");
    }

    #[test]
    fn test_flush_jsonl_roundtrip() {
        let mut j = TradeJournal::new(JournalConfig::default());
        j.record(make_record(1, 100, 10, "FlushA"));
        j.record(make_record(2, 200, 20, "FlushB"));

        let path = "test_trade_journal_flush.jsonl";
        let count = j.flush_jsonl(path).expect("flush failed");
        assert_eq!(count, 2);

        // Read back and verify.
        let content = std::fs::read_to_string(path).expect("read failed");
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("FlushA"));
        assert!(lines[1].contains("FlushB"));

        // Cleanup.
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_paper_live_parity() {
        // Identical trade data in Paper and Live should produce identical
        // records EXCEPT for run_mode. This is the core parity invariant.
        let mut rec_paper = make_record(999, 5_000, 500, "Parity");
        rec_paper.run_mode = RunMode::Paper;

        let mut rec_live = make_record(999, 5_000, 500, "Parity");
        rec_live.run_mode = RunMode::Live;

        // Everything identical except run_mode.
        assert_eq!(rec_paper.slot, rec_live.slot);
        assert_eq!(rec_paper.mint_b58, rec_live.mint_b58);
        assert_eq!(rec_paper.entry_price_fp, rec_live.entry_price_fp);
        assert_eq!(rec_paper.realized_pnl_lamports, rec_live.realized_pnl_lamports);
        assert_eq!(rec_paper.net_lamports(), rec_live.net_lamports());
        assert_ne!(rec_paper.run_mode, rec_live.run_mode);
    }

    #[test]
    fn test_win_rate_no_trades() {
        let j = TradeJournal::new(JournalConfig::default());
        assert_eq!(j.win_rate_bps(), 0);
        assert_eq!(j.total_wins(), 0);
        assert_eq!(j.total_losses(), 0);
    }
}
