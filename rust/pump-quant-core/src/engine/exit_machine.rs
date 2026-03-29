// engine/exit_machine.rs — Signal-based exit state machine.
// Zero heap allocation. All fields Copy. Target: ≤64 bytes, ≤100ns per tick.

// Re-use ExitConfig and TpSlTierV2 from config.rs (single source of truth).
pub use crate::engine::config::{ExitConfig, TpSlTierV2};

/// Conviction level: how many confirming buys arrived after entry. 0–4, clamped.
pub type ConvictionLevel = u8;

/// Exit state machine state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitState {
    /// No confirming buy yet. Position may be dead.
    Unconfirmed,
    /// At least 1 confirming buy. Momentum confirmed.
    Confirmed,
    /// 2+ confirming buys. TP scaled up by conviction multiplier.
    ConvictionScaled { level: ConvictionLevel },
}

/// Result of a state machine tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExitDecision {
    Hold,
    Exit(ExitReasonNew),
}

/// New exit reasons — Engineer B maps these back to the existing ExitReason enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReasonNew {
    TakeProfit,
    TakeProfitScaled,
    StopLoss,
    MomentumDecayFlat,
    MomentumStall,
    TrailingStop,
    MaxHoldSafety,
}

/// The state machine. Embedded inline in Position — no heap.
///
/// Layout (with confirmed_at_ms and trail_stop_price eliminated):
///   state(1) + conviction_level(1) + pad(2) + base_confirmed_tp_fp(4) = 8
///   entry_price_vsol(8) + peak_price_vsol(8) + current_tp_vsol(8) + current_sl_vsol(8) = 32
///   entry_time_ms(8) + last_buy_time_ms(8) = 16
///   trail_active(1) + _pad(3) = 4 → but placed in the first 8-byte group
///   Total: 8 + 32 + 16 + 4 = 56 bytes (with padding: ≤64)
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ExitStateMachine {
    // --- 8 bytes ---
    pub state: ExitState,           // 1
    pub conviction_level: u8,       // 1
    pub trail_active: bool,         // 1
    pub tier_index: u8,             // 1 — index into config.tp_sl_tiers
    pub base_confirmed_tp_fp: u32,  // 4

    // --- 32 bytes ---
    pub entry_price_vsol: f64,
    pub peak_price_vsol: f64,
    pub current_tp_vsol: f64,
    pub current_sl_vsol: f64,

    // --- 16 bytes ---
    pub entry_time_ms: u64,
    pub last_buy_time_ms: u64,      // 0 = no buy yet
}

const _SIZE_CHECK: () = assert!(std::mem::size_of::<ExitStateMachine>() <= 64);

impl ExitStateMachine {
    /// Create a new state machine on position entry.
    #[inline]
    pub fn on_entry(
        config: &ExitConfig,
        trigger_lamports: u64,
        entry_vsol: f64,
        now_ms: u64,
    ) -> Self {
        debug_assert!(entry_vsol > 0.0, "entry_vsol must be positive, got {entry_vsol}");

        let tiers = &config.tp_sl_tiers[..config.tp_sl_tier_count as usize];
        let idx = Self::find_tier_index(tiers, trigger_lamports);
        let tier = &tiers[idx as usize];

        let tp_pct = tier.unconfirmed_tp_fp as f64 / 100_000.0;
        let sl_pct = tier.unconfirmed_sl_fp as f64 / 100_000.0;

        Self {
            state: ExitState::Unconfirmed,
            conviction_level: 0,
            trail_active: false,
            tier_index: idx,
            base_confirmed_tp_fp: tier.confirmed_tp_fp,
            entry_price_vsol: entry_vsol,
            peak_price_vsol: entry_vsol,
            current_tp_vsol: entry_vsol * (1.0 + tp_pct),
            current_sl_vsol: entry_vsol * (1.0 - sl_pct),
            entry_time_ms: now_ms,
            last_buy_time_ms: 0,
        }
    }

    /// Process a buy event for this position's token. Must be ≤100ns.
    #[inline]
    pub fn on_buy_event(&mut self, config: &ExitConfig, current_vsol: f64, now_ms: u64) -> ExitDecision {
        self.last_buy_time_ms = now_ms;

        match self.state {
            ExitState::Unconfirmed => {
                // Only confirm if not underwater — buy while underwater is not momentum confirmation
                if current_vsol < self.entry_price_vsol * 0.995 {
                    return ExitDecision::Hold;
                }
                self.conviction_level = 1;
                self.state = ExitState::Confirmed;
                // Upgrade TP/SL to confirmed levels
                self._apply_conviction_tp(config, 1);
                // Also widen SL to confirmed level
                let confirmed_sl_pct = self._confirmed_sl_pct(config);
                self.current_sl_vsol = self.entry_price_vsol * (1.0 - confirmed_sl_pct);
                ExitDecision::Hold
            }
            ExitState::Confirmed | ExitState::ConvictionScaled { .. } => {
                let new_level = (self.conviction_level + 1).min(4);
                if new_level != self.conviction_level {
                    self.conviction_level = new_level;
                    if new_level >= 2 {
                        self.state = ExitState::ConvictionScaled { level: new_level };
                        self._apply_conviction_tp(config, new_level);
                    }
                }
                ExitDecision::Hold
            }
        }
    }

    /// Process a price update (vSol change). Must be ≤100ns.
    #[inline]
    pub fn on_price_tick(
        &mut self,
        config: &ExitConfig,
        current_vsol: f64,
        now_ms: u64,
    ) -> ExitDecision {
        // Update high water mark
        if current_vsol > self.peak_price_vsol {
            self.peak_price_vsol = current_vsol;
        }

        // 1. SL check (always, any state)
        if current_vsol <= self.current_sl_vsol {
            return ExitDecision::Exit(ExitReasonNew::StopLoss);
        }

        // 2. TP check
        if current_vsol >= self.current_tp_vsol {
            return ExitDecision::Exit(match self.state {
                ExitState::ConvictionScaled { .. } => ExitReasonNew::TakeProfitScaled,
                _ => ExitReasonNew::TakeProfit,
            });
        }

        match self.state {
            ExitState::Unconfirmed => {
                // 3. Confirmation window expired with no buy?
                let elapsed = now_ms.saturating_sub(self.entry_time_ms);
                if elapsed >= config.confirmation_window_ms && self.last_buy_time_ms == 0 {
                    return ExitDecision::Exit(ExitReasonNew::MomentumDecayFlat);
                }
            }
            ExitState::Confirmed => {
                // 4. Momentum stall check
                if self.last_buy_time_ms > 0 {
                    let since_last_buy = now_ms.saturating_sub(self.last_buy_time_ms);
                    if since_last_buy >= config.stall_no_buy_ms {
                        let fade_threshold = self.peak_price_vsol
                            * (1.0 - config.stall_fade_fp as f64 / 100_000.0);
                        if current_vsol < fade_threshold {
                            return ExitDecision::Exit(ExitReasonNew::MomentumStall);
                        }
                    }
                }
            }
            ExitState::ConvictionScaled { level } => {
                // 5. Conviction stall (more generous thresholds)
                if self.last_buy_time_ms > 0 {
                    let since_last_buy = now_ms.saturating_sub(self.last_buy_time_ms);
                    if since_last_buy >= config.stall_conviction_no_buy_ms {
                        let fade_threshold = self.peak_price_vsol
                            * (1.0 - config.stall_conviction_fade_fp as f64 / 100_000.0);
                        if current_vsol < fade_threshold {
                            return ExitDecision::Exit(ExitReasonNew::MomentumStall);
                        }
                    }
                }

                // 6. Trailing stop (conviction >= trail_min_conviction)
                if level >= config.trail_min_conviction {
                    let base_tp_pct = self.base_confirmed_tp_fp as f64 / 100_000.0;
                    let activation_pct =
                        base_tp_pct * config.trail_activation_mult;
                    let activation_price = self.entry_price_vsol * (1.0 + activation_pct);

                    if current_vsol >= activation_price || self.trail_active {
                        self.trail_active = true;
                        // Compute trail stop from peak inline (no stored field)
                        let trail_stop = self.peak_price_vsol * config.trail_keep_mult;
                        if current_vsol <= trail_stop {
                            return ExitDecision::Exit(ExitReasonNew::TrailingStop);
                        }
                    }
                }
            }
        }

        ExitDecision::Hold
    }

    /// Called by the safety timer. Always returns MaxHoldSafety.
    #[inline]
    pub fn on_safety_timeout(&self) -> ExitReasonNew {
        ExitReasonNew::MaxHoldSafety
    }

    /// Recompute current_tp_vsol from base_confirmed_tp_fp × conviction multiplier.
    #[inline(always)]
    fn _apply_conviction_tp(&mut self, config: &ExitConfig, level: u8) {
        let level_idx = level.min(4) as usize;
        let multiplier = config.conviction_tp_multipliers[level_idx] as f64 / 100.0;
        let base_tp_pct = self.base_confirmed_tp_fp as f64 / 100_000.0;
        let scaled_tp_pct = base_tp_pct * multiplier;
        self.current_tp_vsol = self.entry_price_vsol * (1.0 + scaled_tp_pct);
    }

    /// Find the matching TP/SL tier index for a given trigger size.
    #[inline(always)]
    fn find_tier_index(tiers: &[TpSlTierV2], trigger_lamports: u64) -> u8 {
        for (i, t) in tiers.iter().enumerate() {
            if trigger_lamports <= t.trigger_max_lamports {
                return i as u8;
            }
        }
        tiers.len().saturating_sub(1) as u8
    }

    /// Helper: get the confirmed SL pct from the tier stored at entry (O(1) via tier_index).
    #[inline(always)]
    fn _confirmed_sl_pct(&self, config: &ExitConfig) -> f64 {
        let tier = &config.tp_sl_tiers[self.tier_index as usize];
        tier.confirmed_sl_fp as f64 / 100_000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ExitConfig {
        let mut tiers = [TpSlTierV2::default(); 8];
        tiers[0] = TpSlTierV2 {
            trigger_max_lamports: 600_000_000,  // 0.6 SOL
            unconfirmed_tp_fp: 2000,            // 2.0%
            unconfirmed_sl_fp: 1000,            // 1.0%
            confirmed_tp_fp: 3000,              // 3.0%
            confirmed_sl_fp: 1500,              // 1.5%
        };
        tiers[1] = TpSlTierV2 {
            trigger_max_lamports: 800_000_000,  // 0.8 SOL
            unconfirmed_tp_fp: 2500,
            unconfirmed_sl_fp: 1000,
            confirmed_tp_fp: 4000,
            confirmed_sl_fp: 1500,
        };

        ExitConfig {
            confirmation_window_ms: 200,
            stall_no_buy_ms: 500,
            stall_fade_fp: 1000, // 1.0%
            stall_conviction_no_buy_ms: 800,
            stall_conviction_fade_fp: 1500, // 1.5%
            max_hold_safety_ms: 5000,
            conviction_tp_multipliers: [100, 100, 140, 180, 220],
            trail_min_conviction: 2,
            trail_activation_pct_of_base_tp: 60,
            trail_distance_fp: 1500, // 1.5%
            trail_keep_mult: 1.0 - 0.015,       // 0.985
            trail_activation_mult: 60.0 / 100.0, // 0.6
            tp_sl_tiers: tiers,
            tp_sl_tier_count: 2,
        }
    }

    // Test 1: on_buy_event transitions Unconfirmed → Confirmed
    #[test]
    fn test_buy_event_unconfirmed_to_confirmed() {
        let config = test_config();
        let mut sm = ExitStateMachine::on_entry(&config, 500_000_000, 100.0, 1000);

        assert_eq!(sm.state, ExitState::Unconfirmed);
        assert_eq!(sm.conviction_level, 0);

        // Initial TP should be unconfirmed: 100 * 1.02 = 102.0
        assert!((sm.current_tp_vsol - 102.0).abs() < 0.001);

        // Pass current_vsol above entry (100.0 * 1.01 = 101.0) so price guard passes
        let decision = sm.on_buy_event(&config, 100.0 * 1.01, 1050);

        assert_eq!(decision, ExitDecision::Hold);
        assert_eq!(sm.state, ExitState::Confirmed);
        assert_eq!(sm.conviction_level, 1);

        // TP should now be confirmed level: 100 * (1 + 0.03 * 1.0) = 103.0
        assert!(
            (sm.current_tp_vsol - 103.0).abs() < 0.001,
            "expected ~103.0, got {}",
            sm.current_tp_vsol,
        );
    }

    // Test: Buy event while underwater does NOT confirm
    #[test]
    fn test_buy_event_underwater_no_confirm() {
        let config = test_config();
        let mut sm = ExitStateMachine::on_entry(&config, 500_000_000, 100.0, 1000);

        assert_eq!(sm.state, ExitState::Unconfirmed);

        // Pass current_vsol well below entry (100.0 * 0.99 = 99.0) — underwater
        let decision = sm.on_buy_event(&config, 100.0 * 0.99, 1050);

        assert_eq!(decision, ExitDecision::Hold);
        // Should remain Unconfirmed — price guard blocked confirmation
        assert_eq!(sm.state, ExitState::Unconfirmed);
        assert_eq!(sm.conviction_level, 0);
    }

    // Test 2: Confirmation window expiry kills dead position
    #[test]
    fn test_confirmation_window_expiry() {
        let config = test_config();
        let mut sm = ExitStateMachine::on_entry(&config, 500_000_000, 100.0, 1000);

        // At t=1199 (199ms elapsed), should still hold
        let d1 = sm.on_price_tick(&config, 100.0, 1199);
        assert_eq!(d1, ExitDecision::Hold);

        // At t=1201 (201ms elapsed), no buys → MomentumDecayFlat
        let d2 = sm.on_price_tick(&config, 100.0, 1201);
        assert_eq!(d2, ExitDecision::Exit(ExitReasonNew::MomentumDecayFlat));
    }

    // Test 3: Conviction scaling — TP increases per buy
    #[test]
    fn test_conviction_tp_scaling() {
        let config = test_config();
        let mut sm = ExitStateMachine::on_entry(&config, 500_000_000, 100.0, 1000);

        // Buy 1: Unconfirmed → Confirmed (conviction=1, multiplier=1.0)
        sm.on_buy_event(&config, 101.0, 1050);
        assert_eq!(sm.conviction_level, 1);
        // TP = 100 * (1 + 0.03 * 1.0) = 103.0
        assert!((sm.current_tp_vsol - 103.0).abs() < 0.001);

        // Buy 2: conviction=2, multiplier=1.4
        sm.on_buy_event(&config, 101.0, 1100);
        assert_eq!(sm.conviction_level, 2);
        assert_eq!(sm.state, ExitState::ConvictionScaled { level: 2 });
        // TP = 100 * (1 + 0.03 * 1.4) = 104.2
        assert!(
            (sm.current_tp_vsol - 104.2).abs() < 0.001,
            "expected ~104.2, got {}",
            sm.current_tp_vsol,
        );

        // Buy 3: conviction=3, multiplier=1.8
        sm.on_buy_event(&config, 101.0, 1150);
        assert_eq!(sm.conviction_level, 3);
        // TP = 100 * (1 + 0.03 * 1.8) = 105.4
        assert!(
            (sm.current_tp_vsol - 105.4).abs() < 0.001,
            "expected ~105.4, got {}",
            sm.current_tp_vsol,
        );
    }

    // Test 4: Trailing stop activates at conviction >= 2
    #[test]
    fn test_trailing_stop_activation() {
        let config = test_config();
        let mut sm = ExitStateMachine::on_entry(&config, 500_000_000, 100.0, 1000);

        // Get to conviction=2
        sm.on_buy_event(&config, 101.0, 1050);
        sm.on_buy_event(&config, 101.0, 1100);
        assert_eq!(sm.conviction_level, 2);

        // Trail activation: base_tp_pct = 3.0%, activation = 3.0% * 60% = 1.8%
        // activation_price = 100 * 1.018 = 101.8
        // Push price above activation to set trail_active
        let d1 = sm.on_price_tick(&config, 102.5, 1200);
        assert_eq!(d1, ExitDecision::Hold);
        assert!(sm.trail_active);
        // peak is now 102.5

        // Trail distance = 1.5%, trail_stop = 102.5 * (1 - 0.015) = 100.9625
        // Price drops to 100.95 — below trail stop
        let d2 = sm.on_price_tick(&config, 100.95, 1300);
        assert_eq!(d2, ExitDecision::Exit(ExitReasonNew::TrailingStop));
    }

    // Test 5: Safety timeout always exits
    #[test]
    fn test_safety_timeout() {
        let config = test_config();
        let sm = ExitStateMachine::on_entry(&config, 500_000_000, 100.0, 1000);

        assert_eq!(sm.on_safety_timeout(), ExitReasonNew::MaxHoldSafety);

        // Also works from confirmed state
        let mut sm2 = ExitStateMachine::on_entry(&config, 500_000_000, 100.0, 1000);
        sm2.on_buy_event(&config, 101.0, 1050);
        assert_eq!(sm2.on_safety_timeout(), ExitReasonNew::MaxHoldSafety);
    }
}
