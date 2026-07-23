//! Shared domain types for the SocialSourceQualityLedger (constitution §29.8).
//!
//! # Responsibility
//! Define the decomposed determinant score, the token lifecycle phase used by D2,
//! and the eight source-classification states. Kept float-free and `Copy` where
//! cheap so the reducer never allocates on these (§22).

/// One §29.8 determinant score, stored *decomposed* exactly as the constitution
/// requires: the signed bps value together with the sample size it was computed
/// from and the confidence (bps) that sample size affords.
///
/// The `value_bps` sign convention is uniform across determinants: **higher is more
/// alpha-favourable**, negative is fade-favourable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeterminantScore {
    /// Signed determinant value in bps (higher = more favourable).
    pub value_bps: i64,
    /// Number of reconciled samples this value was computed from.
    pub sample_size: u32,
    /// Confidence in bps (0..=10_000) afforded by `sample_size`.
    pub confidence_bps: u16,
}

impl DeterminantScore {
    /// A zero-evidence score: neutral value, no samples, no confidence.
    ///
    /// Used as the fade-first default before any call is reconciled.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            value_bps: 0,
            sample_size: 0,
            confidence_bps: 0,
        }
    }
}

/// Where in a token's lifecycle a call was posted (D2 lifecycle timing, §29.8).
///
/// Persistent `PostPeak` posting is exit-liquidity promotion regardless of tone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecyclePhase {
    /// Before breadth expansion — the only phase that can support pre-flow alpha.
    PreFlow,
    /// During active participation broadening.
    WithFlow,
    /// After the peak — distribution / exit-liquidity territory.
    PostPeak,
}

/// The eight §29.8 source-classification states.
///
/// None is bullish or bearish by default and none is permanent from one call; each
/// carries confidence and decay (see [`crate::classification::Classification`]).
/// Ordering of the enum is arbitrary and carries no ranking meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceState {
    /// Beats the D3 selection control from a pre-flow posture with real markout.
    PreFlowAlpha,
    /// Posts with-flow with positive markout but does not beat the control enough
    /// to be pre-flow alpha — reach that rides moves rather than originating them.
    FlowAmplifier,
    /// Persistent post-peak posting: promotes exit liquidity.
    LateExitLiquidityPromoter,
    /// Wallet-graph evidence of buy-before-call / distribute-into-call (D5).
    PaidShillSuspect,
    /// Inauthentic audience: bot replies, raids, abnormal engagement velocity (D7).
    EngagementFarm,
    /// Low originality — an echo node in the amplification graph (D8).
    CopyEchoAccount,
    /// Authentic organic node without demonstrated pre-flow edge.
    OrganicCommunityNode,
    /// Not enough reconciled evidence to classify — the fade-first default.
    InsufficientSample,
}

/// The platform KIND of a social source (§29 provenance), so realized-outcome
/// attribution never collapses two sources that merely share a numeric id.
///
/// A `source_id: u64` alone is ambiguous across platforms — a Discord room and an
/// X account can both hash to the same `u64`. Pairing the id with its kind in a
/// [`SourceRef`] keeps a paid Discord ALPHA room's realized net SOL distinct from
/// every other source, which is what lets reflection up/down-weight or retire that
/// specific room (§29.8). The discriminant `code` is intentionally aligned with
/// the ingest `SocialPlatform::code`, but this crate stays dependency-free so the
/// alignment is documented, not a compile-time coupling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceKind {
    /// Twitter / X account (incl. the curated-follow designated-caller set).
    X,
    /// TikTok creator.
    TikTok,
    /// Telegram call channel.
    Telegram,
    /// General web / news source.
    Web,
    /// Twitch live-stream broadcaster.
    Twitch,
    /// Pump.fun venue-native commenter.
    Pump,
    /// Aggregator surface (e.g. CoinGecko).
    Aggregator,
    /// Discord alpha room (paid/curated designated-caller community).
    Discord,
}

impl SourceKind {
    /// Stable journalling code, aligned with `SocialPlatform::code` in the ingest
    /// crate (X=1 … Aggregator=7, Discord=8). Total; never panics.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            SourceKind::X => 1,
            SourceKind::TikTok => 2,
            SourceKind::Telegram => 3,
            SourceKind::Web => 4,
            SourceKind::Twitch => 5,
            SourceKind::Pump => 6,
            SourceKind::Aggregator => 7,
            SourceKind::Discord => 8,
        }
    }
}

/// A fully-qualified social source identity: a [`SourceKind`] plus a per-source /
/// per-room id (e.g. an account id, or the FNV hash of a Discord room name).
///
/// This is the realized-outcome attribution key (§29.8/§71): keying on the pair
/// keeps a Discord alpha room's net SOL separate from an X account that happens to
/// share the same numeric id. `Copy` + total `Ord` so it is a deterministic map
/// key with no hashing randomness (§22).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceRef {
    /// The platform kind of the source.
    pub kind: SourceKind,
    /// The per-source / per-room id within that kind.
    pub id: u64,
}

impl SourceRef {
    /// Construct a source reference from its kind and per-source id.
    #[must_use]
    pub const fn new(kind: SourceKind, id: u64) -> Self {
        Self { kind, id }
    }

    /// A Discord alpha-room source keyed by a per-room id (convenience for the
    /// lane the engine will feed).
    #[must_use]
    pub const fn discord(room_id: u64) -> Self {
        Self::new(SourceKind::Discord, room_id)
    }
}
