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

use super::entry_engine::{EntryEngine, EntryInput, EntryAction};
use super::health::HealthMonitor;
use super::positions::{ClosedPosition, ExitReason, PositionManager};
use super::watchlist::Watchlist;
use crate::engine::kelly_sizing::{EntryConviction, BankrollSource, PaperBankroll};

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
    pub migrations: u64,
    pub lp_removals: u64,
}

pub struct HotPath {
    /// V2 entry engine — the ONLY entry path. Kelly-tiered sizing + magnitude prediction.
    entry_engine: EntryEngine,
    /// Risk manager — always active.
    risk_manager: crate::engine::risk_manager::RiskManager,
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

    pub stats: HotPathStats,

    // ── Bankroll management (Kelly sizing) ──────────────────────────
    pub bankroll: BankrollSource,

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
    /// Precomputed bitmask: bit N set = hour N boosted. Eliminates Vec scan.
    boosted_hours_bitmask: u32,
    /// Multiplier for boosted hours (default 1.25).
    tod_boost_multiplier: f64,
    /// Max entry size in lamports (for capping after ToD multiplier).
    max_entry_size_lamports: u64,

    // ── Regime exclusion set ──────────────────────────────────────────
    // Mints flagged as excluded (mayhem, tokenized agent) on creation.
    // Checked before gate evaluation to short-circuit early.
    excluded_mints: hashbrown::HashSet<[u8; 32]>,

    // (risk_manager moved to struct top — always present)

    // ── Two-phase entry watchlist ──────────────────────────────────
    /// Instead of immediate position open, tokens go to watchlist first.
    /// Capital committed only on confirming buy. Eliminates ~62% dead entries.
    watchlist: Watchlist,

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

    // ── ShredStream dedup ring ──────────────────────────────────────
    // When ShredStream emits a Trade event that triggers an entry, we record
    // the sig_prefix here. When PumpPortal later confirms the same trade,
    // we skip re-triggering and instead enrich the existing position with
    // PumpPortal's vSOL data.
    shred_sig_ring: [(u64, u64); 128], // (sig_prefix_u64, timestamp_ms)
    shred_sig_ring_head: u8,           // wraps at 128

    // ── Gate rejection histogram ────────────────────────────────────
    // Fixed-size array indexed by GateRejectReason discriminant.
    // 32 slots covers all current variants with headroom.
    pub gate_reject_counts: [u64; 32],
}

impl HotPath {
    pub fn new(
        entry_engine: EntryEngine,
        risk_manager: crate::engine::risk_manager::RiskManager,
        position_manager: PositionManager,
        _now_ms: fn() -> u64, // LATENCY: kept for API compat, replaced by quanta internally
        paper_mode: bool,
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

        // Precompute boosted hours bitmask (eliminates Vec scan in hot path)
        let boosted_hours_bitmask = boosted_hours_utc.iter().fold(0u32, |acc, &h| acc | (1u32 << h));

        Self {
            entry_engine,
            risk_manager,
            position_manager,
            mint_map: MintHistoryMap::with_capacity(4096),
            clock,
            start_instant,
            start_epoch_ms,
            paper_mode,
            stats: HotPathStats {
                trades_seen: 0,
                gates_passed: 0,
                positions_opened: 0,
                gate_rejects: 0,
                score_rejects: 0,
                prewarms: 0,
                ticks: 0,
                creator_sells: 0,
                migrations: 0,
                lp_removals: 0,
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
            boosted_hours_bitmask,
            tod_boost_multiplier,
            max_entry_size_lamports,
            excluded_mints: hashbrown::HashSet::with_capacity(256),
            bankroll: if paper_mode {
                BankrollSource::Paper(PaperBankroll::new(5_000_000_000)) // 5 SOL default
            } else {
                BankrollSource::Paper(PaperBankroll::new(5_000_000_000)) // TODO: Live RPC bankroll
            },
            watchlist: Watchlist::new(),
            entry_randomizer: super::entry_randomizer::EntryRandomizer::new(randomizer_config),
            helius_sig_ring: [(0u64, 0u64); 256],
            helius_sig_ring_head: 0,
            helius_lead_sum_ms: 0,
            helius_lead_count: 0,
            shred_sig_ring: [(0u64, 0u64); 128],
            shred_sig_ring_head: 0,
            gate_reject_counts: [0u64; 32],
        }
    }

    /// Attach a health monitor to the hot path. Must be called before
    /// the engine loop starts processing events.
    pub fn set_health_monitor(&mut self, monitor: Arc<HealthMonitor>) {
        self.health_monitor = Some(monitor);
    }



    /// Check if a PumpPortal trade's sig was already seen by Helius.
    /// Returns true if Helius already delivered this trade (dedup: skip α/β update).
    ///
    /// Only runs for PumpPortal-source trades (Helius typically leads by <1s).
    /// Linear scan of 256-entry ring (4KB, fits L1, ~128 entries avg scan).
    /// PERF: ~200ns worst-case, only for PP trades with existing positions.
    #[inline(always)]
    fn is_deduped_trade(&self, trade: &TradeEvent) -> bool {
        if trade.source != FeedSource::PumpPortal {
            return false;
        }
        let sig_u64 = u64::from_le_bytes(trade.sig_prefix);
        for &(stored_sig, stored_ts) in &self.helius_sig_ring {
            if stored_sig == sig_u64 && stored_ts > 0 {
                return true;
            }
        }
        false
    }

    /// LATENCY: Fast epoch-ms via quanta RDTSC (~3-5ns) instead of
    /// clock_gettime syscall (~20ns). Calibrated once at construction.
    #[inline(always)]
    fn now_ms(&self) -> u64 {
        let elapsed = self.clock.now().duration_since(self.start_instant);
        self.start_epoch_ms + elapsed.as_millis() as u64
    }

    /// Returns the time-of-day size multiplier for the given UTC hour.
    /// Uses precomputed bitmask (~1ns) instead of Vec::contains scan (~15ns).
    #[inline(always)]
    fn get_tod_multiplier(&self, hour_utc: u8) -> f64 {
        if (self.boosted_hours_bitmask >> hour_utc) & 1 == 1 {
            self.tod_boost_multiplier
        } else {
            1.0
        }
    }

    /// Process a trade event through the full gate → score → position pipeline.
    /// PERF: #[inline(always)] — this is THE hot path (~1000+ calls/sec).
    /// Must stay in the instruction cache. The method is ~60 instructions
    /// (mostly comparisons and loads), well within icache budget.
    #[inline(always)]
    pub fn on_trade(&mut self, trade: &TradeEvent) {
        self.stats.trades_seen += 1;
        let now = self.now_ms();

        // ── ShredStream→PumpPortal dedup ────────────────────────────
        // If ShredStream already triggered this trade (sig_prefix match within 200ms),
        // don't re-trigger entry. Instead, enrich existing position with PumpPortal's
        // vSOL reserves data (which ShredStream doesn't provide).
        if trade.source == FeedSource::PumpPortal {
            let sig_u64 = u64::from_le_bytes(trade.sig_prefix);
            for &(stored_sig, stored_ts) in &self.shred_sig_ring {
                if stored_sig == sig_u64 && stored_ts > 0
                    && now.saturating_sub(stored_ts) < 200
                {
                    // ShredStream already processed this trade.
                    // Enrich existing position's vSOL if we have one open.
                    if trade.vsol_reserves > 0 {
                        if let Some(pos) = self.position_manager.get_position_mut(&trade.mint) {
                            // Update cached vSOL reserves for accurate trail stop tracking
                            pos.current_vsol = trade.vsol_reserves;
                            if trade.vsol_reserves > pos.peak_vsol {
                                pos.peak_vsol = trade.vsol_reserves;
                            }
                        }
                    }
                    return; // Skip re-triggering — ShredStream already handled it
                }
            }
        }

        // NOTE: ShredStream sig dedup recording moved to AFTER watchlist.watch()
        // so that ShredStream trades that fail hard_gate (e.g. vsol=0) don't
        // block PumpPortal from getting its chance to trigger entry.

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
            // Helius dedup: if Helius already delivered this sig, PumpPortal
            // confirmation skips α/β update but still enriches reserves.
            let deduped = self.is_deduped_trade(trade);
            self.position_manager.on_subsequent_trade(trade, now, deduped);
            return;
        }

        // 2b. TWO-PHASE ENTRY: Check if this trade confirms a watched mint.
        //     This runs before the entry engine — confirming buys promote
        //     watched tokens to real positions without re-evaluation.
        if trade.is_buy {
            if let Some(promoted) = self.watchlist.try_promote(trade, now) {
                // Safety checks (same as entry path)
                if !self.paper_mode {
                    self.check_and_reset_daily_loss(now);
                    if self.daily_loss_lamports as u64 >= self.daily_loss_cap_lamports {
                        return;
                    }
                    if now < self.stop_pause_until_ms {
                        return;
                    }
                }
                if promoted.conviction.size_lamports == 0 {
                    return;
                }
                self.position_manager.open_position(
                    trade,
                    promoted.score,
                    now,
                    promoted.magnitude,
                    promoted.conviction.size_lamports,
                    promoted.conviction,
                );
                self.stats.positions_opened += 1;
                self.stats.gates_passed += 1;
                // Feed the confirming buy into RideState's Bayesian model.
                // open_position sets trigger_sig = this trade's sig, so
                // on_subsequent_trade would skip it. Inject evidence directly.
                self.position_manager.feed_initial_buy(&trade.mint, trade.sol_amount, now, &trade.sig);
                // Enrich with entry context from cached mint history
                if let Some(pos) = self.position_manager.get_position_mut(&trade.mint) {
                    let history = self.mint_map.get_or_insert(&trade.mint, now);
                    pos.pre_trigger_buys_1s = history.cached_buy_count_1s;
                    pos.pre_trigger_buys_2s = history.cached_buy_count_2s;
                    pos.pre_trigger_buys_5s = history.cached_buy_count_5s;
                    pos.volume_5s = history.cached_volume_sol_5s;
                    pos.sell_count_5s = history.cached_sell_count_5s;
                    pos.unique_buyers = history.cached_unique_buyers_30s;
                }
                return;
            }
        }

        // 3. Only consider buys for new watchlist entry
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
            // Graduation boundary: reject tokens near graduation (>95% curve)
            // Hardcoded thresholds from old regime_config (was 0.95..1.0)
            if progress >= 0.95 && progress <= 1.0
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

        // ═══ ENTRY ENGINE (V2 — Kelly + Magnitude) ═══
        {
            let engine = &self.entry_engine;
            // Risk manager gate (bypassed in paper mode for full data collection)
            if !self.paper_mode && !self.risk_manager.allows_entry(now, false) {
                self.stats.gate_rejects += 1;
                return;
            }
            let max_wallet = history.max_wallet_buy_vol_30s();
            let total_vol_30s = history.total_buy_vol_30s();
            let input = EntryInput {
                vsol_reserves: trade.vsol_reserves,
                vtoken_reserves: trade.vtoken_reserves,
                sol_amount: trade.sol_amount,
                buy_count_1s,
                buy_count_2s,
                buy_count_5s,
                sell_count_5s,
                unique_buyers_30s,
                _pad: 0,
                volume_sol_5s,
                vsol_delta_3s,
                time_since_last_buy_ms,
                history_age_ms,
                creator_sell_at_ms,
                now_ms: now,
                max_wallet_vol_30s: max_wallet,
                total_buy_vol_30s: total_vol_30s,
            };
            // Kelly bankroll params
            let wallet_balance = self.bankroll.balance();
            let n_open = self.position_manager.open_count() as u8;
            let drawdown_pct = self.bankroll.drawdown_pct();
            let decision = engine.evaluate(&input, wallet_balance, n_open, drawdown_pct);
            match decision.action {
                EntryAction::Reject => {
                    self.stats.gate_rejects += 1;
                    return;
                }
                EntryAction::Ride => {
                    self.stats.gates_passed += 1;
                    // Skip if Kelly says size=0 (bankroll exhausted)
                    if decision.conviction.size_lamports == 0 {
                        self.stats.score_rejects += 1;
                        return;
                    }
                    // TWO-PHASE ENTRY: Add to watchlist instead of opening immediately.
                    // Position will be opened when a confirming buy arrives (see step 2b).
                    self.watchlist.watch(
                        trade,
                        decision.score,
                        decision.magnitude,
                        &decision.conviction,
                        now,
                    );
                    // Record ShredStream sig for dedup ONLY after entry engine accepted.
                    // This ensures ShredStream trades that fail hard_gate (e.g. vsol=0)
                    // don't block PumpPortal from triggering the same trade.
                    if trade.source == FeedSource::ShredStream {
                        let sig_u64 = u64::from_le_bytes(trade.sig_prefix);
                        let idx = (self.shred_sig_ring_head as usize) % 128;
                        self.shred_sig_ring[idx] = (sig_u64, now);
                        self.shred_sig_ring_head = self.shred_sig_ring_head.wrapping_add(1);
                    }
                    return;
                }
            }
        }
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
    /// PERF: #[inline(never)] — cold path, called rarely (~every 500 rejects).
    /// Keep out of icache to avoid polluting hot-path instruction fetch.
    #[inline(never)]
    fn log_gate_rejections(&self) {
        let names = [
            "BlockedHour", "NotBuy", "TriggerTooSmall", "TriggerTooLarge",
            "VSolOutOfRange", "TokenTooOld", "NotEnoughUniqueBuyers", "LargeTriggerLowBuyers",
            "StaleGap", "InsufficientCrowd2s", "InsufficientCrowd5s", "InsufficientVSolAccel",
            "StaleMomentum1s", "InsufficientSellCount", "VSolDeltaTooHigh", "CreatorSellRecent",
            "SellPressure", "TriggerTooIsolated", "ScoreTooLow", "SourceBlocked",
            "RegimeExcluded", "GraduationBoundary", "MaxCurveProgress",
            "LowFlowConcentration", "TooManyBuyers",
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

        // Expire stale watchlist entries every tick (~50ms)
        self.watchlist.expire_stale(ts_ms);

        // Periodically evict stale mints (every 10s)
        if self.stats.ticks % 200 == 0 {
            self.mint_map.evict_stale(ts_ms, 120_000);
        }
    }

    /// Watchlist stats for API/logging.
    pub fn watchlist_stats(&self) -> (u32, u64, u64, u64, u64) {
        (
            self.watchlist.active_count(),
            self.watchlist.watches_added,
            self.watchlist.watches_promoted,
            self.watchlist.watches_expired,
            self.watchlist.watches_evicted,
        )
    }

    /// Mark a creator-sell event on the mint's history.
    ///
    /// If we hold a position for this mint, also:
    ///   1. Flag RideState for immediate exit (CREATOR_SELL flag)
    ///   2. Inject heavy β evidence via on_sell_event (CREATOR_SELL_WEIGHT=50)
    ///
    /// Called from CoreCast feed — cold path (~rare event), not hot path.
    pub fn on_creator_sell(&mut self, mint: &[u8; 32], ts_ms: u64) {
        self.stats.creator_sells += 1;
        if let Some(history) = self.mint_map.get_mut(mint) {
            history.creator_sell_at_ms = ts_ms;
        }
        // Remove from watchlist if being watched (don't enter after creator sell)
        self.watchlist.remove_mint(mint);
        // If we have an open position, mark creator sell + inject β evidence.
        self.position_manager.on_creator_sell_evidence(mint, ts_ms);
    }

    /// Force-exit any open position for a migrated token.
    /// Token migrated to Raydium AMM — bonding curve positions are invalidated.
    /// Uses `ExitReason::MaxHold` as the closest semantic match (positions.rs is read-only).
    /// PERF: #[inline(never)] — cold path (10-30 migrations/day).
    #[inline(never)]
    pub fn on_migration(&mut self, mint: &[u8; 32], ts_ms: u64) {
        self.stats.migrations += 1;
        if self.position_manager.has_position(mint) {
            tracing::info!(
                mint = %bs58::encode(mint).into_string(),
                ts_ms,
                "migration detected — force-closing position"
            );
            self.position_manager.force_close(mint, ExitReason::MaxHold, ts_ms);
        } else {
            tracing::debug!(
                mint = %bs58::encode(mint).into_string(),
                "migration: no open position"
            );
        }
        // Also mark creator_sell_at_ms so the gate rejects future entries for this mint
        if let Some(history) = self.mint_map.get_mut(mint) {
            history.creator_sell_at_ms = ts_ms;
        }
    }

    /// Force-exit any open position on LP removal (rug detection).
    /// Delegates to `on_migration` — same force-exit logic.
    /// PERF: #[inline(never)] — cold path, rare event.
    #[inline(never)]
    pub fn on_lp_removal(&mut self, mint: &[u8; 32], ts_ms: u64) {
        self.stats.lp_removals += 1;
        if self.position_manager.has_position(mint) {
            tracing::info!(
                mint = %bs58::encode(mint).into_string(),
                ts_ms,
                "LP removal detected — force-closing position"
            );
            self.position_manager.force_close(mint, ExitReason::MaxHold, ts_ms);
        } else {
            tracing::debug!(
                mint = %bs58::encode(mint).into_string(),
                "LP removal: no open position"
            );
        }
        // Also mark creator_sell so gates reject
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

        // Consecutive stop-loss circuit breaker (disabled in paper mode)
        if self.paper_mode {
            return None;
        }
        match cp.exit_reason {
            ExitReason::StopLoss => {
                self.consecutive_stops += 1;
                if self.consecutive_stops >= self.consecutive_stop_pause_count {
                    let now = self.now_ms();
                    self.stop_pause_until_ms = now + self.consecutive_stop_pause_ms;
                    let stops = self.consecutive_stops;
                    let pause_ms = self.consecutive_stop_pause_ms;
                    self.consecutive_stops = 0;
                    return Some((stops, pause_ms));
                }
            }
            // Signal-driven exit: treat like a controlled exit (reset consecutive stops).
            // RideSignalExit fires when the composite signal score degrades through
            // the exit threshold — it's a deliberate exit, not a stop-loss.
            ExitReason::RideSignalExit => {
                self.consecutive_stops = 0;
            }
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
/// PERF: #[inline(always)] — called on every gate reject (high frequency).
#[inline(always)]
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
        GateRejectReason::MaxCurveProgress => 22,
        GateRejectReason::LowFlowConcentration => 23,
        GateRejectReason::TooManyBuyers => 24,
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
/// PERF: #[inline(always)] — called on every trade, trivial field copy.
#[inline(always)]
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
