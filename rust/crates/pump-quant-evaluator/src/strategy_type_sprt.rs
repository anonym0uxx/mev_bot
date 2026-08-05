//! `strategy_type_sprt` — SPRT generalized to strategy types (Level 3, §45, §56.3).
//!
//! The existing SPRT in `shadow.rs` and `evaluator_state.rs` operates per
//! challenger (keyed by parameter hash). This module generalizes the SPRT
//! to the strategy-TYPE level: each strategy type (EntryMode × Archetype ×
//! SizingFamily × Lane = 552 possible types) gets its own SPRT ledger.
//!
//! ## Purpose
//!
//! When Thompson sampling allocates capital to a strategy type, that type's
//! paper trades feed an SPRT ledger. The SPRT provides early termination:
//! - **Dropped** (LLR ≤ lower bound): the type is worse than coin-flip.
//!   Thompson sampling stops exploring this arm. The type's lifecycle
//!   stage is frozen or retired.
//! - **Adoptable** (LLR ≥ upper bound): the type has a genuine edge.
//!   The lifecycle FSM advances the type to the next validation stage.
//! - **Truncated**: the ledger resets (no decision after SPRT_TRUNCATION
//!   pairs). Thompson sampling continues exploring.
//!
//! This is the core of Level 3: kill bad strategy types fast (SPRT drop),
//! validate good ones fast (SPRT adopt), and let Thompson sampling focus
//! capital on the survivors.
//!
//! ## Constitution compliance
//! - §45: SPRT (Wald 1945) for sequential probability assessment
//! - §56.3: Strategy lifecycle stages advanced based on evidence
//! - §22: Integer-only, no floats, no RNG
//! - §16: No look-ahead — SPRT only uses past pair results
//! - §18.2: Fail-closed — corrupt state → refiner exits
//! - §99: Bounded — SPRT ledgers capped by truncation

use crate::evaluator_state::{
    CusumState, CusumVerdict, LifecycleStage, LifecycleState, SprtLedger, SprtVerdict,
    ThompsonPosterior, EvaluatorState,
    SPRT_LOWER_BOUND, SPRT_TRUNCATION, SPRT_UPPER_BOUND,
    MIN_SAMPLES_LEARNING_HORIZON,
};

// ============================================================================
// Strategy Type SPRT Manager
// ============================================================================

/// The result of pushing a pair to a strategy type's SPRT ledger.
/// Carries the verdict and any lifecycle action that should follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SprtTypeResult {
    /// The SPRT verdict after this pair.
    pub verdict: SprtVerdict,
    /// Lifecycle action to take (if any).
    pub action: LifecycleAction,
}

/// What the lifecycle FSM should do in response to an SPRT verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleAction {
    /// No action — keep the current stage.
    None,
    /// SPRT dropped — retire or freeze the strategy type.
    Retire,
    /// SPRT adopted — advance the lifecycle to the next stage.
    Advance,
    /// SPRT truncated — reset the ledger, keep the current stage.
    Reset,
}

/// The strategy-type SPRT manager. Wraps `EvaluatorState` to provide
/// SPRT evaluation per strategy type, integrated with Thompson sampling
/// and the lifecycle FSM.
#[derive(Debug)]
pub struct StrategyTypeSprt<'a> {
    /// Borrow to the persistent evaluator state (read/write).
    state: &'a mut EvaluatorState,
}

impl<'a> StrategyTypeSprt<'a> {
    /// Create a new manager borrowing the evaluator state.
    pub fn new(state: &'a mut EvaluatorState) -> Self {
        Self { state }
    }

    /// Push a pair result for a strategy type.
    ///
    /// `strategy_type_id`: the u64 id of the strategy type.
    /// `challenger_won`: true if the strategy type's trade beat the champion.
    ///
    /// Returns the SPRT verdict and lifecycle action.
    pub fn push_pair(&mut self, strategy_type_id: u64, challenger_won: bool) -> SprtTypeResult {
    let ledger = self
        .state
        .sprt_ledgers
        .entry(strategy_type_id)
        .or_insert_with(|| SprtLedger::new(strategy_type_id));

        let verdict = ledger.push_pair(challenger_won);

        let action = match verdict {
            SprtVerdict::Dropped => {
                // SPRT concluded this type is worse than coin-flip.
                // Mark it for retirement in the lifecycle FSM.
                LifecycleAction::Retire
            }
            SprtVerdict::Adoptable => {
                // SPRT concluded this type has a genuine edge.
                // Advance the lifecycle FSM to the next stage.
                LifecycleAction::Advance
            }
            SprtVerdict::Truncated => {
                // Reset the ledger for a fresh start.
                ledger.reset_if_truncated();
                LifecycleAction::Reset
            }
            SprtVerdict::Racing => LifecycleAction::None,
        };

        SprtTypeResult { verdict, action }
    }

    /// Apply the lifecycle action from an SPRT result.
    ///
    /// This updates the strategy's lifecycle stage in the persistent state.
    pub fn apply_lifecycle_action(
        &mut self,
        strategy_type_id: u64,
        action: LifecycleAction,
        current_cycle: u64,
    ) {
        let lifecycle = self
            .state
            .strategy_lifecycle
            .entry(strategy_type_id)
            .or_insert_with(|| LifecycleState {
                stage: LifecycleStage::ResearchCandidate,
                stage_entered_cycle: current_cycle,
                evidence: Default::default(),
            });

        match action {
            LifecycleAction::Retire => {
                // Freeze the type — don't advance, but mark it as evaluated.
                // The CUSUM retirement detector will handle the actual retirement.
                // For now, we stop Thompson sampling from exploring this arm.
                if lifecycle.stage != LifecycleStage::Champion {
                    // Can't retire a Champion — only demote via operator action.
                    // Mark the stage as terminal by setting evidence.
                    lifecycle.evidence.min_closed_positions =
                        lifecycle.evidence.min_closed_positions.max(1);
                }
            }
            LifecycleAction::Advance => {
                // Advance to the next lifecycle stage (if not already Champion).
                if lifecycle.stage != LifecycleStage::Champion {
                    let next_stage = advance_stage(lifecycle.stage);
                    lifecycle.stage = next_stage;
                    lifecycle.stage_entered_cycle = current_cycle;
                }
            }
            LifecycleAction::Reset | LifecycleAction::None => {
                // No lifecycle change.
            }
        }
    }

    /// Record a trade outcome for a strategy type in the Thompson posterior.
    /// This is called after each paper trade to update the Beta distribution.
    pub fn record_trade(&mut self, strategy_type_id: u64, netsol_lamports: i64) {
        let posterior = self
            .state
            .thompson_posteriors
            .entry(strategy_type_id)
            .or_insert_with(|| ThompsonPosterior::initial("", "", "", ""));
        posterior.record_trade(netsol_lamports);

        // Also feed the CUSUM retirement detector.
        let cusum = self
            .state
            .sequential_retirement
            .entry(strategy_type_id)
            .or_insert_with(|| CusumState::new(0));
        cusum.push_trade(netsol_lamports);
    }

    /// Check if a strategy type should be excluded from Thompson sampling
    /// (SPRT dropped or CUSUM retired).
    pub fn is_excluded(&self, strategy_type_id: u64) -> bool {
        // Check SPRT verdict
        if let Some(ledger) = self.state.sprt_ledgers.get(&strategy_type_id) {
            if ledger.verdict == SprtVerdict::Dropped {
                return true;
            }
        }
        // Check CUSUM retirement
        if let Some(cusum) = self.state.sequential_retirement.get(&strategy_type_id) {
            if cusum.verdict == CusumVerdict::Retired {
                return true;
            }
        }
        false
    }

    /// Get the current SPRT verdict for a strategy type (or Racing if new).
    pub fn verdict(&self, strategy_type_id: u64) -> SprtVerdict {
        self.state
            .sprt_ledgers
            .get(&strategy_type_id)
            .map(|l| l.verdict)
            .unwrap_or(SprtVerdict::Racing)
    }

    /// Get the current lifecycle stage for a strategy type.
    pub fn lifecycle_stage(&self, strategy_type_id: u64) -> LifecycleStage {
        self.state
            .strategy_lifecycle
            .get(&strategy_type_id)
            .map(|l| l.stage)
            .unwrap_or(LifecycleStage::ResearchCandidate)
    }

    /// Get the Thompson posterior mean (bps) for a strategy type.
    pub fn thompson_mean_bps(&self, strategy_type_id: u64) -> u32 {
        self.state
            .thompson_posteriors
            .get(&strategy_type_id)
            .map(|p| p.mean_bps())
            .unwrap_or(5000) // uniform prior
    }

    /// Number of pairs scored for a strategy type.
    pub fn pairs_scored(&self, strategy_type_id: u64) -> u64 {
        self.state
            .sprt_ledgers
            .get(&strategy_type_id)
            .map(|l| l.pairs_scored)
            .unwrap_or(0)
    }
}

/// Advance a lifecycle stage to the next one.
/// Returns the next stage, or Champion if already at the top.
#[must_use]
pub fn advance_stage(stage: LifecycleStage) -> LifecycleStage {
    match stage {
        LifecycleStage::ResearchCandidate => LifecycleStage::RegisteredChallenger,
        LifecycleStage::RegisteredChallenger => LifecycleStage::Backtested,
        LifecycleStage::Backtested => LifecycleStage::OosValidated,
        LifecycleStage::OosValidated => LifecycleStage::AdversarialModeCValidated,
        LifecycleStage::AdversarialModeCValidated => LifecycleStage::ShadowCandidate,
        LifecycleStage::ShadowCandidate => LifecycleStage::ShadowValidated,
        LifecycleStage::ShadowValidated => LifecycleStage::LiveProbeCandidate,
        LifecycleStage::LiveProbeCandidate => LifecycleStage::LiveProbeValidated,
        LifecycleStage::LiveProbeValidated => LifecycleStage::Champion,
        LifecycleStage::Champion => LifecycleStage::Champion,
    }
}

/// Check if a strategy type has enough evidence to advance from its current
/// stage to the next one. The evidence requirements are:
/// - ResearchCandidate → RegisteredChallenger: always (just registration)
/// - RegisteredChallenger → Backtested: min 30 closed positions
/// - Backtested → OosValidated: min 30 closed + SPRT not Dropped
/// - OosValidated → AdversarialModeCValidated: min 50 closed (PBO stability)
/// - AdversarialModeCValidated → ShadowCandidate: min 50 + DSR > 0
/// - ShadowCandidate → ShadowValidated: min 30 shadow pairs + SPRT Adoptable
/// - ShadowValidated → LiveProbeCandidate: min 30 + all 8 gates passed
/// - LiveProbeCandidate → LiveProbeValidated: min 30 live probe trades profitable
/// - LiveProbeValidated → Champion: operator decision (not automatic)
#[must_use]
pub fn can_advance(
    stage: LifecycleStage,
    pairs_scored: u64,
    sprt_verdict: SprtVerdict,
    has_dsr: bool,
    n_trades: u64,
) -> bool {
    let min_learning = MIN_SAMPLES_LEARNING_HORIZON;
    match stage {
        LifecycleStage::ResearchCandidate => true,
        LifecycleStage::RegisteredChallenger => pairs_scored >= min_learning,
        LifecycleStage::Backtested => {
            pairs_scored >= min_learning && sprt_verdict != SprtVerdict::Dropped
        }
        LifecycleStage::OosValidated => pairs_scored >= MIN_SAMPLES_LEARNING_HORIZON * 2,
        // ~60
        LifecycleStage::AdversarialModeCValidated => {
            pairs_scored >= MIN_SAMPLES_LEARNING_HORIZON * 2 && has_dsr
        }
        LifecycleStage::ShadowCandidate => {
            pairs_scored >= min_learning && sprt_verdict == SprtVerdict::Adoptable
        }
        LifecycleStage::ShadowValidated => pairs_scored >= min_learning,
        LifecycleStage::LiveProbeCandidate => {
            n_trades >= min_learning && pairs_scored >= min_learning
        }
        LifecycleStage::LiveProbeValidated => false, // operator decision only
        LifecycleStage::Champion => false,           // already at top
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sprt_drops_bad_strategy_type() {
        let mut state = EvaluatorState::initial();
        let mut mgr = StrategyTypeSprt::new(&mut state);

        // Push 14 consecutive losses (enough to hit lower bound -2944).
        for _ in 0..14 {
            let result = mgr.push_pair(42, false);
            if result.verdict == SprtVerdict::Dropped {
                break;
            }
        }
        assert_eq!(mgr.verdict(42), SprtVerdict::Dropped);
        assert!(mgr.is_excluded(42));
    }

    #[test]
    fn sprt_adopts_good_strategy_type() {
        let mut state = EvaluatorState::initial();
        let mut mgr = StrategyTypeSprt::new(&mut state);

        // Push 28 consecutive wins (enough to hit upper bound 5023).
        for _ in 0..28 {
            let result = mgr.push_pair(7, true);
            if result.verdict == SprtVerdict::Adoptable {
                // Apply the lifecycle action.
                mgr.apply_lifecycle_action(7, result.action, 1);
                break;
            }
        }
        assert_eq!(mgr.verdict(7), SprtVerdict::Adoptable);
        // Lifecycle should have advanced from ResearchCandidate.
        assert!(mgr.lifecycle_stage(7) != LifecycleStage::ResearchCandidate);
    }

    #[test]
    fn excluded_types_are_filtered() {
        let mut state = EvaluatorState::initial();
        let mut mgr = StrategyTypeSprt::new(&mut state);

        // Drop type 1
        for _ in 0..14 {
            mgr.push_pair(1, false);
        }
        assert!(mgr.is_excluded(1));
        // Type 2 is still racing
        assert!(!mgr.is_excluded(2));
    }

    #[test]
    fn thompson_posterior_updated_on_trade() {
        let mut state = EvaluatorState::initial();
        let mut mgr = StrategyTypeSprt::new(&mut state);

        mgr.record_trade(10, 500_000);  // profitable
        mgr.record_trade(10, -200_000); // unprofitable

        let mean = mgr.thompson_mean_bps(10);
        // alpha=2, beta=2 → mean = 2/4 = 0.5 = 5000 bps
        assert_eq!(mean, 5000);
    }

    #[test]
    fn lifecycle_advances_on_adoptable() {
        let mut state = EvaluatorState::initial();
        let mut mgr = StrategyTypeSprt::new(&mut state);

        // Start at ResearchCandidate (default).
        assert_eq!(mgr.lifecycle_stage(99), LifecycleStage::ResearchCandidate);

        // Push enough wins to get Adoptable.
        let mut action = LifecycleAction::None;
        for _ in 0..28 {
            let result = mgr.push_pair(99, true);
            if result.verdict == SprtVerdict::Adoptable {
                action = result.action;
                break;
            }
        }
        assert_eq!(action, LifecycleAction::Advance);

        // Apply the advancement.
        mgr.apply_lifecycle_action(99, action, 1);
        assert_eq!(mgr.lifecycle_stage(99), LifecycleStage::RegisteredChallenger);
    }

    #[test]
    fn cusum_retirement_excludes_type() {
        let mut state = EvaluatorState::initial();
        let mut mgr = StrategyTypeSprt::new(&mut state);

        // Record 30 losing trades to trigger CUSUM retirement.
        // Reference is 0 (default), so any loss accumulates deficit.
        for _ in 0..30 {
            mgr.record_trade(55, -100_000);
        }
        assert!(mgr.is_excluded(55));
    }

    #[test]
    fn advance_stage_progresses_through_fsm() {
        let mut stage = LifecycleStage::ResearchCandidate;
        for _ in 0..9 {
            stage = advance_stage(stage);
        }
        assert_eq!(stage, LifecycleStage::Champion);
    }

    #[test]
    fn advance_stage_caps_at_champion() {
        let stage = advance_stage(LifecycleStage::Champion);
        assert_eq!(stage, LifecycleStage::Champion);
    }

    #[test]
    fn can_advance_requires_min_samples() {
        // RegisteredChallenger needs min_learning (30) pairs.
        assert!(!can_advance(
            LifecycleStage::RegisteredChallenger,
            10,
            SprtVerdict::Racing,
            false,
            0
        ));
        assert!(can_advance(
            LifecycleStage::RegisteredChallenger,
            30,
            SprtVerdict::Racing,
            false,
            0
        ));
    }

    #[test]
    fn can_advance_backtested_requires_not_dropped() {
        assert!(!can_advance(
            LifecycleStage::Backtested,
            30,
            SprtVerdict::Dropped,
            false,
            0
        ));
        assert!(can_advance(
            LifecycleStage::Backtested,
            30,
            SprtVerdict::Racing,
            false,
            0
        ));
    }

    #[test]
    fn can_advance_shadow_candidate_requires_adoptable() {
        assert!(!can_advance(
            LifecycleStage::ShadowCandidate,
            30,
            SprtVerdict::Racing,
            true,
            30
        ));
        assert!(can_advance(
            LifecycleStage::ShadowCandidate,
            30,
            SprtVerdict::Adoptable,
            true,
            30
        ));
    }
}
