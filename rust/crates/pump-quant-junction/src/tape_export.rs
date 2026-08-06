//! Tape export bridge — serialize the engine's DecisionJournal + TradeJournal
//! into the evaluator's JSONL tape format (Phase 2 of the autonomous architecture).
//!
//! The evaluator (`pq-evaluator`) reads a *tape*: newline-delimited JSON objects,
//! each tagged by a `"kind"` field. The schema is defined in
//! `pump-quant-evaluator/src/tape.rs` and supports six record kinds:
//!
//! - `"trade"`: reconciled trade → `ReconTrade` (lane, gross, fees, tips, failed)
//! - `"pvalue"`: hypothesis p-value → `Hypothesis` (id, p_ppm)
//! - `"perf"`: CSCV performance row → `Vec<i64>` (row)
//! - `"baseline_event"`: decision event → `TapeEvent` (index, eligible, launch, score, net_hold, net_tpsl)
//! - `"ablation"`: ablation outcome → `AblationRecord` (family, variant, net, tail)
//! - `"candidate"`: candidate id under promotion test (id)
//!
//! This module converts the engine's native `TradeRecord` and `Decision` entries
//! into that JSONL format, so the daemon's tape output can feed the evaluator
//! without any manual format conversion. All values are integers or quoted
//! strings (§22: no floats). The export is deterministic: identical inputs
//! always produce identical output.
//!
//! ## Design notes
//!
//! The `TradeRecord` carries detailed per-trade data (slot, mint, prices, size,
//! fees, slippage, PnL, outcome). The evaluator's `ReconTrade` is coarser: it
//! wants lane, gross, fees, tips, failed costs. The mapping:
//!
//! - `lane`: derived from the trade's source/strategy. Scalp lane for PumpPortal
//!   scalp trades, Early lane for early-confirmation trades. Trades with
//!   unknown lane default to `"scalp"` (the evaluator only supports scalp/early).
//! - `gross`: the realized PnL in lamports (proceeds minus cost basis). For an
//!   open position this is 0; for a closed position it's `realized_pnl_lamports`.
//! - `fees`: `fees_lamports` from the TradeRecord.
//! - `tips`: priority tips are not separately tracked in TradeRecord yet; we
//!   emit 0 until the outbound path records them.
//! - `failed`: `error_code != 0` indicates an on-chain failure; we emit the
//!   trade's `size_lamports` as the failed cost (the capital that was at risk
//!   on the failed attempt). For successful trades, 0.
//!
//! The `Decision::Filled` variant maps directly to a `trade` record. The
//! `Decision::Admitted` and `Decision::Rejected` variants can be used to
//! reconstruct `baseline_event` records for the baseline-family evaluator,
//! but this requires the hold-to-death and TP/SL counterfactuals which the
//! engine doesn't compute at decision time. Those are produced by the
//! evaluator's own replay, not by the tape exporter. The tape exporter focuses
//! on the `trade` records which carry the realized evidence.

use crate::trade_journal::{TradeRecord, TradeOutcome, TradeSide, RunMode};

/// The lane a trade belongs to, matching the evaluator's `Lane` enum.
/// The evaluator only supports "scalp" and "early".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapeLane {
    Scalp,
    Early,
}

impl TapeLane {
    fn tag(&self) -> &'static str {
        match self {
            TapeLane::Scalp => "scalp",
            TapeLane::Early => "early",
        }
    }
}

/// The source tag for a trade_full record, matching the `ProvenanceSource` enum.
/// Serialized as a string field in the enriched tape.
fn source_tag_short(source: &crate::ProvenanceSource) -> &'static str {
    match source {
        crate::ProvenanceSource::PumpPortalTrade => "PumpPortal",
        crate::ProvenanceSource::HeliusAccountSubscribe => "HeliusAcct",
        crate::ProvenanceSource::HeliusTransactionSubscribe => "HeliusTx",
        crate::ProvenanceSource::HeliusReserveDelta => "ReserveDelta",
        crate::ProvenanceSource::LaserStream => "LaserStream",
    }
}

/// A single tape record in the evaluator's JSONL format. Each variant
/// produces exactly one JSON line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TapeRecord {
    /// A reconciled trade (kind: "trade") — coarse 5-field format.
    /// Retained for backward compatibility with the existing evaluator parser.
    Trade {
        lane: TapeLane,
        gross: i128,
        fees: u128,
        tips: u128,
        failed: u128,
    },
    /// A full-fidelity trade record (kind: "trade_full") — all 16 fields
    /// from `TradeRecord`. This is the enriched format that preserves mint
    /// address, entry/exit prices, slot, slippage, strategy_id, trade size,
    /// outcome type, and latencies for attribution analysis and A/B testing.
    /// Constitution §43 (tables/journal schema), §62 (artifact inputs).
    TradeFull {
        slot: u64,
        mint_b58: String,
        side_tag: &'static str,
        entry_price_fp: i128,
        exit_price_fp: i128,
        size_lamports: u64,
        strategy_id: u64,
        source_tag: &'static str,
        outcome_tag: &'static str,
        realized_pnl_lamports: i64,
        fees_lamports: u64,
        slippage_lamports: u64,
        decision_latency_us: u64,
        confirm_latency_us: u64,
        run_mode_tag: &'static str,
        error_code: u32,
        seq: u64,
    },
    /// A hypothesis p-value (kind: "pvalue").
    PValue { id: u64, p_ppm: u32 },
    /// A CSCV performance row (kind: "perf").
    Perf { row: Vec<i64> },
    /// A baseline decision event (kind: "baseline_event").
    BaselineEvent {
        index: u64,
        eligible: bool,
        launch: bool,
        score: i64,
        net_hold: i128,
        net_tpsl: i128,
    },
    /// An ablation outcome (kind: "ablation").
    Ablation {
        family: u32,
        variant: &'static str,
        net: i128,
        tail: i64,
    },
    /// A candidate id (kind: "candidate").
    Candidate { id: u64 },
    /// S6: Lane retirement state snapshot (kind: "lane_state").
    /// Carries the engine's `retired[4]` array so the refiner can observe
    /// which watchlist lanes have been capital-blocked by the sequential
    /// retirement system. This is metadata, not a trade — it tells the
    /// refiner "lane N stopped producing trades because it was retired,
    /// not because the gate tightened."
    ///
    /// The 4-element array maps to the engine's watchlist lane indices:
    ///   [0] = CreationSniper/EarlyConfirmation (Early)
    ///   [1] = GraduationTransition/ActiveMarketScalp (Scalp)
    ///   [2..3] = reserved for future lanes
    ///
    /// The refiner uses this to avoid misattributing a lane's trade drought
    /// to a gate_margin_bps change when it was actually a retirement event.
    LaneState {
        /// The slot at which this snapshot was taken.
        slot: u64,
        /// The retired state of all 4 watchlist lanes.
        retired: [bool; 4],
    },
}

impl TapeRecord {
    /// Serialize this record to a single JSONL line (no trailing newline).
    /// All values are integers or quoted strings. No floats (§22).
    pub fn to_jsonl(&self) -> String {
        match self {
            TapeRecord::Trade { lane, gross, fees, tips, failed } => {
                format!(
                    r#"{{"kind":"trade","lane":"{lane}","gross":{gross},"fees":{fees},"tips":{tips},"failed":{failed}}}"#,
                    lane = lane.tag(),
                    gross = gross,
                    fees = fees,
                    tips = tips,
                    failed = failed,
                )
            }
            TapeRecord::TradeFull {
                slot, mint_b58, side_tag, entry_price_fp, exit_price_fp,
                size_lamports, strategy_id, source_tag, outcome_tag,
                realized_pnl_lamports, fees_lamports, slippage_lamports,
                decision_latency_us, confirm_latency_us, run_mode_tag,
                error_code, seq,
            } => {
                format!(
                    r#"{{"kind":"trade_full","slot":{slot},"mint":"{mint}","side":"{side}","entry_price_fp":{ep},"exit_price_fp":{xp},"size_lamports":{sz},"strategy_id":{sid},"source":"{src}","outcome":"{out}","realized_pnl":{pnl},"fees":{fees},"slippage":{slip},"decision_latency_us":{dl},"confirm_latency_us":{cl},"run_mode":"{rm}","error_code":{ec},"seq":{seq}}}"#,
                    slot = slot,
                    mint = mint_b58,
                    side = side_tag,
                    ep = entry_price_fp,
                    xp = exit_price_fp,
                    sz = size_lamports,
                    sid = strategy_id,
                    src = source_tag,
                    out = outcome_tag,
                    pnl = realized_pnl_lamports,
                    fees = fees_lamports,
                    slip = slippage_lamports,
                    dl = decision_latency_us,
                    cl = confirm_latency_us,
                    rm = run_mode_tag,
                    ec = error_code,
                    seq = seq,
                )
            }
            TapeRecord::PValue { id, p_ppm } => {
                format!(
                    r#"{{"kind":"pvalue","id":{id},"p_ppm":{p_ppm}}}"#,
                    id = id,
                    p_ppm = p_ppm,
                )
            }
            TapeRecord::Perf { row } => {
                let ints: Vec<String> = row.iter().map(|v| v.to_string()).collect();
                format!(
                    r#"{{"kind":"perf","row":[{row}]}}"#,
                    row = ints.join(","),
                )
            }
            TapeRecord::BaselineEvent {
                index,
                eligible,
                launch,
                score,
                net_hold,
                net_tpsl,
            } => {
                format!(
                    r#"{{"kind":"baseline_event","index":{index},"eligible":{eligible},"launch":{launch},"score":{score},"net_hold":{net_hold},"net_tpsl":{net_tpsl}}}"#,
                    index = index,
                    eligible = eligible,
                    launch = launch,
                    score = score,
                    net_hold = net_hold,
                    net_tpsl = net_tpsl,
                )
            }
            TapeRecord::Ablation {
                family,
                variant,
                net,
                tail,
            } => {
                format!(
                    r#"{{"kind":"ablation","family":{family},"variant":"{variant}","net":{net},"tail":{tail}}}"#,
                    family = family,
                    variant = variant,
                    net = net,
                    tail = tail,
                )
            }
            TapeRecord::Candidate { id } => {
                format!(r#"{{"kind":"candidate","id":{id}}}"#, id = id)
            }
            // S6: Lane retirement state snapshot.
            TapeRecord::LaneState { slot, retired } => {
                let flags: Vec<&str> = retired
                    .iter()
                    .map(|b| if *b { "true" } else { "false" })
                    .collect();
                format!(
                    r#"{{"kind":"lane_state","slot":{slot},"retired":[{flags}]}}"#,
                    slot = slot,
                    flags = flags.join(","),
                )
            }
        }
    }
}

/// Convert a `TradeRecord` into a coarse tape `Trade` record (5-field format).
///
/// **Deprecated in favor of `trade_record_to_tape_full`** — this coarse format
/// strips 11 of 16 fields and makes attribution analysis impossible. Retained
/// for backward compatibility with the existing evaluator tape parser which
/// only understands `kind:"trade"`. The enriched `kind:"trade_full"` format
/// should be used for all new tape output.
pub fn trade_record_to_tape(rec: &TradeRecord) -> TapeRecord {
    // S2: Derive lane from the TradeRecord's lane field instead of hardcoding
    // TapeLane::Scalp. If the lane is None (backward compat with older
    // callers), default to Scalp — the engine's primary mode.
    let lane = match rec.lane {
        Some(crate::trade_journal::TradeLane::Scalp) => TapeLane::Scalp,
        Some(crate::trade_journal::TradeLane::Early) => TapeLane::Early,
        None => TapeLane::Scalp, // backward compat default
    };

    // Gross = realized PnL. For open positions, realized_pnl is 0.
    // For closed positions, it's the signed net PnL (proceeds - cost basis).
    // The evaluator's ReconTrade.gross is "proceeds - cost_basis" which is
    // exactly realized_pnl_lamports (before fees, which are separate fields).
    let gross = rec.realized_pnl_lamports as i128;

    // Fees: the total fees paid on this trade.
    let fees = rec.fees_lamports as u128;

    // Tips: not separately tracked yet (§future: outbound priority tip accounting).
    let tips = 0u128;

    // Failed costs: if the trade failed on-chain (OnChainFailure), the
    // capital at risk was the trade size. Otherwise 0.
    let failed = match rec.outcome {
        TradeOutcome::OnChainFailure => rec.size_lamports as u128,
        _ => 0u128,
    };

    TapeRecord::Trade {
        lane,
        gross,
        fees,
        tips,
        failed,
    }
}

/// Convert a `TradeRecord` into an enriched `TradeFull` tape record (16-field
/// format, kind: "trade_full"). This preserves ALL fields from the engine's
/// `TradeRecord` for attribution analysis, A/B testing, and strategy-type
/// discovery.
///
/// Constitution §43 (tables/journal schema), §62 (artifact inputs):
/// the tape must carry enough fidelity to answer "which strategy type,
/// archetype, and parameter config produced which outcome?" The coarse
/// 5-field `Trade` format cannot answer this; `TradeFull` can.
///
/// All values are integers or quoted strings (§22: no floats).
pub fn trade_record_to_tape_full(rec: &TradeRecord) -> TapeRecord {
    TapeRecord::TradeFull {
        slot: rec.slot,
        mint_b58: rec.mint_b58.clone(),
        side_tag: rec.side.tag(),
        entry_price_fp: rec.entry_price_fp,
        exit_price_fp: rec.exit_price_fp,
        size_lamports: rec.size_lamports,
        strategy_id: rec.strategy_id,
        source_tag: source_tag_short(&rec.source),
        outcome_tag: rec.outcome.tag(),
        realized_pnl_lamports: rec.realized_pnl_lamports,
        fees_lamports: rec.fees_lamports,
        slippage_lamports: rec.slippage_lamports,
        decision_latency_us: rec.decision_latency_us,
        confirm_latency_us: rec.confirm_latency_us,
        run_mode_tag: rec.run_mode.tag(),
        error_code: rec.error_code,
        seq: rec.seq,
    }
}

/// A tape exporter that accumulates records and flushes them to a JSONL file.
///
/// The daemon calls `export_trade()` for each closed position and
/// `flush()` periodically (or on shutdown) to write the tape to disk.
/// The file is append-only: each flush appates new records since the last
/// flush, so the tape grows monotonically and survives partial writes.
pub struct TapeExporter {
    /// Records accumulated since the last flush.
    pending: Vec<TapeRecord>,
    /// Total records ever exported (across all flushes).
    total_exported: u64,
    /// The output file path.
    path: String,
}

impl TapeExporter {
    /// Create a new exporter writing to `path`.
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            pending: Vec::new(),
            total_exported: 0,
            path: path.into(),
        }
    }

    /// Add a closed trade to the pending tape.
    ///
    /// Emits the **enriched 16-field `TradeFull` record** (kind: "trade_full")
    /// instead of the deprecated coarse 5-field `Trade` record. The enriched
    /// format preserves mint address, entry/exit prices, slot, slippage,
    /// strategy_id, trade size, outcome type, and latencies — all required
    /// for attribution analysis and A/B testing (§43, §62).
    pub fn export_trade(&mut self, rec: &TradeRecord) {
        // Only export terminal trades (closed positions).
        // Open positions have realized_pnl = 0 and no exit; they don't
        // contribute to the evaluator's reconciliation.
        if rec.outcome.is_terminal() {
            self.pending.push(trade_record_to_tape_full(rec));
        }
    }

    /// Add a raw tape record (for pvalues, perf rows, ablation records
    /// produced by the refiner, not the daemon itself).
    pub fn push(&mut self, record: TapeRecord) {
        self.pending.push(record);
    }

    /// Number of pending records not yet flushed.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Total records ever exported across all flushes.
    pub fn total_exported(&self) -> u64 {
        self.total_exported
    }

    /// Flush pending records to the JSONL file. Appends to the file if it
    /// already exists (the tape grows across daemon restarts). Returns the
    /// number of records written.
    pub fn flush(&mut self) -> Result<usize, String> {
        if self.pending.is_empty() {
            return Ok(0);
        }
        let mut content = String::new();
        for rec in &self.pending {
            content.push_str(&rec.to_jsonl());
            content.push('\n');
        }

        // Append mode: the tape accumulates across daemon restarts.
        // If the file doesn't exist, create it.
        let path = std::path::Path::new(&self.path);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("tape_export: create_dir_all: {e}"))?;
            }
        }

        // Use OpenOptions for append-write.
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("tape_export: open: {e}"))?;

        file.write_all(content.as_bytes())
            .map_err(|e| format!("tape_export: write: {e}"))?;

        let written = self.pending.len();
        self.total_exported += written as u64;
        self.pending.clear();
        Ok(written)
    }
}

/// Serialize a full set of trade records into a JSONL string (for testing
/// and one-shot export). Does not touch the filesystem.
///
/// Emits the enriched 16-field `TradeFull` format (kind: "trade_full").
pub fn trades_to_jsonl(records: &[TradeRecord]) -> String {
    let mut out = String::new();
    for rec in records {
        // Only include closed trades.
        if rec.outcome.is_terminal() {
            out.push_str(&trade_record_to_tape_full(rec).to_jsonl());
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade_journal::{JournalConfig, TradeJournal};
    use crate::ProvenanceSource;

    fn make_trade_record(
        outcome: TradeOutcome,
        pnl: i64,
        fees: u64,
        size: u64,
        slot: u64,
    ) -> TradeRecord {
        TradeRecord {
            slot,
            mint_b58: "TestMint123456789012345".to_string(),
            side: TradeSide::Buy,
            entry_price_fp: 1000000000,
            exit_price_fp: 1100000000,
            size_lamports: size,
            strategy_id: 1,
            source: ProvenanceSource::PumpPortalTrade,
            outcome,
            realized_pnl_lamports: pnl,
            fees_lamports: fees,
            slippage_lamports: 0,
            decision_latency_us: 100,
            confirm_latency_us: 200,
            run_mode: RunMode::Paper,
            error_code: 0,
            seq: 0,
            lane: None,
        }
    }

    // Helper: a filled trade with positive PnL (a win).
    const WIN: TradeOutcome = TradeOutcome::Filled;
    // Helper: a filled trade with negative PnL (a loss).
    const LOSS: TradeOutcome = TradeOutcome::FilledWithSlippage;
    // Helper: an open position.
    const OPEN: TradeOutcome = TradeOutcome::Pending;

    #[test]
    fn trade_record_maps_to_tape_trade() {
        let rec = make_trade_record(WIN, 5000, 200, 100000, 100);
        let tape = trade_record_to_tape(&rec);
        match tape {
            TapeRecord::Trade { lane, gross, fees, tips: _, failed } => {
                assert_eq!(lane, TapeLane::Scalp);
                assert_eq!(gross, 5000);
                assert_eq!(fees, 200);
                assert_eq!(failed, 0);
            }
            _ => panic!("expected Trade record"),
        }
    }

    #[test]
    fn failed_trade_maps_failed_cost() {
        let mut rec = make_trade_record(TradeOutcome::OnChainFailure, 0, 50, 100000, 100);
        rec.error_code = 1;
        let tape = trade_record_to_tape(&rec);
        match tape {
            TapeRecord::Trade { lane, gross, fees, tips: _, failed } => {
                assert_eq!(gross, 0);
                assert_eq!(fees, 50);
                assert_eq!(failed, 100000); // size at risk
                assert_eq!(lane, TapeLane::Scalp);
            }
            _ => panic!("expected Trade record"),
        }
    }

    /// S2: A TradeRecord with lane=Some(TradeLane::Early) must map to
    /// TapeLane::Early in the coarse tape format — not hardcoded Scalp.
    #[test]
    fn s2_early_lane_attribution() {
        let mut rec = make_trade_record(WIN, 10_000, 300, 200_000, 200);
        rec.lane = Some(crate::trade_journal::TradeLane::Early);
        let tape = trade_record_to_tape(&rec);
        match tape {
            TapeRecord::Trade { lane, .. } => {
                assert_eq!(lane, TapeLane::Early,
                    "S2: lane must be Early when TradeRecord.lane=Some(Early)");
            }
            _ => panic!("expected Trade record"),
        }
    }

    /// S2: A TradeRecord with lane=Some(TradeLane::Scalp) must map to
    /// TapeLane::Scalp (explicit, not just the default).
    #[test]
    fn s2_scalp_lane_attribution() {
        let mut rec = make_trade_record(WIN, 10_000, 300, 200_000, 200);
        rec.lane = Some(crate::trade_journal::TradeLane::Scalp);
        let tape = trade_record_to_tape(&rec);
        match tape {
            TapeRecord::Trade { lane, .. } => {
                assert_eq!(lane, TapeLane::Scalp,
                    "S2: lane must be Scalp when TradeRecord.lane=Some(Scalp)");
            }
            _ => panic!("expected Trade record"),
        }
    }

    /// S2: A TradeRecord with lane=None must default to Scalp (backward compat).
    #[test]
    fn s2_none_lane_defaults_to_scalp() {
        let rec = make_trade_record(WIN, 10_000, 300, 200_000, 200);
        assert!(rec.lane.is_none());
        let tape = trade_record_to_tape(&rec);
        match tape {
            TapeRecord::Trade { lane, .. } => {
                assert_eq!(lane, TapeLane::Scalp,
                    "S2: lane must default to Scalp when TradeRecord.lane=None");
            }
            _ => panic!("expected Trade record"),
        }
    }

    #[test]
    fn loss_trade_has_negative_gross() {
        let rec = make_trade_record(LOSS, -3000, 200, 100000, 100);
        let tape = trade_record_to_tape(&rec);
        match tape {
            TapeRecord::Trade { gross, .. } => {
                assert_eq!(gross, -3000);
            }
            _ => panic!("expected Trade record"),
        }
    }

    #[test]
    fn jsonl_format_matches_evaluator_schema() {
        let rec = make_trade_record(WIN, 5000, 200, 100000, 100);
        let jsonl = trade_record_to_tape(&rec).to_jsonl();
        // The evaluator parser expects exactly this format.
        // Verify key fields are present and integer-typed.
        assert!(jsonl.contains(r#""kind":"trade""#));
        assert!(jsonl.contains(r#""lane":"scalp""#));
        assert!(jsonl.contains(r#""gross":5000"#));
        assert!(jsonl.contains(r#""fees":200"#));
        assert!(jsonl.contains(r#""tips":0"#));
        assert!(jsonl.contains(r#""failed":0"#));
        // No floats, no null values.
        assert!(!jsonl.contains('.'));
        assert!(!jsonl.contains("null"));
    }

    #[test]
    fn pvalue_record_format() {
        let rec = TapeRecord::PValue { id: 42, p_ppm: 5000 };
        let jsonl = rec.to_jsonl();
        assert!(jsonl.contains(r#""kind":"pvalue""#));
        assert!(jsonl.contains(r#""id":42"#));
        assert!(jsonl.contains(r#""p_ppm":5000"#));
    }

    #[test]
    fn perf_record_format() {
        let rec = TapeRecord::Perf { row: vec![5, -3, 8, -1] };
        let jsonl = rec.to_jsonl();
        assert!(jsonl.contains(r#""kind":"perf""#));
        assert!(jsonl.contains(r#""row":[5,-3,8,-1]"#));
    }

    #[test]
    fn baseline_event_format() {
        let rec = TapeRecord::BaselineEvent {
            index: 0,
            eligible: true,
            launch: false,
            score: 10,
            net_hold: 5000,
            net_tpsl: 3000,
        };
        let jsonl = rec.to_jsonl();
        assert!(jsonl.contains(r#""kind":"baseline_event""#));
        assert!(jsonl.contains(r#""index":0"#));
        assert!(jsonl.contains(r#""eligible":true"#));
        assert!(jsonl.contains(r#""launch":false"#));
        assert!(jsonl.contains(r#""score":10"#));
        assert!(jsonl.contains(r#""net_hold":5000"#));
        assert!(jsonl.contains(r#""net_tpsl":3000"#));
    }

    #[test]
    fn ablation_record_format() {
        let rec = TapeRecord::Ablation {
            family: 2,
            variant: "removed",
            net: 1200,
            tail: 30,
        };
        let jsonl = rec.to_jsonl();
        assert!(jsonl.contains(r#""kind":"ablation""#));
        assert!(jsonl.contains(r#""family":2"#));
        assert!(jsonl.contains(r#""variant":"removed""#));
        assert!(jsonl.contains(r#""net":1200"#));
        assert!(jsonl.contains(r#""tail":30"#));
    }

    #[test]
    fn candidate_record_format() {
        let rec = TapeRecord::Candidate { id: 1 };
        let jsonl = rec.to_jsonl();
        assert!(jsonl.contains(r#""kind":"candidate""#));
        assert!(jsonl.contains(r#""id":1"#));
    }

    #[test]
    fn exporter_flush_writes_to_file() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("pq_test_tape_export.jsonl");
        let _ = std::fs::remove_file(&path);

        let mut exporter = TapeExporter::new(path.to_str().unwrap());
        let rec1 = make_trade_record(WIN, 5000, 200, 100000, 100);
        let rec2 = make_trade_record(LOSS, -3000, 150, 100000, 200);
        exporter.export_trade(&rec1);
        exporter.export_trade(&rec2);

        assert_eq!(exporter.pending_count(), 2);
        let written = exporter.flush().unwrap();
        assert_eq!(written, 2);
        assert_eq!(exporter.total_exported(), 2);
        assert_eq!(exporter.pending_count(), 0);

        // Read back and verify.
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains(r#""kind":"trade_full""#));
        assert!(lines[1].contains(r#""kind":"trade_full""#));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn exporter_appends_across_flushes() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("pq_test_tape_append.jsonl");
        let _ = std::fs::remove_file(&path);

        let mut exporter = TapeExporter::new(path.to_str().unwrap());
        exporter.export_trade(&make_trade_record(WIN, 1000, 100, 50000, 50));
        exporter.flush().unwrap();

        let mut exporter2 = TapeExporter::new(path.to_str().unwrap());
        exporter2.export_trade(&make_trade_record(LOSS, -500, 50, 50000, 100));
        exporter2.flush().unwrap();

        // Both records should be in the file (append mode).
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn exporter_skips_open_positions() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("pq_test_tape_open.jsonl");
        let _ = std::fs::remove_file(&path);

        let mut exporter = TapeExporter::new(path.to_str().unwrap());
        // Open position — should be skipped.
        exporter.export_trade(&make_trade_record(OPEN, 0, 0, 100000, 100));
        assert_eq!(exporter.pending_count(), 0);

        // Closed position — should be exported.
        exporter.export_trade(&make_trade_record(WIN, 1000, 100, 100000, 200));
        assert_eq!(exporter.pending_count(), 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn trades_to_jsonl_one_shot() {
        let recs = vec![
            make_trade_record(WIN, 1000, 100, 50000, 50),
            make_trade_record(LOSS, -500, 50, 50000, 100),
            make_trade_record(OPEN, 0, 0, 50000, 150), // skipped
        ];
        let jsonl = trades_to_jsonl(&recs);
        let lines: Vec<&str> = jsonl.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2); // only the 2 closed trades
    }

    #[test]
    fn evaluator_parser_can_parse_exported_tape() {
        // Verify round-trip: our exported JSONL can be parsed by the
        // evaluator's tape parser. We simulate the parse here by checking
        // the format matches exactly what parse_jsonl expects.
        let rec = make_trade_record(WIN, 5000, 200, 100000, 100);
        let jsonl = trade_record_to_tape(&rec).to_jsonl();

        // The evaluator parser expects:
        // {"kind":"trade","lane":"scalp","gross":5000,"fees":200,"tips":0,"failed":0}
        // Verify our output matches this exactly.
        let expected = r#"{"kind":"trade","lane":"scalp","gross":5000,"fees":200,"tips":0,"failed":0}"#;
        assert_eq!(jsonl, expected);
    }

    #[test]
    fn negative_pnl_preserved_in_tape() {
        let rec = make_trade_record(LOSS, -99999, 200, 100000, 100);
        let jsonl = trade_record_to_tape(&rec).to_jsonl();
        assert!(jsonl.contains(r#""gross":-99999"#));
    }

    #[test]
    fn tape_exporter_from_journal() {
        // Integration: create a TradeJournal, add records, export via the bridge.
        let mut journal = TradeJournal::new(JournalConfig::default());
        let mut rec = make_trade_record(WIN, 5000, 200, 100000, 100);
        rec.seq = 1;
        journal.record(rec);

        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("pq_test_tape_from_journal.jsonl");
        let _ = std::fs::remove_file(&path);

        let mut exporter = TapeExporter::new(path.to_str().unwrap());
        for rec in journal.iter() {
            exporter.export_trade(rec);
        }
        assert_eq!(exporter.pending_count(), 1);
        exporter.flush().unwrap();

        let _ = std::fs::remove_file(&path);
    }

    // ─── S6: Lane retirement state tests ───────────────────────────────────

    #[test]
    fn s6_lane_state_serialization() {
        let rec = TapeRecord::LaneState {
            slot: 12345,
            retired: [false, true, false, false],
        };
        let jsonl = rec.to_jsonl();
        assert!(jsonl.contains(r#""kind":"lane_state""#));
        assert!(jsonl.contains(r#""slot":12345"#));
        assert!(jsonl.contains(r#""retired":[false,true,false,false]"#));
    }

    #[test]
    fn s6_lane_state_all_retired() {
        let rec = TapeRecord::LaneState {
            slot: 99999,
            retired: [true; 4],
        };
        let jsonl = rec.to_jsonl();
        assert!(jsonl.contains(r#""retired":[true,true,true,true]"#));
    }

    #[test]
    fn s6_lane_state_none_retired() {
        let rec = TapeRecord::LaneState {
            slot: 1,
            retired: [false; 4],
        };
        let jsonl = rec.to_jsonl();
        assert!(jsonl.contains(r#""retired":[false,false,false,false]"#));
    }

    #[test]
    fn s6_lane_state_no_floats() {
        let rec = TapeRecord::LaneState {
            slot: 42,
            retired: [true, false, true, false],
        };
        let jsonl = rec.to_jsonl();
        // No floats allowed (§22)
        assert!(!jsonl.contains('.'));
        assert!(!jsonl.contains("null"));
    }

    #[test]
    fn s6_lane_state_can_be_pushed_to_exporter() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("pq_test_lane_state.jsonl");
        let _ = std::fs::remove_file(&path);

        let mut exporter = TapeExporter::new(path.to_str().unwrap());
        exporter.push(TapeRecord::LaneState {
            slot: 100,
            retired: [false, true, false, false],
        });
        assert_eq!(exporter.pending_count(), 1);
        exporter.flush().unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains(r#""kind":"lane_state""#));
        assert!(content.contains(r#""retired":[false,true,false,false]"#));

        let _ = std::fs::remove_file(&path);
    }
}
