//! Paper-parity test: replay historical TS trades through the Rust GateStack + Scorer,
//! comparing decisions side-by-side.
//!
//! Usage:
//!   PARITY_DATA=path/to/backrun_paper_trades.jsonl cargo run --bin parity-test
//!   cargo run --bin parity-test -- path/to/backrun_paper_trades.jsonl

use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader};

use pump_quant_core::engine::gates::{GateConfig, GateRejectReason, GateStack};
use pump_quant_core::engine::scorer::{ScoreConfig, Scorer};
use pump_quant_core::feeds::{FeedSource, TradeEvent};

// ── JSONL schema (matching actual TS output) ────────────────────────

#[derive(serde::Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct TsTradeRecord {
    mint: String,
    entry_v_sol: f64,
    exit_v_sol: f64,
    #[serde(default)]
    entry_timestamp_ms: u64,
    #[serde(default)]
    exit_timestamp_ms: u64,
    hold_ms: u64,
    size_sol: f64,
    pnl_sol: f64,
    pnl_pct: f64,
    exit_reason: String,
    score: f64,
    #[serde(default)]
    recorded_at: u64,
    #[serde(default)]
    engine_version: Option<String>,
    #[serde(default)]
    data_version: Option<u32>,
    #[serde(default)]
    fees_sol: f64,
    #[serde(default)]
    net_pnl_sol: f64,
    #[serde(default)]
    net_pnl_pct: f64,
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Convert SOL to lamports (1 SOL = 1e9 lamports).
fn sol_to_lamports(sol: f64) -> u64 {
    (sol * 1_000_000_000.0).round() as u64
}

/// Decode a base58 mint string to [u8; 32]. Falls back to zeroed array on error.
fn decode_mint(s: &str) -> [u8; 32] {
    let mut buf = [0u8; 32];
    if let Ok(bytes) = bs58::decode(s).into_vec() {
        let len = bytes.len().min(32);
        buf[..len].copy_from_slice(&bytes[..len]);
    }
    buf
}

/// Build a synthetic TradeEvent from a TS trade record.
/// These represent the *entry* (buy) trade that triggered the position.
fn synth_event(rec: &TsTradeRecord) -> TradeEvent {
    TradeEvent {
        mint: decode_mint(&rec.mint),
        trader: [1u8; 32],           // synthetic trader
        sig: [0u8; 64],
        sig_prefix: [0u8; 8],
        sol_amount: sol_to_lamports(rec.size_sol), // entry size as trigger amount
        token_amount: 1_000_000,     // placeholder
        vsol_reserves: sol_to_lamports(rec.entry_v_sol),
        vtoken_reserves: 100_000_000_000, // placeholder
        market_cap_sol: sol_to_lamports(rec.entry_v_sol) * 2, // rough proxy
        slot: 0,
        timestamp_ms: rec.entry_timestamp_ms,
        is_buy: true,
        source: FeedSource::PumpPortal,
        bonding_curve: [0u8; 32],
        assoc_bonding_curve: [0u8; 32],
    }
}

// ── Statistics ───────────────────────────────────────────────────────

struct Stats {
    total_ts_trades: usize,
    rust_pass_count: usize,
    rust_reject_count: usize,
    rust_scores: Vec<f64>,
    ts_scores: Vec<f64>,
    // Confusion matrix
    rust_pass_ts_win: usize,     // Rust passes, TS trade was profitable
    rust_pass_ts_lose: usize,    // Rust passes, TS trade was unprofitable
    rust_reject_ts_win: usize,   // Rust rejects, but TS trade was profitable (missed opportunity)
    rust_reject_ts_lose: usize,  // Rust rejects, TS trade was unprofitable (good reject)
    // PnL tracking
    all_ts_net_pnl: f64,
    rust_approved_net_pnl: f64,
    // Rejection reasons
    reject_reasons: Vec<(GateRejectReason, usize)>,
    // Per-exit-reason breakdown
    exit_reason_counts: std::collections::HashMap<String, (usize, usize)>, // (total, rust_passed)
}

impl Stats {
    fn new() -> Self {
        Self {
            total_ts_trades: 0,
            rust_pass_count: 0,
            rust_reject_count: 0,
            rust_scores: Vec::new(),
            ts_scores: Vec::new(),
            rust_pass_ts_win: 0,
            rust_pass_ts_lose: 0,
            rust_reject_ts_win: 0,
            rust_reject_ts_lose: 0,
            all_ts_net_pnl: 0.0,
            rust_approved_net_pnl: 0.0,
            reject_reasons: Vec::new(),
            exit_reason_counts: std::collections::HashMap::new(),
        }
    }

    fn record_reject(&mut self, reason: GateRejectReason) {
        if let Some(entry) = self.reject_reasons.iter_mut().find(|(r, _)| *r == reason) {
            entry.1 += 1;
        } else {
            self.reject_reasons.push((reason, 1));
        }
    }

    fn percentile(sorted: &[f64], p: f64) -> f64 {
        if sorted.is_empty() {
            return 0.0;
        }
        let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
        sorted[idx.min(sorted.len() - 1)]
    }

    fn print_report(&mut self) {
        println!("\n╔══════════════════════════════════════════════════════════════╗");
        println!("║             PARITY TEST REPORT — Rust vs TypeScript         ║");
        println!("╚══════════════════════════════════════════════════════════════╝\n");

        // ── Overview ────────────────────────────────────────────────
        println!("─── Overview ───────────────────────────────────────────────");
        println!("  Total TS trades:       {}", self.total_ts_trades);
        println!("  Rust gate PASS:        {} ({:.1}%)",
            self.rust_pass_count,
            self.rust_pass_count as f64 / self.total_ts_trades.max(1) as f64 * 100.0);
        println!("  Rust gate REJECT:      {} ({:.1}%)",
            self.rust_reject_count,
            self.rust_reject_count as f64 / self.total_ts_trades.max(1) as f64 * 100.0);

        // ── Rust Score Distribution ─────────────────────────────────
        self.rust_scores.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!("\n─── Rust Score Distribution ────────────────────────────────");
        println!("  p25:  {:.4}", Self::percentile(&self.rust_scores, 0.25));
        println!("  p50:  {:.4}", Self::percentile(&self.rust_scores, 0.50));
        println!("  p75:  {:.4}", Self::percentile(&self.rust_scores, 0.75));
        println!("  p99:  {:.4}", Self::percentile(&self.rust_scores, 0.99));
        println!("  min:  {:.4}", self.rust_scores.first().copied().unwrap_or(0.0));
        println!("  max:  {:.4}", self.rust_scores.last().copied().unwrap_or(0.0));

        // ── TS Score Distribution (for comparison) ──────────────────
        self.ts_scores.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!("\n─── TS Score Distribution ─────────────────────────────────");
        println!("  p25:  {:.4}", Self::percentile(&self.ts_scores, 0.25));
        println!("  p50:  {:.4}", Self::percentile(&self.ts_scores, 0.50));
        println!("  p75:  {:.4}", Self::percentile(&self.ts_scores, 0.75));
        println!("  p99:  {:.4}", Self::percentile(&self.ts_scores, 0.99));

        // ── Confusion Matrix ────────────────────────────────────────
        let ts_total_wins = self.rust_pass_ts_win + self.rust_reject_ts_win;
        let ts_total_losses = self.rust_pass_ts_lose + self.rust_reject_ts_lose;
        let ts_win_rate = ts_total_wins as f64 / self.total_ts_trades.max(1) as f64 * 100.0;
        let rust_win_rate = if self.rust_pass_count > 0 {
            self.rust_pass_ts_win as f64 / self.rust_pass_count as f64 * 100.0
        } else {
            0.0
        };

        println!("\n─── Confusion Matrix ──────────────────────────────────────");
        println!("                        TS Win    TS Lose    Total");
        println!("  Rust PASS:            {:>6}     {:>6}     {:>6}",
            self.rust_pass_ts_win, self.rust_pass_ts_lose, self.rust_pass_count);
        println!("  Rust REJECT:          {:>6}     {:>6}     {:>6}",
            self.rust_reject_ts_win, self.rust_reject_ts_lose, self.rust_reject_count);
        println!("  Total:                {:>6}     {:>6}     {:>6}",
            ts_total_wins, ts_total_losses, self.total_ts_trades);

        println!("\n─── Win Rates ─────────────────────────────────────────────");
        println!("  All TS trades:         {:.2}% ({}/{})",
            ts_win_rate, ts_total_wins, self.total_ts_trades);
        println!("  Rust-approved subset:  {:.2}% ({}/{})",
            rust_win_rate, self.rust_pass_ts_win, self.rust_pass_count);
        let improvement = rust_win_rate - ts_win_rate;
        println!("  Win rate improvement:  {:+.2} pp", improvement);

        // ── PnL Comparison ──────────────────────────────────────────
        let avg_ts_pnl = self.all_ts_net_pnl / self.total_ts_trades.max(1) as f64;
        let avg_rust_pnl = if self.rust_pass_count > 0 {
            self.rust_approved_net_pnl / self.rust_pass_count as f64
        } else {
            0.0
        };

        println!("\n─── PnL Comparison (SOL) ──────────────────────────────────");
        println!("  All TS total net PnL:     {:+.6}", self.all_ts_net_pnl);
        println!("  All TS avg net PnL:       {:+.6}", avg_ts_pnl);
        println!("  Rust-approved net PnL:    {:+.6}", self.rust_approved_net_pnl);
        println!("  Rust-approved avg PnL:    {:+.6}", avg_rust_pnl);

        // ── Missed Opportunities (Rust rejects profitable trades) ───
        println!("\n─── Missed Opportunities (Rust rejects, but TS won) ─────");
        println!("  Count: {} / {} profitable trades ({:.1}%)",
            self.rust_reject_ts_win, ts_total_wins,
            self.rust_reject_ts_win as f64 / ts_total_wins.max(1) as f64 * 100.0);

        // ── Good Rejections (Rust rejects unprofitable trades) ──────
        println!("\n─── Good Rejections (Rust rejects, TS lost) ───────────────");
        println!("  Count: {} / {} losing trades ({:.1}%)",
            self.rust_reject_ts_lose, ts_total_losses,
            self.rust_reject_ts_lose as f64 / ts_total_losses.max(1) as f64 * 100.0);

        // ── Rejection Reason Breakdown ──────────────────────────────
        self.reject_reasons.sort_by(|a, b| b.1.cmp(&a.1));
        println!("\n─── Rejection Reasons ─────────────────────────────────────");
        for (reason, count) in &self.reject_reasons {
            println!("  {:>6}  {}", count, reason);
        }

        // ── Per-Exit-Reason Breakdown ───────────────────────────────
        println!("\n─── Rust Pass Rate by TS Exit Reason ──────────────────────");
        let mut exit_vec: Vec<_> = self.exit_reason_counts.iter().collect();
        exit_vec.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
        for (reason, (total, passed)) in &exit_vec {
            let rate = *passed as f64 / (*total).max(1) as f64 * 100.0;
            println!("  {:>20}: {:>5}/{:<5} ({:.1}%)", reason, passed, total, rate);
        }

        println!("\n══════════════════════════════════════════════════════════════");
    }
}

// ── Main ────────────────────────────────────────────────────────────

fn main() -> anyhow::Result<()> {
    let data_path = env::var("PARITY_DATA")
        .ok()
        .or_else(|| env::args().nth(1))
        .unwrap_or_else(|| {
            eprintln!("Usage: PARITY_DATA=path cargo run --bin parity-test");
            eprintln!("   or: cargo run --bin parity-test -- path/to/backrun_paper_trades.jsonl");
            std::process::exit(1);
        });

    println!("Loading trade data from: {}", data_path);

    let file = File::open(&data_path)?;
    let reader = BufReader::new(file);

    // ── Initialize Rust engine components ────────────────────────
    let gate_config = GateConfig::default();
    let gate_stack = GateStack::new(gate_config);

    let score_config = ScoreConfig::default();
    let scorer = Scorer::new(
        score_config,
        gate_stack.config().min_vsol_lamports,
        gate_stack.config().max_vsol_lamports,
    );

    let mut stats = Stats::new();
    let mut parse_errors = 0usize;

    for (line_no, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("  [line {}] read error: {}", line_no + 1, e);
                parse_errors += 1;
                continue;
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let rec: TsTradeRecord = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                if parse_errors < 5 {
                    eprintln!("  [line {}] parse error: {}", line_no + 1, e);
                }
                parse_errors += 1;
                continue;
            }
        };

        stats.total_ts_trades += 1;
        stats.ts_scores.push(rec.score);

        // Track exit reason
        let exit_entry = stats.exit_reason_counts
            .entry(rec.exit_reason.clone())
            .or_insert((0, 0));
        exit_entry.0 += 1;

        // Was this TS trade profitable (net)?
        let ts_win = rec.net_pnl_sol > 0.0;
        stats.all_ts_net_pnl += rec.net_pnl_sol;

        // Build synthetic TradeEvent for Rust evaluation
        let event = synth_event(&rec);

        // ── Compute Rust score ──────────────────────────────────
        // Use synthetic but realistic aggregates that represent
        // "this trade already passed TS gates" — we're testing whether
        // the Rust scorer + gate threshold would agree.
        //
        // We set aggregates to values that pass all non-score gates,
        // so the parity test isolates the SCORE gate comparison.
        let trigger_lamports = event.sol_amount;
        let vsol_lamports = event.vsol_reserves;

        // Synthetic aggregates: values that pass all momentum/crowd gates
        let unique_buyers_30s: u16 = 8;
        let buy_count_1s: u16 = 3;
        let buy_count_2s: u16 = 5;
        let buy_count_5s: u16 = 8;
        let sell_count_5s: u16 = 2;
        let volume_5s_lamports: u64 = 3_000_000_000; // 3 SOL
        let max_wallet_vol: u64 = 500_000_000;       // 0.5 SOL (distributed)
        let total_buy_vol_30s: u64 = 8_000_000_000;  // 8 SOL

        let sc = scorer.compute(
            trigger_lamports,
            vsol_lamports,
            unique_buyers_30s,
            buy_count_1s,
            buy_count_2s,
            volume_5s_lamports,
            max_wallet_vol,
            total_buy_vol_30s,
        );

        let rust_score = sc.final_score;
        stats.rust_scores.push(rust_score);

        // ── Run through full gate stack ─────────────────────────
        let gate_result = gate_stack.evaluate(
            &event,
            10_000,                  // history_age_ms: 10s (young token)
            unique_buyers_30s,
            buy_count_1s,
            buy_count_2s,
            buy_count_5s,
            sell_count_5s,
            volume_5s_lamports,
            500_000_000,             // vsol_delta_3s: 0.5 SOL
            500,                     // time_since_last_buy_ms
            0,                       // creator_sell_at_ms (none)
            event.timestamp_ms + 1,  // now_ms
            rust_score,              // score from Rust scorer
        );

        let rust_passed = gate_result.is_ok();

        if rust_passed {
            stats.rust_pass_count += 1;
            stats.rust_approved_net_pnl += rec.net_pnl_sol;
            // Track exit reason pass
            stats.exit_reason_counts
                .get_mut(&rec.exit_reason)
                .unwrap()
                .1 += 1;

            if ts_win {
                stats.rust_pass_ts_win += 1;
            } else {
                stats.rust_pass_ts_lose += 1;
            }
        } else {
            stats.rust_reject_count += 1;
            if let Err(reason) = gate_result {
                stats.record_reject(reason);
            }

            if ts_win {
                stats.rust_reject_ts_win += 1;
            } else {
                stats.rust_reject_ts_lose += 1;
            }
        }
    }

    if parse_errors > 0 {
        println!("\n⚠ Parse errors: {} lines skipped", parse_errors);
    }

    stats.print_report();

    Ok(())
}
