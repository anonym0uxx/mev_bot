//! `defense_in_depth` — defense-in-depth controls (§57, §58, §59).
//!
//! Three layers of defense:
//!
//! 1. **Cliff veto**: Catastrophic drawdown veto. If a challenger's max DD
//!    exceeds the cliff threshold (e.g., >50% of bankroll), promotion is
//!    vetoed regardless of other gates. This is a hard stop, not a
//!    statistical test.
//!
//! 2. **Circuit breaker**: Consecutive loss streak breaker. If a strategy
//!    experiences N consecutive losing trades (paper or live), trading is
//!    halted for that strategy until manual reset or cooldown period.
//!
//! 3. **Kill switch**: Manual emergency stop. The operator (Alon) can
//!    freeze all trading immediately. This is a global, not per-strategy,
//!    control. The daemon checks this flag before every trade.
//!
//! ## Constitution compliance
//! - §57: Cliff veto (catastrophic drawdown)
//! - §58: Circuit breaker (consecutive losses)
//! - §59: Kill switch (manual emergency stop)
//! - §22: Integer-only, no floats

use crate::evaluator_state::LifecycleStage;

// ============================================================================
// Cliff Veto
// ============================================================================

/// Cliff veto configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CliffVetoConfig {
    /// Maximum allowed drawdown as a fraction of bankroll in basis points.
    /// e.g., 5000 = 50% of bankroll. If max DD exceeds this, veto.
    pub max_dd_bps: u32,
    /// Bankroll in lamports (e.g., 2 SOL = 2_000_000_000).
    pub bankroll_lamports: i64,
}

impl Default for CliffVetoConfig {
    fn default() -> Self {
        Self {
            max_dd_bps: 5_000, // 50% of bankroll
            bankroll_lamports: 2_000_000_000, // 2 SOL
        }
    }
}

impl CliffVetoConfig {
    /// Compute the cliff threshold in lamports.
    /// threshold = bankroll * max_dd_bps / 10000
    #[must_use]
    pub fn threshold_lamports(&self) -> i64 {
        (self.bankroll_lamports as i128 * self.max_dd_bps as i128 / 10_000) as i64
    }

    /// Check if a drawdown exceeds the cliff threshold.
    /// Returns true if the DD is catastrophic (veto promotion).
    #[must_use]
    pub fn is_catastrophic(&self, max_dd_lamports: i64) -> bool {
        max_dd_lamports > self.threshold_lamports()
    }
}

/// Cliff veto verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CliffVetoVerdict {
    /// True if the veto is triggered (DD exceeds cliff).
    pub vetoed: bool,
    /// The DD that was observed (lamports, always positive).
    pub observed_dd_lamports: i64,
    /// The threshold that was exceeded (lamports).
    pub threshold_lamports: i64,
    /// The fraction of bankroll lost (in bps).
    pub dd_bps: u32,
}

/// Evaluate the cliff veto for a given drawdown.
#[must_use]
pub fn evaluate_cliff_veto(
    max_dd_lamports: i64,
    config: &CliffVetoConfig,
) -> CliffVetoVerdict {
    let threshold = config.threshold_lamports();
    let dd_bps = if config.bankroll_lamports > 0 {
        ((max_dd_lamports.max(0) as i128 * 10_000) / config.bankroll_lamports as i128) as u32
    } else {
        0
    };
    CliffVetoVerdict {
        vetoed: max_dd_lamports > threshold,
        observed_dd_lamports: max_dd_lamports,
        threshold_lamports: threshold,
        dd_bps,
    }
}

// ============================================================================
// Circuit Breaker
// ============================================================================

/// Circuit breaker configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CircuitBreakerConfig {
    /// Maximum consecutive losses before triggering.
    /// e.g., 5 = 5 consecutive losing trades triggers the breaker.
    pub max_consecutive_losses: u32,
    /// Cooldown period in cycles before the breaker auto-resets.
    /// 0 = manual reset only.
    pub cooldown_cycles: u32,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            max_consecutive_losses: 5,
            cooldown_cycles: 10,
        }
    }
}

/// Circuit breaker state for one strategy type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CircuitBreakerState {
    /// Current consecutive loss streak.
    pub consecutive_losses: u32,
    /// Whether the breaker is tripped.
    pub tripped: bool,
    /// Cycle when the breaker was tripped (for cooldown).
    pub tripped_cycle: u64,
    /// Total times the breaker has tripped (for audit).
    pub total_trips: u64,
}

impl CircuitBreakerState {
    /// Record a trade result. Returns true if the breaker trips.
    pub fn record_trade(
        &mut self,
        profitable: bool,
        cycle: u64,
        config: &CircuitBreakerConfig,
    ) -> bool {
        if profitable {
            self.consecutive_losses = 0;
        } else {
            self.consecutive_losses = self.consecutive_losses.saturating_add(1);
            if self.consecutive_losses >= config.max_consecutive_losses && !self.tripped {
                self.tripped = true;
                self.tripped_cycle = cycle;
                self.total_trips = self.total_trips.saturating_add(1);
                return true;
            }
        }
        false
    }

    /// Check if the breaker should auto-reset after cooldown.
    /// Returns true if the breaker was reset.
    pub fn check_cooldown(&mut self, current_cycle: u64, config: &CircuitBreakerConfig) -> bool {
        if !self.tripped || config.cooldown_cycles == 0 {
            return false;
        }
        let cycles_since = current_cycle.saturating_sub(self.tripped_cycle);
        if cycles_since >= config.cooldown_cycles as u64 {
            self.tripped = false;
            self.consecutive_losses = 0;
            return true;
        }
        false
    }

    /// Manual reset of the breaker.
    pub fn reset(&mut self) {
        self.tripped = false;
        self.consecutive_losses = 0;
    }

    /// Whether trading is currently allowed (breaker not tripped).
    #[must_use]
    pub fn trading_allowed(&self) -> bool {
        !self.tripped
    }
}

// ============================================================================
// Kill Switch
// ============================================================================

/// Global kill switch state. This is a singleton — it applies to ALL
/// strategy types and all trading activity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct KillSwitch {
    /// True if the kill switch is active (all trading halted).
    pub active: bool,
    /// Cycle when the kill switch was activated.
    pub activated_cycle: u64,
    /// Reason for activation (free-form tag, limited to 32 chars in practice).
    pub reason_tag: u32, // enum-like: 0=inactive, 1=manual, 2=auto_dd, 3=auto_latency
}

impl KillSwitch {
    /// Activate the kill switch.
    pub fn activate(&mut self, cycle: u64, reason: KillReason) {
        self.active = true;
        self.activated_cycle = cycle;
        self.reason_tag = reason as u32;
    }

    /// Deactivate the kill switch (manual unfreeze).
    pub fn deactivate(&mut self) {
        self.active = false;
        self.reason_tag = 0;
    }

    /// Whether trading is allowed (kill switch not active).
    #[must_use]
    pub fn trading_allowed(&self) -> bool {
        !self.active
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillReason {
    /// Inactive (default).
    Inactive = 0,
    /// Manual activation by operator.
    Manual = 1,
    /// Automatic: catastrophic DD detected.
    AutoDrawdown = 2,
    /// Automatic: RPC latency spike.
    AutoLatency = 3,
    /// Automatic: wallet balance below minimum.
    AutoInsufficientBalance = 4,
}

// ============================================================================
// Combined Defense Check
// ============================================================================

/// Combined defense-in-depth verdict for a strategy type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DefenseVerdict {
    /// True if ANY defense layer blocks trading.
    pub blocked: bool,
    /// True if cliff veto triggered.
    pub cliff_vetoed: bool,
    /// True if circuit breaker tripped.
    pub breaker_tripped: bool,
    /// True if kill switch active.
    pub kill_switch_active: bool,
    /// Lifecycle stage of the strategy (for context).
    pub stage: LifecycleStage,
}

impl DefenseVerdict {
    /// Whether trading is allowed (no defense layer blocks it).
    #[must_use]
    pub fn trading_allowed(&self) -> bool {
        !self.blocked
    }
}

/// Evaluate all three defense layers for a strategy type.
#[must_use]
pub fn evaluate_defense(
    max_dd_lamports: i64,
    cliff_config: &CliffVetoConfig,
    breaker_state: &CircuitBreakerState,
    kill_switch: &KillSwitch,
    stage: LifecycleStage,
) -> DefenseVerdict {
    let cliff = evaluate_cliff_veto(max_dd_lamports, cliff_config);
    let cliff_vetoed = cliff.vetoed;
    let breaker_tripped = breaker_state.tripped;
    let kill_active = kill_switch.active;

    let blocked = cliff_vetoed || breaker_tripped || kill_active;

    DefenseVerdict {
        blocked,
        cliff_vetoed,
        breaker_tripped,
        kill_switch_active: kill_active,
        stage,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cliff_veto_below_threshold() {
        let config = CliffVetoConfig::default();
        // threshold = 2e9 * 5000 / 10000 = 1e9 (1B lamports = 50% of 2 SOL)
        let verdict = evaluate_cliff_veto(500_000_000, &config); // 500M < 1B
        assert!(!verdict.vetoed);
        assert_eq!(verdict.dd_bps, 2500); // 25%
    }

    #[test]
    fn cliff_veto_above_threshold() {
        let config = CliffVetoConfig::default();
        let verdict = evaluate_cliff_veto(1_500_000_000, &config); // 1.5B > 1B
        assert!(verdict.vetoed);
        assert_eq!(verdict.dd_bps, 7500); // 75%
    }

    #[test]
    fn cliff_veto_exact_threshold() {
        let config = CliffVetoConfig::default();
        let threshold = config.threshold_lamports();
        let verdict = evaluate_cliff_veto(threshold, &config);
        // At exactly the threshold, NOT vetoed (strict >).
        assert!(!verdict.vetoed);
    }

    #[test]
    fn circuit_breaker_trips_after_streak() {
        let config = CircuitBreakerConfig::default();
        let mut state = CircuitBreakerState::default();
        // 4 losses: not tripped yet (max = 5)
        for _ in 0..4 {
            let tripped = state.record_trade(false, 0, &config);
            assert!(!tripped);
        }
        assert!(!state.tripped);
        // 5th loss: trips
        let tripped = state.record_trade(false, 0, &config);
        assert!(tripped);
        assert!(state.tripped);
        assert_eq!(state.total_trips, 1);
    }

    #[test]
    fn circuit_breaker_resets_on_profit() {
        let config = CircuitBreakerConfig::default();
        let mut state = CircuitBreakerState::default();
        state.record_trade(false, 0, &config);
        state.record_trade(false, 0, &config);
        state.record_trade(false, 0, &config);
        assert_eq!(state.consecutive_losses, 3);
        // A profitable trade resets the streak.
        state.record_trade(true, 0, &config);
        assert_eq!(state.consecutive_losses, 0);
        assert!(!state.tripped);
    }

    #[test]
    fn circuit_breaker_cooldown_auto_reset() {
        let config = CircuitBreakerConfig {
            max_consecutive_losses: 3,
            cooldown_cycles: 5,
        };
        let mut state = CircuitBreakerState::default();
        // Trip the breaker at cycle 10.
        for _ in 0..3 {
            state.record_trade(false, 10, &config);
        }
        assert!(state.tripped);
        // Cycle 12: not yet cooled down (need 5 cycles = cycle 15).
        state.check_cooldown(12, &config);
        assert!(state.tripped);
        // Cycle 15: cooldown complete.
        state.check_cooldown(15, &config);
        assert!(!state.tripped);
    }

    #[test]
    fn circuit_breaker_manual_reset() {
        let config = CircuitBreakerConfig::default();
        let mut state = CircuitBreakerState::default();
        for _ in 0..5 {
            state.record_trade(false, 0, &config);
        }
        assert!(state.tripped);
        state.reset();
        assert!(!state.tripped);
        assert_eq!(state.consecutive_losses, 0);
    }

    #[test]
    fn kill_switch_activate_deactivate() {
        let mut ks = KillSwitch::default();
        assert!(ks.trading_allowed());
        ks.activate(42, KillReason::Manual);
        assert!(!ks.trading_allowed());
        assert_eq!(ks.activated_cycle, 42);
        assert_eq!(ks.reason_tag, KillReason::Manual as u32);
        ks.deactivate();
        assert!(ks.trading_allowed());
    }

    #[test]
    fn combined_defense_all_clear() {
        let verdict = evaluate_defense(
            100_000, // small DD
            &CliffVetoConfig::default(),
            &CircuitBreakerState::default(), // not tripped
            &KillSwitch::default(), // not active
            LifecycleStage::ShadowValidated,
        );
        assert!(!verdict.blocked);
        assert!(verdict.trading_allowed());
    }

    #[test]
    fn combined_defense_cliff_veto_blocks() {
        let verdict = evaluate_defense(
            2_000_000_000, // 100% DD = catastrophic
            &CliffVetoConfig::default(),
            &CircuitBreakerState::default(),
            &KillSwitch::default(),
            LifecycleStage::ShadowValidated,
        );
        assert!(verdict.blocked);
        assert!(verdict.cliff_vetoed);
        assert!(!verdict.trading_allowed());
    }

    #[test]
    fn combined_defense_breaker_blocks() {
        let mut breaker = CircuitBreakerState::default();
        let config = CircuitBreakerConfig::default();
        for _ in 0..5 {
            breaker.record_trade(false, 0, &config);
        }
        let verdict = evaluate_defense(
            100_000,
            &CliffVetoConfig::default(),
            &breaker,
            &KillSwitch::default(),
            LifecycleStage::ShadowValidated,
        );
        assert!(verdict.blocked);
        assert!(verdict.breaker_tripped);
    }

    #[test]
    fn combined_defense_kill_switch_blocks() {
        let mut ks = KillSwitch::default();
        ks.activate(1, KillReason::Manual);
        let verdict = evaluate_defense(
            100_000,
            &CliffVetoConfig::default(),
            &CircuitBreakerState::default(),
            &ks,
            LifecycleStage::Champion,
        );
        assert!(verdict.blocked);
        assert!(verdict.kill_switch_active);
        // Even a champion can't trade with kill switch active.
        assert!(!verdict.trading_allowed());
    }

    #[test]
    fn zero_cooldown_means_manual_only() {
        let config = CircuitBreakerConfig {
            max_consecutive_losses: 2,
            cooldown_cycles: 0, // manual reset only
        };
        let mut state = CircuitBreakerState::default();
        state.record_trade(false, 0, &config);
        state.record_trade(false, 0, &config);
        assert!(state.tripped);
        // Check cooldown at a much later cycle — should NOT auto-reset.
        state.check_cooldown(1000, &config);
        assert!(state.tripped);
        // Manual reset works.
        state.reset();
        assert!(!state.tripped);
    }
}
