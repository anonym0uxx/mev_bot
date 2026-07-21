//! # signal_horizon — Signal-Horizon Matching Law gate (criterion 96)
//!
//! Mechanical horizon-admission: a feature with measured end-to-end detection +
//! capture latency `L` is admissible to a lane with decision horizon `H` only when
//! `L + margin <= H` — slow intelligence can inform holds/exits/sizing/meta but is
//! structurally excluded from any lane whose entry horizon it cannot beat. On top
//! of the latency compare there is a **horizon-classification table**: launch-time
//! social-linkage features are admissible only to early-entry lanes; TikTok
//! content/virality is confined to hold/exit-context, source-quality, and
//! meta-emergence; on-chain flow is admissible everywhere.
//!
//! ## Constitution
//! §46 Signal-Horizon Matching Law, §29.7 horizon classification. §22 integer
//! (latencies in ns); pure deterministic lookup + compare.

/// A decision lane, with its natural decision horizon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    /// Earliest entry lane (creation snipe) — shortest horizon.
    CreationSniper,
    /// Early-confirmation entry lane.
    EarlyEntry,
    /// Position-management: holds and exits of running positions.
    HoldExitContext,
    /// Source-quality / reputation context.
    SourceQuality,
    /// Category-level meta / regime emergence.
    MetaEmergence,
}

impl Lane {
    /// Whether this lane makes fresh **entry** decisions (latency-critical).
    #[inline]
    pub fn is_entry_lane(self) -> bool {
        matches!(self, Lane::CreationSniper | Lane::EarlyEntry)
    }
}

/// The horizon class of a feature — the classification-table axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeatureClass {
    /// On-chain / decoded swap flow — fastest, admissible to any lane.
    OnChainFlow,
    /// Launch-time declared social linkage (early-available durability predictor).
    LaunchSocialLinkage,
    /// X/CT text — fast social.
    XText,
    /// TikTok content/virality — structurally late.
    TikTokVirality,
    /// Meta/regime category signal.
    Meta,
}

/// The admission verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HorizonVerdict {
    /// Admissible: passes both the class table and the latency compare.
    Admissible,
    /// The feature class is not admissible to this lane (table rejection).
    ClassForbidden,
    /// The class is allowed here but the measured latency does not beat the
    /// lane horizon with margin.
    TooSlow,
}

/// Horizon-classification table (leaf helper).
///
/// * `OnChainFlow` — admissible to every lane.
/// * `LaunchSocialLinkage` — admissible only to entry lanes (the sole social
///   input allowed near entry) and to source-quality context.
/// * `XText` — admissible to entry lanes and all context lanes (fast social).
/// * `TikTokVirality` — confined to hold/exit-context, source-quality, and
///   meta-emergence; never an entry lane.
/// * `Meta` — admissible to meta-emergence, hold/exit-context, and source-quality;
///   never a per-token entry lane.
pub fn class_admissible_to(class: FeatureClass, lane: Lane) -> bool {
    match class {
        FeatureClass::OnChainFlow => true,
        FeatureClass::LaunchSocialLinkage => lane.is_entry_lane() || lane == Lane::SourceQuality,
        FeatureClass::XText => true,
        FeatureClass::TikTokVirality => matches!(
            lane,
            Lane::HoldExitContext | Lane::SourceQuality | Lane::MetaEmergence
        ),
        FeatureClass::Meta => matches!(
            lane,
            Lane::MetaEmergence | Lane::HoldExitContext | Lane::SourceQuality
        ),
    }
}

/// The pure latency compare `L + margin <= H` (leaf helper).
///
/// Overflow-safe (saturating add). A feature exactly meeting the horizon with
/// margin is admissible.
#[inline]
pub fn latency_beats_horizon(
    feature_latency_ns: u64,
    lane_horizon_ns: u64,
    margin_ns: u64,
) -> bool {
    feature_latency_ns.saturating_add(margin_ns) <= lane_horizon_ns
}

/// The full Signal-Horizon Matching gate (leaf **sh_admit**).
///
/// A feature is [`HorizonVerdict::Admissible`] iff its class is permitted for the
/// lane (classification table) **and** its measured latency beats the lane horizon
/// with margin. The class check runs first, so a class-forbidden feature reports
/// [`HorizonVerdict::ClassForbidden`] even if it were fast enough. Pure and
/// deterministic.
pub fn admit_feature_to_lane(
    feature_latency_ns: u64,
    class: FeatureClass,
    lane: Lane,
    lane_horizon_ns: u64,
    margin_ns: u64,
) -> HorizonVerdict {
    if !class_admissible_to(class, lane) {
        return HorizonVerdict::ClassForbidden;
    }
    if latency_beats_horizon(feature_latency_ns, lane_horizon_ns, margin_ns) {
        HorizonVerdict::Admissible
    } else {
        HorizonVerdict::TooSlow
    }
}
