//! Position tracking, TP/SL/next-buyer exit logic, and momentum decay engine.
//!
//! Core of the backrun bot's risk management. Tracks open positions,
//! evaluates exit conditions on every trade event and 50ms tick,
//! and emits ClosedPosition events via crossbeam channel.

use crossbeam_channel::Sender;
use hashbrown::HashMap;

use crate::feeds::TradeEvent;
use super::bonding_curve;

// ─── Position Structs ──────────────────────────────────────────────

/// An open (held) position.
pub struct OpenPosition {
    pub mint: [u8; 32],
    pub bonding_curve: [u8; 32],
    pub assoc_bonding_curve: [u8; 32],
    /// Virtual SOL reserves at entry (lamports).
    pub entry_vsol: u64,
    /// Our position size (lamports).
    pub size_sol: u64,
    /// Entry timestamp (epoch ms).
    pub entry_ts_ms: u64,
    /// Maximum vSol seen while holding (for trailing stop / MFE).
    pub peak_vsol: u64,
    /// Minimum vSol seen while holding.
    pub trough_vsol: u64,
    /// Most recent vSol (updated on every trade event).
    pub current_vsol: u64,
    /// Most recent vToken reserves.
    pub current_vtokens: u64,
    /// Tokens we hold (computed via simulate_buy at entry).
    pub tokens_held: u64,
    /// Composite score at entry.
    pub score: f64,
    /// Trigger trade size (lamports) — used for tier lookup.
    pub trigger_sol: u64,
    /// Number of trade events seen after our entry.
    pub trades_seen_after_entry: u32,
    /// Total buy flow (lamports) since entry.
    pub flow_since_entry: u64,
    /// Number of buy events since entry.
    pub buys_since_entry: u32,
    /// Signature of the trigger event (to skip it in subsequent processing).
    pub trigger_sig: [u8; 64],
    /// Time-of-day multiplier (1.0 normal, 1.25 during boosted hours).
    pub tod_multiplier: f64,
    // ── Entry context (for rich logging / ML training data) ──
    pub pre_trigger_buys_1s: u16,
    pub pre_trigger_buys_2s: u16,
    pub pre_trigger_buys_5s: u16,
    pub unique_buyers: u16,
    pub vsol_delta_3s: u64,
    pub volume_5s: u64,
    pub sell_count_5s: u16,
}

/// Why a position was closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    TakeProfit,
    StopLoss,
    NextBuyer,
    MaxHold,
    IntraHoldTrail,
    MomentumDecayFlat,
    MomentumDecayFade,
}

/// A closed position with full PnL accounting.
pub struct ClosedPosition {
    pub mint: [u8; 32],
    pub entry_vsol: u64,
    pub exit_vsol: u64,
    pub entry_ts_ms: u64,
    pub exit_ts_ms: u64,
    pub hold_ms: u64,
    pub size_sol: u64,
    /// Gross PnL in lamports (signed).
    pub gross_pnl_sol: i64,
    /// Net PnL in lamports (after fees).
    pub net_pnl_sol: i64,
    /// Total fees (pump + jito) in lamports.
    pub fees_sol: u64,
    pub exit_reason: ExitReason,
    pub score: f64,
    pub tokens_held: u64,
    pub current_vtokens: u64,
    /// Current vSol at exit (needed for sell tx building).
    pub current_vsol: u64,
    pub bonding_curve: [u8; 32],
    pub assoc_bonding_curve: [u8; 32],
    // ── Rich logging fields (training data for gate/scorer tuning) ──
    /// Max vSol during hold (MFE - max favorable excursion).
    pub peak_vsol: u64,
    /// Min vSol during hold (MAE - max adverse excursion).
    pub trough_vsol: u64,
    /// Trigger trade size (lamports).
    pub trigger_sol: u64,
    /// Trades seen after entry (next-buyer flow data).
    pub trades_after_entry: u32,
    /// Buy events after entry.
    pub buys_after_entry: u32,
    /// Total buy flow since entry (lamports).
    pub flow_after_entry: u64,
    /// Pre-trigger buy counts at entry.
    pub pre_trigger_buys_1s: u16,
    pub pre_trigger_buys_2s: u16,
    pub pre_trigger_buys_5s: u16,
    /// Unique buyers at entry time.
    pub unique_buyers: u16,
    /// vSol delta 3s at entry.
    pub vsol_delta_3s: u64,
    /// Volume 5s at entry (lamports).
    pub volume_5s: u64,
    /// Sell count 5s at entry.
    pub sell_count_5s: u16,
    /// ToD multiplier applied.
    pub tod_multiplier: f64,
}

// ─── Config Structs ────────────────────────────────────────────────

/// Take-profit / stop-loss tier. Tiers are checked in order;
/// the first tier where `trigger_sol <= trigger_max_lamports` is used.
#[derive(Debug, Clone)]
pub struct TpSlTier {
    pub trigger_max_lamports: u64,
    pub tp_pct: f64,
    pub sl_pct: f64,
}

/// Position sizing tier.
#[derive(Debug, Clone)]
pub struct SizeTier {
    pub trigger_max_lamports: u64,
    pub size_lamports: u64,
}

/// Full position manager configuration.
#[derive(Debug, Clone)]
pub struct PositionConfig {
    /// Max hold time before forced exit (ms).
    pub max_hold_ms: u64,
    /// Momentum decay check interval (ms). Drives on_tick frequency.
    pub momentum_decay_check_ms: u64,
    /// Gate 1: if MFE% < this, exit as MomentumDecayFlat.
    pub momentum_decay_min_mfe_pct: f64,
    /// Gate 2: if drawdown from peak > this, exit as MomentumDecayFade.
    pub momentum_decay_max_drawdown_pct: f64,
    /// Intra-hold trailing stop: exit if price drops this much from peak.
    pub intra_hold_trailing_stop_pct: f64,
    /// Minimum MFE required before the intra-hold trailing stop activates.
    pub intra_hold_trailing_stop_min_mfe_pct: f64,
    /// Next-buyer: minimum profit % to consider early NB exit.
    pub next_buyer_profit_exit_pct: f64,
    /// Next-buyer: aggregate buy flow / size_sol ratio threshold.
    pub next_buyer_aggregate_flow_ratio: f64,
    /// Next-buyer: minimum buy count threshold.
    pub next_buyer_count_threshold: u32,
    /// Next-buyer: single buy / size_sol ratio threshold.
    pub next_buyer_single_buy_ratio: f64,
    /// TP/SL tiers (checked in order).
    pub tp_tiers: Vec<TpSlTier>,
    /// Position sizing tiers.
    pub size_tiers: Vec<SizeTier>,
    /// Maximum concurrent open positions.
    pub max_concurrent_positions: usize,
    /// Hard cap on entry size (lamports).
    pub max_entry_size_lamports: u64,
    /// Random variance applied to position size (e.g. 0.20 = ±20%).
    pub size_variance_pct: f64,
    /// Jito tip per bundle (lamports).
    pub jito_tip_lamports: u64,
    /// Minimum hold time before any NB/profit exit (ms).
    pub min_hold_before_exit_ms: u64,
    /// ToD boost multiplier for boosted hours.
    pub tod_boost_multiplier: f64,
    /// UTC hours that get the ToD boost.
    pub boosted_hours_utc: Vec<u8>,
}

// ─── Position Manager ──────────────────────────────────────────────

pub struct PositionManager {
    positions: HashMap<[u8; 32], OpenPosition>,
    config: PositionConfig,
    closed_tx: Sender<ClosedPosition>,
}

impl PositionManager {
    pub fn new(config: PositionConfig, closed_tx: Sender<ClosedPosition>) -> Self {
        assert!(
            config.min_hold_before_exit_ms < config.max_hold_ms,
            "CONFIG ERROR: min_hold_before_exit_ms ({}) must be < max_hold_ms ({}). \
             Next-buyer exits can never fire when the min-hold gate exceeds max hold time.",
            config.min_hold_before_exit_ms, config.max_hold_ms
        );
        Self {
            positions: HashMap::with_capacity(config.max_concurrent_positions),
            config,
            closed_tx,
        }
    }

    /// Number of currently open positions.
    #[inline]
    pub fn open_count(&self) -> usize {
        self.positions.len()
    }

    /// Whether we already hold a position for this mint.
    #[inline]
    pub fn has_position(&self, mint: &[u8; 32]) -> bool {
        self.positions.contains_key(mint)
    }

    /// Get a mutable reference to an open position (for enriching entry context).
    pub fn get_position_mut(&mut self, mint: &[u8; 32]) -> Option<&mut OpenPosition> {
        self.positions.get_mut(mint)
    }

    /// Look up position size for a given trigger trade size.
    /// Returns the size from the first matching tier, capped at max_entry_size_lamports.
    fn lookup_size(&self, trigger_sol: u64) -> u64 {
        for tier in &self.config.size_tiers {
            if trigger_sol <= tier.trigger_max_lamports {
                return tier.size_lamports.min(self.config.max_entry_size_lamports);
            }
        }
        // If no tier matches (trigger larger than all tiers), use last tier or cap
        self.config
            .size_tiers
            .last()
            .map(|t| t.size_lamports)
            .unwrap_or(0)
            .min(self.config.max_entry_size_lamports)
    }

    /// Look up TP/SL percentages for a given trigger trade size.
    /// Returns (tp_pct, sl_pct) from the first matching tier.
    fn lookup_tp_sl(&self, trigger_sol: u64) -> (f64, f64) {
        for tier in &self.config.tp_tiers {
            if trigger_sol <= tier.trigger_max_lamports {
                return (tier.tp_pct, tier.sl_pct);
            }
        }
        // Fallback to last tier
        self.config
            .tp_tiers
            .last()
            .map(|t| (t.tp_pct, t.sl_pct))
            .unwrap_or((0.025, 0.015))
    }

    /// Open a new position. Runs simulate_buy to compute tokens_held.
    ///
    /// The `event` is the trigger trade that caused us to enter.
    /// `score` is the composite score from the scorer.
    /// `now_ms` is the current timestamp in epoch ms.
    pub fn open_position(&mut self, event: &TradeEvent, score: f64, now_ms: u64) {
        if self.positions.len() >= self.config.max_concurrent_positions {
            return;
        }
        if self.positions.contains_key(&event.mint) {
            return;
        }

        let trigger_sol = event.sol_amount;
        let size_sol = self.lookup_size(trigger_sol);
        if size_sol == 0 {
            return;
        }

        // Simulate our buy to get tokens_held
        let buy = bonding_curve::simulate_buy(
            event.vsol_reserves,
            event.vtoken_reserves,
            size_sol,
            0, // no slippage calc needed for position tracking
        );

        let pos = OpenPosition {
            mint: event.mint,
            bonding_curve: event.bonding_curve,
            assoc_bonding_curve: event.assoc_bonding_curve,
            entry_vsol: event.vsol_reserves,
            size_sol,
            entry_ts_ms: now_ms,
            peak_vsol: event.vsol_reserves,
            trough_vsol: event.vsol_reserves,
            current_vsol: event.vsol_reserves,
            current_vtokens: event.vtoken_reserves,
            tokens_held: buy.tokens_out,
            score,
            trigger_sol,
            trades_seen_after_entry: 0,
            flow_since_entry: 0,
            buys_since_entry: 0,
            trigger_sig: event.sig,
            tod_multiplier: 1.0, // caller can update if needed
            // Entry context — populated by caller (hot_path) from MintHistory
            pre_trigger_buys_1s: 0,
            pre_trigger_buys_2s: 0,
            pre_trigger_buys_5s: 0,
            unique_buyers: 0,
            vsol_delta_3s: 0,
            volume_5s: 0,
            sell_count_5s: 0,
        };

        self.positions.insert(event.mint, pos);
    }

    /// Called on every subsequent trade event for held mints.
    ///
    /// Updates position state (reserves, peak/trough, flow tracking),
    /// then evaluates exit conditions in priority order:
    /// 1. Max hold timeout
    /// 2. Stop loss
    /// 3. Take profit
    /// 4. Intra-hold trailing stop
    /// 5. Next-buyer exits (requires min hold + min trades)
    ///
    /// Returns `true` if the position was closed.
    /// PERF: #[inline] — called for every trade on held mints. Not #[inline(always)]
    /// because the method body is large (exit logic branches); let the compiler decide.
    #[inline]
    pub fn on_subsequent_trade(&mut self, event: &TradeEvent, now_ms: u64) -> bool {
        // Skip events with zero reserves (Helius pre-warm)
        if event.vsol_reserves == 0 {
            return false;
        }

        let config = &self.config;

        // We need to check if position exists and get mutable ref.
        // Using a two-phase approach to satisfy borrow checker.
        let pos = match self.positions.get_mut(&event.mint) {
            Some(p) => p,
            None => return false,
        };

        // Skip the trigger event itself
        if event.sig == pos.trigger_sig {
            return false;
        }

        // ── Update position state ───────────────────────────────────
        pos.trades_seen_after_entry += 1;
        pos.current_vsol = event.vsol_reserves;
        pos.current_vtokens = event.vtoken_reserves;

        if event.vsol_reserves > pos.peak_vsol {
            pos.peak_vsol = event.vsol_reserves;
        }
        if event.vsol_reserves < pos.trough_vsol {
            pos.trough_vsol = event.vsol_reserves;
        }

        // Track buy flow for next-buyer logic
        if event.is_buy {
            pos.flow_since_entry += event.sol_amount;
            pos.buys_since_entry += 1;
        }

        // ── Compute current P&L metrics ─────────────────────────────
        let hold_ms = now_ms.saturating_sub(pos.entry_ts_ms);
        let entry_vsol = pos.entry_vsol as f64;
        let current_vsol = event.vsol_reserves as f64;
        let peak_vsol = pos.peak_vsol as f64;

        // Price change % (positive = profit)
        let pnl_pct = (current_vsol - entry_vsol) / entry_vsol;

        // MFE (max favorable excursion) from entry
        let mfe_pct = (peak_vsol - entry_vsol) / entry_vsol;

        // Drawdown from peak
        let drawdown_from_peak = if pos.peak_vsol > 0 {
            (peak_vsol - current_vsol) / peak_vsol
        } else {
            0.0
        };

        // ── Exit checks (priority order) ────────────────────────────

        // 1. Max hold timeout — always checked, no min-hold gate
        if hold_ms >= config.max_hold_ms {
            let reason = ExitReason::MaxHold;
            self.close_position_inner(&event.mint, reason, now_ms);
            return true;
        }

        // Look up TP/SL for this position's tier
        let (tp_pct, sl_pct) = self.lookup_tp_sl(
            self.positions.get(&event.mint).unwrap().trigger_sol,
        );

        // 2. Stop loss — always checked, no min-hold gate
        if pnl_pct <= -sl_pct {
            self.close_position_inner(&event.mint, ExitReason::StopLoss, now_ms);
            return true;
        }

        // 3. Take profit
        if pnl_pct >= tp_pct {
            self.close_position_inner(&event.mint, ExitReason::TakeProfit, now_ms);
            return true;
        }

        // 4. Intra-hold trailing stop (only if MFE exceeds minimum threshold)
        if mfe_pct >= config.intra_hold_trailing_stop_min_mfe_pct
            && drawdown_from_peak >= config.intra_hold_trailing_stop_pct
        {
            self.close_position_inner(&event.mint, ExitReason::IntraHoldTrail, now_ms);
            return true;
        }

        // ── Gated exits: require min trades + min hold time ─────────
        let pos = self.positions.get(&event.mint).unwrap();
        let enough_data = pos.trades_seen_after_entry >= 2
            && hold_ms >= config.min_hold_before_exit_ms;

        if enough_data {
            // 5. Next-buyer exits
            let size_sol = pos.size_sol as f64;

            // 5a. Aggregate flow ratio: if enough buy flow has come in AND we're in profit
            if pnl_pct >= config.next_buyer_profit_exit_pct {
                let flow_ratio = pos.flow_since_entry as f64 / size_sol;
                if flow_ratio >= config.next_buyer_aggregate_flow_ratio {
                    self.close_position_inner(&event.mint, ExitReason::NextBuyer, now_ms);
                    return true;
                }
            }

            // 5b. Buy count threshold with profit
            if pnl_pct >= config.next_buyer_profit_exit_pct
                && pos.buys_since_entry >= config.next_buyer_count_threshold
            {
                self.close_position_inner(&event.mint, ExitReason::NextBuyer, now_ms);
                return true;
            }

            // 5c. Single large buy relative to our position
            if event.is_buy && pnl_pct >= config.next_buyer_profit_exit_pct {
                let single_buy_ratio = event.sol_amount as f64 / size_sol;
                if single_buy_ratio >= config.next_buyer_single_buy_ratio {
                    self.close_position_inner(&event.mint, ExitReason::NextBuyer, now_ms);
                    return true;
                }
            }
        }

        false
    }

    /// Called by the 50ms tick timer for dead-token momentum decay.
    ///
    /// For every open position, checks:
    /// - Max hold timeout
    /// - Gate 1 (MomentumDecayFlat): MFE < min_mfe_pct → position never moved, exit
    /// - Gate 2 (MomentumDecayFade): drawdown from peak > max_drawdown_pct → fading, exit
    ///
    /// Uses `current_vsol` for exit price (NOT peak_vsol).
    pub fn on_tick(&mut self, now_ms: u64) {
        // Collect mints to close (can't mutate while iterating)
        let mut to_close: Vec<([u8; 32], ExitReason)> = Vec::new();

        for (mint, pos) in self.positions.iter() {
            let hold_ms = now_ms.saturating_sub(pos.entry_ts_ms);

            // Max hold check
            if hold_ms >= self.config.max_hold_ms {
                to_close.push((*mint, ExitReason::MaxHold));
                continue;
            }

            // Only check momentum decay after the check interval
            if hold_ms < self.config.momentum_decay_check_ms {
                continue;
            }

            let entry_vsol = pos.entry_vsol as f64;
            let peak_vsol = pos.peak_vsol as f64;
            let current_vsol = pos.current_vsol as f64;

            // MFE from entry
            let mfe_pct = (peak_vsol - entry_vsol) / entry_vsol;

            // Gate 1: Flat — token never moved meaningfully
            if mfe_pct < self.config.momentum_decay_min_mfe_pct {
                to_close.push((*mint, ExitReason::MomentumDecayFlat));
                continue;
            }

            // Gate 2: Fade — had some momentum but it's fading
            let drawdown_from_peak = if peak_vsol > 0.0 {
                (peak_vsol - current_vsol) / peak_vsol
            } else {
                0.0
            };

            if drawdown_from_peak > self.config.momentum_decay_max_drawdown_pct {
                to_close.push((*mint, ExitReason::MomentumDecayFade));
            }
        }

        // Execute closes
        for (mint, reason) in to_close {
            self.close_position_inner(&mint, reason, now_ms);
        }
    }

    /// Force-close all positions (on shutdown).
    pub fn close_all(&mut self, now_ms: u64) {
        let mints: Vec<[u8; 32]> = self.positions.keys().copied().collect();
        for mint in mints {
            self.close_position_inner(&mint, ExitReason::MaxHold, now_ms);
        }
    }

    /// Force-close a specific position.
    pub fn force_close(&mut self, mint: &[u8; 32], reason: ExitReason, now_ms: u64) {
        self.close_position_inner(mint, reason, now_ms);
    }

    /// Internal: close a position, compute PnL, emit ClosedPosition, remove from map.
    fn close_position_inner(&mut self, mint: &[u8; 32], reason: ExitReason, now_ms: u64) {
        let pos = match self.positions.remove(mint) {
            Some(p) => p,
            None => return,
        };

        let hold_ms = now_ms.saturating_sub(pos.entry_ts_ms);
        let exit_vsol = pos.current_vsol;

        // PnL calculation:
        // gross_pnl = (exit_vsol - entry_vsol) * size_sol / entry_vsol
        let gross_pnl_sol = if pos.entry_vsol > 0 {
            let delta = exit_vsol as i128 - pos.entry_vsol as i128;
            (delta * pos.size_sol as i128 / pos.entry_vsol as i128) as i64
        } else {
            0
        };

        // Fees: 1% buy + 1% sell = 2% of position size
        let pump_fees = pos.size_sol * 2 / 100;
        // Jito tips: entry bundle + exit bundle
        let jito_fees = self.config.jito_tip_lamports * 2;
        let total_fees = pump_fees + jito_fees;

        let net_pnl_sol = gross_pnl_sol - total_fees as i64;

        let closed = ClosedPosition {
            mint: pos.mint,
            entry_vsol: pos.entry_vsol,
            exit_vsol,
            entry_ts_ms: pos.entry_ts_ms,
            exit_ts_ms: now_ms,
            hold_ms,
            size_sol: pos.size_sol,
            gross_pnl_sol,
            net_pnl_sol,
            fees_sol: total_fees,
            exit_reason: reason,
            score: pos.score,
            tokens_held: pos.tokens_held,
            current_vtokens: pos.current_vtokens,
            current_vsol: pos.current_vsol,
            bonding_curve: pos.bonding_curve,
            assoc_bonding_curve: pos.assoc_bonding_curve,
            // Rich logging fields from OpenPosition
            peak_vsol: pos.peak_vsol,
            trough_vsol: pos.trough_vsol,
            trigger_sol: pos.trigger_sol,
            trades_after_entry: pos.trades_seen_after_entry,
            buys_after_entry: pos.buys_since_entry,
            flow_after_entry: pos.flow_since_entry,
            // These will be populated from entry context (passed via open_position)
            pre_trigger_buys_1s: pos.pre_trigger_buys_1s,
            pre_trigger_buys_2s: pos.pre_trigger_buys_2s,
            pre_trigger_buys_5s: pos.pre_trigger_buys_5s,
            unique_buyers: pos.unique_buyers,
            vsol_delta_3s: pos.vsol_delta_3s,
            volume_5s: pos.volume_5s,
            sell_count_5s: pos.sell_count_5s,
            tod_multiplier: pos.tod_multiplier,
        };

        // Best-effort send — if the receiver is gone, we just drop it.
        let _ = self.closed_tx.try_send(closed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feeds::{FeedSource, TradeEvent};

    fn test_config() -> PositionConfig {
        PositionConfig {
            max_hold_ms: 500,
            momentum_decay_check_ms: 50,
            momentum_decay_min_mfe_pct: 0.001,
            momentum_decay_max_drawdown_pct: 0.003,
            intra_hold_trailing_stop_pct: 0.025,
            intra_hold_trailing_stop_min_mfe_pct: 0.01,
            next_buyer_profit_exit_pct: 0.015,
            next_buyer_aggregate_flow_ratio: 0.5,
            next_buyer_count_threshold: 5,
            next_buyer_single_buy_ratio: 0.25,
            tp_tiers: vec![
                TpSlTier { trigger_max_lamports: 100_000_000, tp_pct: 0.025, sl_pct: 0.015 },
                TpSlTier { trigger_max_lamports: 500_000_000, tp_pct: 0.020, sl_pct: 0.012 },
                TpSlTier { trigger_max_lamports: u64::MAX, tp_pct: 0.015, sl_pct: 0.010 },
            ],
            size_tiers: vec![
                SizeTier { trigger_max_lamports: 100_000_000, size_lamports: 50_000_000 },
                SizeTier { trigger_max_lamports: 500_000_000, size_lamports: 100_000_000 },
                SizeTier { trigger_max_lamports: u64::MAX, size_lamports: 200_000_000 },
            ],
            max_concurrent_positions: 10,
            max_entry_size_lamports: 500_000_000,
            size_variance_pct: 0.20,
            jito_tip_lamports: 1_000_000, // 0.001 SOL
            min_hold_before_exit_ms: 200,
            tod_boost_multiplier: 1.25,
            boosted_hours_utc: vec![14, 15],
        }
    }

    fn make_trade_event(
        mint: [u8; 32],
        sig: [u8; 64],
        sol_amount: u64,
        vsol: u64,
        vtokens: u64,
        is_buy: bool,
    ) -> TradeEvent {
        TradeEvent {
            mint,
            trader: [1u8; 32],
            sig,
            sig_prefix: {
                let mut p = [0u8; 8];
                p.copy_from_slice(&sig[..8]);
                p
            },
            sol_amount,
            token_amount: 1_000_000,
            vsol_reserves: vsol,
            vtoken_reserves: vtokens,
            market_cap_sol: vsol * 2,
            slot: 100,
            timestamp_ms: 0,
            is_buy,
            source: FeedSource::PumpPortal,
            bonding_curve: [2u8; 32],
            assoc_bonding_curve: [3u8; 32],
        }
    }

    #[test]
    fn test_open_position() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut pm = PositionManager::new(test_config(), tx);

        let mint = [0xAAu8; 32];
        let sig = [0xBBu8; 64];
        let event = make_trade_event(mint, sig, 50_000_000, 30_000_000_000, 1_000_000_000_000_000, true);

        pm.open_position(&event, 0.85, 1000);

        assert_eq!(pm.open_count(), 1);
        assert!(pm.has_position(&mint));
        assert!(rx.try_recv().is_err()); // no close yet
    }

    #[test]
    fn test_skip_trigger_event() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut pm = PositionManager::new(test_config(), tx);

        let mint = [0xAAu8; 32];
        let sig = [0xBBu8; 64];
        let event = make_trade_event(mint, sig, 50_000_000, 30_000_000_000, 1_000_000_000_000_000, true);

        pm.open_position(&event, 0.85, 1000);

        // Same sig as trigger — should be skipped
        let closed = pm.on_subsequent_trade(&event, 1010);
        assert!(!closed);
        assert_eq!(pm.open_count(), 1);
    }

    #[test]
    fn test_skip_zero_reserves() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut pm = PositionManager::new(test_config(), tx);

        let mint = [0xAAu8; 32];
        let sig = [0xBBu8; 64];
        let event = make_trade_event(mint, sig, 50_000_000, 30_000_000_000, 1_000_000_000_000_000, true);
        pm.open_position(&event, 0.85, 1000);

        // Zero reserves event — should be skipped
        let zero_event = make_trade_event(mint, [0xCCu8; 64], 10_000_000, 0, 0, true);
        let closed = pm.on_subsequent_trade(&zero_event, 1010);
        assert!(!closed);
    }

    #[test]
    fn test_max_hold_exit() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut pm = PositionManager::new(test_config(), tx);

        let mint = [0xAAu8; 32];
        let sig = [0xBBu8; 64];
        let event = make_trade_event(mint, sig, 50_000_000, 30_000_000_000, 1_000_000_000_000_000, true);
        pm.open_position(&event, 0.85, 1000);

        // Trade after max_hold_ms (400ms)
        let late_event = make_trade_event(mint, [0xCCu8; 64], 10_000_000, 30_000_000_000, 1_000_000_000_000_000, true);
        let closed = pm.on_subsequent_trade(&late_event, 1500);

        assert!(closed);
        assert_eq!(pm.open_count(), 0);
        let cp = rx.try_recv().unwrap();
        assert_eq!(cp.exit_reason, ExitReason::MaxHold);
    }

    #[test]
    fn test_stop_loss_exit() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut pm = PositionManager::new(test_config(), tx);

        let mint = [0xAAu8; 32];
        let sig = [0xBBu8; 64];
        let entry_vsol = 30_000_000_000u64;
        let event = make_trade_event(mint, sig, 50_000_000, entry_vsol, 1_000_000_000_000_000, true);
        pm.open_position(&event, 0.85, 1000);

        // Price drops 2% (sl_pct is 0.015 = 1.5% for this tier)
        let drop_vsol = (entry_vsol as f64 * 0.98) as u64;
        let drop_event = make_trade_event(mint, [0xCCu8; 64], 10_000_000, drop_vsol, 1_000_000_000_000_000, false);
        let closed = pm.on_subsequent_trade(&drop_event, 1050);

        assert!(closed);
        let cp = rx.try_recv().unwrap();
        assert_eq!(cp.exit_reason, ExitReason::StopLoss);
        assert!(cp.net_pnl_sol < 0);
    }

    #[test]
    fn test_take_profit_exit() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut pm = PositionManager::new(test_config(), tx);

        let mint = [0xAAu8; 32];
        let sig = [0xBBu8; 64];
        let entry_vsol = 30_000_000_000u64;
        let event = make_trade_event(mint, sig, 50_000_000, entry_vsol, 1_000_000_000_000_000, true);
        pm.open_position(&event, 0.85, 1000);

        // Price rises 3% (tp_pct is 0.025 = 2.5% for this tier)
        let up_vsol = (entry_vsol as f64 * 1.03) as u64;
        let up_event = make_trade_event(mint, [0xCCu8; 64], 10_000_000, up_vsol, 1_000_000_000_000_000, true);
        let closed = pm.on_subsequent_trade(&up_event, 1050);

        assert!(closed);
        let cp = rx.try_recv().unwrap();
        assert_eq!(cp.exit_reason, ExitReason::TakeProfit);
    }

    #[test]
    fn test_momentum_decay_flat() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut pm = PositionManager::new(test_config(), tx);

        let mint = [0xAAu8; 32];
        let sig = [0xBBu8; 64];
        let entry_vsol = 30_000_000_000u64;
        let event = make_trade_event(mint, sig, 50_000_000, entry_vsol, 1_000_000_000_000_000, true);
        pm.open_position(&event, 0.85, 1000);

        // No price movement at all — MFE stays 0 which is < 0.001
        // Tick after momentum_decay_check_ms (50ms)
        pm.on_tick(1060);

        assert_eq!(pm.open_count(), 0);
        let cp = rx.try_recv().unwrap();
        assert_eq!(cp.exit_reason, ExitReason::MomentumDecayFlat);
    }

    #[test]
    fn test_momentum_decay_fade() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut pm = PositionManager::new(test_config(), tx);

        let mint = [0xAAu8; 32];
        let sig = [0xBBu8; 64];
        let entry_vsol = 30_000_000_000u64;
        let event = make_trade_event(mint, sig, 50_000_000, entry_vsol, 1_000_000_000_000_000, true);
        pm.open_position(&event, 0.85, 1000);

        // First, move price up to establish MFE > min_mfe_pct (0.001)
        let up_vsol = (entry_vsol as f64 * 1.005) as u64; // 0.5% up
        let up_event = make_trade_event(mint, [0xCCu8; 64], 10_000_000, up_vsol, 1_000_000_000_000_000, true);
        pm.on_subsequent_trade(&up_event, 1020);

        // Now price drops back, creating drawdown from peak > 0.003
        let down_vsol = (up_vsol as f64 * 0.995) as u64; // ~0.5% drop from peak
        let down_event = make_trade_event(mint, [0xDDu8; 64], 10_000_000, down_vsol, 1_000_000_000_000_000, false);
        pm.on_subsequent_trade(&down_event, 1040);

        // Tick after check interval
        pm.on_tick(1060);

        assert_eq!(pm.open_count(), 0);
        let cp = rx.try_recv().unwrap();
        assert_eq!(cp.exit_reason, ExitReason::MomentumDecayFade);
    }

    #[test]
    fn test_close_all() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut pm = PositionManager::new(test_config(), tx);

        // Open 3 positions
        for i in 0..3u8 {
            let mint = [i + 1; 32];
            let sig = [i + 10; 64];
            let event = make_trade_event(mint, sig, 50_000_000, 30_000_000_000, 1_000_000_000_000_000, true);
            pm.open_position(&event, 0.85, 1000);
        }

        assert_eq!(pm.open_count(), 3);
        pm.close_all(2000);
        assert_eq!(pm.open_count(), 0);

        // Should have 3 closed positions
        let mut count = 0;
        while rx.try_recv().is_ok() {
            count += 1;
        }
        assert_eq!(count, 3);
    }

    #[test]
    fn test_max_concurrent_positions() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut config = test_config();
        config.max_concurrent_positions = 2;
        let mut pm = PositionManager::new(config, tx);

        for i in 0..3u8 {
            let mint = [i + 1; 32];
            let sig = [i + 10; 64];
            let event = make_trade_event(mint, sig, 50_000_000, 30_000_000_000, 1_000_000_000_000_000, true);
            pm.open_position(&event, 0.85, 1000);
        }

        // Should cap at 2
        assert_eq!(pm.open_count(), 2);
    }

    #[test]
    fn test_pnl_calculation() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut pm = PositionManager::new(test_config(), tx);

        let mint = [0xAAu8; 32];
        let sig = [0xBBu8; 64];
        let entry_vsol = 30_000_000_000u64;
        let event = make_trade_event(mint, sig, 50_000_000, entry_vsol, 1_000_000_000_000_000, true);
        pm.open_position(&event, 0.85, 1000);

        // Force close at same price
        pm.force_close(&mint, ExitReason::MaxHold, 1500);

        let cp = rx.try_recv().unwrap();
        // gross_pnl should be 0 (same price)
        assert_eq!(cp.gross_pnl_sol, 0);
        // net_pnl should be negative (fees)
        assert!(cp.net_pnl_sol < 0);
        // fees = size * 2% + jito * 2
        let expected_fees = cp.size_sol * 2 / 100 + 1_000_000 * 2;
        assert_eq!(cp.fees_sol, expected_fees);
    }

    #[test]
    fn test_nb_exit_requires_min_hold_and_trades() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut pm = PositionManager::new(test_config(), tx);

        let mint = [0xAAu8; 32];
        let sig = [0xBBu8; 64];
        let entry_vsol = 30_000_000_000u64;
        let event = make_trade_event(mint, sig, 50_000_000, entry_vsol, 1_000_000_000_000_000, true);
        pm.open_position(&event, 0.85, 1000);

        // Large buy that would trigger NB exit, but only 1 trade and < 200ms min_hold
        let up_vsol = (entry_vsol as f64 * 1.02) as u64; // in profit
        let big_buy = make_trade_event(mint, [0xCCu8; 64], 50_000_000, up_vsol, 1_000_000_000_000_000, true);
        let closed = pm.on_subsequent_trade(&big_buy, 1100); // only 100ms hold, < 200ms gate

        // Should NOT close — not enough trades or hold time (need >= 2 trades AND >= 200ms)
        assert!(!closed);
        assert!(rx.try_recv().is_err());
    }
}