//! §29.6 `SocialCatalystClassifier` — the ten-class attention-trigger taxonomy.
//!
//! This is the *catalyst* taxonomy (what kind of thing is driving the attention
//! spike), distinct from the §29.6 narrative *category* ([`crate::NarrativeClass`])
//! and from the §29.8 per-source `SocialSourceQualityLedger` (which classifies an
//! account's track record). Here the subject is the attention trigger itself.
//!
//! Hard invariants (constitution):
//! * §22 — integer / fixed-point only, no float on the outcome path.
//! * Deterministic, total, ordered classifier (first matching rule wins), so
//!   every variant is reachable and the mapping is a pure function of inputs.
//! * §29.5 fade-first — with no data the classifier returns
//!   [`SocialCatalyst::Unknown`]; absence is never fabricated into a signal.
//! * Corroboration-tier / feature-admission-gated (§29.2 / §46): the output is a
//!   feature for the research plane, never a standalone trade authorization.
//!
//! Feature inputs are supplied by the caller (the live capture that produces
//! them — `pump-quant-social` copy-echo density, D5 skin-in-game, platform lead —
//! is server-side); this module is the portable classification logic over a
//! given integer feature vector.

/// The ten enumerated social-catalyst classes (§29.6).
///
/// Ordering is the canonical constitution order and is load-bearing: variants
/// earlier in manipulation severity are matched first by [`classify`]. `Unknown`
/// is last — the §29.5 fade-first no-data default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SocialCatalyst {
    /// Attention rising *before* any on-chain flow — the earliest, most prized
    /// setup (one-step-ahead, §783).
    PreFlowDiscovery,
    /// Attention rising *alongside* live positive net flow — a genuine
    /// amplifier riding confirmed money.
    LiveFlowAmplifier,
    /// Promotion while money is *exiting* — attention manufactured to supply
    /// exit liquidity to insiders (the §29.6 late-exit trap).
    LateExitLiquidityPromotion,
    /// Synchronized, duplicated posting bursts — coordinated inauthentic spam.
    CoordinatedSpam,
    /// High semantic duplication without burst synchronization — copy/echo
    /// amplification.
    CopyEcho,
    /// The push is funded from the creator's own wallet (D5 skin-in-game
    /// inversion) — a paid self-promotion.
    CreatorFundedPush,
    /// Broad, independent, low-duplication community growth — the rare genuine
    /// formation.
    GenuineCommunityFormation,
    /// A live stream / stunt drove the attention spike.
    StreamStuntAttention,
    /// A platform's algorithmic visibility surge (trending/feed placement)
    /// rather than organic sources.
    PlatformVisibilitySurge,
    /// No data / no dominant driver — §29.5 fade-first default.
    Unknown,
}

/// Integer/fixed-point feature vector consumed by [`classify`].
///
/// Every field is a caller-supplied deterministic measurement (§22). Basis-point
/// fields are in `[0, 10_000]` (`10_000` == 100%). The classifier never reads
/// wall-clock or live state; it reads only this vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalystFeatures {
    /// Number of observed mentions/samples backing this classification.
    /// `0` forces [`SocialCatalyst::Unknown`] (no data, §29.5).
    pub sample_count: u32,
    /// Semantic-duplication density in basis points (from
    /// `pump-quant-social::copy_echo::copy_echo_density_bps`): share of mentions
    /// that are near-duplicates.
    pub copy_echo_density_bps: u64,
    /// Burst-synchronization coordination in basis points: share of mentions
    /// clustered in synchronized time bursts (distinguishes coordinated spam
    /// from organic copy/echo).
    pub coordination_bps: u64,
    /// Creator-wallet funding share of the push in basis points (D5 skin-in-game
    /// inversion, `pump-quant-social::determinants::d5_skin_in_game`).
    pub creator_funding_bps: u64,
    /// Platform algorithmic-visibility share in basis points (feed/trending
    /// placement not attributable to organic sources).
    pub platform_surge_bps: u64,
    /// Whether a live stream / stunt event coincided with the spike.
    pub streamer_event: bool,
    /// On-chain net SOL flow over the window (sign matters; negative == net
    /// outflow / insiders exiting).
    pub net_flow: i64,
    /// Attention velocity (first difference of the attention level). `> 0`
    /// means attention is rising.
    pub attention_velocity: i64,
    /// Count of independent sources (breadth).
    pub unique_sources: u32,
    /// Count of independent buyer communities the attention converted into
    /// (from market-state breadth) — the genuine-formation signal.
    pub independent_breadth: u32,
}

/// Decision thresholds for [`classify`]. All integer/fixed-point (§22).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalystThresholds {
    /// Copy/echo density (bps) at or above which duplication dominates.
    pub echo_bps: u64,
    /// Coordination (bps) at or above which bursts count as coordinated.
    pub coordination_bps: u64,
    /// Creator-funding share (bps) at or above which the push is creator-funded.
    pub creator_bps: u64,
    /// Platform-visibility share (bps) at or above which a surge is platform-led.
    pub platform_bps: u64,
    /// Net-flow floor: flow strictly below `-exit_flow` counts as an active
    /// exit (drives late-exit-liquidity promotion).
    pub exit_flow: i64,
    /// Net-flow floor at or below which money is "not yet flowing" (pre-flow).
    pub flow_floor: i64,
    /// Minimum unique sources for genuine community formation.
    pub genuine_sources: u32,
    /// Minimum independent breadth for genuine community formation.
    pub genuine_breadth: u32,
}

impl CatalystThresholds {
    /// A standard, non-degenerate threshold set (documented defaults, no float).
    ///
    /// Echo/coordination/platform gates at 60%/50%/50%, creator funding at 40%,
    /// a small exit floor and a zero flow floor, breadth gates for genuine
    /// formation. Callers may override per regime.
    pub const fn standard() -> Self {
        Self {
            echo_bps: 6_000,
            coordination_bps: 5_000,
            creator_bps: 4_000,
            platform_bps: 5_000,
            exit_flow: 0,
            flow_floor: 0,
            genuine_sources: 8,
            genuine_breadth: 5,
        }
    }
}

/// Classify the social catalyst from a feature vector and thresholds.
///
/// Ordered, total, deterministic (first match wins). Manipulation classes are
/// tested before benign flow classes so that a spike which is *both* rising and
/// coordinated is reported as manipulation, not discovery. Ordered rules:
/// 1. `sample_count == 0` → [`SocialCatalyst::Unknown`] (no data, §29.5).
/// 2. duplication ≥ `echo_bps` **and** coordination ≥ `coordination_bps` →
///    [`SocialCatalyst::CoordinatedSpam`].
/// 3. duplication ≥ `echo_bps` → [`SocialCatalyst::CopyEcho`].
/// 4. creator funding ≥ `creator_bps` → [`SocialCatalyst::CreatorFundedPush`].
/// 5. platform surge ≥ `platform_bps` → [`SocialCatalyst::PlatformVisibilitySurge`].
/// 6. `streamer_event` → [`SocialCatalyst::StreamStuntAttention`].
/// 7. attention rising **and** net flow strictly below `-exit_flow` →
///    [`SocialCatalyst::LateExitLiquidityPromotion`].
/// 8. attention rising **and** net flow ≤ `flow_floor` (no flow yet) →
///    [`SocialCatalyst::PreFlowDiscovery`].
/// 9. sources ≥ `genuine_sources` **and** breadth ≥ `genuine_breadth` →
///    [`SocialCatalyst::GenuineCommunityFormation`].
/// 10. attention rising (flow present, narrow) →
///     [`SocialCatalyst::LiveFlowAmplifier`].
/// 11. otherwise → [`SocialCatalyst::Unknown`].
///
/// Overflow-free: comparisons only, no arithmetic that can wrap.
pub fn classify(f: &CatalystFeatures, t: &CatalystThresholds) -> SocialCatalyst {
    if f.sample_count == 0 {
        return SocialCatalyst::Unknown;
    }
    // Manipulation tier (severity-ordered) preempts benign interpretation.
    if f.copy_echo_density_bps >= t.echo_bps && f.coordination_bps >= t.coordination_bps {
        return SocialCatalyst::CoordinatedSpam;
    }
    if f.copy_echo_density_bps >= t.echo_bps {
        return SocialCatalyst::CopyEcho;
    }
    if f.creator_funding_bps >= t.creator_bps {
        return SocialCatalyst::CreatorFundedPush;
    }
    if f.platform_surge_bps >= t.platform_bps {
        return SocialCatalyst::PlatformVisibilitySurge;
    }
    if f.streamer_event {
        return SocialCatalyst::StreamStuntAttention;
    }
    // Flow interpretation tier.
    let rising = f.attention_velocity > 0;
    // `exit_flow >= 0`; `-exit_flow` cannot overflow because exit_flow is small
    // by contract, but guard with a checked negate that saturates (§22).
    let exit_bound = t.exit_flow.checked_neg().unwrap_or(i64::MAX);
    if rising && f.net_flow < exit_bound {
        return SocialCatalyst::LateExitLiquidityPromotion;
    }
    if rising && f.net_flow <= t.flow_floor {
        return SocialCatalyst::PreFlowDiscovery;
    }
    if f.unique_sources >= t.genuine_sources && f.independent_breadth >= t.genuine_breadth {
        return SocialCatalyst::GenuineCommunityFormation;
    }
    if rising {
        return SocialCatalyst::LiveFlowAmplifier;
    }
    SocialCatalyst::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> CatalystFeatures {
        CatalystFeatures {
            sample_count: 100,
            copy_echo_density_bps: 0,
            coordination_bps: 0,
            creator_funding_bps: 0,
            platform_surge_bps: 0,
            streamer_event: false,
            net_flow: 0,
            attention_velocity: 0,
            unique_sources: 0,
            independent_breadth: 0,
        }
    }

    #[test]
    fn no_data_is_unknown() {
        let mut f = base();
        f.sample_count = 0;
        // Even with strong manipulation signals present, zero samples => Unknown.
        f.copy_echo_density_bps = 9_000;
        f.coordination_bps = 9_000;
        assert_eq!(
            classify(&f, &CatalystThresholds::standard()),
            SocialCatalyst::Unknown
        );
    }

    #[test]
    fn coordinated_spam_needs_both_echo_and_coordination() {
        let t = CatalystThresholds::standard();
        let mut f = base();
        f.copy_echo_density_bps = 7_000;
        f.coordination_bps = 6_000;
        assert_eq!(classify(&f, &t), SocialCatalyst::CoordinatedSpam);
        // Duplication without coordination downgrades to CopyEcho.
        f.coordination_bps = 100;
        assert_eq!(classify(&f, &t), SocialCatalyst::CopyEcho);
    }

    #[test]
    fn manipulation_preempts_rising_flow() {
        let t = CatalystThresholds::standard();
        let mut f = base();
        // A rising, pre-flow spike that is also coordinated is spam, not discovery.
        f.attention_velocity = 500;
        f.net_flow = -10;
        f.copy_echo_density_bps = 8_000;
        f.coordination_bps = 8_000;
        assert_eq!(classify(&f, &t), SocialCatalyst::CoordinatedSpam);
    }

    #[test]
    fn creator_and_platform_and_stream() {
        let t = CatalystThresholds::standard();
        let mut f = base();
        f.creator_funding_bps = 4_000;
        assert_eq!(classify(&f, &t), SocialCatalyst::CreatorFundedPush);

        let mut f = base();
        f.platform_surge_bps = 6_000;
        assert_eq!(classify(&f, &t), SocialCatalyst::PlatformVisibilitySurge);

        let mut f = base();
        f.streamer_event = true;
        assert_eq!(classify(&f, &t), SocialCatalyst::StreamStuntAttention);
    }

    #[test]
    fn late_exit_when_promoting_into_outflow() {
        let mut t = CatalystThresholds::standard();
        t.exit_flow = 100; // outflow beyond -100 counts as active exit.
        let mut f = base();
        f.attention_velocity = 300;
        f.net_flow = -500;
        assert_eq!(classify(&f, &t), SocialCatalyst::LateExitLiquidityPromotion);
    }

    #[test]
    fn pre_flow_discovery_vs_live_amplifier() {
        let t = CatalystThresholds::standard();
        let mut f = base();
        // Rising, no flow yet => discovery.
        f.attention_velocity = 300;
        f.net_flow = 0;
        assert_eq!(classify(&f, &t), SocialCatalyst::PreFlowDiscovery);
        // Rising with live positive flow, narrow breadth => amplifier.
        f.net_flow = 5_000;
        assert_eq!(classify(&f, &t), SocialCatalyst::LiveFlowAmplifier);
    }

    #[test]
    fn genuine_community_formation() {
        let t = CatalystThresholds::standard();
        let mut f = base();
        // Not rising (velocity 0) but broad & independent => genuine formation.
        f.unique_sources = 20;
        f.independent_breadth = 12;
        assert_eq!(classify(&f, &t), SocialCatalyst::GenuineCommunityFormation);
    }

    #[test]
    fn flat_narrow_is_unknown() {
        let t = CatalystThresholds::standard();
        let f = base(); // sample_count>0 but no dominant driver.
        assert_eq!(classify(&f, &t), SocialCatalyst::Unknown);
    }

    #[test]
    fn all_ten_variants_reachable() {
        let t = CatalystThresholds::standard();
        use SocialCatalyst::*;
        let mut seen = std::collections::HashSet::new();

        let mut z = base();
        z.sample_count = 0;
        seen.insert(classify(&z, &t));

        let mut spam = base();
        spam.copy_echo_density_bps = 9_000;
        spam.coordination_bps = 9_000;
        seen.insert(classify(&spam, &t));

        let mut echo = base();
        echo.copy_echo_density_bps = 9_000;
        seen.insert(classify(&echo, &t));

        let mut cr = base();
        cr.creator_funding_bps = 9_000;
        seen.insert(classify(&cr, &t));

        let mut pl = base();
        pl.platform_surge_bps = 9_000;
        seen.insert(classify(&pl, &t));

        let mut st = base();
        st.streamer_event = true;
        seen.insert(classify(&st, &t));

        let mut le = base();
        le.attention_velocity = 1;
        le.net_flow = -1;
        seen.insert(classify(&le, &t));

        let mut pf = base();
        pf.attention_velocity = 1;
        pf.net_flow = 0;
        seen.insert(classify(&pf, &t));

        let mut gc = base();
        gc.unique_sources = 100;
        gc.independent_breadth = 100;
        seen.insert(classify(&gc, &t));

        let mut la = base();
        la.attention_velocity = 1;
        la.net_flow = 10_000;
        seen.insert(classify(&la, &t));

        for v in [
            PreFlowDiscovery,
            LiveFlowAmplifier,
            LateExitLiquidityPromotion,
            CoordinatedSpam,
            CopyEcho,
            CreatorFundedPush,
            GenuineCommunityFormation,
            StreamStuntAttention,
            PlatformVisibilitySurge,
            Unknown,
        ] {
            assert!(seen.contains(&v), "variant {v:?} not reachable");
        }
        assert_eq!(seen.len(), 10);
    }
}
