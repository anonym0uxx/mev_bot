//! Engine hot-path: gate → score → position pipeline.
//!
//! Runs on the main thread. Each method processes a single event
//! and drives the full decision pipeline with zero heap allocation
//! in the common (reject) path.

use super::gates::GateStack;
use super::positions::PositionManager;
use super::scorer::Scorer;
use crate::core::mint_map::MintHistoryMap;
use crate::core::trade_record::TradeRecord;
use crate::feeds::{PreWarmEvent, TradeEvent};

/// Statistics counters — exposed for periodic logging.
pub struct HotPathStats {
    pub trades_seen: u64,
    pub gates_passed: u64,
    pub positions_opened: u64,
    pub gate_rejects: u64,
    pub score_rejects: u64,
    pub prewarms: u64,
    pub ticks: u64,
    pub creator_sells: u64,
}

pub struct HotPath {
    gate_stack: GateStack,
    scorer: Scorer,
    position_manager: PositionManager,
    mint_map: MintHistoryMap,
    now_ms: fn() -> u64,
    #[allow(dead_code)]
    paper_mode: bool,
    min_score: f64,
    pub stats: HotPathStats,
}

impl HotPath {
    pub fn new(
        gate_stack: GateStack,
        scorer: Scorer,
        position_manager: PositionManager,
        now_ms: fn() -> u64,
        paper_mode: bool,
        min_score: f64,
    ) -> Self {
        Self {
            gate_stack,
            scorer,
            position_manager,
            mint_map: MintHistoryMap::with_capacity(4096),
            now_ms,
            paper_mode,
            min_score,
            stats: HotPathStats {
                trades_seen: 0,
                gates_passed: 0,
                positions_opened: 0,
                gate_rejects: 0,
                score_rejects: 0,
                prewarms: 0,
                ticks: 0,
                creator_sells: 0,
            },
        }
    }

    /// Process a trade event through the full gate → score → position pipeline.
    #[inline]
    pub fn on_trade(&mut self, trade: &TradeEvent) {
        self.stats.trades_seen += 1;
        let now = (self.now_ms)();

        // 1. Push into mint_map (updates cached aggregates)
        let record = trade_to_record(trade, now);
        let history = self.mint_map.get_or_insert(&trade.mint, now);
        history.push(record, now);

        // 2. If we already have a position for this mint → on_subsequent_trade for exit logic
        if self.position_manager.has_position(&trade.mint) {
            self.position_manager.on_subsequent_trade(trade, now);
            return;
        }

        // 3. Only consider buys for new position entry
        if !trade.is_buy {
            return;
        }

        // 4. Extract cached aggregates from history
        let history = self.mint_map.get(&trade.mint).unwrap();
        let history_age_ms = now.saturating_sub(history.first_seen_ms);
        let unique_buyers_30s = history.cached_unique_buyers_30s;
        let buy_count_1s = history.cached_buy_count_1s;
        let buy_count_2s = history.cached_buy_count_2s;
        let buy_count_5s = history.cached_buy_count_5s;
        let sell_count_5s = history.cached_sell_count_5s;
        let volume_sol_5s = history.cached_volume_sol_5s;
        let creator_sell_at_ms = history.creator_sell_at_ms;

        // vSol delta 3s: current vsol - oldest vsol in 3s window
        let vsol_delta_3s = trade
            .vsol_reserves
            .saturating_sub(history.cached_vsol_oldest_3s);

        // Time since last buy (approximate: now - last_trade_ms)
        let time_since_last_buy_ms = if history.last_trade_ms > 0 {
            now.saturating_sub(history.last_trade_ms)
        } else {
            0
        };

        // 5. Compute score first (needed by gate stack as last gate)
        let score_components = self.scorer.compute(
            trade.sol_amount,
            trade.vsol_reserves,
            unique_buyers_30s,
            buy_count_1s,
            buy_count_2s,
            volume_sol_5s,
            0, // max_wallet_volume_lamports — not tracked per-wallet in hot path
            volume_sol_5s.saturating_mul(6), // estimate 30s vol from 5s window
        );
        let score = score_components.final_score;

        // 6. Run gate stack (score is last gate)
        match self.gate_stack.evaluate(
            trade,
            history_age_ms,
            unique_buyers_30s,
            buy_count_1s,
            buy_count_2s,
            buy_count_5s,
            sell_count_5s,
            volume_sol_5s,
            vsol_delta_3s,
            time_since_last_buy_ms,
            creator_sell_at_ms,
            now,
            score,
        ) {
            Ok(()) => {
                self.stats.gates_passed += 1;
            }
            Err(_reason) => {
                self.stats.gate_rejects += 1;
                return;
            }
        }

        // 7. Score threshold check (redundant with gate 17, but explicit)
        if score < self.min_score {
            self.stats.score_rejects += 1;
            return;
        }

        // 8. Open position
        self.position_manager.open_position(trade, score, now);
        self.stats.positions_opened += 1;
    }

    /// Process a pre-warm event (Helius). Adds to mint history without
    /// triggering gate/score evaluation.
    pub fn on_prewarm(&mut self, prewarm: &PreWarmEvent) {
        self.stats.prewarms += 1;
        let now = (self.now_ms)();

        let record = TradeRecord {
            timestamp_ms: prewarm.timestamp_ms,
            sol_amount: prewarm.sol_amount,
            token_amount: 0,
            is_buy: prewarm.is_buy,
            _pad0: [0; 7],
            trader: prewarm.trader,
            vsol_reserves: 0,
            vtoken_reserves: 0,
            market_cap_sol: 0,
            slot: 0,
            sig_prefix: {
                let mut p = [0u8; 8];
                p.copy_from_slice(&prewarm.sig[..8]);
                p
            },
            _pad1: [0; 24],
        };

        let history = self.mint_map.get_or_insert(&prewarm.mint, now);
        history.add_trade_to_history(record, now);
    }

    /// 50ms tick: drive position manager time-based exits
    /// (max_hold, momentum decay).
    pub fn on_tick(&mut self, ts_ms: u64) {
        self.stats.ticks += 1;
        self.position_manager.on_tick(ts_ms);

        // Periodically evict stale mints (every 10s)
        if self.stats.ticks % 200 == 0 {
            self.mint_map.evict_stale(ts_ms, 120_000);
        }
    }

    /// Mark a creator-sell event on the mint's history.
    pub fn on_creator_sell(&mut self, mint: &[u8; 32], ts_ms: u64) {
        self.stats.creator_sells += 1;
        if let Some(history) = self.mint_map.get_mut(mint) {
            history.creator_sell_at_ms = ts_ms;
        }
    }

    /// Number of currently open positions.
    pub fn open_positions(&self) -> usize {
        self.position_manager.open_count()
    }

    /// Force close all positions (e.g. on shutdown).
    pub fn close_all(&mut self, now_ms: u64) {
        self.position_manager.close_all(now_ms);
    }
}

/// Convert a TradeEvent into a TradeRecord for the ring buffer.
#[inline]
fn trade_to_record(trade: &TradeEvent, now_ms: u64) -> TradeRecord {
    TradeRecord {
        timestamp_ms: now_ms,
        sol_amount: trade.sol_amount,
        token_amount: trade.token_amount,
        is_buy: trade.is_buy,
        _pad0: [0; 7],
        trader: trade.trader,
        vsol_reserves: trade.vsol_reserves,
        vtoken_reserves: trade.vtoken_reserves,
        market_cap_sol: trade.market_cap_sol,
        slot: trade.slot,
        sig_prefix: trade.sig_prefix,
        _pad1: [0; 24],
    }
}
