//! `evidence_status` — evidence-status label enum and the proven-live-edge guard
//! (constitution §55, §26, §64).
//!
//! Responsibility: give every cohort a single, ordered evidence label and forbid
//! the central honesty violation — a paper or shadow cohort claiming a *proven
//! live edge*. Paper and shadow outcomes are counterfactual: no real capital was
//! at risk and no fills were reconciled to finalized chain truth, so they can
//! describe a *hypothesis*, never a proven live edge. This enum is deliberately
//! distinct from the ingest delivery-mode labels (Live / ProviderReplay / …); it
//! grades *evidentiary strength of an edge claim*, not how bytes were delivered.
//!
//! Pure type + guard, no arithmetic — trivially deterministic.

/// Ordered evidence-strength label for a cohort/edge claim (constitution §55).
///
/// The ordering is the promotion ladder: each variant strictly outranks the
/// ones before it. Only [`EvidenceStatus::ReconciledLive`] and above rest on
/// real capital reconciled to finalized chain truth.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EvidenceStatus {
    /// Backtest / simulated fills only — no capital, no live execution.
    Paper,
    /// Live market data, decisions recorded, but orders not actually sent.
    Shadow,
    /// Minimum real capital deployed as a paid probe (constitution §26, §64).
    LiveProbe,
    /// Live fills reconciled to finalized chain truth (constitution §14).
    ReconciledLive,
    /// Reconciled-live evidence that has additionally cleared the promotion
    /// gates — the only status that asserts a proven live edge.
    ProvenLiveEdge,
}

impl EvidenceStatus {
    /// True iff this status is backed by real, reconciled live capital.
    ///
    /// Responsibility (constitution §55): paper and shadow are counterfactual;
    /// live-probe and above put real capital at risk. This is the dividing line
    /// the guard below enforces.
    pub fn is_live_backed(&self) -> bool {
        matches!(
            self,
            EvidenceStatus::LiveProbe
                | EvidenceStatus::ReconciledLive
                | EvidenceStatus::ProvenLiveEdge
        )
    }

    /// True iff a cohort at this status may legitimately *claim* a proven live
    /// edge. Only [`EvidenceStatus::ProvenLiveEdge`] itself qualifies.
    pub fn claims_proven_live_edge(&self) -> bool {
        matches!(self, EvidenceStatus::ProvenLiveEdge)
    }
}

/// Why a promotion to [`EvidenceStatus::ProvenLiveEdge`] was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvidenceError {
    /// A paper or shadow cohort tried to claim a proven live edge — forbidden
    /// outright (constitution §55): no counterfactual cohort is ever promotable
    /// straight to proven-live.
    CounterfactualCohort {
        /// The offending source status.
        status: EvidenceStatus,
    },
    /// A live-backed but not-yet-reconciled cohort (e.g. an in-flight probe)
    /// cannot yet claim a proven edge; it must reach [`EvidenceStatus::ReconciledLive`].
    NotYetReconciled {
        /// The offending source status.
        status: EvidenceStatus,
    },
}

/// Attempt to tag a cohort as having a proven live edge.
///
/// Responsibility (constitution §55): the type guard the leaf exists for. A
/// promotion to [`EvidenceStatus::ProvenLiveEdge`] is granted **only** from
/// [`EvidenceStatus::ReconciledLive`]. Paper and shadow are rejected as
/// [`EvidenceError::CounterfactualCohort`] — they can *never* be tagged
/// proven-live — and live-but-unreconciled stages as
/// [`EvidenceError::NotYetReconciled`]. An already-proven status is idempotently
/// accepted. Deterministic, total over the enum.
pub fn tag_proven_live_edge(current: EvidenceStatus) -> Result<EvidenceStatus, EvidenceError> {
    match current {
        EvidenceStatus::ProvenLiveEdge | EvidenceStatus::ReconciledLive => {
            Ok(EvidenceStatus::ProvenLiveEdge)
        }
        EvidenceStatus::Paper | EvidenceStatus::Shadow => {
            Err(EvidenceError::CounterfactualCohort { status: current })
        }
        EvidenceStatus::LiveProbe => Err(EvidenceError::NotYetReconciled { status: current }),
    }
}
