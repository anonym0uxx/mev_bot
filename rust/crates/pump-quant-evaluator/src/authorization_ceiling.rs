//! `authorization_ceiling` — backtest→shadow/probe authorization ceiling
//! (constitution §26, §64).
//!
//! Responsibility: encode the §64 authority path as a deterministic ceiling the
//! supervisor enforces but that is *proven* in Rust. Evidence that is only
//! backtested can authorize at most a shadow run or a minimum paid probe — never
//! scaled capital. Real, reconciled live edge is the only thing that unlocks
//! scaled capital. The mapping is total and monotone: stronger evidence never
//! authorizes *less*.
//!
//! Pure enum mapping, no arithmetic — trivially deterministic (constitution §22).

/// Strength of the evidence standing behind a policy (constitution §64).
///
/// Ordered from weakest to strongest; the ordering is the authority ladder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EvidenceStage {
    /// Backtest / simulation only — no live execution of any kind.
    BacktestOnly,
    /// Backtest that additionally survived chronological walk-forward (§16).
    WalkForwardValidated,
    /// A shadow run against live data (decisions recorded, no orders sent).
    ShadowValidated,
    /// A minimum live probe that reconciled to chain truth.
    LiveProbeValidated,
    /// A reconciled live edge cleared for scaling.
    ReconciledLiveEdge,
}

/// The maximum action a given evidence stage may authorize (constitution §64).
///
/// Ordered from least to most powerful; a *ceiling* is the highest action
/// permitted, so anything at or below it is allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ActionCeiling {
    /// No action authorized at all.
    NoAction,
    /// At most a shadow run (no capital).
    Shadow,
    /// At most a minimum paid probe (smallest real-capital deployment).
    MinimumProbe,
    /// At most a scaled probe (probe size may grow, still bounded).
    ScaledProbe,
    /// Scaled capital authorized.
    ScaledCapital,
}

/// Map an evidence stage to the maximum action it may authorize.
///
/// Responsibility (constitution §26, §64): the deterministic ceiling. In
/// particular **backtest-only and walk-forward-validated evidence can never
/// exceed [`ActionCeiling::MinimumProbe`]** — no amount of in-sample or
/// out-of-sample *simulation* authorizes scaled capital. Only
/// [`EvidenceStage::ReconciledLiveEdge`] unlocks [`ActionCeiling::ScaledCapital`].
/// The mapping is monotone non-decreasing in evidence strength.
pub fn max_authorized_action(evidence_stage: EvidenceStage) -> ActionCeiling {
    match evidence_stage {
        // Backtest-family evidence: shadow or a minimum probe, never scaled.
        EvidenceStage::BacktestOnly | EvidenceStage::WalkForwardValidated => {
            ActionCeiling::MinimumProbe
        }
        // A validated shadow run may graduate to a first minimum probe.
        EvidenceStage::ShadowValidated => ActionCeiling::MinimumProbe,
        // A reconciled minimum probe may grow the probe, still bounded.
        EvidenceStage::LiveProbeValidated => ActionCeiling::ScaledProbe,
        // Only reconciled live edge authorizes scaled capital.
        EvidenceStage::ReconciledLiveEdge => ActionCeiling::ScaledCapital,
    }
}

/// True iff the stage may authorize scaled capital.
///
/// Responsibility (constitution §64): the single most safety-critical query —
/// only [`EvidenceStage::ReconciledLiveEdge`] returns `true`.
pub fn authorizes_scaled_capital(evidence_stage: EvidenceStage) -> bool {
    max_authorized_action(evidence_stage) == ActionCeiling::ScaledCapital
}
