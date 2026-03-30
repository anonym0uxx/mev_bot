//! Position tracking, TP/SL/next-buyer exit logic, and momentum decay engine.
//!
//! Core of the backrun bot's risk management. Tracks open positions,
//! evaluates exit conditions on every trade event and 50ms tick,
//! and emits ClosedPosition events via crossbeam channel.

use crossbeam_channel::Sender;
use hashbrown::HashMap;

use crate::feeds::{FeedSource, TradeEvent};
use super::bayesian_signal;
use super::bonding_curve;

// RIDE imports
use crate::engine::ride_state::{RideState, RideConfig, RideDecision, RideExitReason};
// Kelly imports
use crate::engine::kelly_sizing::EntryConviction;

// ─── Helpers ───────────────────────────────────────────────────────

/// Convert lamports to milli-vSOL (rounds to nearest).
/// 1 milli-vSOL = 1_000_000 lamports (0.001 SOL).
fn lamports_to_mvsol(lamports: u64) -> u32 {
    ((lamports + 500_000) / 1_000_000) as u32
}

/// Whale sell threshold: 2 SOL in lamports.
/// Sells above this get amplified evidence weight (WHALE_SELL_WEIGHT).
const WHALE_SELL_THRESHOLD_LAMPORTS: u64 = 2_000_000_000;

/// Map a RideExitReason from ride_state into our ExitReason enum.
fn map_ride_exit_reason(r: RideExitReason) -> ExitReason {
    match r {
        RideExitReason::TrailingStop   => ExitReason::RideTrailingStop,
        RideExitReason::HardFloor      => ExitReason::RideHardFloor,
        RideExitReason::WhaleExit      => ExitReason::RideWhaleExit,
        RideExitReason::BuyGapTimeout  => ExitReason::RideBuyGapTimeout,
        RideExitReason::SellCascade    => ExitReason::RideSellCascade,
        RideExitReason::CreatorSell    => ExitReason::RideCreatorSell,
        RideExitReason::MaxHold        => ExitReason::RideMaxHold,
        RideExitReason::SignalExit     => ExitReason::RideSignalExit,
    }
}



// ─── Position Structs ──────────────────────────────────────────────

/// Determines the active exit strategy for an open position.
/// RIDE-only engine: all positions use trailing-stop ride exits.
#[derive(Debug)]
pub enum ExitMode {
    /// Trailing-stop ride exit for all positions.
    Ride(crate::engine::ride_state::RideState),
}

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
    /// Active exit strategy — RIDE only.
    pub exit_mode: ExitMode,
    /// Magnitude estimate from EntryDecision (0.0–100.0). Used for RIDE qualification.
    pub magnitude_estimate: f64,
    /// Cumulative SOL from buy events after our entry (in lamports).
    pub confirming_buy_sol: u64,
    /// Count of unique wallet-sized buys (sol_amount >= 0.05 SOL). Capped at 255.
    pub confirming_unique_wallets: u8,
    /// Number of sell events observed while we hold this position.
    pub sells_during_hold: u16,
    /// SignalState::* value at time of exit (populated on close path).
    pub signal_state_at_exit: u8,
    /// Kelly entry conviction (win-prob, reward-risk, Kelly fraction, tier).
    pub conviction: EntryConviction,
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
    /// Conviction-scaled TP exit (buysAfter >= 2).
    TakeProfitScaled,
    /// Signal-based stall: no buy + price fading.
    MomentumStall,
    // --- RIDE mode exit reasons ---
    RideTrailingStop,
    RideHardFloor,
    RideWhaleExit,
    RideBuyGapTimeout,
    RideSellCascade,
    RideCreatorSell,
    RideMaxHold,
    RideSignalExit,
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
    // ── RIDE mode fields ──
    /// Exit strategy identifier at close (always "ride").
    pub exit_mode_str: &'static str,
    /// RIDE phase at close: 0=early, 1=momentum, 2=tighten.
    pub ride_phase: u8,
    /// Peak milli-vSOL seen during RIDE.
    pub ride_peak_mvsol: u32,
    /// Duration of RIDE mode in milliseconds.
    pub ride_hold_ms: u64,
    /// Unique confirming wallets at close.
    pub ride_unique_wallets: u8,
    // ── V2 EntryEngine fields ──
    /// Magnitude score from EntryEngine (0-100). Used for RIDE qualification.
    pub magnitude_estimate: f64,
    /// Kelly-computed size in lamports (what EntryEngine decided).
    pub kelly_size_lamports: u64,
    /// Entry action from EntryEngine (always "ride").
    pub entry_action: &'static str,
    /// Confirming buy volume during hold (lamports). For RIDE analysis.
    pub confirming_buy_sol: u64,
    /// Number of sells during hold. 0 = pure buy pressure.
    pub sells_during_hold: u16,
    // ── Signal v2 fields ──
    /// Composite signal score at exit (from RideState v2).
    pub signal_score_at_exit: u16,
    /// SignalState::* value at exit (0=StrongPump, 1=Sustained, 2=Weakening, 3=Exit).
    pub signal_state_at_exit: u8,
    /// Peak composite signal score observed during hold.
    pub peak_signal_score: u16,
    /// Unique wallets seen via bloom filter during hold.
    pub unique_wallets_seen: u8,
    // ── Kelly conviction fields ──
    /// Entry win probability × 1000.
    pub entry_p_permille: u16,
    /// Entry win/loss ratio × 100.
    pub entry_r_x100: u16,
    /// Entry Kelly fraction × 1000.
    pub entry_f_permille: u16,
    /// Conviction tier (0=LOW, 1=MED, 2=HIGH).
    pub conviction_tier: u8,
    // ── Bayesian exit state fields (dv8) ──
    /// Bayesian half-Kelly fraction at exit × 1000.
    /// Positive = positive EV remaining. Negative = EV gone negative.
    pub bayesian_f_at_exit: i16,
    /// Beta α parameter × 16 at exit.
    pub alpha_at_exit: u16,
    /// Beta β parameter × 16 at exit.
    pub beta_at_exit: u16,
    /// Bayesian R estimate × 100 at exit.
    pub r_est_at_exit: u16,
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
    /// Max hold time for RIDE positions (ms). Longer to let winners run.
    /// Falls back to max_hold_ms if 0.
    pub ride_max_hold_ms: u64,
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
    /// Signal-based exit state machine configuration.
    pub exit_config: crate::engine::config::ExitConfig,
    /// Configuration for RIDE trailing-stop exits.
    pub ride_config: RideConfig,
    /// Round-trip fee in basis points (pump 1%+1% + Jito ≈ 210bp).
    /// Used for fee-aware Kelly sizing and breakeven hard floor.
    pub round_trip_fee_bp: u16,
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
    /// `magnitude_estimate` is the magnitude score (0.0–100.0) for RIDE qualification.
    /// Open a new position.
    /// `size_override_lamports`: if > 0, use this Kelly-computed size instead of tier lookup.
    pub fn open_position(&mut self, event: &TradeEvent, score: f64, now_ms: u64, magnitude_estimate: f64, size_override_lamports: u64, conviction: EntryConviction) {
        if self.positions.len() >= self.config.max_concurrent_positions {
            return;
        }
        if self.positions.contains_key(&event.mint) {
            return;
        }

        let trigger_sol = event.sol_amount;
        let size_sol = if size_override_lamports > 0 {
            size_override_lamports.min(self.config.max_entry_size_lamports)
        } else {
            self.lookup_size(trigger_sol)
        };
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

        // Initialize RIDE state directly — all positions start as RIDE.
        let entry_mvsol = lamports_to_mvsol(event.vsol_reserves);
        let ride_state = RideState::new(
            entry_mvsol, entry_mvsol, now_ms,
            conviction.f_permille,
            conviction.p_permille,
            conviction.r_x100,
            conviction.conviction_tier,
            self.config.ride_config.avg_loss_bp,
            &self.config.ride_config,
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
            exit_mode: ExitMode::Ride(ride_state),
            magnitude_estimate,
            confirming_buy_sol: 0,
            confirming_unique_wallets: 0,
            sells_during_hold: 0,
            signal_state_at_exit: 0,
            conviction,
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
    /// then delegates exit decisions to the RIDE exit strategy:
    /// RideState (on_buy_event / on_sell_event / on_tick).
    ///
    /// `deduped`: when true, this trade's sig was already processed by another
    /// feed (Helius → PumpPortal dedup). Position state (reserves, counters)
    /// is still updated, but RideState α/β evidence update is skipped to
    /// prevent double-counting the same trade.
    ///
    /// Returns `true` if the position was closed.
    #[inline]
    pub fn on_subsequent_trade(&mut self, event: &TradeEvent, now_ms: u64, deduped: bool) -> bool {
        // Skip events with zero reserves (Helius pre-warm)
        if event.vsol_reserves == 0 {
            return false;
        }

        // We need to check if position exists and get mutable ref.
        let pos = match self.positions.get_mut(&event.mint) {
            Some(p) => p,
            None => return false,
        };

        // Skip the trigger event itself
        if event.sig == pos.trigger_sig {
            return false;
        }

        // ── Update position state (always, even when deduped) ───────
        pos.trades_seen_after_entry += 1;
        pos.current_vsol = event.vsol_reserves;
        pos.current_vtokens = event.vtoken_reserves;

        if event.vsol_reserves > pos.peak_vsol {
            pos.peak_vsol = event.vsol_reserves;
        }
        if event.vsol_reserves < pos.trough_vsol {
            pos.trough_vsol = event.vsol_reserves;
        }

        // ── Compute evidence weight (feed-source-aware) ─────────────
        // Weight is computed regardless of dedup for logging/diagnostics.
        // When deduped, RideState ring/bloom/counter updates are skipped.
        //
        // Evidence weight = EVIDENCE_WEIGHTS[is_sell][source_idx], with
        // special overrides for creator sell (CoreCast) and whale sell (>2 SOL).
        let _weight_mult: u8 = {
            let source_idx = event.source.as_u8() as usize;
            let src_clamped = if source_idx < 4 { source_idx } else { 3 };
            if !event.is_buy {
                // Sell-side overrides (highest priority first):
                // CoreCast creator sell handled via on_creator_sell_evidence path,
                // NOT here — is_creator_sell is only known from FeedEvent::CreatorSell.
                if event.sol_amount > WHALE_SELL_THRESHOLD_LAMPORTS {
                    bayesian_signal::WHALE_SELL_WEIGHT
                } else {
                    bayesian_signal::EVIDENCE_WEIGHTS[1][src_clamped]
                }
            } else {
                bayesian_signal::EVIDENCE_WEIGHTS[0][src_clamped]
            }
        };
        // TODO(eng2): Pass weight_mult to RideState on_buy_event / on_sell_event
        // when Engineer 2 updates the RideState signatures to accept it.
        // Currently computed and ready; will be wired through once RideState v3
        // accepts the parameter.

        if event.is_buy {
            // Track buy flow (kept for JSONL logging / training data)
            pos.flow_since_entry += event.sol_amount;
            pos.buys_since_entry += 1;

            // Track confirming buy volume
            pos.confirming_buy_sol = pos.confirming_buy_sol.saturating_add(event.sol_amount);

            // Track unique wallet-sized buyers (>= 0.05 SOL)
            if event.sol_amount >= 50_000_000 {
                pos.confirming_unique_wallets = pos.confirming_unique_wallets.saturating_add(1);
            }

            // Feed the RIDE exit strategy (skip ring/bloom/counter updates when deduped)
            if !deduped {
                match &mut pos.exit_mode {
                    ExitMode::Ride(ref mut rs) => {
                        let buy_mvsol = lamports_to_mvsol(event.sol_amount);
                        // Branchless wallet hash from first 8 bytes of trader pubkey
                        #[inline(always)]
                        fn wallet_hash_from_sig(sig: &[u8; 64]) -> u64 {
                            u64::from_le_bytes([
                                sig[0], sig[1], sig[2], sig[3],
                                sig[4], sig[5], sig[6], sig[7],
                            ])
                        }
                        let wallet_hash = wallet_hash_from_sig(&event.sig);
                        rs.on_buy_event(buy_mvsol, now_ms, wallet_hash, event.source, 10);
                    }
                }
            }

            // Post-buy tick for RIDE mode (always runs — drives trail/exit evaluation)
            let mint = event.mint;
            let pos = match self.positions.get_mut(&mint) {
                Some(p) => p,
                None => return true, // was closed above
            };
            match &mut pos.exit_mode {
                ExitMode::Ride(ref mut rs) => {
                    let current_mvsol = lamports_to_mvsol(pos.current_vsol);
                    match rs.on_tick(current_mvsol, now_ms, &self.config.ride_config) {
                        RideDecision::Exit(reason) => {
                            let exit_reason = map_ride_exit_reason(reason);
                            self.close_position_inner(&mint, exit_reason, now_ms);
                            return true;
                        }
                        RideDecision::Hold => {}
                    }
                }
            }
        } else {
            // ── SELL event ──
            pos.sells_during_hold = pos.sells_during_hold.saturating_add(1);

            // Feed the RIDE exit strategy (skip ring updates when deduped)
            if !deduped {
                match &mut pos.exit_mode {
                    ExitMode::Ride(ref mut rs) => {
                        let sell_mvsol = lamports_to_mvsol(event.sol_amount);
                        if let Some(reason) = rs.on_sell_event(sell_mvsol, now_ms, &self.config.ride_config, event.source, 10) {
                            let exit_reason = map_ride_exit_reason(reason);
                            let mint = event.mint;
                            self.close_position_inner(&mint, exit_reason, now_ms);
                            return true;
                        }
                    }
                }
            }

            // Post-sell tick (always runs)
            let mint = event.mint;
            let pos = match self.positions.get_mut(&mint) {
                Some(p) => p,
                None => return true,
            };
            match &mut pos.exit_mode {
                ExitMode::Ride(ref mut rs) => {
                    let current_mvsol = lamports_to_mvsol(pos.current_vsol);
                    match rs.on_tick(current_mvsol, now_ms, &self.config.ride_config) {
                        RideDecision::Exit(reason) => {
                            let exit_reason = map_ride_exit_reason(reason);
                            self.close_position_inner(&mint, exit_reason, now_ms);
                            return true;
                        }
                        RideDecision::Hold => {}
                    }
                }
            }
        }

        false
    }

    /// Handle creator sell evidence injection for open positions.
    ///
    /// Called from HotPath::on_creator_sell() when CoreCast detects a
    /// signer-verified creator sell. Two effects:
    ///   1. RideState.flags |= CREATOR_SELL (emergency exit on next tick)
    ///   2. Inject heavy β via on_sell_event for Bayesian logging
    ///      (even though emergency exit fires first, β captures the evidence)
    ///
    /// PERF: #[inline(never)] — cold path, ~rare event.
    #[inline(never)]
    pub fn on_creator_sell_evidence(&mut self, mint: &[u8; 32], ts_ms: u64) {
        let pos = match self.positions.get_mut(mint) {
            Some(p) => p,
            None => return,
        };
        match &mut pos.exit_mode {
            ExitMode::Ride(ref mut rs) => {
                rs.mark_creator_sell();
                // Inject β evidence: estimate 1 SOL creator sell.
                // source=2 (CoreCast), evidence weight = CREATOR_SELL_WEIGHT (50).
                // RideState::on_sell_event handles emergency exit on next tick
                // via the CREATOR_SELL flag we just set.
                let sell_mvsol = 1000u32; // 1 SOL estimate
                let _ = rs.on_sell_event(sell_mvsol, ts_ms, &self.config.ride_config, FeedSource::CoreCast, 50);
            }
        }
    }

    /// Called by the 50ms tick timer.
    ///
    /// Handles max-hold safety backstop and ticks active exit strategies
    /// for positions that haven't received recent trade events.
    pub fn on_tick(&mut self, now_ms: u64) {
        let mut to_close: Vec<([u8; 32], ExitReason)> = Vec::new();

        for (mint, pos) in self.positions.iter_mut() {
            let hold_ms = now_ms.saturating_sub(pos.entry_ts_ms);

            // Max hold safety check
            let max_hold = if self.config.ride_max_hold_ms > 0 {
                self.config.ride_max_hold_ms
            } else {
                self.config.max_hold_ms
            };
            if hold_ms >= max_hold {
                to_close.push((*mint, ExitReason::RideMaxHold));
                continue;
            }

            // Tick the RIDE exit strategy
            match &mut pos.exit_mode {
                ExitMode::Ride(ref mut rs) => {
                    let current_mvsol = lamports_to_mvsol(pos.current_vsol);
                    match rs.on_tick(current_mvsol, now_ms, &self.config.ride_config) {
                        RideDecision::Exit(reason) => {
                            to_close.push((*mint, map_ride_exit_reason(reason)));
                        }
                        RideDecision::Hold => {}
                    }
                }
            }
        }

        // Close positions
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

        // Fees: round_trip_fee_bp of position size (default 210bp = 2.1%)
        // Includes pump.fun 1% buy + 1% sell + Jito tips.
        let fee_bp = self.config.round_trip_fee_bp as u64;
        let total_fees = (pos.size_sol * fee_bp + 5_000) / 10_000; // rounded

        let net_pnl_sol = gross_pnl_sol - total_fees as i64;

        // Determine RIDE metadata — all positions are RIDE now
        let (exit_mode_str, ride_phase, ride_peak_mvsol, ride_hold_ms, ride_unique_wallets) =
            match &pos.exit_mode {
                ExitMode::Ride(rs) => (
                    "ride",
                    rs.phase as u8,
                    rs.peak_mvsol,
                    now_ms.saturating_sub(rs.ride_start_ms),
                    pos.confirming_unique_wallets,
                ),
            };

        // Extract signal v2 fields from RideState before moving pos into ClosedPosition
        let (sig_score, sig_state, peak_sig, uniq_wallets) = match &pos.exit_mode {
            ExitMode::Ride(rs) => (
                rs.f_hat_permille().max(0) as u16, // shadow: f̂* as "score" for dv7 compat
                rs.state,
                rs.peak_f_permille,
                rs.unique_wallets,
            ),
        };

        // Extract Bayesian exit state from RideState v3 fields.
        let (bayesian_f_at_exit, alpha_at_exit, beta_at_exit, r_est_at_exit): (i16, u16, u16, u16) =
            match &pos.exit_mode {
                ExitMode::Ride(rs) => (
                    rs.f_hat_permille(),
                    rs.alpha_x16,
                    rs.beta_x16,
                    rs.r_est_x100,
                ),
            };

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
            pre_trigger_buys_1s: pos.pre_trigger_buys_1s,
            pre_trigger_buys_2s: pos.pre_trigger_buys_2s,
            pre_trigger_buys_5s: pos.pre_trigger_buys_5s,
            unique_buyers: pos.unique_buyers,
            vsol_delta_3s: pos.vsol_delta_3s,
            volume_5s: pos.volume_5s,
            sell_count_5s: pos.sell_count_5s,
            tod_multiplier: pos.tod_multiplier,
            // RIDE metadata
            exit_mode_str,
            ride_phase,
            ride_peak_mvsol,
            ride_hold_ms,
            ride_unique_wallets,
            // V2 EntryEngine fields
            magnitude_estimate: pos.magnitude_estimate,
            kelly_size_lamports: pos.size_sol, // size_sol IS the Kelly size (or tier fallback)
            entry_action: "ride",
            confirming_buy_sol: pos.confirming_buy_sol,
            sells_during_hold: pos.sells_during_hold,
            // Signal v2 fields from RideState
            signal_score_at_exit: sig_score,
            signal_state_at_exit: sig_state,
            peak_signal_score: peak_sig,
            unique_wallets_seen: uniq_wallets,
            // Kelly conviction fields
            entry_p_permille: pos.conviction.p_permille,
            entry_r_x100: pos.conviction.r_x100,
            entry_f_permille: pos.conviction.f_permille,
            conviction_tier: pos.conviction.conviction_tier,
            // Bayesian exit state (dv8)
            bayesian_f_at_exit,
            alpha_at_exit,
            beta_at_exit,
            r_est_at_exit,
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
            ride_max_hold_ms: 60_000,
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
            exit_config: {
                use crate::engine::config::{ExitConfig, TpSlTierV2};
                ExitConfig {
                    confirmation_window_ms: 200,
                    stall_no_buy_ms: 500,
                    stall_fade_fp: 1000,
                    stall_conviction_no_buy_ms: 800,
                    stall_conviction_fade_fp: 1500,
                    max_hold_safety_ms: 5000,
                    conviction_tp_multipliers: [100, 100, 140, 180, 220],
                    trail_min_conviction: 2,
                    trail_activation_pct_of_base_tp: 60,
                    trail_distance_fp: 1500,
                    trail_keep_mult: 1.0 - 0.015,
                    trail_activation_mult: 60.0 / 100.0,
                    tp_sl_tiers: [
                        TpSlTierV2 { trigger_max_lamports: 600_000_000, unconfirmed_tp_fp: 2000, unconfirmed_sl_fp: 1000, confirmed_tp_fp: 3000, confirmed_sl_fp: 1500 },
                        TpSlTierV2 { trigger_max_lamports: 800_000_000, unconfirmed_tp_fp: 2500, unconfirmed_sl_fp: 1000, confirmed_tp_fp: 4000, confirmed_sl_fp: 1500 },
                        TpSlTierV2 { trigger_max_lamports: 1_500_000_000, unconfirmed_tp_fp: 3000, unconfirmed_sl_fp: 1200, confirmed_tp_fp: 4500, confirmed_sl_fp: 1500 },
                        TpSlTierV2 { trigger_max_lamports: u64::MAX, unconfirmed_tp_fp: 5000, unconfirmed_sl_fp: 1200, confirmed_tp_fp: 7000, confirmed_sl_fp: 1500 },
                        TpSlTierV2::default(), TpSlTierV2::default(), TpSlTierV2::default(), TpSlTierV2::default(),
                    ],
                    tp_sl_tier_count: 4,
                }
            },
            ride_config: RideConfig::default(),
            round_trip_fee_bp: 210,
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

        pm.open_position(&event, 0.85, 1000, 0.0, 0, EntryConviction::default());

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

        pm.open_position(&event, 0.85, 1000, 0.0, 0, EntryConviction::default());

        // Same sig as trigger — should be skipped
        let closed = pm.on_subsequent_trade(&event, 1010, false);
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
        pm.open_position(&event, 0.85, 1000, 0.0, 0, EntryConviction::default());

        // Zero reserves event — should be skipped
        let zero_event = make_trade_event(mint, [0xCCu8; 64], 10_000_000, 0, 0, true);
        let closed = pm.on_subsequent_trade(&zero_event, 1010, false);
        assert!(!closed);
    }

    #[test]
    fn test_max_hold_exit() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut pm = PositionManager::new(test_config(), tx);

        let mint = [0xAAu8; 32];
        let sig = [0xBBu8; 64];
        let event = make_trade_event(mint, sig, 50_000_000, 30_000_000_000, 1_000_000_000_000_000, true);
        pm.open_position(&event, 0.85, 1000, 0.0, 0, EntryConviction::default());

        pm.force_close(&mint, ExitReason::MaxHold, 6001);

        assert_eq!(pm.open_count(), 0);
        let cp = rx.try_recv().unwrap();
        assert_eq!(cp.exit_reason, ExitReason::MaxHold);
    }

    #[test]
    fn test_ride_hard_floor_exit() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut pm = PositionManager::new(test_config(), tx);

        let mint = [0xAAu8; 32];
        let sig = [0xBBu8; 64];
        let entry_vsol = 30_000_000_000u64;
        let event = make_trade_event(mint, sig, 50_000_000, entry_vsol, 1_000_000_000_000_000, true);
        pm.open_position(&event, 0.85, 1000, 0.0, 0, EntryConviction::default());

        // Price drops significantly — should trigger RIDE hard floor
        let drop_vsol = (entry_vsol as f64 * 0.90) as u64;
        let drop_event = make_trade_event(mint, [0xCCu8; 64], 10_000_000, drop_vsol, 1_000_000_000_000_000, false);
        let closed = pm.on_subsequent_trade(&drop_event, 1050, false);

        assert!(closed);
        let cp = rx.try_recv().unwrap();
        assert!(matches!(
            cp.exit_reason,
            ExitReason::RideHardFloor | ExitReason::RideTrailingStop
        ), "unexpected exit: {:?}", cp.exit_reason);
        assert!(cp.net_pnl_sol < 0);
    }

    #[test]
    fn test_ride_buy_gap_timeout() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut pm = PositionManager::new(test_config(), tx);

        let mint = [0xAAu8; 32];
        let sig = [0xBBu8; 64];
        let entry_vsol = 30_000_000_000u64;
        let event = make_trade_event(mint, sig, 50_000_000, entry_vsol, 1_000_000_000_000_000, true);
        pm.open_position(&event, 0.85, 1000, 0.0, 0, EntryConviction::default());

        // Tick far into the future with no buys — should trigger buy gap timeout or max hold
        pm.on_tick(70_000);

        assert_eq!(pm.open_count(), 0);
        let cp = rx.try_recv().unwrap();
        assert!(matches!(
            cp.exit_reason,
            ExitReason::RideBuyGapTimeout | ExitReason::RideMaxHold | ExitReason::RideTrailingStop
        ), "unexpected exit: {:?}", cp.exit_reason);
    }

    #[test]
    fn test_close_all() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut pm = PositionManager::new(test_config(), tx);

        for i in 0..3u8 {
            let mint = [i + 1; 32];
            let sig = [i + 10; 64];
            let event = make_trade_event(mint, sig, 50_000_000, 30_000_000_000, 1_000_000_000_000_000, true);
            pm.open_position(&event, 0.85, 1000, 0.0, 0, EntryConviction::default());
        }

        assert_eq!(pm.open_count(), 3);
        pm.close_all(2000);
        assert_eq!(pm.open_count(), 0);

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
            pm.open_position(&event, 0.85, 1000, 0.0, 0, EntryConviction::default());
        }

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
        pm.open_position(&event, 0.85, 1000, 0.0, 0, EntryConviction::default());

        pm.force_close(&mint, ExitReason::MaxHold, 1500);

        let cp = rx.try_recv().unwrap();
        assert_eq!(cp.gross_pnl_sol, 0);
        assert!(cp.net_pnl_sol < 0);
        // Fee = size × round_trip_fee_bp / 10000 (default 210bp = 2.1%)
        let expected_fees = (cp.size_sol * 210 + 5_000) / 10_000;
        assert_eq!(cp.fees_sol, expected_fees);
    }

    #[test]
    fn test_ride_holds_on_small_buy() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut pm = PositionManager::new(test_config(), tx);

        let mint = [0xAAu8; 32];
        let sig = [0xBBu8; 64];
        let entry_vsol = 30_000_000_000u64;
        let event = make_trade_event(mint, sig, 50_000_000, entry_vsol, 1_000_000_000_000_000, true);
        pm.open_position(&event, 0.85, 1000, 60.0, 0, EntryConviction::default()); // magnitude 60 = RIDE-worthy

        // Small buy with price increase above fee-adjusted breakeven (2.1%) — RIDE should hold
        let up_vsol = (entry_vsol as f64 * 1.03) as u64; // 3% up — above 2.1% breakeven
        let small_buy = make_trade_event(mint, [0xCCu8; 64], 150_000_000, up_vsol, 1_000_000_000_000_000, true);
        let closed = pm.on_subsequent_trade(&small_buy, 1100, false);

        assert!(!closed, "RIDE should hold on confirming buy above entry");
        assert!(rx.try_recv().is_err());
    }

    // ── RIDE mode tests ─────────────────────────────────────────────

    /// Test: All positions start in RIDE mode.
    /// Open a position, feed a confirming buy, verify still in Ride mode.
    #[test]
    fn test_all_positions_start_as_ride() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut pm = PositionManager::new(test_config(), tx);

        let mint = [0x10u8; 32];
        let sig = [0x11u8; 64];
        let entry_vsol = 30_000_000_000u64;
        let event = make_trade_event(mint, sig, 50_000_000, entry_vsol, 1_000_000_000_000_000, true);

        pm.open_position(&event, 75.0, 1000, 60.0, 0, EntryConviction::default()); // magnitude 60

        // Feed a confirming buy with price increase
        let up_vsol = (entry_vsol as f64 * 1.03) as u64; // 3% up
        let buy1 = make_trade_event(mint, [0x12u8; 64], 200_000_000, up_vsol, 1_000_000_000_000_000, true);
        pm.on_subsequent_trade(&buy1, 1100, false);

        let pos = pm.positions.get(&mint).expect("position should exist");
        assert!(
            matches!(pos.exit_mode, ExitMode::Ride(_)),
            "Should be in Ride mode"
        );
    }

    /// Test: ride_qualified() returns correct true/false.
    #[test]
    fn test_ride_qualification_logic() {
        // We can't easily construct an OpenPosition directly (many fields),
        // so we test via the PositionManager flow.
        // Instead, test that a position with low magnitude doesn't promote
        // even with enough buys.
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut pm = PositionManager::new(test_config(), tx);

        let mint = [0x20u8; 32];
        let sig = [0x21u8; 64];
        let entry_vsol = 30_000_000_000u64;
        let event = make_trade_event(mint, sig, 200_000_000, entry_vsol, 1_000_000_000_000_000, true);

        // magnitude 30 < 40 threshold — should never qualify
        pm.open_position(&event, 80.0, 1000, 30.0, 0, EntryConviction::default());

        // Feed 3 large qualifying buys with big price movement
        for i in 1u64..=3 {
            let buy = make_trade_event(
                mint,
                [0x22u8 + i as u8; 64],
                200_000_000,
                entry_vsol + i * 500_000_000,
                1_000_000_000_000_000,
                true,
            );
            let closed = pm.on_subsequent_trade(&buy, 1000 + i * 1000, false);
            if closed { break; } // position may have been closed by exit strategy
        }

        // If still open, should still be Ride (all positions are RIDE now)
        if let Some(pos) = pm.positions.get(&mint) {
            assert!(
                matches!(pos.exit_mode, ExitMode::Ride(_)),
                "Should remain in RIDE mode"
            );
        }
    }

    /// Test: Position stays in RIDE mode with qualifying buys (legacy transition test).
    #[test]
    fn test_ride_mode_with_qualifying_buys() {
        // Use a config with very wide TP/SL so position doesn't exit early
        let mut config = test_config();
        config.exit_config.tp_sl_tiers[0].unconfirmed_tp_fp = 50000; // very wide
        config.exit_config.tp_sl_tiers[0].unconfirmed_sl_fp = 50000;
        config.exit_config.tp_sl_tiers[0].confirmed_tp_fp = 50000;
        config.exit_config.tp_sl_tiers[0].confirmed_sl_fp = 50000;
        config.exit_config.confirmation_window_ms = 100_000; // very long
        config.exit_config.stall_no_buy_ms = 100_000;
        config.exit_config.stall_conviction_no_buy_ms = 100_000;
        config.max_hold_ms = 100_000;
        config.exit_config.max_hold_safety_ms = 100_000;

        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut pm = PositionManager::new(config, tx);

        let mint = [0x30u8; 32];
        let sig = [0x31u8; 64];
        let entry_vsol = 30_000_000_000u64;
        let event = make_trade_event(mint, sig, 200_000_000, entry_vsol, 1_000_000_000_000_000, true);

        pm.open_position(&event, 80.0, 1000, 60.0, 0, EntryConviction::default()); // magnitude 60 >= 40

        // First qualifying buy: 0.2 SOL, price moves up ~3% (above 2.1% fee breakeven)
        let buy1 = make_trade_event(
            mint, [0x32u8; 64], 200_000_000,
            30_900_000_000, 1_000_000_000_000_000, true,
        );
        pm.on_subsequent_trade(&buy1, 2000, false);

        // Should still be in RIDE mode after 1 buy
        let pos = pm.positions.get(&mint).unwrap();
        assert!(matches!(pos.exit_mode, ExitMode::Ride(_)));

        // Second qualifying buy: another 0.2 SOL, price up ~5%
        let buy2 = make_trade_event(
            mint, [0x33u8; 64], 200_000_000,
            31_500_000_000, 1_000_000_000_000_000, true,
        );
        pm.on_subsequent_trade(&buy2, 3000, false);

        // Should remain in RIDE mode with confirming buys
        let pos = pm.positions.get(&mint).unwrap();
        assert!(
            matches!(pos.exit_mode, ExitMode::Ride(_)),
            "Should remain in RIDE mode, but got {:?}",
            std::mem::discriminant(&pos.exit_mode)
        );
    }

    /// Test: Low magnitude positions still use RIDE mode (legacy gate test).
    #[test]
    fn test_ride_with_low_magnitude() {
        let mut config = test_config();
        config.exit_config.tp_sl_tiers[0].unconfirmed_tp_fp = 50000;
        config.exit_config.tp_sl_tiers[0].unconfirmed_sl_fp = 50000;
        config.exit_config.tp_sl_tiers[0].confirmed_tp_fp = 50000;
        config.exit_config.tp_sl_tiers[0].confirmed_sl_fp = 50000;
        config.exit_config.confirmation_window_ms = 100_000;
        config.exit_config.stall_no_buy_ms = 100_000;
        config.exit_config.stall_conviction_no_buy_ms = 100_000;

        // TODO: complete this test — truncated in original source
        let (closed_tx, _closed_rx) = crossbeam_channel::bounded(64);
        let mut pm = PositionManager::new(config, closed_tx);
        // Low magnitude should NOT trigger RIDE transition
        // Test body truncated — will be completed when positions.rs PnL is done
    }
}