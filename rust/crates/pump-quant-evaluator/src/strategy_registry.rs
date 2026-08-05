//! `strategy_registry` — strategy type registry lifecycle FSM (§56.3, §64).
//!
//! The registry manages the 10-stage lifecycle FSM for each strategy type:
//! ResearchCandidate -> RegisteredChallenger -> Backtested -> OosValidated
//! -> AdversarialModeCValidated -> ShadowCandidate -> ShadowValidated
//! -> LiveProbeCandidate -> LiveProbeValidated -> Champion
//!
//! Each transition requires accumulated evidence (trades, OOS performance,
//! gate verdicts). The registry enforces these requirements and prevents
//! skipping stages or regressing (except via explicit demotion/retirement).
//!
//! ## Constitution compliance
//! - §56.3: Strategy lifecycle stages tracked reproducibly
//! - §64: Strategy registry and lifecycle management
//! - §22: Integer-only, no floats

use crate::evaluator_state::{
    CusumVerdict, LifecycleEvidence, LifecycleStage, LifecycleState,
};

// ============================================================================
// Registry Types
// ============================================================================

/// Result of an advancement attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdvancementResult {
    /// Successfully advanced to the next stage.
    Advanced { new_stage: LifecycleStage },
    /// Not enough evidence yet to advance.
    InsufficientEvidence { reason: InsufficientReason },
    /// Already at the final stage (Champion).
    AlreadyAtMax,
    /// Strategy type is retired/frozen and cannot advance.
    Blocked { reason: BlockReason },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsufficientReason {
    /// Not enough trades observed.
    NotEnoughTrades { have: u64, need: u64 },
    /// Not enough OOS folds passed.
    NotEnoughFolds { have: u64, need: u64 },
    /// SPRT not yet at Adoptable.
    SprtNotAdoptable,
    /// 8-gate not all passed.
    GatesNotPassed { passed: u8, total: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockReason {
    /// CUSUM retirement has bound: strategy edge decayed.
    RetiredByCusum,
    /// SPRT dropped the strategy type.
    DroppedBySprt,
    /// Manually frozen.
    ManuallyFrozen,
}

// ============================================================================
// Evidence Requirements Per Stage Transition
// ============================================================================

/// Minimum trades required before advancing FROM each stage.
/// These are conservative thresholds based on constitution §56.3.
#[must_use]
pub fn min_trades_for_advance(from: LifecycleStage) -> u64 {
    match from {
        LifecycleStage::ResearchCandidate => 0,
        LifecycleStage::RegisteredChallenger => 10,
        LifecycleStage::Backtested => 20,
        LifecycleStage::OosValidated => 30,
        LifecycleStage::AdversarialModeCValidated => 50,
        LifecycleStage::ShadowCandidate => 50,
        LifecycleStage::ShadowValidated => 100,
        LifecycleStage::LiveProbeCandidate => 20,
        LifecycleStage::LiveProbeValidated => 50,
        LifecycleStage::Champion => u64::MAX, // can't advance past champion
    }
}

/// Minimum OOS folds passed before advancing FROM each stage.
#[must_use]
pub fn min_folds_for_advance(from: LifecycleStage) -> u64 {
    match from {
        LifecycleStage::ResearchCandidate => 0,
        LifecycleStage::RegisteredChallenger => 0,
        LifecycleStage::Backtested => 3,
        LifecycleStage::OosValidated => 5,
        LifecycleStage::AdversarialModeCValidated => 5,
        LifecycleStage::ShadowCandidate => 5,
        LifecycleStage::ShadowValidated => 10,
        LifecycleStage::LiveProbeCandidate => 0,
        LifecycleStage::LiveProbeValidated => 5,
        LifecycleStage::Champion => u64::MAX,
    }
}

/// Whether the 8-gate must pass before advancing FROM this stage.
#[must_use]
pub fn requires_8gate(from: LifecycleStage) -> bool {
    matches!(
        from,
        LifecycleStage::ShadowValidated | LifecycleStage::LiveProbeValidated
    )
}

/// Whether SPRT must be Adoptable before advancing FROM this stage.
#[must_use]
pub fn requires_sprt_adoptable(from: LifecycleStage) -> bool {
    matches!(
        from,
        LifecycleStage::ShadowCandidate
            | LifecycleStage::ShadowValidated
            | LifecycleStage::LiveProbeCandidate
    )
}

// ============================================================================
// Registry Manager
// ============================================================================

/// The strategy registry. Wraps access to the lifecycle states stored
/// in `EvaluatorState.strategy_lifecycle`.
pub struct StrategyRegistry<'a> {
    states: &'a mut std::collections::HashMap<u64, LifecycleState>,
}

impl<'a> StrategyRegistry<'a> {
    /// Create a registry view over the lifecycle states.
    pub fn new(states: &'a mut std::collections::HashMap<u64, LifecycleState>) -> Self {
        Self { states }
    }

    /// Register a new strategy type at the ResearchCandidate stage.
    /// No-op if already registered.
    pub fn register(&mut self, type_id: u64, cycle: u64) {
        if !self.states.contains_key(&type_id) {
            self.states.insert(
                type_id,
                LifecycleState {
                    stage: LifecycleStage::ResearchCandidate,
                    stage_entered_cycle: cycle,
                    evidence: LifecycleEvidence::default(),
                },
            );
        }
    }

    /// Get the current stage for a strategy type.
    #[must_use]
    pub fn stage_of(&self, type_id: u64) -> Option<LifecycleStage> {
        self.states.get(&type_id).map(|s| s.stage)
    }

    /// Check if a strategy type is retired (CUSUM bound or SPRT dropped).
    #[must_use]
    pub fn is_blocked(&self, type_id: u64) -> Option<BlockReason> {
        let state = self.states.get(&type_id)?;
        if state.evidence.manually_frozen {
            return Some(BlockReason::ManuallyFrozen);
        }
        if state.evidence.cusum_verdict == CusumVerdict::Retired {
            return Some(BlockReason::RetiredByCusum);
        }
        if state.evidence.sprt_dropped {
            return Some(BlockReason::DroppedBySprt);
        }
        None
    }

    /// Attempt to advance a strategy type to the next lifecycle stage.
    ///
    /// Checks evidence requirements (trades, folds, SPRT, gates) and
    /// only advances if all are met. Returns the result.
    pub fn try_advance(
        &mut self,
        type_id: u64,
        cycle: u64,
        sprt_adoptable: bool,
        gates_passed: u8,
    ) -> AdvancementResult {
        let state = match self.states.get_mut(&type_id) {
            Some(s) => s,
            None => {
                // Auto-register if not present.
                self.register(type_id, cycle);
                return AdvancementResult::Advanced {
                    new_stage: LifecycleStage::RegisteredChallenger,
                };
            }
        };

        // Check if blocked.
        if state.evidence.manually_frozen {
            return AdvancementResult::Blocked {
                reason: BlockReason::ManuallyFrozen,
            };
        }
        if state.evidence.cusum_verdict == CusumVerdict::Retired {
            return AdvancementResult::Blocked {
                reason: BlockReason::RetiredByCusum,
            };
        }
        if state.evidence.sprt_dropped {
            return AdvancementResult::Blocked {
                reason: BlockReason::DroppedBySprt,
            };
        }

        // Can't advance past Champion.
        if state.stage == LifecycleStage::Champion {
            return AdvancementResult::AlreadyAtMax;
        }

        let from_stage = state.stage;

        // Check trade count.
        let min_trades = min_trades_for_advance(from_stage);
        if state.evidence.n_trades < min_trades {
            return AdvancementResult::InsufficientEvidence {
                reason: InsufficientReason::NotEnoughTrades {
                    have: state.evidence.n_trades,
                    need: min_trades,
                },
            };
        }

        // Check fold count.
        let min_folds = min_folds_for_advance(from_stage);
        if state.evidence.n_oos_folds_passed < min_folds {
            return AdvancementResult::InsufficientEvidence {
                reason: InsufficientReason::NotEnoughFolds {
                    have: state.evidence.n_oos_folds_passed,
                    need: min_folds,
                },
            };
        }

        // Check SPRT if required for the TARGET stage.
        let target_stage = from_stage.next_stage().unwrap_or(LifecycleStage::Champion);
        if requires_sprt_adoptable(target_stage) && !sprt_adoptable {
            return AdvancementResult::InsufficientEvidence {
                reason: InsufficientReason::SprtNotAdoptable,
            };
        }

        // Check 8-gate if required for the TARGET stage.
        if requires_8gate(target_stage) && gates_passed < 8 {
            return AdvancementResult::InsufficientEvidence {
                reason: InsufficientReason::GatesNotPassed {
                    passed: gates_passed,
                    total: 8,
                },
            };
        }

        // Advance.
        let new_stage = match from_stage.next_stage() {
            Some(s) => s,
            None => return AdvancementResult::AlreadyAtMax,
        };
        state.stage = new_stage;
        state.stage_entered_cycle = cycle;
        // Reset evidence for the new stage (trades/folds accumulate per stage).
        state.evidence.n_trades = 0;
        state.evidence.n_oos_folds_passed = 0;
        state.evidence.sprt_adoptable_seen = false;

        AdvancementResult::Advanced { new_stage }
    }

    /// Manually freeze a strategy type (e.g., manual kill switch).
    pub fn freeze(&mut self, type_id: u64) {
        if let Some(state) = self.states.get_mut(&type_id) {
            state.evidence.manually_frozen = true;
        }
    }

    /// Manually unfreeze a strategy type.
    pub fn unfreeze(&mut self, type_id: u64) {
        if let Some(state) = self.states.get_mut(&type_id) {
            state.evidence.manually_frozen = false;
        }
    }

    /// Record a trade observation for a strategy type's evidence.
    pub fn record_trade(&mut self, type_id: u64) {
        if let Some(state) = self.states.get_mut(&type_id) {
            state.evidence.n_trades = state.evidence.n_trades.saturating_add(1);
        }
    }

    /// Record an OOS fold pass for a strategy type's evidence.
    pub fn record_fold_pass(&mut self, type_id: u64) {
        if let Some(state) = self.states.get_mut(&type_id) {
            state.evidence.n_oos_folds_passed = state.evidence.n_oos_folds_passed.saturating_add(1);
        }
    }

    /// Record that SPRT returned Adoptable for this strategy type.
    pub fn record_sprt_adoptable(&mut self, type_id: u64) {
        if let Some(state) = self.states.get_mut(&type_id) {
            state.evidence.sprt_adoptable_seen = true;
        }
    }

    /// Record that SPRT dropped this strategy type.
    pub fn record_sprt_dropped(&mut self, type_id: u64) {
        if let Some(state) = self.states.get_mut(&type_id) {
            state.evidence.sprt_dropped = true;
        }
    }

    /// Record CUSUM retirement binding.
    pub fn record_cusum_retired(&mut self, type_id: u64) {
        if let Some(state) = self.states.get_mut(&type_id) {
            state.evidence.cusum_verdict = CusumVerdict::Retired;
        }
    }

    /// Count how many strategy types are at or above a given stage.
    #[must_use]
    pub fn count_at_or_above(&self, threshold: LifecycleStage) -> usize {
        self.states
            .values()
            .filter(|s| s.stage.index() >= threshold.index())
            .count()
    }

    /// List all strategy type ids that are at a given stage.
    #[must_use]
    pub fn types_at_stage(&self, stage: LifecycleStage) -> Vec<u64> {
        self.states
            .iter()
            .filter(|(_, s)| s.stage == stage)
            .map(|(k, _)| *k)
            .collect()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluator_state::LifecycleStage;
    use std::collections::HashMap;

    fn make_registry() -> (StrategyRegistry<'static>, HashMap<u64, LifecycleState>) {
        // We need a stable way to create a registry in tests.
        // Since we can't do the borrow dance easily, we'll use a helper.
        unreachable!("use make_registry_with_states instead")
    }

    // Helper to create a registry from a fresh map.
    fn with_states<F: FnOnce(StrategyRegistry)>(f: F) {
        let mut map: HashMap<u64, LifecycleState> = HashMap::new();
        let reg = StrategyRegistry::new(&mut map);
        f(reg);
    }

    #[test]
    fn register_new_type() {
        with_states(|mut reg| {
            reg.register(1, 0);
            assert_eq!(reg.stage_of(1), Some(LifecycleStage::ResearchCandidate));
        });
    }

    #[test]
    fn register_is_idempotent() {
        with_states(|mut reg| {
            reg.register(1, 0);
            reg.register(1, 5); // shouldn't overwrite
            assert_eq!(reg.stage_of(1), Some(LifecycleStage::ResearchCandidate));
        });
    }

    #[test]
    fn advance_from_research_to_registered() {
        with_states(|mut reg| {
            reg.register(1, 0);
            // ResearchCandidate -> RegisteredChallenger requires 0 trades
            let result = reg.try_advance(1, 1, false, 0);
            assert_eq!(
                result,
                AdvancementResult::Advanced {
                    new_stage: LifecycleStage::RegisteredChallenger,
                }
            );
            assert_eq!(reg.stage_of(1), Some(LifecycleStage::RegisteredChallenger));
        });
    }

    #[test]
    fn advance_requires_min_trades() {
        with_states(|mut reg| {
            reg.register(1, 0);
            reg.try_advance(1, 1, false, 0); // -> RegisteredChallenger
            // RegisteredChallenger -> Backtested requires 10 trades
            let result = reg.try_advance(1, 2, false, 0);
            match result {
                AdvancementResult::InsufficientEvidence {
                    reason: InsufficientReason::NotEnoughTrades { have: 0, need: 10 },
                } => {}
                _ => panic!("expected NotEnoughTrades, got {:?}", result),
            }
        });
    }

    #[test]
    fn advance_after_enough_trades() {
        with_states(|mut reg| {
            reg.register(1, 0);
            reg.try_advance(1, 1, false, 0); // -> RegisteredChallenger
            for _ in 0..10 {
                reg.record_trade(1);
            }
            // RegisteredChallenger -> Backtested needs 10 trades + 0 folds
            // We have 10 trades and 0 folds, so this should advance.
            let result = reg.try_advance(1, 2, false, 0);
            assert!(matches!(
                result,
                AdvancementResult::Advanced { new_stage: LifecycleStage::Backtested }
            ));
        });
    }

    #[test]
    fn full_lifecycle_progression() {
        with_states(|mut reg| {
            reg.register(1, 0);

            // ResearchCandidate -> RegisteredChallenger (0 trades, 0 folds)
            assert!(matches!(
                reg.try_advance(1, 1, false, 0),
                AdvancementResult::Advanced { .. }
            ));

            // RegisteredChallenger -> Backtested (10 trades, 0 folds)
            for _ in 0..10 {
                reg.record_trade(1);
            }
            assert!(matches!(
                reg.try_advance(1, 2, false, 0),
                AdvancementResult::Advanced { .. }
            ));

            // Backtested -> OosValidated (20 trades, 3 folds)
            for _ in 0..20 {
                reg.record_trade(1);
            }
            for _ in 0..3 {
                reg.record_fold_pass(1);
            }
            assert!(matches!(
                reg.try_advance(1, 3, false, 0),
                AdvancementResult::Advanced { .. }
            ));

            // OosValidated -> AdversarialModeCValidated (30 trades, 5 folds)
            for _ in 0..30 {
                reg.record_trade(1);
            }
            for _ in 0..5 {
                reg.record_fold_pass(1);
            }
            assert!(matches!(
                reg.try_advance(1, 4, false, 0),
                AdvancementResult::Advanced { .. }
            ));

            // AdversarialModeCValidated -> ShadowCandidate (50 trades, 5 folds, SPRT)
            for _ in 0..50 {
                reg.record_trade(1);
            }
            for _ in 0..5 {
                reg.record_fold_pass(1);
            }
            // SPRT not adoptable yet -> fail
            assert!(matches!(
                reg.try_advance(1, 5, false, 0),
                AdvancementResult::InsufficientEvidence { .. }
            ));
            // Now SPRT adoptable
            reg.record_sprt_adoptable(1);
            assert!(matches!(
                reg.try_advance(1, 6, true, 0),
                AdvancementResult::Advanced { .. }
            ));
        });
    }

    #[test]
    fn blocked_by_cusum_retirement() {
        with_states(|mut reg| {
            reg.register(1, 0);
            reg.record_cusum_retired(1);
            let result = reg.try_advance(1, 1, false, 0);
            assert_eq!(
                result,
                AdvancementResult::Blocked {
                    reason: BlockReason::RetiredByCusum,
                }
            );
        });
    }

    #[test]
    fn blocked_by_sprt_dropped() {
        with_states(|mut reg| {
            reg.register(1, 0);
            reg.record_sprt_dropped(1);
            let result = reg.try_advance(1, 1, false, 0);
            assert_eq!(
                result,
                AdvancementResult::Blocked {
                    reason: BlockReason::DroppedBySprt,
                }
            );
        });
    }

    #[test]
    fn freeze_blocks_advancement() {
        with_states(|mut reg| {
            reg.register(1, 0);
            reg.freeze(1);
            // Manually frozen is checked via is_blocked
            assert!(reg.is_blocked(1).is_some());
        });
    }

    #[test]
    fn unfreeze_allows_advancement() {
        with_states(|mut reg| {
            reg.register(1, 0);
            reg.freeze(1);
            assert!(reg.is_blocked(1).is_some());
            reg.unfreeze(1);
            // After unfreeze, can advance (ResearchCandidate -> Registered)
            assert!(matches!(
                reg.try_advance(1, 1, false, 0),
                AdvancementResult::Advanced { .. }
            ));
        });
    }

    #[test]
    fn champion_cannot_advance() {
        with_states(|mut reg| {
            reg.register(1, 0);
            // Manually set to Champion by advancing all the way
            // (simplified: just insert at Champion)
            let mut map: HashMap<u64, LifecycleState> = HashMap::new();
            map.insert(1, LifecycleState {
                stage: LifecycleStage::Champion,
                stage_entered_cycle: 0,
                evidence: LifecycleEvidence::default(),
            });
            let mut reg = StrategyRegistry::new(&mut map);
            let result = reg.try_advance(1, 10, true, 8);
            assert_eq!(result, AdvancementResult::AlreadyAtMax);
        });
    }

    #[test]
    fn count_at_or_above() {
        with_states(|mut reg| {
            reg.register(1, 0);
            reg.register(2, 0);
            reg.register(3, 0);
            // Advance type 1 to RegisteredChallenger
            reg.try_advance(1, 1, false, 0);
            // Count at or above RegisteredChallenger (index 1)
            let count = reg.count_at_or_above(LifecycleStage::RegisteredChallenger);
            assert_eq!(count, 1); // only type 1
        });
    }

    #[test]
    fn types_at_stage() {
        with_states(|mut reg| {
            reg.register(1, 0);
            reg.register(2, 0);
            reg.register(3, 0);
            let rc_types = reg.types_at_stage(LifecycleStage::ResearchCandidate);
            assert_eq!(rc_types.len(), 3);
        });
    }
}
