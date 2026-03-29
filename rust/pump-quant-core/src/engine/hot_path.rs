//! Engine hot-path: gate → score → position pipeline.
//!
//! Runs on the main thread. Each method processes a single event
//! and drives the full decision pipeline with zero heap allocation
//! in the common (reject) path.
//!
//! LATENCY: Uses quanta::Clock for sub-nanosecond monotonic timestamps
//! calibrated to epoch ms at startup. Avoids clock_gettime syscall (~20ns)
//! on every trade — quanta uses RDTSC (~3-5ns) via TSC.

use std::sync::Arc;

use super::gates::GateStack;
use super::health::HealthMonitor;
use super::positions::{ClosedPosition, ExitReason, PositionManager};
use super::scorer::Scorer;
use crate::core::mint_map::MintHistoryMap;
use crate::core::trade_record::TradeRecord;
use crate::feeds::{FeedSource, PreWarmEvent, TokenCreatedEvent, TradeEvent};
use super::gates::GateRejectReason;
use super::regime;

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
    /// LATENCY: quanta::Clock uses RDTSC (~3ns) instead of clock_gettime (~20ns).
    /// `start_instant` + `start_epoch_ms` are calibrated once at construction.
    /// `now_ms()` = start_epoch_ms + elapsed_since(start_instant).as_millis()
    clock: quanta::Clock,
    start_instant: quanta::Instant,
    start_epoch_ms: u64,
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

    // ── Regime exclusion set ──────────────────────────────────────────
    // Mints flagged as excluded (mayhem, tokenized agent) on creation.
    // Checked before gate evaluation to short-circuit early.
    excluded_mints: hashbrown::HashSet<[u8; 32]>,

    // ── Entry randomizer ────────────────────────────────────────────
    pub entry_randomizer: super::entry_randomizer::EntryRandomizer,

    // ── Helius lead-time tracking ───────────────────────────────────
    // Fixed-size ring buffer of (sig_prefix, helius_timestamp_ms) from PreWarm events.
    // When a PumpPortal trade arrives, we check if Helius already saw the same sig_prefix.
    // This measures Helius lead time and enables future Helius-as-primary-trigger.
    //
    // ARCHITECTURE NOTE: Helius `logsSubscribe` does NOT provide accountKeys (mint address).
    // PreWarm events arrive with mint=[0u8;32], so we CANNOT use them for entry triggers.
    // To enable Helius-as-primary-trigger, we need one of:
    //   1. accountSubscribe on pump.fun bonding curves → full account data with reserves
    //   2. LaserStream Preprocessed Transactions → full decoded tx with all accounts
    //   3. Helius Enhanced Transactions API → adds HTTP latency, defeats purpose
    // Until then, Helius stays as pre-warmer + lead-time measurement only.
    helius_sig_ring: [(u64, u64); 256], // (sig_prefix_u64, timestamp_ms) ring buffer
    helius_sig_ring_head: u8, // wraps at 256
    pub helius_lead_sum_ms: u64,
    pub helius_lead_count: u64,

    // ── Gate rejection histogram ────────────────────────────────────
    // Fixed-size array indexed by GateRejectReason discriminant.
    // 32 slots covers all current variants with headroom.
    pub gate_reject_counts: [u64; 32],
}

impl HotPath {
    pub fn new(
        gate_stack: GateStack,
        scorer: Scorer,
        position_manager: PositionManager,
        _now_ms: fn() -> u64, // LATENCY: kept for API compat, replaced by quanta internally
        paper_mode: bool,
        min_score: f64,
        daily_loss_cap_lamports: u64,
        consecutive_stop_pause_count: u32,
        consecutive_stop_pause_ms: u64,
        boosted_hours_utc: Vec<u8>,
        tod_boost_multiplier: f64,
        max_entry_size_lamports: u64,
        randomizer_config: super::entry_randomizer::RandomizerConfig,
    ) -> Self {
        // LATENCY: Calibrate quanta clock against SystemTime once at construction.
        // quanta::Clock::now() returns a calibrated Instant (nanoseconds).
        // We record start_instant + start_epoch_ms, then compute:
        //   now_epoch_ms = start_epoch_ms + (clock.now() - start_instant).as_millis()
        // This replaces clock_gettime (~20ns) with RDTSC (~3-5ns) on every trade.
        let clock = quanta::Clock::new();
        let start_instant = clock.now();
        let start_epoch_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            gate_stack,
            scorer,
            position_manager,
            mint_map: MintHistoryMap::with_capacity(4096),
            clock,
            start_instant,
            start_epoch_ms,
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
            excluded_mints: hashbrown::HashSet::with_capacity(256),
            entry_randomizer: super::entry_randomizer::EntryRandomizer::new(randomizer_config),
            helius_sig_ring: [(0u64, 0u64); 256],
            helius_sig_ring_head: 0,
            helius_lead_sum_ms: 0,
            helius_lead_count: 0,
            gate_reject_counts: [0u64; 32],
        }
    }

    /// Attach a health monitor to the hot path. Must be called before
    /// the engine loop starts processing events.
    pub fn set_health_monitor(&mut self, monitor: Arc<HealthMonitor>) {
        self.health_monitor = Some(monitor);
    }

    /// LATENCY: Fast epoch-ms via quanta RDTSC (~3-5ns) instead of
    /// clock_gettime syscall (~20ns). Calibrated once at construction.
    #[inline(always)]
    fn now_ms(&self) -> u64 {
        let elapsed = self.clock.now().duration_since(self.start_instant);
        self.start_epoch_ms + elapsed.as_millis() as u64
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
        let now = self.now_ms();

        // Helius lead-time measurement: check if Helius pre-warmed this sig
        if trade.source == FeedSource::PumpPortal {
            self.check_helius_lead(trade);
        }

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

        // 3a. Regime exclusion: skip mints flagged as mayhem/tokenized agent
        if self.excluded_mints.contains(&trade.mint) {
            self.stats.gate_rejects += 1;
            let idx = gate_reject_index(&GateRejectReason::RegimeExcluded);
            if idx < 32 { self.gate_reject_counts[idx] += 1; }
            return;
        }

        // 3b. Regime: graduation boundary check (vToken-based)
        // Compute bonding curve progress from vToken reserves
        if trade.vtoken_reserves > 0 {
            let progress = regime::compute_bonding_curve_progress(
                trade.vtoken_reserves,
                regime::INITIAL_VIRTUAL_TOKENS,
            );
            let rc = &self.gate_stack.config.regime_config;
            if progress >= 0.0
                && progress >= rc.graduation_boundary_start
                && progress <= rc.graduation_boundary_end
            {
                self.stats.gate_rejects += 1;
                let idx = gate_reject_index(&GateRejectReason::GraduationBoundary);
                if idx < 32 { self.gate_reject_counts[idx] += 1; }
                return;
            }
        }

        // 3c. Health check: block new entries if feeds are stale
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
            Err(reason) => {
                self.stats.gate_rejects += 1;
                let idx = gate_reject_index(&reason);
                if idx < 32 {
                    self.gate_reject_counts[idx] += 1;
                }
                // Log gate rejection breakdown every 100 rejects
                if self.stats.gate_rejects % 100 == 0 {
                    self.log_gate_rejections();
                }
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

    /// Process a token creation event. Checks regime exclusion flags and
    /// stores excluded mints for fast rejection in on_trade().
    pub fn on_token_created(&mut self, event: &TokenCreatedEvent) {
        if event.is_mayhem || event.is_tokenized_agent {
            self.excluded_mints.insert(event.mint);
            tracing::debug!(
                mint = %bs58::encode(&event.mint).into_string(),
                mayhem = event.is_mayhem,
                agent = event.is_tokenized_agent,
                "Token excluded by regime classifier"
            );
        }
    }

    /// Process a pre-warm event (Helius/ShredStream). Adds to mint history
    /// without triggering gate/score evaluation.
    ///
    /// When source is Helius, also records the sig_prefix + timestamp in a
    /// ring buffer for lead-time measurement. When PumpPortal confirms the
    /// same sig, we can measure how much earlier Helius saw it.
    ///
    /// HELIUS TRIGGER STATUS: BLOCKED — Helius logsSubscribe does not provide
    /// accountKeys (mint address). PreWarm events arrive with mint=[0u8;32].
    /// Cannot look up MintHistory, cannot run gates, cannot open positions.
    /// See helius_sig_ring doc comment for what's needed to unblock.
    pub fn on_prewarm(&mut self, prewarm: &PreWarmEvent) {
        self.stats.prewarms += 1;
        let now = self.now_ms();

        // ── Helius lead-time tracking: store sig_prefix for correlation ──
        if prewarm.source == FeedSource::Helius {
            let sig_prefix_u64 = u64::from_le_bytes({
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&prewarm.sig[..8]);
                buf
            });
            let idx = self.helius_sig_ring_head as usize;
            self.helius_sig_ring[idx] = (sig_prefix_u64, prewarm.timestamp_ms);
            self.helius_sig_ring_head = self.helius_sig_ring_head.wrapping_add(1);
        }

        // If mint is zero (Helius — no accountKeys in logsSubscribe), skip MintMap insert.
        // We still recorded the sig_prefix above for correlation measurement.
        if prewarm.mint == [0u8; 32] {
            return;
        }

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

    /// Check if a PumpPortal trade's sig was already seen by Helius.
    /// Returns `Some(lead_ms)` if Helius saw it first, `None` otherwise.
    /// Scans the 256-entry ring buffer (trivial cost — fits in 4KB, 1 cache line per 4 entries).
    #[inline]
    fn check_helius_lead(&mut self, trade: &TradeEvent) -> Option<u64> {
        let trade_sig_u64 = u64::from_le_bytes(trade.sig_prefix);
        for &(sig_u64, helius_ts) in &self.helius_sig_ring {
            if sig_u64 == trade_sig_u64 && helius_ts > 0 {
                let lead_ms = trade.timestamp_ms.saturating_sub(helius_ts);
                // Only count if Helius was actually earlier and within reasonable window
                if lead_ms > 0 && lead_ms < 5_000 {
                    self.helius_lead_sum_ms += lead_ms;
                    self.helius_lead_count += 1;
                    return Some(lead_ms);
                }
            }
        }
        None
    }

    /// 50ms tick: drive position manager time-based exits
    /// (max_hold, momentum decay).
    /// Log gate rejection histogram. Called every 500 rejects.
    fn log_gate_rejections(&self) {
        let names = [
            "BlockedHour", "NotBuy", "TriggerTooSmall", "TriggerTooLarge",
            "VSolOutOfRange", "TokenTooOld", "NotEnoughUniqueBuyers", "LargeTriggerLowBuyers",
            "StaleGap", "InsufficientCrowd2s", "InsufficientCrowd5s", "InsufficientVSolAccel",
            "StaleMomentum1s", "InsufficientSellCount", "VSolDeltaTooHigh", "CreatorSellRecent",
            "SellPressure", "TriggerTooIsolated", "ScoreTooLow", "SourceBlocked",
            "RegimeExcluded", "GraduationBoundary",
        ];
        // Find top 5 rejection reasons
        let mut indexed: Vec<(u64, usize)> = self.gate_reject_counts.iter()
            .enumerate()
            .map(|(i, &c)| (c, i))
            .filter(|(c, _)| *c > 0)
            .collect();
        indexed.sort_by(|a, b| b.0.cmp(&a.0));
        let top: Vec<String> = indexed.iter().take(5).map(|(count, idx)| {
            let name = if *idx < names.len() { names[*idx] } else { "Unknown" };
            format!("{}={}", name, count)
        }).collect();
        tracing::info!(
            total_rejects = self.stats.gate_rejects,
            "gate rejections: {}", top.join(", ")
        );
    }

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
                    let now = self.now_ms();
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

/// Map GateRejectReason to a stable index for the rejection histogram.
fn gate_reject_index(reason: &GateRejectReason) -> usize {
    match reason {
        GateRejectReason::BlockedHour => 0,
        GateRejectReason::NotBuy => 1,
        GateRejectReason::TriggerTooSmall => 2,
        GateRejectReason::TriggerTooLarge => 3,
        GateRejectReason::VSolOutOfRange => 4,
        GateRejectReason::TokenTooOld => 5,
        GateRejectReason::NotEnoughUniqueBuyers => 6,
        GateRejectReason::LargeTriggerLowBuyers => 7,
        GateRejectReason::StaleGap => 8,
        GateRejectReason::InsufficientCrowd2s => 9,
        GateRejectReason::InsufficientCrowd5s => 10,
        GateRejectReason::InsufficientVSolAccel => 11,
        GateRejectReason::StaleMomentum1s => 12,
        GateRejectReason::InsufficientSellCount => 13,
        GateRejectReason::VSolDeltaTooHigh => 14,
        GateRejectReason::CreatorSellRecent => 15,
        GateRejectReason::SellPressure => 16,
        GateRejectReason::TriggerTooIsolated => 17,
        GateRejectReason::ScoreTooLow(_) => 18,
        GateRejectReason::SourceBlocked => 19,
        GateRejectReason::RegimeExcluded => 20,
        GateRejectReason::GraduationBoundary => 21,
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
