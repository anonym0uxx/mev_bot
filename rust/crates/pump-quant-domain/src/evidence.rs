//! Provenance and evidence-quality vocabulary.
//!
//! ## Responsibility
//! The stable enums that label *how authoritative* a piece of observed truth is
//! ([`EvidenceStage`]), *how it was delivered* ([`DeliveryMode`]), *what fidelity
//! class* a whole dataset carries ([`DatasetFidelity`]), and *where a source sits
//! in its lifecycle* ([`SourceLifecycleStatus`]). These are pure labels; the one
//! piece of behaviour is the deliberately-chosen **authority ordering** on
//! [`EvidenceStage`], derived from its discriminants so `Ord` is total and
//! meaningful.
//!
//! ## Constitution alignment
//! * **Section 17:** `authority_class` (`EarliestSignal | StructuredObservation |
//!   CanonicalRepair | ReconciledExecution`).
//! * **Section 16 / 18.6:** [`DeliveryMode`] with replay never equated to live.
//! * **Section 16 fidelity ladder:** [`DatasetFidelity`].
//! * **Section 18.8:** [`SourceLifecycleStatus`].

use core::fmt;

/// The evidentiary authority of an observation — *how much a downstream decision
/// may trust it as ground truth*. Ordered from least to most authoritative, so
/// `EarliestSignal < StructuredObservation < CanonicalRepair < ReconciledExecution`
/// and derived [`Ord`] answers "is this at least as canonical as X?".
///
/// This ordering is a **claim about trust, not about timing**: an earliest signal
/// arrives first in wall time but ranks *lowest* in authority. Constitution
/// Section 17 (`SourceAuthorityClass`) and Section 18 (canonical authority is
/// never changed by latency/quality measurements).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[repr(u8)]
pub enum EvidenceStage {
    /// Earliest low-latency signal (e.g. shred-derived): fastest, least verified.
    EarliestSignal = 0,
    /// Structured decoded observation from a production feed (e.g. LaserStream).
    StructuredObservation = 1,
    /// Canonical RPC repair / historical reconstruction.
    CanonicalRepair = 2,
    /// Reconciled finalized truth for the system's own submitted transactions:
    /// the highest authority, the primary calibration source.
    ReconciledExecution = 3,
}

impl EvidenceStage {
    /// All stages in ascending authority order.
    pub const ALL: [EvidenceStage; 4] = [
        EvidenceStage::EarliestSignal,
        EvidenceStage::StructuredObservation,
        EvidenceStage::CanonicalRepair,
        EvidenceStage::ReconciledExecution,
    ];

    /// Authority rank (0 = weakest .. 3 = canonical). Equal to the discriminant;
    /// exposed so callers can compare numerically without matching.
    #[inline]
    pub const fn authority_rank(self) -> u8 {
        self as u8
    }

    /// `true` only for [`EvidenceStage::ReconciledExecution`], the sole stage that
    /// may be treated as finalized ground truth for calibration (Section 16
    /// fidelity ladder: reconciled execution is the primary calibration source).
    #[inline]
    pub const fn is_canonical_truth(self) -> bool {
        matches!(self, EvidenceStage::ReconciledExecution)
    }

    /// Whether `self` is at least as authoritative as `other` (total, via the
    /// derived ordering). Named helper so intent reads clearly at call sites.
    #[inline]
    pub fn at_least_as_authoritative_as(self, other: EvidenceStage) -> bool {
        self >= other
    }

    /// Reconstruct from a stable discriminant; `None` fails closed on unknown.
    #[inline]
    pub const fn from_u8(v: u8) -> Option<EvidenceStage> {
        match v {
            0 => Some(EvidenceStage::EarliestSignal),
            1 => Some(EvidenceStage::StructuredObservation),
            2 => Some(EvidenceStage::CanonicalRepair),
            3 => Some(EvidenceStage::ReconciledExecution),
            _ => None,
        }
    }
}

impl fmt::Display for EvidenceStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EvidenceStage::EarliestSignal => "EarliestSignal",
            EvidenceStage::StructuredObservation => "StructuredObservation",
            EvidenceStage::CanonicalRepair => "CanonicalRepair",
            EvidenceStage::ReconciledExecution => "ReconciledExecution",
        })
    }
}

/// How an observation reached this machine. Replay/repair timing must never be
/// equated with original live timing (Section 16 / 18.6), so the mode travels
/// with every observation rather than being inferred.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[repr(u8)]
pub enum DeliveryMode {
    /// Original live delivery from an active source.
    Live = 0,
    /// Provider-side historical stream replay (distinct timing claims).
    ProviderReplay = 1,
    /// Canonical RPC gap repair.
    RpcRepair = 2,
    /// Offline canonical backfill from sealed archives.
    CanonicalBackfill = 3,
}

impl DeliveryMode {
    /// `true` only for [`DeliveryMode::Live`]; live-timing metrics may only be
    /// computed from live-delivered observations (Section 18.6).
    #[inline]
    pub const fn is_live(self) -> bool {
        matches!(self, DeliveryMode::Live)
    }

    /// Reconstruct from a stable discriminant; `None` fails closed on unknown.
    #[inline]
    pub const fn from_u8(v: u8) -> Option<DeliveryMode> {
        match v {
            0 => Some(DeliveryMode::Live),
            1 => Some(DeliveryMode::ProviderReplay),
            2 => Some(DeliveryMode::RpcRepair),
            3 => Some(DeliveryMode::CanonicalBackfill),
            _ => None,
        }
    }
}

impl fmt::Display for DeliveryMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            DeliveryMode::Live => "Live",
            DeliveryMode::ProviderReplay => "ProviderReplay",
            DeliveryMode::RpcRepair => "RpcRepair",
            DeliveryMode::CanonicalBackfill => "CanonicalBackfill",
        })
    }
}

/// The fidelity class of a whole dataset / result, ordered from weakest
/// (arithmetic-only backfill) to strongest (reconciled live execution). Derived
/// `Ord` answers "is this dataset at least as trustworthy for calibration as X?".
/// Constitution Section 16 fidelity ladder.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[repr(u8)]
pub enum DatasetFidelity {
    /// Protocol arithmetic + lifecycle reconstruction, estimated timing only.
    CanonicalBackfill = 0,
    /// Multi-feed recording: lead/lag, order, decode delay, reconnects, gaps.
    DualFeedRecorded = 1,
    /// Live-shadow recording: real signal timing + simulated landing.
    LiveShadowRecorded = 2,
    /// Reconciled live execution: the primary calibration source (landing, fees,
    /// slippage, retries, capacity).
    ReconciledLiveExecution = 3,
}

impl DatasetFidelity {
    /// Fidelity rank (0 = weakest .. 3 = strongest); equals the discriminant.
    #[inline]
    pub const fn rank(self) -> u8 {
        self as u8
    }

    /// `true` only for the reconciled-live-execution class.
    #[inline]
    pub const fn is_reconciled_live(self) -> bool {
        matches!(self, DatasetFidelity::ReconciledLiveExecution)
    }
}

impl fmt::Display for DatasetFidelity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            DatasetFidelity::CanonicalBackfill => "CanonicalBackfill",
            DatasetFidelity::DualFeedRecorded => "DualFeedRecorded",
            DatasetFidelity::LiveShadowRecorded => "LiveShadowRecorded",
            DatasetFidelity::ReconciledLiveExecution => "ReconciledLiveExecution",
        })
    }
}

/// A source's lifecycle status in the source registry (Section 18.8). Governs
/// eligibility: only `ActivePrimary`/`ActiveRedundant` back new-position-critical
/// state without caveats; `Disabled`/`Retired` supply nothing live.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[repr(u8)]
pub enum SourceLifecycleStatus {
    /// Primary active source for its role.
    ActivePrimary = 0,
    /// Active redundant/secondary source.
    ActiveRedundant = 1,
    /// Transitional / sunset-aware (e.g. Jito ShredStream before shutdown).
    Transitional = 2,
    /// Degraded but still delivering.
    Degraded = 3,
    /// Sunset announced, shutdown pending.
    SunsetPending = 4,
    /// Administratively disabled.
    Disabled = 5,
    /// Permanently retired; delivers nothing.
    Retired = 6,
}

impl SourceLifecycleStatus {
    /// `true` when the source may currently back live decisions without a
    /// transitional/degraded caveat (only the two active-primary/redundant
    /// states qualify).
    #[inline]
    pub const fn is_active_for_new_positions(self) -> bool {
        matches!(
            self,
            SourceLifecycleStatus::ActivePrimary | SourceLifecycleStatus::ActiveRedundant
        )
    }

    /// `true` when the source is terminal (delivers nothing further).
    #[inline]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            SourceLifecycleStatus::Disabled | SourceLifecycleStatus::Retired
        )
    }

    /// Reconstruct from a stable discriminant; `None` fails closed on unknown.
    #[inline]
    pub const fn from_u8(v: u8) -> Option<SourceLifecycleStatus> {
        match v {
            0 => Some(SourceLifecycleStatus::ActivePrimary),
            1 => Some(SourceLifecycleStatus::ActiveRedundant),
            2 => Some(SourceLifecycleStatus::Transitional),
            3 => Some(SourceLifecycleStatus::Degraded),
            4 => Some(SourceLifecycleStatus::SunsetPending),
            5 => Some(SourceLifecycleStatus::Disabled),
            6 => Some(SourceLifecycleStatus::Retired),
            _ => None,
        }
    }
}

impl fmt::Display for SourceLifecycleStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SourceLifecycleStatus::ActivePrimary => "ActivePrimary",
            SourceLifecycleStatus::ActiveRedundant => "ActiveRedundant",
            SourceLifecycleStatus::Transitional => "Transitional",
            SourceLifecycleStatus::Degraded => "Degraded",
            SourceLifecycleStatus::SunsetPending => "SunsetPending",
            SourceLifecycleStatus::Disabled => "Disabled",
            SourceLifecycleStatus::Retired => "Retired",
        })
    }
}
