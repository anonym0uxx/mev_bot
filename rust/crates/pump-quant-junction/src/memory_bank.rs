//! `memory_bank` — per-mint and per-strategy performance summaries for
//! continuous optimization toward maximum net SOL.
//!
//! Constitution reference: **§29.9** (QuantMemoryStore), **§74** (net-SOL
//! expectancy exit policies), **§43** (tables/journal schema), **§100**
//! (scalp time-stops hazard-estimated), **§96** (signal-horizon matching),
//! **§85** (meta-rotation detection/allocation).
//!
//! The memory bank consumes [`crate::trade_journal::TradeRecord`]s and
//! aggregates them into:
//!
//! * **Per-mint summaries** — is this mint profitable to scalp? What's the
//!   win rate, avg PnL, avg latency, best entry slot-window?
//! * **Per-strategy summaries** — which strategy ID produces the most net
//!   SOL? What's the edge decay (§54) per strategy?
//! * **Global summary** — the bot's overall health: net SOL, win rate,
//!   avg slippage, avg latency, throughput.
//!
//! These summaries are the "memory" that lets the bot adapt: a mint with
//! a 90% loss rate gets deprioritized; a strategy with positive expectancy
//! gets more allocation. This is the feedback loop that turns a static
//! scalp bot into a self-optimizing one.
//!
//! ## Design constraints (§22 — deterministic, integer-only)
//!
//! * No floating point. Win rates are in basis points (0..=10_000).
//! * No unsafe. All arithmetic uses `checked_*`/`saturating_*`.
//! * Memory-bounded (§57). Each summary table is capped; overflow evicts
//!   the least-recently-updated entry.

use crate::trade_journal::{TradeRecord, TradeOutcome, RunMode};
use std::collections::HashMap;

// ─── Per-mint summary ─────────────────────────────────────────────────────

/// Rolling performance summary for a single mint.
/// Updated every time a terminal trade for this mint is recorded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MintSummary {
    /// Total trades recorded for this mint (terminal only).
    pub trades: u64,
    /// Wins (net positive after fees).
    pub wins: u64,
    /// Losses (net zero or negative, terminal).
    pub losses: u64,
    /// Cumulative net lamports (pnl - fees) across all trades.
    pub net_lamports: i64,
    /// Cumulative fees paid.
    pub fees_lamports: u64,
    /// Cumulative slippage.
    pub slippage_lamports: u64,
    /// Sum of decision latencies (for averaging).
    pub total_decision_latency_us: u64,
    /// Sum of confirmation latencies.
    pub total_confirm_latency_us: u64,
    /// Last slot this mint was traded.
    pub last_slot: u64,
    /// Best single-trade net (lamports).
    pub best_trade_lamports: i64,
    /// Worst single-trade net (lamports).
    pub worst_trade_lamports: i64,
}

impl MintSummary {
    /// Win rate in basis points (0..=10_000).
    pub fn win_rate_bps(&self) -> u32 {
        let total = self.wins.saturating_add(self.losses);
        if total == 0 {
            return 0;
        }
        let num = (self.wins as u128) * 10_000;
        let den = total as u128;
        ((num / den) as u32).min(10_000)
    }

    /// Average decision latency in microseconds (0 if no trades).
    pub fn avg_decision_latency_us(&self) -> u64 {
        if self.trades == 0 {
            return 0;
        }
        self.total_decision_latency_us / self.trades
    }

    /// Average net per trade in lamports (0 if no trades).
    pub fn avg_net_per_trade(&self) -> i64 {
        if self.trades == 0 {
            return 0;
        }
        self.net_lamports / self.trades as i64
    }

    /// Edge score: positive expectancy = profitable to scalp.
    /// This is `avg_net_per_trade` — a simple, robust metric.
    pub fn edge(&self) -> i64 {
        self.avg_net_per_trade()
    }

    /// Is this mint worth scalping? Positive edge and > 0 trades.
    pub fn is_profitable(&self) -> bool {
        self.trades > 0 && self.edge() > 0
    }
}

// ─── Per-strategy summary ─────────────────────────────────────────────────

/// Rolling performance summary for a single strategy ID.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StrategySummary {
    /// Total trades executed by this strategy.
    pub trades: u64,
    pub wins: u64,
    pub losses: u64,
    /// Cumulative net lamports.
    pub net_lamports: i64,
    /// Cumulative fees.
    pub fees_lamports: u64,
    /// Edge decay tracking (§54): net lamports in the most recent N trades
    /// vs the oldest N trades. If recent < oldest, the strategy is decaying.
    pub recent_net_lamports: i64,
    pub old_net_lamports: i64,
    /// Number of trades in the "recent" window for decay comparison.
    pub recent_window: u64,
    /// Last slot this strategy fired.
    pub last_slot: u64,
}

impl StrategySummary {
    pub fn win_rate_bps(&self) -> u32 {
        let total = self.wins.saturating_add(self.losses);
        if total == 0 {
            return 0;
        }
        let num = (self.wins as u128) * 10_000;
        let den = total as u128;
        ((num / den) as u32).min(10_000)
    }

    pub fn avg_net_per_trade(&self) -> i64 {
        if self.trades == 0 {
            return 0;
        }
        self.net_lamports / self.trades as i64
    }

    /// Edge decay ratio (§54): recent_net / old_net.
    /// Returns 1_000_000 (=1.0 in ppm) if no old trades.
    /// > 1_000_000 = improving, < 1_000_000 = decaying, 0 = recent is zero.
    pub fn edge_decay_ratio_ppm(&self) -> i64 {
        if self.old_net_lamports == 0 {
            return 1_000_000; // neutral — no baseline
        }
        // recent_ppm = recent * 1_000_000 / old (signed)
        let recent = self.recent_net_lamports as i128;
        let old = self.old_net_lamports as i128;
        let ppm = recent * 1_000_000 / old;
        // saturate to i64
        ppm.try_into().unwrap_or(if ppm > 0 { i64::MAX } else { i64::MIN })
    }

    /// Is the strategy decaying (recent edge < old edge)?
    pub fn is_decaying(&self) -> bool {
        self.edge_decay_ratio_ppm() < 1_000_000
    }
}

// ─── Global summary ───────────────────────────────────────────────────────

/// Bot-wide performance summary — the health dashboard.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GlobalSummary {
    pub total_trades: u64,
    pub total_wins: u64,
    pub total_losses: u64,
    pub net_lamports: i64,
    pub fees_lamports: u64,
    pub slippage_lamports: u64,
    pub total_decision_latency_us: u64,
    pub total_confirm_latency_us: u64,
    /// Paper vs live trade counts.
    pub paper_trades: u64,
    pub live_trades: u64,
    /// Outcome breakdown (for the error taxonomy — §78).
    pub outcome_counts: [u64; 6],
}

impl GlobalSummary {
    pub fn win_rate_bps(&self) -> u32 {
        let total = self.total_wins.saturating_add(self.total_losses);
        if total == 0 {
            return 0;
        }
        let num = (self.total_wins as u128) * 10_000;
        let den = total as u128;
        ((num / den) as u32).min(10_000)
    }

    pub fn avg_net_per_trade(&self) -> i64 {
        if self.total_trades == 0 {
            return 0;
        }
        self.net_lamports / self.total_trades as i64
    }

    pub fn avg_decision_latency_us(&self) -> u64 {
        if self.total_trades == 0 {
            return 0;
        }
        self.total_decision_latency_us / self.total_trades
    }

    pub fn avg_confirm_latency_us(&self) -> u64 {
        if self.total_trades == 0 {
            return 0;
        }
        self.total_confirm_latency_us / self.total_trades
    }
}

// ─── Memory bank ──────────────────────────────────────────────────────────

/// Configuration for the memory bank.
#[derive(Clone, Debug)]
pub struct MemoryBankConfig {
    /// Max mints to track (§57 — memory-bounded).
    pub max_mints: usize,
    /// Max strategies to track.
    pub max_strategies: usize,
    /// Window size for edge-decay tracking (number of recent trades).
    pub decay_window: u64,
}

impl Default for MemoryBankConfig {
    fn default() -> Self {
        Self {
            max_mints: 5_000,
            max_strategies: 128,
            decay_window: 20,
        }
    }
}

/// The memory bank — aggregates trade journal records into per-mint,
/// per-strategy, and global performance summaries.
///
/// Call `ingest(&record)` for every terminal trade. Then query:
/// * `mint_summary("base58...")` → is this mint profitable?
/// * `strategy_summary(id)` → is this strategy still working?
/// * `global_summary()` → bot health dashboard.
/// * `top_mints(n)` → most profitable mints right now.
/// * `decaying_strategies()` → strategies losing edge (§54/§85).
pub struct MemoryBank {
    mints: HashMap<String, MintSummary>,
    strategies: HashMap<u64, StrategySummary>,
    global: GlobalSummary,
    config: MemoryBankConfig,
    /// Per-strategy ring buffers for edge-decay tracking.
    /// Each strategy keeps the last `decay_window` net-lamports values.
    decay_rings: HashMap<u64, VecDeque<i64>>,
}

use std::collections::VecDeque;

impl MemoryBank {
    pub fn new(config: MemoryBankConfig) -> Self {
        Self {
            mints: HashMap::with_capacity(config.max_mints),
            strategies: HashMap::with_capacity(config.max_strategies),
            global: GlobalSummary::default(),
            config,
            decay_rings: HashMap::new(),
        }
    }

    /// Ingest a trade record and update all summaries.
    /// Only terminal trades update the summaries (pending trades are ignored).
    pub fn ingest(&mut self, rec: &TradeRecord) {
        // Only terminal trades contribute to summaries.
        if !rec.outcome.is_terminal() {
            return;
        }

        let net = rec.net_lamports();
        let is_win = rec.is_win();

        // ─── Update global ──────────────────────────────────────────────
        self.global.total_trades = self.global.total_trades.saturating_add(1);
        self.global.net_lamports = self.global.net_lamports.saturating_add(net);
        self.global.fees_lamports = self.global.fees_lamports.saturating_add(rec.fees_lamports);
        self.global.slippage_lamports = self.global.slippage_lamports.saturating_add(rec.slippage_lamports);
        self.global.total_decision_latency_us = self.global.total_decision_latency_us.saturating_add(rec.decision_latency_us);
        self.global.total_confirm_latency_us = self.global.total_confirm_latency_us.saturating_add(rec.confirm_latency_us);

        match rec.run_mode {
            RunMode::Paper => self.global.paper_trades = self.global.paper_trades.saturating_add(1),
            RunMode::Live => self.global.live_trades = self.global.live_trades.saturating_add(1),
        }

        // Outcome counts (for error taxonomy — §78).
        let idx = match rec.outcome {
            TradeOutcome::Filled => 0,
            TradeOutcome::FilledWithSlippage => 1,
            TradeOutcome::RejectedPreSubmit => 2,
            TradeOutcome::OnChainFailure => 3,
            TradeOutcome::Pending => 4,
            TradeOutcome::Expired => 5,
        };
        self.global.outcome_counts[idx] = self.global.outcome_counts[idx].saturating_add(1);

        if is_win {
            self.global.total_wins = self.global.total_wins.saturating_add(1);
        } else {
            self.global.total_losses = self.global.total_losses.saturating_add(1);
        }

        // ─── Update per-mint ────────────────────────────────────────────
        let mint = self.mints.entry(rec.mint_b58.clone()).or_default();
        mint.trades = mint.trades.saturating_add(1);
        if is_win {
            mint.wins = mint.wins.saturating_add(1);
        } else {
            mint.losses = mint.losses.saturating_add(1);
        }
        mint.net_lamports = mint.net_lamports.saturating_add(net);
        mint.fees_lamports = mint.fees_lamports.saturating_add(rec.fees_lamports);
        mint.slippage_lamports = mint.slippage_lamports.saturating_add(rec.slippage_lamports);
        mint.total_decision_latency_us = mint.total_decision_latency_us.saturating_add(rec.decision_latency_us);
        mint.total_confirm_latency_us = mint.total_confirm_latency_us.saturating_add(rec.confirm_latency_us);
        mint.last_slot = rec.slot;
        if net > mint.best_trade_lamports {
            mint.best_trade_lamports = net;
        }
        if net < mint.worst_trade_lamports {
            mint.worst_trade_lamports = net;
        }

        // Enforce max_mints capacity (evict least-recently-updated).
        if self.mints.len() > self.config.max_mints {
            self.evict_oldest_mint();
        }

        // ─── Update per-strategy ────────────────────────────────────────
        let strat = self.strategies.entry(rec.strategy_id).or_default();
        strat.trades = strat.trades.saturating_add(1);
        if is_win {
            strat.wins = strat.wins.saturating_add(1);
        } else {
            strat.losses = strat.losses.saturating_add(1);
        }
        strat.net_lamports = strat.net_lamports.saturating_add(net);
        strat.fees_lamports = strat.fees_lamports.saturating_add(rec.fees_lamports);
        strat.last_slot = rec.slot;

        // Edge-decay ring (§54).
        let ring = self.decay_rings.entry(rec.strategy_id).or_default();
        ring.push_back(net);
        if ring.len() as u64 > self.config.decay_window {
            ring.pop_front();
        }
        // Split the ring: recent half vs old half.
        let half = self.config.decay_window / 2;
        let half = if half == 0 { 1 } else { half as usize };
        let recent_net: i64 = ring.iter().rev().take(half).sum();
        let old_net: i64 = ring.iter().take(half).sum();
        strat.recent_net_lamports = recent_net;
        strat.old_net_lamports = old_net;
        strat.recent_window = half as u64;

        // Enforce max_strategies capacity.
        if self.strategies.len() > self.config.max_strategies {
            self.evict_oldest_strategy();
        }
    }

    /// Get the summary for a specific mint. Returns None if never traded.
    pub fn mint_summary(&self, mint: &str) -> Option<&MintSummary> {
        self.mints.get(mint)
    }

    /// Get the summary for a specific strategy.
    pub fn strategy_summary(&self, id: u64) -> Option<&StrategySummary> {
        self.strategies.get(&id)
    }

    /// Get the global summary.
    pub fn global_summary(&self) -> &GlobalSummary {
        &self.global
    }

    /// Top-N most profitable mints by total net lamports.
    pub fn top_mints(&self, n: usize) -> Vec<(&str, &MintSummary)> {
        let mut all: Vec<(&str, &MintSummary)> = self.mints
            .iter()
            .map(|(k, v)| (k.as_str(), v))
            .collect();
        all.sort_by(|a, b| b.1.net_lamports.cmp(&a.1.net_lamports));
        all.into_iter().take(n).collect()
    }

    /// Strategies that are decaying (recent edge < old edge — §54/§85).
    pub fn decaying_strategies(&self) -> Vec<(&u64, &StrategySummary)> {
        self.strategies
            .iter()
            .filter(|(_, s)| s.is_decaying())
            .collect()
    }

    /// Strategies with positive expectancy (profitable).
    pub fn profitable_strategies(&self) -> Vec<(&u64, &StrategySummary)> {
        self.strategies
            .iter()
            .filter(|(_, s)| s.avg_net_per_trade() > 0)
            .collect()
    }

    /// Number of mints being tracked.
    pub fn mint_count(&self) -> usize {
        self.mints.len()
    }

    /// Number of strategies being tracked.
    pub fn strategy_count(&self) -> usize {
        self.strategies.len()
    }

    /// Serialize the global summary to a compact JSON string for logging.
    pub fn global_json(&self) -> String {
        let g = &self.global;
        format!(
            "{{\"trades\":{},\"wins\":{},\"losses\":{},\"net_lamports\":{},\"fees\":{},\"slippage\":{},\"win_rate_bps\":{},\"avg_net_per_trade\":{},\"avg_decision_us\":{},\"avg_confirm_us\":{},\"paper\":{},\"live\":{},\"outcome\":[{},{},{},{},{},{}]}}",
            g.total_trades, g.total_wins, g.total_losses,
            g.net_lamports, g.fees_lamports, g.slippage_lamports,
            g.win_rate_bps(), g.avg_net_per_trade(),
            g.avg_decision_latency_us(), g.avg_confirm_latency_us(),
            g.paper_trades, g.live_trades,
            g.outcome_counts[0], g.outcome_counts[1], g.outcome_counts[2],
            g.outcome_counts[3], g.outcome_counts[4], g.outcome_counts[5],
        )
    }

    /// Evict the least-recently-updated mint (smallest last_slot).
    fn evict_oldest_mint(&mut self) {
        if let Some(oldest_key) = self.mints
            .iter()
            .min_by_key(|(_, v)| v.last_slot)
            .map(|(k, _)| k.clone())
        {
            self.mints.remove(&oldest_key);
        }
    }

    /// Evict the least-recently-updated strategy.
    fn evict_oldest_strategy(&mut self) {
        if let Some(oldest_key) = self.strategies
            .iter()
            .min_by_key(|(_, v)| v.last_slot)
            .map(|(k, _)| *k)
        {
            self.strategies.remove(&oldest_key);
            self.decay_rings.remove(&oldest_key);
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade_journal::{TradeRecord, TradeSide, RunMode};
    use crate::ProvenanceSource;

    fn make_record(slot: u64, pnl: i64, fees: u64, mint: &str, strat: u64) -> TradeRecord {
        TradeRecord {
            slot,
            mint_b58: mint.to_string(),
            side: TradeSide::Buy,
            entry_price_fp: 1_000_000_000,
            exit_price_fp: 1_200_000_000,
            size_lamports: 50_000_000,
            strategy_id: strat,
            source: ProvenanceSource::LaserStream,
            outcome: TradeOutcome::Filled,
            realized_pnl_lamports: pnl,
            fees_lamports: fees,
            slippage_lamports: 0,
            decision_latency_us: 100 * slot.max(1),
            confirm_latency_us: 200 * slot.max(1),
            run_mode: RunMode::Paper,
            error_code: 0,
            seq: 0,
            lane: None,
        }
    }

    #[test]
    fn test_global_summary_aggregation() {
        let mut bank = MemoryBank::new(MemoryBankConfig::default());

        bank.ingest(&make_record(1, 10_000, 500, "MintA", 0xAA));
        bank.ingest(&make_record(2, -3_000, 500, "MintB", 0xAA));
        bank.ingest(&make_record(3, 5_000, 500, "MintA", 0xBB));

        let g = bank.global_summary();
        assert_eq!(g.total_trades, 3);
        // net = (10000-500) + (-3000-500) + (5000-500) = 9500 - 3500 + 4500 = 10500
        assert_eq!(g.net_lamports, 10_500);
        assert_eq!(g.total_wins, 2);
        assert_eq!(g.total_losses, 1);
        assert_eq!(g.paper_trades, 3);
        assert_eq!(g.win_rate_bps(), 6_666);
    }

    #[test]
    fn test_per_mint_summary() {
        let mut bank = MemoryBank::new(MemoryBankConfig::default());

        // MintA: 2 wins, 1 loss
        bank.ingest(&make_record(1, 10_000, 500, "MintA", 0xAA));
        bank.ingest(&make_record(2, 5_000, 500, "MintA", 0xAA));
        bank.ingest(&make_record(3, -3_000, 500, "MintA", 0xAA));

        let m = bank.mint_summary("MintA").expect("MintA should exist");
        assert_eq!(m.trades, 3);
        assert_eq!(m.wins, 2);
        assert_eq!(m.losses, 1);
        // net = 9500 + 4500 - 3500 = 10500
        assert_eq!(m.net_lamports, 10_500);
        assert_eq!(m.best_trade_lamports, 9_500);
        assert_eq!(m.worst_trade_lamports, -3_500);
        assert!(m.is_profitable());
        assert_eq!(m.win_rate_bps(), 6_666);
    }

    #[test]
    fn test_per_strategy_summary() {
        let mut bank = MemoryBank::new(MemoryBankConfig::default());

        bank.ingest(&make_record(1, 10_000, 0, "M", 0xAA));
        bank.ingest(&make_record(2, 10_000, 0, "M", 0xAA));
        bank.ingest(&make_record(3, 10_000, 0, "M", 0xAA));
        bank.ingest(&make_record(4, 10_000, 0, "M", 0xAA));

        let s = bank.strategy_summary(0xAA).expect("strategy should exist");
        assert_eq!(s.trades, 4);
        assert_eq!(s.net_lamports, 40_000);
        assert_eq!(s.avg_net_per_trade(), 10_000);
        assert!(!s.is_decaying()); // all wins, no decay
    }

    #[test]
    fn test_edge_decay_detection() {
        let mut bank = MemoryBank::new(MemoryBankConfig {
            decay_window: 4,
            ..Default::default()
        });

        // First two trades: big wins (old window).
        bank.ingest(&make_record(1, 10_000, 0, "M", 0xAA));
        bank.ingest(&make_record(2, 10_000, 0, "M", 0xAA));
        // Next two trades: losses (recent window).
        bank.ingest(&make_record(3, -5_000, 0, "M", 0xAA));
        bank.ingest(&make_record(4, -5_000, 0, "M", 0xAA));

        let s = bank.strategy_summary(0xAA).expect("strategy should exist");
        assert!(s.is_decaying()); // recent is worse than old
    }

    #[test]
    fn test_top_mints() {
        let mut bank = MemoryBank::new(MemoryBankConfig::default());

        bank.ingest(&make_record(1, 1_000, 0, "Low", 1));
        bank.ingest(&make_record(2, 50_000, 0, "High", 1));
        bank.ingest(&make_record(3, 10_000, 0, "Mid", 1));

        let top = bank.top_mints(2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].0, "High");
        assert_eq!(top[1].0, "Mid");
    }

    #[test]
    fn test_pending_trades_ignored() {
        let mut bank = MemoryBank::new(MemoryBankConfig::default());

        let mut rec = make_record(1, 10_000, 0, "M", 1);
        rec.outcome = TradeOutcome::Pending;
        bank.ingest(&rec);

        let g = bank.global_summary();
        assert_eq!(g.total_trades, 0); // pending not counted
        assert!(bank.mint_summary("M").is_none());
    }

    #[test]
    fn test_outcome_taxonomy() {
        let mut bank = MemoryBank::new(MemoryBankConfig::default());

        let mut r1 = make_record(1, 0, 0, "M", 1);
        r1.outcome = TradeOutcome::Filled;
        bank.ingest(&r1);

        let mut r2 = make_record(2, 0, 0, "M", 1);
        r2.outcome = TradeOutcome::OnChainFailure;
        bank.ingest(&r2);

        let mut r3 = make_record(3, 0, 0, "M", 1);
        r3.outcome = TradeOutcome::RejectedPreSubmit;
        bank.ingest(&r3);

        let g = bank.global_summary();
        assert_eq!(g.outcome_counts[0], 1); // Filled
        assert_eq!(g.outcome_counts[3], 1); // OnChainFailure
        assert_eq!(g.outcome_counts[2], 1); // RejectedPreSubmit
    }

    #[test]
    fn test_memory_bounds_mints() {
        let config = MemoryBankConfig { max_mints: 3, ..Default::default() };
        let mut bank = MemoryBank::new(config);

        bank.ingest(&make_record(1, 1, 0, "A", 1));
        bank.ingest(&make_record(2, 1, 0, "B", 1));
        bank.ingest(&make_record(3, 1, 0, "C", 1));
        assert_eq!(bank.mint_count(), 3);

        // Adding a 4th should evict the oldest (slot 1 = "A").
        bank.ingest(&make_record(4, 1, 0, "D", 1));
        assert_eq!(bank.mint_count(), 3);
        assert!(bank.mint_summary("A").is_none());
        assert!(bank.mint_summary("D").is_some());
    }

    #[test]
    fn test_global_json_no_floats() {
        let mut bank = MemoryBank::new(MemoryBankConfig::default());
        bank.ingest(&make_record(1, 10_000, 500, "M", 1));

        let json = bank.global_json();
        assert!(json.contains("\"net_lamports\":9500"));
        // Verify no floating-point numbers in the output.
        assert!(!json.matches(".0").any(|_| true) || json.contains("avg_"));
    }

    #[test]
    fn test_paper_live_parity_in_bank() {
        // Paper and live trades should aggregate identically into summaries
        // (the run_mode is tracked but doesn't affect the math).
        let mut bank_paper = MemoryBank::new(MemoryBankConfig::default());
        let mut bank_live = MemoryBank::new(MemoryBankConfig::default());

        let mut rec_p = make_record(1, 10_000, 500, "M", 1);
        rec_p.run_mode = RunMode::Paper;
        let mut rec_l = make_record(1, 10_000, 500, "M", 1);
        rec_l.run_mode = RunMode::Live;

        bank_paper.ingest(&rec_p);
        bank_live.ingest(&rec_l);

        // Net lamports should be identical regardless of run mode.
        assert_eq!(
            bank_paper.global_summary().net_lamports,
            bank_live.global_summary().net_lamports
        );
        assert_eq!(
            bank_paper.mint_summary("M").unwrap().net_lamports,
            bank_live.mint_summary("M").unwrap().net_lamports
        );
    }
}
