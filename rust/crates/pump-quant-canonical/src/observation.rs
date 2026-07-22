//! The provenance-tagged input record consumed by the canonicalizer (§15–§17).

use crate::types::{Commitment, DeliveryMode, ForkStatus, Provider, Signature, SourceClass};

/// A single provenance-tagged observation of one transaction from one source
/// (§17, `RawObservation` reduced to the transaction-canonicalization essentials).
///
/// # Responsibility
/// Carry exactly what one feed asserted about one transaction, tagged with full
/// provenance and local receive timing, so the canonicalizer can merge many of
/// these while preserving disagreement and never equating timing across classes.
///
/// All timing fields are integer nanoseconds; there is no floating point (§22).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransactionObservation {
    /// Monotone, source-assigned observation id. Also the deterministic
    /// tie-breaker for equal-authority field resolution and timeline ties.
    pub observation_id: u64,
    /// The transaction this observation is about (the merge key).
    pub signature: Signature,
    /// Source authority class (§15) — sets canonical authority, never collapsed.
    pub source_class: SourceClass,
    /// Provider identity (provenance / tie-break only, never authority).
    pub provider: Provider,
    /// Delivery mode (§16) — governs whether receive timing counts as live.
    pub delivery_mode: DeliveryMode,
    /// Local arrival time in nanoseconds (observation truth). For
    /// [`DeliveryMode::Live`] this is live first-seen timing; for other modes it
    /// is receipt time recorded distinctly and never treated as live (§18.6).
    pub receive_time_ns: u64,
    /// For shred-class earliest sources: local time at which reconstruction of
    /// the transaction completed, if distinct from first-packet receipt (§17
    /// `reconstructed_earliest_ns`).
    pub reconstructed_time_ns: Option<u64>,
    /// Provider-asserted timestamp where present; never treated as local arrival
    /// (§17). Retained for provenance only.
    pub provider_timestamp_ns: Option<u64>,
    /// Source sequence number where the feed provides one (§17).
    pub source_sequence: Option<u64>,
    /// Connection epoch of the source stream (§17).
    pub connection_epoch: u64,
    /// Content hash of the raw payload (§17) — provenance integrity.
    pub payload_hash: [u8; 32],
    /// The transaction facts this source asserts. Any subset may be present;
    /// disagreement across sources is preserved, not resolved away (§15).
    pub claim: TxClaim,
}

/// The canonical facts a single source asserts about a transaction (§17). Every
/// field is optional because different feeds carry different subsets, and the
/// values may **disagree** across feeds — that disagreement is preserved by the
/// canonicalizer, not silently reconciled (§15).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TxClaim {
    /// Claimed slot of inclusion.
    pub slot: Option<u64>,
    /// Claimed transaction index within the block.
    pub tx_index: Option<u32>,
    /// Claimed success / failure.
    pub success: Option<bool>,
    /// Claimed base fee (lamports).
    pub base_fee_lamports: Option<u64>,
    /// Claimed priority fee (lamports).
    pub priority_fee_lamports: Option<u64>,
    /// Claimed Jito tip (lamports).
    pub jito_tip_lamports: Option<u64>,
    /// Claimed compute units consumed.
    pub compute_units: Option<u64>,
    /// Claimed commitment / confirmation status.
    pub commitment: Option<Commitment>,
    /// Claimed fork inclusion status.
    pub fork: Option<ForkStatus>,
}

/// A timestamp attributed to the source that produced it (§15). Used throughout
/// the dual timelines so that no time value is ever anonymous or pooled across
/// source classes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SourcedTime {
    /// The time in nanoseconds.
    pub time_ns: u64,
    /// The provider that supplied it (provenance).
    pub provider: Provider,
    /// The observation that carried it (provenance / determinism).
    pub observation_id: u64,
}
