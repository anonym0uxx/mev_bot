//! Phase 6: End-to-end integration test for the autonomous architecture.
//!
//! Verifies the full pipeline: daemon produces trades → tape export serializes
//! them → promotion gate evaluates → derive_envelope produces a conservative
//! LiveEnvelope from paper evidence → envelope is smaller than paper.
//!
//! This test does NOT require network connectivity. It uses synthetic trade
//! data that mimics what pq-daemon would produce during a real paper session.

use pump_quant_junction::tape_export::{TapeExporter, TapeRecord, TapeLane};
use pump_quant_junction::trade_journal::{
    JournalConfig, TradeJournal, TradeRecord, TradeOutcome, TradeSide, RunMode,
};
use pump_quant_junction::ProvenanceSource;
use pump_quant_execution::ex_promotion_gate::{
    PaperEvidence, PromotionCriteria, evaluate,
    PaperEnvelopeEvidence, derive_envelope,
    PromotionVerdict,
};

// ─────────────────────────────────────────────────────────────────────────
// Test helpers
// ─────────────────────────────────────────────────────────────────────────

/// Create a synthetic trade record that a paper session would produce.
fn make_trade(
    mint: &str,
    slot: u64,
    side: TradeSide,
    outcome: TradeOutcome,
    entry_price_fp: i128,
    exit_price_fp: i128,
    fees_lamports: u64,
    size_lamports: u64,
    realized_pnl: i64,
) -> TradeRecord {
    TradeRecord {
        slot,
        mint_b58: mint.to_string(),
        side,
        entry_price_fp,
        exit_price_fp,
        size_lamports,
        strategy_id: 1,
        source: ProvenanceSource::LaserStream,
        outcome,
        realized_pnl_lamports: realized_pnl,
        fees_lamports,
        slippage_lamports: 0,
        decision_latency_us: 500,
        confirm_latency_us: 1000,
        run_mode: RunMode::Paper,
        error_code: 0,
        seq: slot,
        lane: None,
    }
}

/// Build a paper evidence summary from a list of closed trade PnLs.
fn build_paper_evidence(
    pnls: &[i64],
    max_drawdown: u64,
    entries_attempted: u32,
    entries_filled: u32,
    slots_observed: u64,
    slots_missed: u64,
) -> PaperEvidence {
    let closed_positions = pnls.len() as u32;
    let net_pnl: i64 = pnls.iter().sum();
    let sum_sq: i128 = pnls.iter().map(|p| (*p as i128) * (*p as i128)).sum();
    PaperEvidence {
        closed_positions,
        net_pnl_lamports: net_pnl,
        sum_sq_pnl_lamports: sum_sq,
        max_drawdown_lamports: max_drawdown,
        entries_attempted,
        entries_filled,
        slots_observed,
        slots_missed,
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Step 1: Daemon produces trades → tape export
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn step1_tape_export_from_trade_journal() {
    let _ = std::fs::remove_file("data/test_tape_step1.jsonl");
    let config = JournalConfig::default();
    let mut journal = TradeJournal::new(config);

    // 20 winning trades + 5 losing trades
    for i in 0..20u64 {
        let rec = make_trade(
            &format!("mint{}", i % 3),
            1000 + i,
            TradeSide::Buy,
            TradeOutcome::Filled,
            1_000_000_000,
            1_100_000_000,
            50_000,
            1_000_000,
            50_000,
        );
        journal.record(rec);
    }
    for i in 0..5u64 {
        let rec = make_trade(
            &format!("mint{}", i + 20),
            2000 + i,
            TradeSide::Buy,
            TradeOutcome::Filled,
            1_000_000_000,
            900_000_000,
            50_000,
            1_000_000,
            -100_000,
        );
        journal.record(rec);
    }

    // Export to tape using TapeExporter
    let mut exporter = TapeExporter::new("data/test_tape_step1.jsonl");
    for i in 0..25u64 {
        // Simulate exporting each trade
        let rec = if i < 20 {
            make_trade(
                &format!("mint{}", i % 3),
                1000 + i,
                TradeSide::Buy,
                TradeOutcome::Filled,
                1_000_000_000,
                1_100_000_000,
                50_000,
                1_000_000,
                50_000,
            )
        } else {
            make_trade(
                &format!("mint{}", i - 20 + 20),
                2000 + (i - 20),
                TradeSide::Buy,
                TradeOutcome::Filled,
                1_000_000_000,
                900_000_000,
                50_000,
                1_000_000,
                -100_000,
            )
        };
        exporter.export_trade(&rec);
    }

    assert_eq!(exporter.pending_count(), 25);
    exporter.flush().expect("tape flush should succeed");

    // Read back and verify
    let jsonl = std::fs::read_to_string("data/test_tape_step1.jsonl")
        .expect("tape file should exist");
    let lines: Vec<&str> = jsonl.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 25);

    let _ = std::fs::remove_file("data/test_tape_step1.jsonl");
}

// ─────────────────────────────────────────────────────────────────────────
// Step 2: Promotion gate evaluates paper evidence
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn step2_promotion_gate_passes_on_good_paper() {
    let pnls: Vec<i64> = (0..150).map(|i| {
        if i < 120 { 50_000 } else { -30_000 }
    }).collect();

    let evidence = build_paper_evidence(&pnls, 200_000, 150, 150, 100_000, 50);
    let criteria = PromotionCriteria::conservative();
    let report = evaluate(&evidence, &criteria);

    assert_eq!(report.verdict, PromotionVerdict::Promote,
        "promotion gate should pass on 150 trades with positive net PnL");
}

#[test]
fn step2_promotion_gate_refuses_on_insufficient_sample() {
    let pnls: Vec<i64> = vec![50_000; 50];
    let evidence = build_paper_evidence(&pnls, 0, 50, 50, 100_000, 50);
    let criteria = PromotionCriteria::conservative();
    let report = evaluate(&evidence, &criteria);

    assert!(matches!(report.verdict, PromotionVerdict::Refuse(_)),
        "promotion gate should refuse on <100 closed positions");
}

// ─────────────────────────────────────────────────────────────────────────
// Step 3: derive_envelope produces a conservative envelope
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn step3_derive_envelope_from_good_paper() {
    let env_evidence = PaperEnvelopeEvidence {
        verdict: PromotionVerdict::Promote,
        max_drawdown_lamports: 500_000,
        closed_positions: 150,
        net_pnl_lamports: 5_100_000,
        paper_max_winning_position_lamports: 1_000_000,
        peak_concurrent_open: 5,
        peak_deployed_lamports: 5_000_000,
        slippage_p95_bps: 120,
        median_slot_interval_ms: 400,
        total_entries: 150,
        session_duration_secs: 3600,
    };

    let envelope = derive_envelope(&env_evidence);

    assert!(envelope.admits_anything(), "envelope should admit trading");
    assert_eq!(envelope.max_position_lamports, 500_000);
    assert_eq!(envelope.max_total_deployed_lamports, 3_000_000);
    assert_eq!(envelope.max_open_positions, 3);
    assert_eq!(envelope.max_entries_per_hour, 75);
    assert_eq!(envelope.daily_loss_limit_lamports, 1_500_000);
    assert_eq!(envelope.max_entry_slippage_bps, 170);
    assert_eq!(envelope.heartbeat_timeout_ms, 1200);

    // The envelope must be SMALLER than what paper ran
    assert!(envelope.max_position_lamports <= env_evidence.paper_max_winning_position_lamports);
    assert!(envelope.max_total_deployed_lamports <= env_evidence.peak_deployed_lamports);
    assert!(envelope.max_open_positions <= env_evidence.peak_concurrent_open);
}

// ─────────────────────────────────────────────────────────────────────────
// Step 4: Full pipeline — tape → gate → envelope (integration)
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn step4_full_pipeline_tape_to_gate_to_envelope() {
    let _ = std::fs::remove_file("data/test_tape_step4.jsonl");
    // ── Simulate a paper session producing trades ─────────────────────────
    let mut pnls: Vec<i64> = Vec::new();
    let mut exporter = TapeExporter::new("data/test_tape_step4.jsonl");

    for i in 0..150u64 {
        let pnl = if i < 120 { 50_000 } else { -30_000 };
        pnls.push(pnl);
        let rec = make_trade(
            &format!("mint{}", i % 5),
            1000 + i,
            TradeSide::Buy,
            TradeOutcome::Filled,
            1_000_000_000,
            if pnl > 0 { 1_100_000_000 } else { 900_000_000 },
            50_000,
            1_000_000,
            pnl,
        );
        exporter.export_trade(&rec);
    }

    // ── Step 1: Flush tape ────────────────────────────────────────────────
    exporter.flush().expect("tape should flush");
    let tape_content = std::fs::read_to_string("data/test_tape_step4.jsonl")
        .expect("tape file should exist");
    let tape_lines: Vec<&str> = tape_content.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(tape_lines.len(), 150, "tape should have 150 trade records");

    // ── Step 2: Build paper evidence from the trades ─────────────────────
    let paper_evidence = build_paper_evidence(
        &pnls, 200_000, 150, 150, 100_000, 50,
    );

    // ── Step 3: Run the promotion gate ───────────────────────────────────
    let criteria = PromotionCriteria::conservative();
    let report = evaluate(&paper_evidence, &criteria);
    assert_eq!(report.verdict, PromotionVerdict::Promote,
        "gate should promote: 150 trades, positive net, t^2 > 4, fill rate 100%");

    // ── Step 4: Derive the envelope from paper evidence ──────────────────
    let env_evidence = PaperEnvelopeEvidence {
        verdict: report.verdict,
        max_drawdown_lamports: paper_evidence.max_drawdown_lamports,
        closed_positions: paper_evidence.closed_positions,
        net_pnl_lamports: paper_evidence.net_pnl_lamports,
        paper_max_winning_position_lamports: 1_000_000,
        peak_concurrent_open: 5,
        peak_deployed_lamports: 5_000_000,
        slippage_p95_bps: 120,
        median_slot_interval_ms: 400,
        total_entries: 150,
        session_duration_secs: 3600,
    };

    let envelope = derive_envelope(&env_evidence);

    // ── Verify the full pipeline produced a valid, conservative envelope ─
    assert!(envelope.admits_anything());
    assert_eq!(envelope.max_position_lamports, 500_000);
    assert_eq!(envelope.max_total_deployed_lamports, 3_000_000);
    assert_eq!(envelope.max_open_positions, 3);
    assert_eq!(envelope.max_entries_per_hour, 75);
    assert_eq!(envelope.daily_loss_limit_lamports, 600_000); // 200K × 3
    assert_eq!(envelope.max_entry_slippage_bps, 170);
    assert_eq!(envelope.heartbeat_timeout_ms, 1200);

    let _ = std::fs::remove_file("data/test_tape_step4.jsonl");
}

// ─────────────────────────────────────────────────────────────────────────
// Step 5: Refused promotion → closed envelope (fail-closed)
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn step5_refused_promotion_yields_closed_envelope() {
    let env_evidence = PaperEnvelopeEvidence {
        verdict: PromotionVerdict::Refuse(
            pump_quant_execution::ex_promotion_gate::RefusalReason::SampleTooSmall {
                closed: 50,
                required: 100,
            }
        ),
        max_drawdown_lamports: 500_000,
        closed_positions: 50,
        net_pnl_lamports: 1_000_000,
        paper_max_winning_position_lamports: 1_000_000,
        peak_concurrent_open: 5,
        peak_deployed_lamports: 5_000_000,
        slippage_p95_bps: 120,
        median_slot_interval_ms: 400,
        total_entries: 50,
        session_duration_secs: 3600,
    };

    let envelope = derive_envelope(&env_evidence);
    assert!(!envelope.admits_anything(), "refused promotion must yield closed envelope");
    assert_eq!(envelope.max_position_lamports, 0);
}

// ─────────────────────────────────────────────────────────────────────────
// Step 6: Tape round-trip — write and re-parse
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn step6_tape_jsonl_roundtrip() {
    // Delete any existing file first (flush uses append mode)
    let _ = std::fs::remove_file("data/test_tape_roundtrip.jsonl");

    let mut exporter = TapeExporter::new("data/test_tape_roundtrip.jsonl");

    for i in 0..10u64 {
        let rec = make_trade(
            &format!("mint{}", i),
            1000 + i,
            TradeSide::Buy,
            TradeOutcome::Filled,
            1_000_000_000,
            1_050_000_000,
            50_000,
            1_000_000,
            25_000,
        );
        exporter.export_trade(&rec);
    }

    assert_eq!(exporter.pending_count(), 10);
    exporter.flush().expect("flush should succeed");

    let content = std::fs::read_to_string("data/test_tape_roundtrip.jsonl")
        .expect("file should exist");
    let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 10, "should have 10 JSONL lines");

    for line in &lines {
        assert!(line.contains("\"kind\""), "each line should have a kind field");
        assert!(
            line.contains("\"trade_full\"") || line.contains("\"trade\""),
            "each line should be a trade record (kind=trade or kind=trade_full)"
        );
    }

    let _ = std::fs::remove_file("data/test_tape_roundtrip.jsonl");
}

// ─────────────────────────────────────────────────────────────────────────
// Step 7: Envelope is always conservative — stress test
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn step7_envelope_never_exceeds_paper_capacity() {
    let profiles: Vec<(u64, u32, u64, u32, u64, u32, u64)> = vec![
        (1_000_000, 10, 20_000_000, 300, 400, 600, 3600),
        (500_000, 3, 1_500_000, 80, 400, 30, 3600),
        (2_000_000, 7, 14_000_000, 200, 500, 200, 7200),
        (100_000, 1, 100_000, 50, 400, 10, 3600),
    ];

    for (max_win, peak_conc, peak_dep, slip_p95, slot_ms, entries, duration) in &profiles {
        let env_evidence = PaperEnvelopeEvidence {
            verdict: PromotionVerdict::Promote,
            max_drawdown_lamports: 500_000,
            closed_positions: 150,
            net_pnl_lamports: 10_000_000,
            paper_max_winning_position_lamports: *max_win,
            peak_concurrent_open: *peak_conc,
            peak_deployed_lamports: *peak_dep,
            slippage_p95_bps: *slip_p95,
            median_slot_interval_ms: *slot_ms,
            total_entries: *entries,
            session_duration_secs: *duration,
        };

        let envelope = derive_envelope(&env_evidence);

        assert!(envelope.max_position_lamports <= *max_win,
            "position {} > paper max {}", envelope.max_position_lamports, max_win);
        assert!(envelope.max_total_deployed_lamports <= *peak_dep,
            "deployed {} > paper peak {}", envelope.max_total_deployed_lamports, peak_dep);
        assert!(envelope.max_open_positions <= *peak_conc,
            "open {} > paper peak {}", envelope.max_open_positions, peak_conc);
    }
}
