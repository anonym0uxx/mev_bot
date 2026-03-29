//! Engine hot-path: gate → score → position pipeline.
//!
//! Runs on the main thread. Each method processes a single event
//! and drives the full decision pipeline with zero heap allocation
//! in the common (reject) path.

use std::sync::Arc;

use super::gates::GateStack;
use super::health::HealthMonitor;
use super::positions::{ClosedPosition, ExitReason, PositionManager};
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

    // ── Safety: daily loss cap ──────────────────────────────────────
    /// Accumulated daily loss in lamports (positive value = total losses).
    daily_loss_lamports: i64,
    /// UTC day-of-month when `daily_loss_lamports` was last reset (1-31).
    /// Initialized to 0 so the first trade always triggers a reset.
    daily_reset_day: u32,
    /// Daily loss cap in lamports. New entries are rejected when
    /// `daily_loss_lamports >= daily_loss_cap_lamports`.
    daily_loss_cap_lamports: u64,

    // ── Safety: consecutive stop-loss circuit breaker ────────────────
    /// Running count of consecutive stop-loss exits.
    consecutive_stops: u32,
    /// Epoch ms until which new entries are blocked after the breaker fires.
    stop_pause_until_ms: u64,
    /// Config: how many consecutive stops trigger the pause.
    consecutive_stop_pause_count: u32,
    /// Config: how long (ms) to pause after the breaker fires.
    consecutive_stop_pause_ms: u64,

    // ── Feed health monitor ─────────────────────────────────────────
    /// Optional health monitor — checked before opening new positions.
    health_monitor: Option<Arc<HealthMonitor>>,

    // ── ToD size multiplier ─────────────────────────────────────────
    /// UTC hours that get the ToD boost.
    boosted_hours_utc: Vec<u8>,
    /// Multiplier for boosted hours (default 1.25).
    tod_boost_multiplier: f64,
    /// Max entry size in lamports (for capping after ToD multiplier).
    max_entry_size_lamports: u64,
}

impl HotPath {
    pub fn new(
        gate_stack: GateStack,
        scorer: Scorer,
        position_manager: PositionManager,
        now_ms: fn() -> u64,
        paper_mode: bool,
        min_score: f64,
        daily_loss_cap_lamports: u64,
        consecutive_stop_pause_count: u32,
        consecutive_stop_pause_ms: u64,
        boosted_hours_utc: Vec<u8>,
        tod_boost_multiplier: f64,
        max_entry_size_lamports: u64,
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
            daily_loss_lamports: 0,
            daily_reset_day: 0,
            daily_loss_cap_lamports,
            consecutive_stops: 0,
            stop_pause_until_ms: 0,
            consecutive_stop_pause_count,
            consecutive_stop_pause_ms,
            health_monitor: None,
            boosted_hours_utc,
            tod_boost_multiplier,
            max_entry_size_lamports,
        }
    }

    /// Attach a health monitor to the hot path. Must be called before
    /// the engine loop starts processing events.
    pub fn set_health_monitor(&mut self, monitor: Arc<HealthMonitor>) {
        self.health_monitor = Some(monitor);
    }

    /// Returns the time-of-day size multiplier for the given UTC hour.
    #[inline]
    fn get_tod_multiplier(&self, hour_utc: u8) -> f64 {
        if self.boosted_hours_utc.contains(&hour_utc) {
            self.tod_boost_multiplier
        } else {
            1.0
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

        // 3b. Health check: block new entries if feeds are stale
        if let Some(ref hm) = self.health_monitor {
            if !hm.is_trading_allowed() {
                return;
            }
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
        // TASK-8: Use real per-wallet concentration data from MintHistory
        let max_wallet = history.max_wallet_buy_vol_30s();
        let total_vol_30s = history.total_buy_vol_30s();
        let score_components = self.scorer.compute(
            trade.sol_amount,
            trade.vsol_reserves,
            unique_buyers_30s,
            buy_count_1s,
            buy_count_2s,
            volume_sol_5s,
            max_wallet,
            total_vol_30s,
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

        // 8. Safety: daily loss cap
        self.check_and_reset_daily_loss(now);
        if self.daily_loss_lamports as u64 >= self.daily_loss_cap_lamports {
            return;
        }

        // 9. Safety: consecutive stop-loss circuit breaker
        if now < self.stop_pause_until_ms {
            return;
        }

        // 10. Open position
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

    // ── Safety feedback: called by the main loop after draining closed positions ─

    /// Process a closed position for safety tracking (daily loss + circuit breaker).
    /// Called from the main event loop after receiving a `ClosedPosition` from the channel.
    ///
    /// Returns `Some((consecutive_stops, pause_ms))` if the circuit breaker just fired,
    /// `None` otherwise.
    pub fn on_position_closed(&mut self, cp: &ClosedPosition) -> Option<(u32, u64)> {
        // Daily loss: accumulate absolute value of losses (only negative PnL trades)
        if cp.net_pnl_sol < 0 {
            self.daily_loss_lamports += cp.net_pnl_sol.abs();
        }

        // Consecutive stop-loss circuit breaker
        match cp.exit_reason {
            ExitReason::StopLoss => {
                self.consecutive_stops += 1;
                if self.consecutive_stops >= self.consecutive_stop_pause_count {
                    let now = (self.now_ms)();
                    self.stop_pause_until_ms = now + self.consecutive_stop_pause_ms;
                    let stops = self.consecutive_stops;
                    let pause_ms = self.consecutive_stop_pause_ms;
                    // Reset counter after triggering pause (matches TS behavior)
                    self.consecutive_stops = 0;
                    return Some((stops, pause_ms));
                }
            }
            // Any non-SL exit resets the consecutive counter
            _ => {
                self.consecutive_stops = 0;
            }
        }
        None
    }

    /// Check if UTC day changed and reset daily loss counter.
    /// TS uses `new Date().getUTCDate()` (day of month 1-31).
    fn check_and_reset_daily_loss(&mut self, now_ms: u64) {
        // UTC day of month: ms → seconds → divide by 86400 gives day count,
        // but TS uses getUTCDate() which is day-of-month (1-31).
        // We compute: seconds since epoch / 86400 gives a day index,
        // then convert to day-of-month by getting the date.
        // Simpler: (now_ms / 1000) → unix secs, derive UTC day of month.
        let secs = now_ms / 1000;
        // Days since epoch
        let days_since_epoch = secs / 86_400;
        // Convert to a rough UTC day-of-month. We use a well-known formula.
        // For simplicity and correctness: extract UTC day-of-month.
        let utc_day = utc_day_of_month(days_since_epoch);

        if utc_day != self.daily_reset_day {
            self.daily_loss_lamports = 0;
            self.daily_reset_day = utc_day;
        }
    }
}

/// Compute UTC day-of-month (1-31) from days since Unix epoch.
/// Uses civil_from_days algorithm (Howard Hinnant).
fn utc_day_of_month(days_since_epoch: u64) -> u32 {
    let z = days_since_epoch as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    d
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
