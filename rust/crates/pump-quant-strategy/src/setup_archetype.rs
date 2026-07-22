//! # setup_archetype — deterministic SetupArchetypeClassifier (§25)
//!
//! Classifies a candidate into exactly one of the 23 named setup archetypes
//! (`FRESH_MINT_FLOW` … `UNKNOWN`) that §25 requires to drive per-archetype
//! entry/size/exit policy. The label is emitted as the `u16` consumed by
//! [`thesis::ThesisInputs::archetype`](crate::thesis::ThesisInputs) and mapped
//! into the evaluator's archetype-stratification key.
//!
//! The classifier is a deterministic priority cascade over already-decoded
//! integer feature measures (from the features/signals crates). Mechanical
//! untradeable states and manipulation traps are resolved first (they dominate
//! any bullish reading), then market-structure archetypes, migration-phase
//! archetypes, attention/social launch archetypes, and finally the
//! fresh/organic and fallback classes. First match wins, so the outcome is a
//! total, order-independent function of the inputs.
//!
//! ## Constitution
//! §22: integer-only, no floats, deterministic. §25: this is the shared
//! classifier that must run identically in LIVE/SHADOW/REPLAY. Every threshold
//! is supplied via [`ClassifierThresholds`] (operator-tunable, versioned) rather
//! than hardcoded in the decision path.

// ---------------------------------------------------------------------------
// Archetype enum + stable labels
// ---------------------------------------------------------------------------

/// The 23 named setup archetypes of §25.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum SetupArchetype {
    /// Brand-new mint with early flow.
    FreshMintFlow = 0,
    /// Clean, organic buyer breadth.
    CleanOrganicBreadth = 1,
    /// Crypto-Twitter attention shock.
    CtAttentionShock = 2,
    /// Live-streamed pump.
    PumpLiveStream = 3,
    /// Creator cult / cohesive community.
    CreatorCultOrCommunity = 4,
    /// Deployer with recycle/rug history.
    DevRecycleRisk = 5,
    /// Coordinated wallet-cluster pump.
    WalletClusterPump = 6,
    /// Bundle-sniper launch trap.
    BundleSniperTrap = 7,
    /// Momentum into migration.
    MigrationMomentum = 8,
    /// Post-migration revival.
    PostMigrationRevival = 9,
    /// Social stunt or meta narrative.
    SocialStuntOrMeta = 10,
    /// Platform visibility / trending spike.
    PlatformVisibilitySpike = 11,
    /// High-risk but tradeable impulse.
    HighRiskTradableImpulse = 12,
    /// Active continuation of an existing trend.
    ActiveContinuation = 13,
    /// Breakout then retest.
    BreakoutRetest = 14,
    /// Failed breakdown reversal.
    FailedBreakdownReversal = 15,
    /// Reclaim of a lost level.
    Reclaim = 16,
    /// Compression-then-expansion.
    CompressionExpansion = 17,
    /// Mean-reversion snapback.
    MeanReversionSnap = 18,
    /// Liquidity dislocation.
    LiquidityDislocation = 19,
    /// Capital-rotation scalp.
    CapitalRotationScalp = 20,
    /// Mechanically untradeable trap.
    UntradeableTrap = 21,
    /// Unclassifiable / insufficient signal.
    Unknown = 22,
}

impl SetupArchetype {
    /// Stable `u16` label consumed downstream as `ThesisInputs.archetype`.
    #[inline]
    pub fn label(self) -> u16 {
        self as u16
    }
}

// ---------------------------------------------------------------------------
// Feature inputs
// ---------------------------------------------------------------------------

/// Migration lifecycle phase of the candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrationPhase {
    /// On the bonding curve, pre-migration.
    PreMigration,
    /// At the migration edge (about to / just migrating).
    MigrationEdge,
    /// Migrated to the AMM.
    PostMigration,
}

/// Deterministic market-structure state (from the §21.6 bar/market-structure
/// feature family). `None` means no distinctive structure was detected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StructureState {
    /// No distinctive structure.
    None,
    /// Active continuation of trend.
    ActiveContinuation,
    /// Breakout followed by retest.
    BreakoutRetest,
    /// Failed breakdown, reversing up.
    FailedBreakdownReversal,
    /// Reclaim of a prior level.
    Reclaim,
    /// Compression resolving into expansion.
    CompressionExpansion,
    /// Mean-reversion snapback.
    MeanReversionSnap,
    /// Liquidity dislocation.
    LiquidityDislocation,
}

/// Already-decoded integer feature measures a candidate is classified from.
///
/// All fields are deterministic point-in-time measures produced by the
/// features/signals crates — no wall-clock, no floats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArchetypeFeatures {
    /// Whether a sell route mechanically exists (false ⇒ untradeable).
    pub mechanically_sellable: bool,
    /// Fraction of position that can be exited at acceptable impact (bps).
    pub exit_capacity_bps: u32,
    /// Token age in seconds since mint.
    pub token_age_secs: u64,
    /// Migration lifecycle phase.
    pub migration: MigrationPhase,
    /// Fraction of supply held by same-block bundle snipers (bps).
    pub bundle_sniper_bps: u32,
    /// Fraction of breadth attributable to a single wallet cluster (bps).
    pub cluster_share_bps: u32,
    /// Creator ownership of supply (bps).
    pub creator_ownership_bps: u32,
    /// Deployer has reconciled prior rug / recycle history.
    pub creator_recycle: bool,
    /// Count of independent (cluster-adjusted) buyers.
    pub independent_buyers: u32,
    /// Attention-velocity measure (fixed-point, signed).
    pub attention_velocity: i64,
    /// A livestream is currently active on the launch.
    pub livestream_active: bool,
    /// Community / cult cohesion measure (bps).
    pub community_score_bps: u32,
    /// Social-stunt / meta-narrative measure (bps).
    pub social_stunt_bps: u32,
    /// Platform visibility / trending-spike measure (bps).
    pub platform_visibility_bps: u32,
    /// Deterministic market-structure state.
    pub structure: StructureState,
    /// Net capital rotating in from other tokens (fixed-point, signed).
    pub rotation_inflow: i64,
}

/// Operator-tunable classification thresholds (versioned, not hardcoded in path).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClassifierThresholds {
    /// Below this exit capacity the candidate is an untradeable trap (bps).
    pub min_exit_capacity_bps: u32,
    /// At/above this bundle-sniper share ⇒ bundle-sniper trap (bps).
    pub bundle_trap_bps: u32,
    /// At/above this single-cluster breadth share ⇒ wallet-cluster pump (bps).
    pub cluster_pump_bps: u32,
    /// At/above this attention velocity ⇒ CT attention shock.
    pub attention_shock: i64,
    /// At/above this social-stunt measure ⇒ social stunt / meta (bps).
    pub social_stunt_bps: u32,
    /// At/above this platform-visibility measure ⇒ visibility spike (bps).
    pub platform_visibility_bps: u32,
    /// At/above this community measure ⇒ creator cult / community (bps).
    pub community_bps: u32,
    /// At/above this rotation inflow ⇒ capital-rotation scalp.
    pub rotation_inflow: i64,
    /// At/below this token age (secs) a launch counts as a fresh mint.
    pub fresh_mint_secs: u64,
    /// At/above this many independent buyers ⇒ clean organic breadth.
    pub clean_breadth_buyers: u32,
    /// At/above this creator ownership (bps) ⇒ high-risk tradeable impulse.
    pub high_risk_creator_bps: u32,
}

impl ClassifierThresholds {
    /// A deterministic fixture used by the tests.
    pub fn test() -> Self {
        ClassifierThresholds {
            min_exit_capacity_bps: 1_000,
            bundle_trap_bps: 5_000,
            cluster_pump_bps: 6_000,
            attention_shock: 1_000,
            social_stunt_bps: 6_000,
            platform_visibility_bps: 6_000,
            community_bps: 6_000,
            rotation_inflow: 1_000,
            fresh_mint_secs: 300,
            clean_breadth_buyers: 40,
            high_risk_creator_bps: 3_000,
        }
    }
}

// ---------------------------------------------------------------------------
// Classifier (leaf: sa_classify)
// ---------------------------------------------------------------------------

/// Classify a candidate into one of the 23 §25 archetypes (leaf **sa_classify**).
///
/// Priority cascade, first match wins: mechanically untradeable / exit-starved
/// maps to `UntradeableTrap`; then bundle-sniper trap, wallet-cluster pump,
/// deployer recycle risk; then the seven distinctive market-structure archetypes;
/// then migration edge (momentum) and post-migration (revival); then livestream,
/// attention shock, social stunt / meta, platform-visibility spike, creator cult
/// / community, and capital-rotation inflow; then fresh mint by age, clean organic
/// breadth by buyer count, and high creator ownership (high-risk tradeable
/// impulse); else `Unknown`.
///
/// Pure integer, deterministic — identical features always yield the same label.
pub fn classify_archetype(f: &ArchetypeFeatures, th: &ClassifierThresholds) -> SetupArchetype {
    use SetupArchetype::*;

    // 1. Mechanically untradeable dominates everything.
    if !f.mechanically_sellable || f.exit_capacity_bps < th.min_exit_capacity_bps {
        return UntradeableTrap;
    }
    // 2-4. Manipulation / deployer traps outrank any bullish reading.
    if f.bundle_sniper_bps >= th.bundle_trap_bps {
        return BundleSniperTrap;
    }
    if f.cluster_share_bps >= th.cluster_pump_bps {
        return WalletClusterPump;
    }
    if f.creator_recycle {
        return DevRecycleRisk;
    }
    // 5. Distinctive market structure (active-market archetypes).
    match f.structure {
        StructureState::ActiveContinuation => return ActiveContinuation,
        StructureState::BreakoutRetest => return BreakoutRetest,
        StructureState::FailedBreakdownReversal => return FailedBreakdownReversal,
        StructureState::Reclaim => return Reclaim,
        StructureState::CompressionExpansion => return CompressionExpansion,
        StructureState::MeanReversionSnap => return MeanReversionSnap,
        StructureState::LiquidityDislocation => return LiquidityDislocation,
        StructureState::None => {}
    }
    // 6-7. Migration phase.
    match f.migration {
        MigrationPhase::MigrationEdge => return MigrationMomentum,
        MigrationPhase::PostMigration => return PostMigrationRevival,
        MigrationPhase::PreMigration => {}
    }
    // 8-13. Attention / social / rotation launch archetypes.
    if f.livestream_active {
        return PumpLiveStream;
    }
    if f.attention_velocity >= th.attention_shock {
        return CtAttentionShock;
    }
    if f.social_stunt_bps >= th.social_stunt_bps {
        return SocialStuntOrMeta;
    }
    if f.platform_visibility_bps >= th.platform_visibility_bps {
        return PlatformVisibilitySpike;
    }
    if f.community_score_bps >= th.community_bps {
        return CreatorCultOrCommunity;
    }
    if f.rotation_inflow >= th.rotation_inflow {
        return CapitalRotationScalp;
    }
    // 14-16. Fresh / organic / residual-risk classes.
    if f.token_age_secs <= th.fresh_mint_secs {
        return FreshMintFlow;
    }
    if f.independent_buyers >= th.clean_breadth_buyers {
        return CleanOrganicBreadth;
    }
    if f.creator_ownership_bps >= th.high_risk_creator_bps {
        return HighRiskTradableImpulse;
    }
    Unknown
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// A clean, unremarkable, tradeable baseline that falls through to `Unknown`
    /// unless a test flips a specific field.
    fn base() -> ArchetypeFeatures {
        ArchetypeFeatures {
            mechanically_sellable: true,
            exit_capacity_bps: 8_000,
            token_age_secs: 100_000,
            migration: MigrationPhase::PreMigration,
            bundle_sniper_bps: 0,
            cluster_share_bps: 0,
            creator_ownership_bps: 0,
            creator_recycle: false,
            independent_buyers: 0,
            attention_velocity: 0,
            livestream_active: false,
            community_score_bps: 0,
            social_stunt_bps: 0,
            platform_visibility_bps: 0,
            structure: StructureState::None,
            rotation_inflow: 0,
        }
    }

    fn classify(f: ArchetypeFeatures) -> SetupArchetype {
        classify_archetype(&f, &ClassifierThresholds::test())
    }

    #[test]
    fn labels_are_stable_and_distinct() {
        assert_eq!(SetupArchetype::FreshMintFlow.label(), 0);
        assert_eq!(SetupArchetype::UntradeableTrap.label(), 21);
        assert_eq!(SetupArchetype::Unknown.label(), 22);
    }

    #[test]
    fn untradeable_dominates_even_a_bullish_reading() {
        let mut f = base();
        f.mechanically_sellable = false;
        f.attention_velocity = 100_000; // would otherwise be an attention shock
        f.structure = StructureState::BreakoutRetest;
        assert_eq!(classify(f), SetupArchetype::UntradeableTrap);

        let mut g = base();
        g.exit_capacity_bps = 500; // below the 1_000 bps min
        assert_eq!(classify(g), SetupArchetype::UntradeableTrap);
    }

    #[test]
    fn manipulation_traps_outrank_structure() {
        let mut f = base();
        f.bundle_sniper_bps = 6_000;
        f.structure = StructureState::ActiveContinuation;
        assert_eq!(classify(f), SetupArchetype::BundleSniperTrap);

        let mut g = base();
        g.cluster_share_bps = 7_000;
        assert_eq!(classify(g), SetupArchetype::WalletClusterPump);

        let mut h = base();
        h.creator_recycle = true;
        assert_eq!(classify(h), SetupArchetype::DevRecycleRisk);
    }

    #[test]
    fn each_structure_state_maps_to_its_archetype() {
        let cases = [
            (
                StructureState::ActiveContinuation,
                SetupArchetype::ActiveContinuation,
            ),
            (
                StructureState::BreakoutRetest,
                SetupArchetype::BreakoutRetest,
            ),
            (
                StructureState::FailedBreakdownReversal,
                SetupArchetype::FailedBreakdownReversal,
            ),
            (StructureState::Reclaim, SetupArchetype::Reclaim),
            (
                StructureState::CompressionExpansion,
                SetupArchetype::CompressionExpansion,
            ),
            (
                StructureState::MeanReversionSnap,
                SetupArchetype::MeanReversionSnap,
            ),
            (
                StructureState::LiquidityDislocation,
                SetupArchetype::LiquidityDislocation,
            ),
        ];
        for (s, want) in cases {
            let mut f = base();
            f.structure = s;
            assert_eq!(classify(f), want, "structure {s:?}");
        }
    }

    #[test]
    fn migration_phases() {
        let mut edge = base();
        edge.migration = MigrationPhase::MigrationEdge;
        assert_eq!(classify(edge), SetupArchetype::MigrationMomentum);

        let mut post = base();
        post.migration = MigrationPhase::PostMigration;
        assert_eq!(classify(post), SetupArchetype::PostMigrationRevival);
    }

    #[test]
    fn attention_and_social_launch_archetypes() {
        let mut live = base();
        live.livestream_active = true;
        assert_eq!(classify(live), SetupArchetype::PumpLiveStream);

        let mut shock = base();
        shock.attention_velocity = 2_000;
        assert_eq!(classify(shock), SetupArchetype::CtAttentionShock);

        let mut stunt = base();
        stunt.social_stunt_bps = 7_000;
        assert_eq!(classify(stunt), SetupArchetype::SocialStuntOrMeta);

        let mut vis = base();
        vis.platform_visibility_bps = 7_000;
        assert_eq!(classify(vis), SetupArchetype::PlatformVisibilitySpike);

        let mut cult = base();
        cult.community_score_bps = 7_000;
        assert_eq!(classify(cult), SetupArchetype::CreatorCultOrCommunity);

        let mut rot = base();
        rot.rotation_inflow = 5_000;
        assert_eq!(classify(rot), SetupArchetype::CapitalRotationScalp);
    }

    #[test]
    fn fresh_organic_and_residual_classes() {
        let mut fresh = base();
        fresh.token_age_secs = 120; // within the 300s fresh window
        assert_eq!(classify(fresh), SetupArchetype::FreshMintFlow);

        let mut organic = base();
        organic.independent_buyers = 60; // above 40 clean-breadth threshold
        assert_eq!(classify(organic), SetupArchetype::CleanOrganicBreadth);

        let mut risky = base();
        risky.creator_ownership_bps = 4_000; // above high-risk-creator threshold
        assert_eq!(classify(risky), SetupArchetype::HighRiskTradableImpulse);
    }

    #[test]
    fn falls_through_to_unknown() {
        assert_eq!(classify(base()), SetupArchetype::Unknown);
    }

    #[test]
    fn deterministic_repeat() {
        let f = base();
        assert_eq!(classify(f), classify(f));
    }
}
