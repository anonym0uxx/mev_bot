//! Core identity and status types for the provenance canonicalizer (§15–§17).

/// A Solana transaction signature — the canonical identity key under which all
/// multi-source observations of one transaction are merged (§17).
///
/// Stored as the raw 64-byte Ed25519 signature. Ordering is byte-lexicographic
/// and is used only for deterministic iteration, never for authority.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Signature(pub [u8; 64]);

impl Signature {
    /// Constructs a signature from its raw 64 bytes.
    pub const fn new(bytes: [u8; 64]) -> Self {
        Signature(bytes)
    }

    /// Returns the raw 64 signature bytes.
    pub const fn bytes(&self) -> &[u8; 64] {
        &self.0
    }
}

impl core::fmt::Debug for Signature {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Compact, deterministic hex of the first 4 bytes — enough to identify in
        // test output without dumping 64 bytes. No float, no allocation growth.
        write!(
            f,
            "Signature({:02x}{:02x}{:02x}{:02x}..)",
            self.0[0], self.0[1], self.0[2], self.0[3]
        )
    }
}

/// Source authority levels (§15). These are the four levels the constitution
/// forbids collapsing into one another; their numeric [`SourceClass::rank`]
/// defines **canonical authority** for field resolution and is the only ordering
/// that may decide which claimed value becomes canonical (§18.8: source-quality
/// measurements may influence role designation but never change this authority).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceClass {
    /// Level 1 — earliest observed signal (shred-class sources; unconfirmed, may
    /// be dropped from a fork).
    EarliestSignal,
    /// Level 2 — structured observation (Helius LaserStream gRPC mainnet;
    /// observation truth, not automatically finalized).
    StructuredObservation,
    /// Level 3 — canonical repaired event (canonical Helius / Solana RPC repair).
    CanonicalRepair,
    /// Level 4 — finalized execution truth (reconciled outcomes for the system's
    /// own transactions).
    ReconciledExecution,
}

impl SourceClass {
    /// All source classes, ascending by authority. Used for bounded, deterministic
    /// iteration; the array length is the memory bound on per-class timelines.
    pub const ALL: [SourceClass; 4] = [
        SourceClass::EarliestSignal,
        SourceClass::StructuredObservation,
        SourceClass::CanonicalRepair,
        SourceClass::ReconciledExecution,
    ];

    /// Canonical authority rank: higher wins when resolving a canonical field
    /// value. Reconciled execution truth outranks canonical repair, which
    /// outranks structured observation, which outranks the earliest signal (§15).
    pub const fn rank(self) -> u8 {
        match self {
            SourceClass::EarliestSignal => 0,
            SourceClass::StructuredObservation => 1,
            SourceClass::CanonicalRepair => 2,
            SourceClass::ReconciledExecution => 3,
        }
    }
}

/// Provider identity for a feed (§17 examples: HELIUS, JITO, successor shred
/// providers, canonical RPC). Providers do **not** set authority — [`SourceClass`]
/// does. Provider identity is preserved only for provenance and deterministic
/// tie-breaking within a class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Provider {
    /// Helius (LaserStream / RPC).
    Helius,
    /// Jito ShredStream (transitional, sunset-bound — §18.3).
    Jito,
    /// A successor raw-shred provider (§18.3.4), identified by a small tag.
    SuccessorShred(u16),
    /// Canonical Solana / Helius RPC used for repair and reconciliation.
    CanonicalRpc,
    /// Any other provider, identified by a small tag, for portability (§18.8).
    Other(u16),
}

/// Delivery mode of an observation (§16). Timing carried by different delivery
/// modes may never be equated: provider-replay receipt time is not original live
/// time (§18.6), and canonical backfill timing is estimated only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeliveryMode {
    /// Original live delivery — the only mode whose receive time is treated as
    /// live observation timing.
    Live,
    /// Provider historical replay / reconnect recovery — receipt time recorded
    /// distinctly, never used as live timing (§18.6).
    ProviderReplay,
    /// Canonical RPC repair retrieval.
    RpcRepair,
    /// Canonical backfill reconstruction — estimated timing only (§16).
    CanonicalBackfill,
}

/// Commitment / confirmation status of a transaction as asserted by a source
/// (§17). Ordering is the natural chain progression and is monotone; it is used
/// to pick the highest observed commitment among equally-authoritative sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Commitment {
    /// Seen but not yet confirmed at any commitment level.
    Seen,
    /// Processed.
    Processed,
    /// Confirmed.
    Confirmed,
    /// Finalized.
    Finalized,
}

impl Commitment {
    /// Monotone progression rank (higher = further along the chain lifecycle).
    pub const fn rank(self) -> u8 {
        match self {
            Commitment::Seen => 0,
            Commitment::Processed => 1,
            Commitment::Confirmed => 2,
            Commitment::Finalized => 3,
        }
    }
}

/// Fork status of a transaction's inclusion (§15, §17: dropped-fork status). An
/// earliest-signal or structured observation may precede canonical inclusion and
/// may later be dropped; this is preserved rather than assumed canonical.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ForkStatus {
    /// Fork state not yet known from any source.
    Unknown,
    /// Observed on a fork not yet known to be canonical (e.g. shred-class,
    /// unconfirmed).
    OnFork,
    /// Included on the canonical chain.
    Canonical,
    /// Dropped from the canonical chain (fork was abandoned).
    Dropped,
}

/// Names the canonical fields that may carry cross-source disagreement (§15).
/// Used to label a [`crate::FieldDisagreement`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FieldName {
    /// Slot the transaction landed in.
    Slot,
    /// Transaction index within its block.
    TxIndex,
    /// Success / failure of the transaction.
    Success,
    /// Base fee in lamports.
    BaseFeeLamports,
    /// Priority fee in lamports.
    PriorityFeeLamports,
    /// Jito tip in lamports.
    JitoTipLamports,
    /// Compute units consumed.
    ComputeUnits,
    /// Commitment / confirmation status.
    Commitment,
    /// Fork inclusion status.
    Fork,
}
