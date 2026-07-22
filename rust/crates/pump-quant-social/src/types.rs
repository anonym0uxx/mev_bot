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
