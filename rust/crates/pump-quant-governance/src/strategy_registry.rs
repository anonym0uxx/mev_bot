//! Strategy-registry promotion lifecycle + probe-readiness gate (constitution
//! §56.3, §64).
//!
//! ## Responsibility
//! The §56.3 `StrategyRegistry` row and its promotion state machine: every
//! strategy version is pinned by its [`crate::hashing::StrategyHash`] /
//! [`crate::hashing::EvaluatorReleaseHash`] and moves through the §56.3
//! promotion statuses under the *only* legal forward chain (§64):
//!
//! `ResearchCandidate → RegisteredChallenger → Backtested → OosValidated →
//! AdversarialModeCValidated → ShadowCandidate → ShadowValidated →
//! LiveProbeCandidate → LiveProbeValidated → Champion`
//!
//! single-step only — skipping a stage is refused. This module is the single
//! authority on which promotion moves are legal, mirroring the source-registry
//! FSM in [`crate::lifecycle`].
//!
//! ## Governing laws enforced here
//! * **Mode C or nothing (§38, §54).** Fill-model evidence classes are typed
//!   ([`FillModelClass`]); any advancement into
//!   [`PromotionStatus::AdversarialModeCValidated`] or beyond with evidence
//!   graded under anything other than
//!   [`FillModelClass::CalibratedAdversarial`] is refused with
//!   [`AdvanceError::ModeCRequired`]. An optimistic ceiling can *never*
//!   satisfy promotion.
//! * **ProbeReadinessGate (§64).** Advancement into
//!   [`PromotionStatus::LiveProbeCandidate`] requires the full pre-probe gate
//!   set ([`ProbeReadinessGate`]) to pass on a fail-closed
//!   [`EvidenceGrade`]; the first failed [`ProbeCriterion`] is reported.
//! * **Missing capability is not a permission question (§64).** Advancement
//!   into the live ward ([`PromotionStatus::LiveProbeValidated`] /
//!   [`PromotionStatus::Champion`]) while live capability is absent yields
//!   [`AdvanceError::AwaitingLiveCapability`] — a missing-capability signal,
//!   explicitly *not* a human-approval requirement.
//! * **Fast kill, slow promote (§56.2).** [`StrategyLifecycle::demote`] moves
//!   any non-terminal strategy to `Demoted` / `Retired` / `Rejected` /
//!   `Quarantined` without evidence; recovery re-enters at `ShadowCandidate`
//!   and goes back through the gates.
//!
//! ## §22 / §57 compliance
//! No floating point, no wall-clock, no IO — `sequence` is a caller-supplied
//! monotone ordering value (replay / injected clock ordering). The per-record
//! transition audit trail is a bounded ring buffer capped at
//! [`TRANSITION_LOG_CAPACITY`] that evicts oldest entries (§57: no unbounded
//! growth).

use crate::hashing::{EvaluatorReleaseHash, StrategyHash};

/// Maximum retained transitions in a [`StrategyRecord`]'s audit trail (§57
/// memory bound). Once full, the oldest entry is evicted.
pub const TRANSITION_LOG_CAPACITY: usize = 64;

/// Strategy promotion statuses (§56.3), exactly the constitution's
/// authoritative fourteen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PromotionStatus {
    /// An idea under research; not yet registered for evaluation.
    ResearchCandidate,
    /// Registered as a challenger with pinned hashes; evaluation may begin.
    RegisteredChallenger,
    /// Passed in-sample backtest evaluation.
    Backtested,
    /// Passed out-of-sample validation.
    OosValidated,
    /// Validated under the calibrated adversarial fill model — Mode C (§38).
    AdversarialModeCValidated,
    /// Selected for shadow (paper) trading against live data.
    ShadowCandidate,
    /// Shadow trading validated the strategy's live-data behavior.
    ShadowValidated,
    /// Cleared the full pre-probe gate set (§64); awaiting a live probe.
    LiveProbeCandidate,
    /// A live probe with real (bounded) capital validated the strategy.
    LiveProbeValidated,
    /// The current champion for its lane.
    Champion,
    /// Demoted from live standing (fast kill, §56.2). Recoverable: re-entry at
    /// [`PromotionStatus::ShadowCandidate`] goes back through the gates.
    Demoted,
    /// Permanently retired. Terminal.
    Retired,
    /// Permanently rejected. Terminal.
    Rejected,
    /// Quarantined pending investigation. Recoverable via re-validation (first
    /// demote to [`PromotionStatus::Demoted`], then re-enter the gates).
    Quarantined,
}

impl PromotionStatus {
    /// Terminal states admit no further transitions.
    ///
    /// Only `Retired` and `Rejected` are terminal — `Quarantined` and
    /// `Demoted` are recoverable via re-validation (§56.2: a fast kill is not
    /// a death sentence; re-promotion goes back through the gates).
    pub fn is_terminal(&self) -> bool {
        matches!(self, PromotionStatus::Retired | PromotionStatus::Rejected)
    }

    /// Whether this status is in the *live ward*: statuses in which the
    /// strategy holds (or held) real live-capital standing
    /// (`LiveProbeValidated`, `Champion`). Entering the live ward carries the
    /// extra §64 capability and emergency-stop guards.
    pub fn is_live_ward(&self) -> bool {
        matches!(
            self,
            PromotionStatus::LiveProbeValidated | PromotionStatus::Champion
        )
    }

    /// A stable numeric code for registry rows and the transition audit trail.
    ///
    /// Codes are append-only and never reused (audit stability): they follow
    /// the §56.3 declaration order, `0..=13`.
    pub fn code(&self) -> u8 {
        match self {
            PromotionStatus::ResearchCandidate => 0,
            PromotionStatus::RegisteredChallenger => 1,
            PromotionStatus::Backtested => 2,
            PromotionStatus::OosValidated => 3,
            PromotionStatus::AdversarialModeCValidated => 4,
            PromotionStatus::ShadowCandidate => 5,
            PromotionStatus::ShadowValidated => 6,
            PromotionStatus::LiveProbeCandidate => 7,
            PromotionStatus::LiveProbeValidated => 8,
            PromotionStatus::Champion => 9,
            PromotionStatus::Demoted => 10,
            PromotionStatus::Retired => 11,
            PromotionStatus::Rejected => 12,
            PromotionStatus::Quarantined => 13,
        }
    }

    /// The single legal forward successor on the §64 promotion chain, if any.
    ///
    /// `Champion` is the end of the chain; the kill/park statuses (`Demoted`,
    /// `Retired`, `Rejected`, `Quarantined`) have no *forward* successor —
    /// recovery is a [`StrategyLifecycle::demote`] re-entry, not an advance.
    fn forward_successor(&self) -> Option<PromotionStatus> {
        use PromotionStatus::*;
        match self {
            ResearchCandidate => Some(RegisteredChallenger),
            RegisteredChallenger => Some(Backtested),
            Backtested => Some(OosValidated),
            OosValidated => Some(AdversarialModeCValidated),
            AdversarialModeCValidated => Some(ShadowCandidate),
            ShadowCandidate => Some(ShadowValidated),
            ShadowValidated => Some(LiveProbeCandidate),
            LiveProbeCandidate => Some(LiveProbeValidated),
            LiveProbeValidated => Some(Champion),
            Champion | Demoted | Retired | Rejected | Quarantined => None,
        }
    }
}

/// The class of fill model under which a strategy's evidence was produced —
/// §38 Modes A/B/C.
///
/// Only `CalibratedAdversarial` (Mode C) may support movement toward live
/// probe (§38).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FillModelClass {
    /// Mode A — causal replay of observed fills. Honest but backward-looking;
    /// insufficient for promotion past `OosValidated`.
    CausalReplay,
    /// Mode B — optimistic ceiling. A best-case bound that can *never* satisfy
    /// promotion toward live probe (§38, §54): a ceiling is an upper bound on
    /// hope, not evidence of edge.
    OptimisticCeiling,
    /// Mode C — calibrated adversarial fill model. The only class that may
    /// support movement toward live probe (§38).
    CalibratedAdversarial,
}

/// The §64 scale/probe criteria as typed booleans, plus the evidence's fill
/// model and reconciled-trade count.
///
/// ## Fail-closed contract
/// Every field the caller cannot *prove* must be passed `false` (and
/// `reconciled_trades` at the count actually reconciled against finalized
/// on-chain truth, not submitted or assumed). There is no "unknown" value:
/// unknown ≙ `false`, so absent evidence can never accidentally pass a gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvidenceGrade {
    /// The fill-model class the evidence was produced under (§38).
    pub fill_model: FillModelClass,
    /// Count of trades reconciled against finalized on-chain execution truth.
    pub reconciled_trades: u32,
    /// Proven: the strategy defeats the mandatory trivial baselines.
    pub baselines_defeated: bool,
    /// Proven: sequential (anytime-valid) edge estimate is positive.
    pub sequential_edge_positive: bool,
    /// Proven: sell-path reliability is clean (no stuck exits / failed sells).
    pub sell_reliability_clean: bool,
    /// Proven: drawdown stayed within the configured limits.
    pub drawdown_within_limits: bool,
    /// Proven: data-health posture is strong (feeds, staleness, gaps).
    pub data_health_strong: bool,
}

/// One criterion of the §64 pre-probe gate set, used to report *which* check
/// failed first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProbeCriterion {
    /// Sequential (anytime-valid) edge estimate must be positive.
    SequentialEdge,
    /// The mandatory trivial baselines must be defeated.
    BaselinesDefeated,
    /// Sell-path reliability must be clean.
    SellReliability,
    /// Drawdown must be within configured limits.
    Drawdown,
    /// Data-health posture must be strong.
    DataHealth,
    /// At least the configured minimum of reconciled trades.
    MinReconciledTrades,
}

/// The full pre-probe gate set (§64: "ProbeReadinessGate ≙ the full pre-probe
/// gate set").
///
/// Evaluates *every* §64 criterion against a fail-closed [`EvidenceGrade`];
/// the only tunable is the reconciled-trade floor (an integer count — §22, no
/// floating point thresholds live here; ratio-like criteria arrive
/// pre-decided as typed booleans the caller must prove).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeReadinessGate {
    /// Minimum number of reconciled trades the evidence must carry.
    pub min_reconciled_trades: u32,
}

impl ProbeReadinessGate {
    /// Check the full gate set, returning `Err` with the *first* failed
    /// criterion in the fixed §64 evaluation order: sequential edge,
    /// baselines, sell reliability, drawdown, data health, then the
    /// reconciled-trade floor.
    ///
    /// The order is deterministic and stable so that a given grade always
    /// reports the same criterion (§19/§22 determinism).
    pub fn evaluate(&self, g: &EvidenceGrade) -> Result<(), ProbeCriterion> {
        if !g.sequential_edge_positive {
            return Err(ProbeCriterion::SequentialEdge);
        }
        if !g.baselines_defeated {
            return Err(ProbeCriterion::BaselinesDefeated);
        }
        if !g.sell_reliability_clean {
            return Err(ProbeCriterion::SellReliability);
        }
        if !g.drawdown_within_limits {
            return Err(ProbeCriterion::Drawdown);
        }
        if !g.data_health_strong {
            return Err(ProbeCriterion::DataHealth);
        }
        if g.reconciled_trades < self.min_reconciled_trades {
            return Err(ProbeCriterion::MinReconciledTrades);
        }
        Ok(())
    }
}

/// Why a promotion move was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdvanceError {
    /// The `from → to` move is not in the legal transition set (§64: the
    /// forward chain is single-step; there is no stage skipping and no
    /// forward move out of a kill/park status).
    IllegalTransition {
        /// The status the strategy was in.
        from: PromotionStatus,
        /// The status that was requested.
        to: PromotionStatus,
    },
    /// Advancement into [`PromotionStatus::AdversarialModeCValidated`] or
    /// beyond was attempted with evidence not graded under
    /// [`FillModelClass::CalibratedAdversarial`]. An optimistic ceiling can
    /// *never* satisfy promotion (§38, §54).
    ModeCRequired,
    /// The §64 pre-probe gate set refused advancement into
    /// [`PromotionStatus::LiveProbeCandidate`]; carries the first failed
    /// criterion.
    ProbeGateFailed {
        /// The first criterion that failed, in the gate's fixed order.
        first_failed: ProbeCriterion,
    },
    /// Advancement into the live ward (`LiveProbeValidated` / `Champion`) was
    /// attempted while live capability is absent. This is a
    /// missing-capability signal, explicitly *not* a human-approval
    /// requirement (§64): nothing here awaits a sign-off — the transition
    /// becomes legal the moment the live execution capability exists.
    AwaitingLiveCapability,
    /// Advancement into the live ward was attempted while the emergency stop
    /// is engaged. Demotion is never blocked by the emergency stop (§56.2:
    /// the kill path must always be open).
    EmergencyStopped,
    /// The strategy is already terminal (`Retired` / `Rejected`).
    Terminal,
}

/// One §56.3 strategy-registry row: pinned identity hashes, current promotion
/// status, lane, and a bounded transition audit trail.
///
/// ## Constitution §56.3 / §57
/// `strategy_hash` and `evaluator_hash` pin the exact configuration and
/// evaluator release the record's evidence refers to; `config_hash_fnv`,
/// `protocol_registry_hash`, and `feature_schema_version` pin the runtime
/// config, protocol registry, and feature schema the strategy was validated
/// against. All identity fields are fixed at construction — a changed config
/// is a *new* record, never a mutated one. The audit trail is a fixed-capacity
/// ring buffer of `(from_code, to_code, sequence)` triples capped at
/// [`TRANSITION_LOG_CAPACITY`] (§57): once full, the oldest is evicted.
#[derive(Clone, Debug)]
pub struct StrategyRecord {
    /// Reproducible strategy-configuration digest (§56.3).
    strategy_hash: StrategyHash,
    /// Reproducible evaluator-release digest (§56.3, §44).
    evaluator_hash: EvaluatorReleaseHash,
    /// FNV digest of the runtime configuration the evidence was produced under.
    config_hash_fnv: u64,
    /// Digest of the protocol registry version in force during validation.
    protocol_registry_hash: u64,
    /// Feature-schema version the strategy's inputs were computed under.
    feature_schema_version: u32,
    /// Current promotion status.
    status: PromotionStatus,
    /// `EntryMode` discriminant: the lane this record competes in.
    lane: u16,
    /// Bounded ring buffer of `(from_code, to_code, sequence)` transitions.
    transitions: Vec<(u8, u8, u64)>,
    /// Index of the oldest entry in the ring (0 while not yet wrapped).
    transitions_head: usize,
}

impl StrategyRecord {
    /// Register a new strategy record in its initial status,
    /// [`PromotionStatus::ResearchCandidate`] (§64: every strategy starts at
    /// the bottom of the chain — there is no pre-validated entry).
    pub fn new(
        strategy_hash: StrategyHash,
        evaluator_hash: EvaluatorReleaseHash,
        config_hash_fnv: u64,
        protocol_registry_hash: u64,
        feature_schema_version: u32,
        lane: u16,
    ) -> Self {
        Self {
            strategy_hash,
            evaluator_hash,
            config_hash_fnv,
            protocol_registry_hash,
            feature_schema_version,
            status: PromotionStatus::ResearchCandidate,
            lane,
            transitions: Vec::with_capacity(TRANSITION_LOG_CAPACITY),
            transitions_head: 0,
        }
    }

    /// The pinned strategy-configuration digest (§56.3; immutable).
    pub fn strategy_hash(&self) -> StrategyHash {
        self.strategy_hash
    }

    /// The pinned evaluator-release digest (§56.3, §44; immutable).
    pub fn evaluator_hash(&self) -> EvaluatorReleaseHash {
        self.evaluator_hash
    }

    /// The pinned runtime-config FNV digest (immutable).
    pub fn config_hash_fnv(&self) -> u64 {
        self.config_hash_fnv
    }

    /// The pinned protocol-registry digest (immutable).
    pub fn protocol_registry_hash(&self) -> u64 {
        self.protocol_registry_hash
    }

    /// The pinned feature-schema version (immutable).
    pub fn feature_schema_version(&self) -> u32 {
        self.feature_schema_version
    }

    /// The current promotion status.
    pub fn status(&self) -> PromotionStatus {
        self.status
    }

    /// The lane (`EntryMode` discriminant) this record competes in.
    pub fn lane(&self) -> u16 {
        self.lane
    }

    /// Number of transitions currently retained in the bounded audit trail.
    pub fn transitions_len(&self) -> usize {
        self.transitions.len()
    }

    /// The retained transition history — `(from_code, to_code, sequence)` —
    /// in chronological (oldest-first) order.
    ///
    /// Bounded to [`TRANSITION_LOG_CAPACITY`] entries (§57); older transitions
    /// beyond the bound have been evicted.
    pub fn history(&self) -> Vec<(u8, u8, u64)> {
        // Reassemble the ring in chronological order starting at the head.
        let n = self.transitions.len();
        let mut out = Vec::with_capacity(n);
        for offset in 0..n {
            out.push(self.transitions[(self.transitions_head + offset) % n]);
        }
        out
    }

    /// Apply a *pre-validated* transition: record it in the bounded ring and
    /// update the status. Only [`StrategyLifecycle`] calls this, after all
    /// legality checks have passed.
    fn apply_transition(&mut self, to: PromotionStatus, sequence: u64) {
        let entry = (self.status.code(), to.code(), sequence);
        if self.transitions.len() < TRANSITION_LOG_CAPACITY {
            // Still filling: append. The head stays at 0 until we wrap.
            self.transitions.push(entry);
        } else {
            // Full: overwrite the oldest slot and advance the head (§57).
            self.transitions[self.transitions_head] = entry;
            self.transitions_head = (self.transitions_head + 1) % TRANSITION_LOG_CAPACITY;
        }
        self.status = to;
    }
}

/// The per-record promotion driver: the single authority on legal promotion
/// (§64) and demotion (§56.2) moves for a [`StrategyRecord`].
///
/// Stateless by design — all state lives in the record; the driver exists so
/// the transition *law* has exactly one home and one test surface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StrategyLifecycle;

impl StrategyLifecycle {
    /// Create a promotion driver.
    pub fn new() -> Self {
        StrategyLifecycle
    }

    /// Attempt a single-step forward advancement of `record` to `to`.
    ///
    /// ## Transition law (§38, §54, §56.3, §64)
    /// Checks are applied in this fixed order; the first failure is returned
    /// and the record is left unchanged:
    ///
    /// 1. **Terminal** — a `Retired`/`Rejected` record refuses everything.
    /// 2. **Single-step chain** — `to` must be the *immediate* successor of
    ///    the record's current status on the §64 forward chain; skipping a
    ///    stage, moving backward, or advancing out of a kill/park status is
    ///    [`AdvanceError::IllegalTransition`].
    /// 3. **Mode C** — any advance *into* `AdversarialModeCValidated` or
    ///    beyond requires `grade.fill_model == CalibratedAdversarial`; an
    ///    optimistic ceiling can never satisfy promotion (§38, §54).
    /// 4. **Probe gate** — an advance into `LiveProbeCandidate` requires the
    ///    full §64 pre-probe gate set to pass on `grade`.
    /// 5. **Live ward** — an advance into `LiveProbeValidated` or `Champion`
    ///    additionally requires `live_capability_present` (else
    ///    [`AdvanceError::AwaitingLiveCapability`] — a missing-capability
    ///    signal, not a human-approval requirement) and `!emergency_stopped`
    ///    (else [`AdvanceError::EmergencyStopped`]).
    ///
    /// `sequence` is a caller-supplied monotone ordering value (replay /
    /// injected clock ordering, never a wall-clock read — §22). On success the
    /// transition is recorded in the bounded audit trail and the status
    /// updated.
    // The argument list mirrors the constitution's advance signature verbatim
    // (§64): each input is a distinct, independently-sourced governance fact,
    // and bundling them would blur which subsystem attests to what.
    #[allow(clippy::too_many_arguments)]
    pub fn advance(
        &mut self,
        record: &mut StrategyRecord,
        to: PromotionStatus,
        grade: &EvidenceGrade,
        gate: &ProbeReadinessGate,
        live_capability_present: bool,
        emergency_stopped: bool,
        sequence: u64,
    ) -> Result<(), AdvanceError> {
        if record.status.is_terminal() {
            return Err(AdvanceError::Terminal);
        }
        // Single-step forward chain only (§64): `to` must be the immediate
        // successor of the current status.
        if record.status.forward_successor() != Some(to) {
            return Err(AdvanceError::IllegalTransition {
                from: record.status,
                to,
            });
        }
        // Mode C law (§38, §54): everything from AdversarialModeCValidated
        // onward demands calibrated-adversarial evidence.
        let requires_mode_c = matches!(
            to,
            PromotionStatus::AdversarialModeCValidated
                | PromotionStatus::ShadowCandidate
                | PromotionStatus::ShadowValidated
                | PromotionStatus::LiveProbeCandidate
                | PromotionStatus::LiveProbeValidated
                | PromotionStatus::Champion
        );
        if requires_mode_c && grade.fill_model != FillModelClass::CalibratedAdversarial {
            return Err(AdvanceError::ModeCRequired);
        }
        // Probe gate (§64): the full pre-probe gate set guards entry into
        // LiveProbeCandidate.
        if to == PromotionStatus::LiveProbeCandidate {
            if let Err(first_failed) = gate.evaluate(grade) {
                return Err(AdvanceError::ProbeGateFailed { first_failed });
            }
        }
        // Live ward (§64): entering LiveProbeValidated/Champion needs the
        // live capability present and the emergency stop disengaged.
        if to.is_live_ward() {
            if !live_capability_present {
                return Err(AdvanceError::AwaitingLiveCapability);
            }
            if emergency_stopped {
                return Err(AdvanceError::EmergencyStopped);
            }
        }
        record.apply_transition(to, sequence);
        Ok(())
    }

    /// Attempt a demotion / kill / recovery-re-entry move of `record` to `to`.
    ///
    /// ## Transition law (§56.2 "fast kill, slow promote")
    /// * From **any** non-terminal status, `Demoted`, `Retired`, `Rejected`,
    ///   and `Quarantined` are always reachable — no evidence, no gate, no
    ///   capability check, and the emergency stop never blocks a kill (the
    ///   kill path must always be open).
    /// * `Demoted → ShadowCandidate` is the one legal recovery re-entry:
    ///   recovery goes *back through the gates* (shadow → probe gate → live
    ///   ward), never straight to live standing. A `Quarantined` record
    ///   recovers by first demoting to `Demoted`, then re-entering.
    /// * A no-op self-transition is never legal (a transition must change
    ///   state).
    /// * `Retired`/`Rejected` are terminal and refuse everything.
    ///
    /// `sequence` is the caller-supplied monotone ordering value (§22).
    pub fn demote(
        &mut self,
        record: &mut StrategyRecord,
        to: PromotionStatus,
        sequence: u64,
    ) -> Result<(), AdvanceError> {
        if record.status.is_terminal() {
            return Err(AdvanceError::Terminal);
        }
        use PromotionStatus::*;
        let legal = match to {
            // Fast kill: always reachable from any non-terminal state, except
            // as a no-op self-transition.
            Demoted | Retired | Rejected | Quarantined => record.status != to,
            // Recovery re-entry: back through the gates from Demoted only.
            ShadowCandidate => record.status == Demoted,
            _ => false,
        };
        if !legal {
            return Err(AdvanceError::IllegalTransition {
                from: record.status,
                to,
            });
        }
        record.apply_transition(to, sequence);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> StrategyRecord {
        StrategyRecord::new(
            StrategyHash([0xAB; 32]),
            EvaluatorReleaseHash([0xCD; 32]),
            0x1234_5678_9ABC_DEF0,
            0x0FED_CBA9_8765_4321,
            3,
            7,
        )
    }

    /// A fully-proven Mode C evidence grade that passes every probe criterion.
    fn full_grade(reconciled_trades: u32) -> EvidenceGrade {
        EvidenceGrade {
            fill_model: FillModelClass::CalibratedAdversarial,
            reconciled_trades,
            baselines_defeated: true,
            sequential_edge_positive: true,
            sell_reliability_clean: true,
            drawdown_within_limits: true,
            data_health_strong: true,
        }
    }

    /// The fail-closed default: nothing proven, no reconciled trades.
    fn empty_grade(fill_model: FillModelClass) -> EvidenceGrade {
        EvidenceGrade {
            fill_model,
            reconciled_trades: 0,
            baselines_defeated: false,
            sequential_edge_positive: false,
            sell_reliability_clean: false,
            drawdown_within_limits: false,
            data_health_strong: false,
        }
    }

    fn gate() -> ProbeReadinessGate {
        ProbeReadinessGate {
            min_reconciled_trades: 30,
        }
    }

    /// Advance `record` along the legal chain up to (and including) `target`
    /// with fully-qualified evidence, capability present, no emergency.
    fn advance_to(
        driver: &mut StrategyLifecycle,
        record: &mut StrategyRecord,
        target: PromotionStatus,
    ) {
        let grade = full_grade(100);
        let gate = gate();
        let mut seq = 0u64;
        while record.status() != target {
            let next = record.status().forward_successor().expect("on the chain");
            driver
                .advance(record, next, &grade, &gate, true, false, seq)
                .expect("legal chain advance");
            seq += 1;
        }
    }

    #[test]
    fn codes_are_stable_and_unique() {
        use PromotionStatus::*;
        let all = [
            ResearchCandidate,
            RegisteredChallenger,
            Backtested,
            OosValidated,
            AdversarialModeCValidated,
            ShadowCandidate,
            ShadowValidated,
            LiveProbeCandidate,
            LiveProbeValidated,
            Champion,
            Demoted,
            Retired,
            Rejected,
            Quarantined,
        ];
        for (expected, status) in all.iter().enumerate() {
            assert_eq!(status.code() as usize, expected);
        }
    }

    #[test]
    fn terminal_and_live_ward_classification() {
        use PromotionStatus::*;
        assert!(Retired.is_terminal());
        assert!(Rejected.is_terminal());
        // Quarantined and Demoted are recoverable, not terminal.
        assert!(!Quarantined.is_terminal());
        assert!(!Demoted.is_terminal());
        assert!(!Champion.is_terminal());
        assert!(LiveProbeValidated.is_live_ward());
        assert!(Champion.is_live_ward());
        assert!(!LiveProbeCandidate.is_live_ward());
        assert!(!ShadowValidated.is_live_ward());
    }

    #[test]
    fn full_legal_chain_reaches_champion() {
        let mut driver = StrategyLifecycle::new();
        let mut r = record();
        assert_eq!(r.status(), PromotionStatus::ResearchCandidate);
        advance_to(&mut driver, &mut r, PromotionStatus::Champion);
        assert_eq!(r.status(), PromotionStatus::Champion);
        // Nine single steps were recorded, in order.
        let history = r.history();
        assert_eq!(history.len(), 9);
        assert_eq!(history[0], (0, 1, 0));
        assert_eq!(history[8], (8, 9, 8));
    }

    #[test]
    fn skipping_a_stage_is_illegal() {
        let mut driver = StrategyLifecycle::new();
        let mut r = record();
        let err = driver
            .advance(
                &mut r,
                PromotionStatus::Backtested, // skips RegisteredChallenger
                &full_grade(100),
                &gate(),
                true,
                false,
                0,
            )
            .unwrap_err();
        assert_eq!(
            err,
            AdvanceError::IllegalTransition {
                from: PromotionStatus::ResearchCandidate,
                to: PromotionStatus::Backtested,
            }
        );
        assert_eq!(r.status(), PromotionStatus::ResearchCandidate);
    }

    #[test]
    fn backward_advance_is_illegal() {
        let mut driver = StrategyLifecycle::new();
        let mut r = record();
        advance_to(&mut driver, &mut r, PromotionStatus::OosValidated);
        let err = driver
            .advance(
                &mut r,
                PromotionStatus::Backtested,
                &full_grade(100),
                &gate(),
                true,
                false,
                99,
            )
            .unwrap_err();
        assert_eq!(
            err,
            AdvanceError::IllegalTransition {
                from: PromotionStatus::OosValidated,
                to: PromotionStatus::Backtested,
            }
        );
    }

    #[test]
    fn optimistic_ceiling_cannot_reach_mode_c_validated() {
        let mut driver = StrategyLifecycle::new();
        let mut r = record();
        advance_to(&mut driver, &mut r, PromotionStatus::OosValidated);
        let ceiling = EvidenceGrade {
            fill_model: FillModelClass::OptimisticCeiling,
            ..full_grade(1_000)
        };
        let err = driver
            .advance(
                &mut r,
                PromotionStatus::AdversarialModeCValidated,
                &ceiling,
                &gate(),
                true,
                false,
                10,
            )
            .unwrap_err();
        assert_eq!(err, AdvanceError::ModeCRequired);
        assert_eq!(r.status(), PromotionStatus::OosValidated);
    }

    #[test]
    fn causal_replay_cannot_advance_beyond_mode_c_boundary() {
        let mut driver = StrategyLifecycle::new();
        let mut r = record();
        advance_to(
            &mut driver,
            &mut r,
            PromotionStatus::AdversarialModeCValidated,
        );
        let replay = EvidenceGrade {
            fill_model: FillModelClass::CausalReplay,
            ..full_grade(1_000)
        };
        let err = driver
            .advance(
                &mut r,
                PromotionStatus::ShadowCandidate,
                &replay,
                &gate(),
                true,
                false,
                10,
            )
            .unwrap_err();
        assert_eq!(err, AdvanceError::ModeCRequired);
    }

    #[test]
    fn non_mode_c_evidence_may_still_advance_early_stages() {
        // Below the Mode C boundary the fill-model class is not yet decisive:
        // ResearchCandidate → ... → OosValidated is legal under Mode A/B.
        let mut driver = StrategyLifecycle::new();
        let mut r = record();
        let replay = empty_grade(FillModelClass::CausalReplay);
        for (seq, next) in [
            PromotionStatus::RegisteredChallenger,
            PromotionStatus::Backtested,
            PromotionStatus::OosValidated,
        ]
        .into_iter()
        .enumerate()
        {
            driver
                .advance(&mut r, next, &replay, &gate(), false, false, seq as u64)
                .expect("pre-Mode-C stages need no Mode C evidence");
        }
        assert_eq!(r.status(), PromotionStatus::OosValidated);
    }

    #[test]
    fn probe_gate_reports_first_failed_criterion_in_order() {
        let gate = gate();
        // All-false fails on SequentialEdge first.
        let mut g = empty_grade(FillModelClass::CalibratedAdversarial);
        assert_eq!(gate.evaluate(&g), Err(ProbeCriterion::SequentialEdge));
        g.sequential_edge_positive = true;
        assert_eq!(gate.evaluate(&g), Err(ProbeCriterion::BaselinesDefeated));
        g.baselines_defeated = true;
        assert_eq!(gate.evaluate(&g), Err(ProbeCriterion::SellReliability));
        g.sell_reliability_clean = true;
        assert_eq!(gate.evaluate(&g), Err(ProbeCriterion::Drawdown));
        g.drawdown_within_limits = true;
        assert_eq!(gate.evaluate(&g), Err(ProbeCriterion::DataHealth));
        g.data_health_strong = true;
        // Everything proven but too few reconciled trades: floor fails last.
        assert_eq!(gate.evaluate(&g), Err(ProbeCriterion::MinReconciledTrades));
        g.reconciled_trades = 30;
        assert_eq!(gate.evaluate(&g), Ok(()));
    }

    #[test]
    fn probe_gate_failure_blocks_live_probe_candidacy() {
        let mut driver = StrategyLifecycle::new();
        let mut r = record();
        advance_to(&mut driver, &mut r, PromotionStatus::ShadowValidated);
        let thin = EvidenceGrade {
            reconciled_trades: 29, // below the floor of 30
            ..full_grade(0)
        };
        let err = driver
            .advance(
                &mut r,
                PromotionStatus::LiveProbeCandidate,
                &thin,
                &gate(),
                true,
                false,
                50,
            )
            .unwrap_err();
        assert_eq!(
            err,
            AdvanceError::ProbeGateFailed {
                first_failed: ProbeCriterion::MinReconciledTrades,
            }
        );
        assert_eq!(r.status(), PromotionStatus::ShadowValidated);
    }

    #[test]
    fn missing_live_capability_is_awaiting_capability_not_approval() {
        let mut driver = StrategyLifecycle::new();
        let mut r = record();
        advance_to(&mut driver, &mut r, PromotionStatus::LiveProbeCandidate);
        // Fully qualified in every way — but the live capability is absent.
        let err = driver
            .advance(
                &mut r,
                PromotionStatus::LiveProbeValidated,
                &full_grade(1_000),
                &gate(),
                false, // live capability absent
                false,
                60,
            )
            .unwrap_err();
        // A missing-capability signal — not a human-approval error, and not an
        // illegal transition: the record simply waits, still a candidate.
        assert_eq!(err, AdvanceError::AwaitingLiveCapability);
        assert_eq!(r.status(), PromotionStatus::LiveProbeCandidate);
        // The moment the capability exists, the same move succeeds.
        driver
            .advance(
                &mut r,
                PromotionStatus::LiveProbeValidated,
                &full_grade(1_000),
                &gate(),
                true,
                false,
                61,
            )
            .expect("capability present: transition is legal");
        assert_eq!(r.status(), PromotionStatus::LiveProbeValidated);
    }

    #[test]
    fn emergency_stop_blocks_live_ward_but_not_demotion() {
        let mut driver = StrategyLifecycle::new();
        let mut r = record();
        advance_to(&mut driver, &mut r, PromotionStatus::LiveProbeCandidate);
        let err = driver
            .advance(
                &mut r,
                PromotionStatus::LiveProbeValidated,
                &full_grade(1_000),
                &gate(),
                true,
                true, // emergency stop engaged
                70,
            )
            .unwrap_err();
        assert_eq!(err, AdvanceError::EmergencyStopped);
        assert_eq!(r.status(), PromotionStatus::LiveProbeCandidate);
        // The kill path stays open regardless of the emergency stop.
        driver
            .demote(&mut r, PromotionStatus::Demoted, 71)
            .expect("demotion is never blocked");
        assert_eq!(r.status(), PromotionStatus::Demoted);
    }

    #[test]
    fn emergency_stop_does_not_block_non_live_ward_advances() {
        let mut driver = StrategyLifecycle::new();
        let mut r = record();
        driver
            .advance(
                &mut r,
                PromotionStatus::RegisteredChallenger,
                &empty_grade(FillModelClass::CausalReplay),
                &gate(),
                false,
                true, // emergency stop engaged — irrelevant below the live ward
                0,
            )
            .expect("non-live-ward advance is not emergency-gated");
        assert_eq!(r.status(), PromotionStatus::RegisteredChallenger);
    }

    #[test]
    fn demotion_from_champion_is_always_allowed() {
        let mut driver = StrategyLifecycle::new();
        for kill in [
            PromotionStatus::Demoted,
            PromotionStatus::Retired,
            PromotionStatus::Rejected,
            PromotionStatus::Quarantined,
        ] {
            let mut r = record();
            advance_to(&mut driver, &mut r, PromotionStatus::Champion);
            driver
                .demote(&mut r, kill, 100)
                .expect("fast kill from Champion");
            assert_eq!(r.status(), kill);
        }
    }

    #[test]
    fn demotion_reachable_from_any_non_terminal_state() {
        let mut driver = StrategyLifecycle::new();
        let mut r = record(); // ResearchCandidate: no evidence at all
        driver
            .demote(&mut r, PromotionStatus::Quarantined, 0)
            .expect("kill needs no evidence");
        // Quarantined is recoverable: demote to Demoted, then re-enter.
        driver
            .demote(&mut r, PromotionStatus::Demoted, 1)
            .expect("Quarantined → Demoted");
        driver
            .demote(&mut r, PromotionStatus::ShadowCandidate, 2)
            .expect("Demoted → ShadowCandidate re-entry");
        assert_eq!(r.status(), PromotionStatus::ShadowCandidate);
    }

    #[test]
    fn demoted_reentry_goes_back_through_the_gates() {
        let mut driver = StrategyLifecycle::new();
        let mut r = record();
        advance_to(&mut driver, &mut r, PromotionStatus::Champion);
        driver
            .demote(&mut r, PromotionStatus::Demoted, 200)
            .expect("kill");
        driver
            .demote(&mut r, PromotionStatus::ShadowCandidate, 201)
            .expect("re-entry");
        // Recovery does not skip: the next legal advance is ShadowValidated,
        // and jumping straight back to Champion is illegal.
        let err = driver
            .advance(
                &mut r,
                PromotionStatus::Champion,
                &full_grade(1_000),
                &gate(),
                true,
                false,
                202,
            )
            .unwrap_err();
        assert_eq!(
            err,
            AdvanceError::IllegalTransition {
                from: PromotionStatus::ShadowCandidate,
                to: PromotionStatus::Champion,
            }
        );
        driver
            .advance(
                &mut r,
                PromotionStatus::ShadowValidated,
                &full_grade(1_000),
                &gate(),
                true,
                false,
                203,
            )
            .expect("back through the gates, one step at a time");
    }

    #[test]
    fn reentry_to_shadow_candidate_only_legal_from_demoted() {
        let mut driver = StrategyLifecycle::new();
        let mut r = record();
        driver
            .demote(&mut r, PromotionStatus::Quarantined, 0)
            .expect("kill");
        let err = driver
            .demote(&mut r, PromotionStatus::ShadowCandidate, 1)
            .unwrap_err();
        assert_eq!(
            err,
            AdvanceError::IllegalTransition {
                from: PromotionStatus::Quarantined,
                to: PromotionStatus::ShadowCandidate,
            }
        );
    }

    #[test]
    fn no_op_demotion_is_illegal() {
        let mut driver = StrategyLifecycle::new();
        let mut r = record();
        driver
            .demote(&mut r, PromotionStatus::Demoted, 0)
            .expect("kill");
        let err = driver
            .demote(&mut r, PromotionStatus::Demoted, 1)
            .unwrap_err();
        assert_eq!(
            err,
            AdvanceError::IllegalTransition {
                from: PromotionStatus::Demoted,
                to: PromotionStatus::Demoted,
            }
        );
    }

    #[test]
    fn terminal_states_refuse_everything() {
        let mut driver = StrategyLifecycle::new();
        for terminal in [PromotionStatus::Retired, PromotionStatus::Rejected] {
            let mut r = record();
            driver
                .demote(&mut r, terminal, 0)
                .expect("kill to terminal");
            let adv = driver
                .advance(
                    &mut r,
                    PromotionStatus::RegisteredChallenger,
                    &full_grade(1_000),
                    &gate(),
                    true,
                    false,
                    1,
                )
                .unwrap_err();
            assert_eq!(adv, AdvanceError::Terminal);
            let dem = driver
                .demote(&mut r, PromotionStatus::Demoted, 2)
                .unwrap_err();
            assert_eq!(dem, AdvanceError::Terminal);
            assert_eq!(r.status(), terminal);
        }
    }

    #[test]
    fn failed_moves_leave_no_audit_entry() {
        let mut driver = StrategyLifecycle::new();
        let mut r = record();
        let _ = driver.advance(
            &mut r,
            PromotionStatus::Champion,
            &full_grade(1_000),
            &gate(),
            true,
            false,
            0,
        );
        assert_eq!(r.transitions_len(), 0);
        assert!(r.history().is_empty());
    }

    #[test]
    fn transition_log_is_bounded_and_evicts_oldest() {
        let mut driver = StrategyLifecycle::new();
        let mut r = record();
        // Bounce Demoted ↔ ShadowCandidate far past the capacity.
        driver
            .demote(&mut r, PromotionStatus::Demoted, 0)
            .expect("initial kill");
        let total = TRANSITION_LOG_CAPACITY as u64 + 10;
        for seq in 1..=total {
            let to = if r.status() == PromotionStatus::Demoted {
                PromotionStatus::ShadowCandidate
            } else {
                PromotionStatus::Demoted
            };
            driver.demote(&mut r, to, seq).expect("bounce");
        }
        assert_eq!(r.transitions_len(), TRANSITION_LOG_CAPACITY);
        let history = r.history();
        assert_eq!(history.len(), TRANSITION_LOG_CAPACITY);
        // Oldest surviving entry is total - capacity + 1; newest is total —
        // strictly increasing across the retained window.
        assert_eq!(history[0].2, total - TRANSITION_LOG_CAPACITY as u64 + 1);
        assert_eq!(history[TRANSITION_LOG_CAPACITY - 1].2, total);
        for pair in history.windows(2) {
            assert!(pair[0].2 < pair[1].2);
        }
    }

    #[test]
    fn record_pins_identity_and_starts_at_research_candidate() {
        let r = record();
        assert_eq!(r.strategy_hash(), StrategyHash([0xAB; 32]));
        assert_eq!(r.evaluator_hash(), EvaluatorReleaseHash([0xCD; 32]));
        assert_eq!(r.config_hash_fnv(), 0x1234_5678_9ABC_DEF0);
        assert_eq!(r.protocol_registry_hash(), 0x0FED_CBA9_8765_4321);
        assert_eq!(r.feature_schema_version(), 3);
        assert_eq!(r.lane(), 7);
        assert_eq!(r.status(), PromotionStatus::ResearchCandidate);
        assert_eq!(r.transitions_len(), 0);
    }
}
