//! `archetype` — named, measurable **style lenses** over the setup fingerprint
//! (constitution 22 integer-only, 46 small-n, 57 bounded, 100 phase separation,
//! 102 named thresholds).
//!
//! # What these are, and what they are explicitly not
//!
//! The lenses below — [`StyleLens::EarlyRotation`], [`StyleLens::FlowScalper`],
//! [`StyleLens::Sniper`], [`StyleLens::ConvictionSize`] — are **style archetypes
//! derived from observable trading behaviour**: recurring, measurable
//! configurations of the twenty market features this crate already quantizes. They
//! are *not* models of, claims about, or imitations of any specific individual
//! trader, and nothing here is derived from any person's private information,
//! statements, or claimed results.
//!
//! The distinction matters operationally, not just legally. "Trade like a famous
//! wallet" is not implementable: we cannot observe their reasoning, their sizing,
//! their risk budget, or the half of their trades they never posted about, and any
//! attempt to reconstruct it would be fitting noise to a survivor. What *is*
//! implementable is the honest residue of the idea: **there are distinguishable
//! ways to trade this market, they leave different fingerprints, and they pay
//! differently at different times.** A lens is a documented
//! [`FeatureWeights`] profile plus a [`RecallFilter`] shape plus a set of bucket
//! preferences — a hypothesis about which features matter for a style, written
//! down where it can be argued with.
//!
//! Validation is the second half of the discipline. A lens is only ever confirmed
//! or refuted against **our own realized net SOL**, through
//! [`archetype_performance`], which is `RecallVerdict`-shaped and therefore
//! fail-closed. No lens is ever credited because it is popular, because a well-known
//! trader is associated with it, or because it feels right. If `EarlyRotation` has
//! not paid *us* over a sufficient sample this month, this module says so, and
//! [`best_paying_lens`] will hand reflection a different one.
//!
//! # The four lenses
//!
//! * **[`StyleLens::EarlyRotation`]** — be early on the next meta. Meta emergence
//!   and attention velocity leading price, very young tokens, rotation-aware.
//!   Weights attention velocity, meta category and meta saturation highest; pins
//!   the meta category in its recall filter, because rotation is a question *about*
//!   a meta.
//! * **[`StyleLens::FlowScalper`]** — read the tape. Order-flow imbalance, CVD and
//!   burst phase dominant, cost-sensitive because the style is high frequency and
//!   friction compounds. Short holds. No meta pin: structure travels across metas.
//! * **[`StyleLens::Sniper`]** — extreme earliness. The creation window and the
//!   earliest age bucket, thin liquidity, almost no corroboration available, so
//!   creator class carries unusual weight — it is nearly the only prior that exists
//!   at second thirty. Pins the [`DiscoveryLane::NewMint`] lane.
//! * **[`StyleLens::ConvictionSize`]** — fewer, better. Multi-factor confluence
//!   (structure *and* flow *and* breadth *and* authenticity), higher authenticity
//!   floor, longer holds. The most rules to satisfy, so the hardest lens to fit —
//!   which is the point.
//!
//! # Affinity scoring
//!
//! Each lens owns a table of [`AffinityRule`]s: a field, a bucket
//! [`FieldPreference`], and integer points. [`classify`] awards a rule's points
//! when the query's bucket satisfies the preference, and reports
//! `points_met * 10_000 / points_possible` in basis points. Scores are therefore
//! comparable across lenses even though the tables differ in size. A lens "fits"
//! at [`ARCHETYPE_FIT_MIN_BP`]. Ties on the best lens break by ascending
//! [`StyleLens::ordinal`] — deterministic, never by iteration order.
//!
//! # Phase separation (constitution 100)
//!
//! [`archetype_performance`] takes a mandatory [`VenuePhase`], exactly as
//! [`RecallFilter`] does. There is no phase-pooled per-lens statistic and there
//! never will be: a scalp on the bonding curve and a scalp in the migrated pool have
//! different fee, slippage and adversary structure, and averaging them produces a
//! number describing neither.
//!
//! # Determinism
//!
//! No wall clock, no RNG, no floats, no unordered iteration. Every function here is
//! a pure function of the fingerprint and the index.

use crate::episode::DiscoveryLane;
use crate::fingerprint::{
    unweighted_distance, weighted_distance, FeatureWeights, SetupFingerprint, VenuePhase,
    FIELD_COUNT, F_ATTENTION_VELOCITY, F_AUTHENTICITY, F_BURST_PHASE, F_BUYER_BREADTH,
    F_CREATOR_CLASS, F_CVD_DECADE, F_DESIGNATED_CALLER, F_HOLDER_GROWTH_ACCEL, F_LIQUIDITY_DECADE,
    F_META_CATEGORY, F_META_SATURATION, F_NARRATIVE_CLASS, F_OFI, F_RANGE_STATE, F_REALIZED_VOL,
    F_ROUND_TRIP_COST, F_TIME_OF_DAY, F_TOKEN_AGE, F_TREND_STRUCTURE, F_VENUE_PHASE,
};
use crate::recall::{
    order_stat_i128, order_stat_u64, EpisodicIndex, RecallFilter, RecallParams, RecallStats,
    RecallUnknown, RecallVerdict, BPS_SCALE_U32, MIN_SAMPLE_DEFAULT, P25, P50, P75,
};

// ---------------------------------------------------------------------------
// Named constants (constitution 102)
// ---------------------------------------------------------------------------

/// Number of style lenses defined here (constitution 102).
pub const LENS_COUNT: usize = 4;

/// Affinity at or above which a setup is said to *fit* a lens (constitution 102).
/// Sixty percent of the lens's available points: enough that most of the style's
/// defining features are present, loose enough that one missing feature does not
/// disqualify a setup.
pub const ARCHETYPE_FIT_MIN_BP: u32 = 6_000;

/// Default minimum matched episodes before a per-lens statistic is reported
/// (constitution 46). Inherited from [`MIN_SAMPLE_DEFAULT`] so the whole crate has
/// one small-n floor.
pub const ARCHETYPE_MIN_SAMPLE: u32 = MIN_SAMPLE_DEFAULT;

// ---------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------

/// A condition on one fingerprint field's bucket.
///
/// Deliberately expressed in **bucket ordinals**, not raw market quantities: the
/// ladders in [`crate::fingerprint`] are the single place a boundary is defined, and
/// a lens that re-derived its own thresholds would be a second, silently diverging
/// source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldPreference {
    /// Bucket must be at or below this value.
    AtMost(u8),
    /// Bucket must be at or above this value.
    AtLeast(u8),
    /// Bucket must equal this value exactly.
    Equals(u8),
    /// Bucket must lie in this inclusive range.
    Between(u8, u8),
}

impl FieldPreference {
    /// Whether a bucket satisfies the preference.
    #[must_use]
    pub const fn is_satisfied(self, bucket: u8) -> bool {
        match self {
            Self::AtMost(x) => bucket <= x,
            Self::AtLeast(x) => bucket >= x,
            Self::Equals(x) => bucket == x,
            Self::Between(lo, hi) => bucket >= lo && bucket <= hi,
        }
    }

    /// The single most representative bucket for this preference, used to build a
    /// lens's exemplar fingerprint.
    #[must_use]
    pub const fn exemplar(self) -> u8 {
        match self {
            Self::AtMost(x) | Self::AtLeast(x) | Self::Equals(x) => x,
            Self::Between(lo, hi) => (lo + hi) / 2,
        }
    }
}

/// One weighted condition in a lens's affinity table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AffinityRule {
    /// Field index — one of the `F_*` constants in [`crate::fingerprint`].
    pub field: usize,
    /// The condition on that field's bucket.
    pub pref: FieldPreference,
    /// Points awarded when the condition holds.
    pub points: u32,
}

/// Affinity table for [`StyleLens::EarlyRotation`] (constitution 102).
///
/// Young token, attention already accelerating, meta not yet saturated, holders
/// compounding. Attention velocity and meta position carry the most points because
/// the style's entire claim is that *attention leads price* early in a rotation.
pub const EARLY_ROTATION_RULES: &[AffinityRule] = &[
    rule(F_TOKEN_AGE, FieldPreference::AtMost(2), 5),
    rule(F_ATTENTION_VELOCITY, FieldPreference::AtLeast(3), 6),
    rule(F_META_SATURATION, FieldPreference::AtMost(1), 6),
    rule(F_HOLDER_GROWTH_ACCEL, FieldPreference::AtLeast(3), 4),
    rule(F_BUYER_BREADTH, FieldPreference::AtLeast(1), 2),
    rule(F_AUTHENTICITY, FieldPreference::AtLeast(2), 3),
];

/// Affinity table for [`StyleLens::FlowScalper`] (constitution 102).
///
/// A live burst, decisive signed flow, enough volatility to pay for the round trip,
/// and — the rule that keeps the style honest — **low friction**. A scalper who
/// ignores round-trip cost is not a scalper, they are a donor.
pub const FLOW_SCALPER_RULES: &[AffinityRule] = &[
    rule(F_BURST_PHASE, FieldPreference::Between(1, 2), 6),
    rule(F_OFI, FieldPreference::AtLeast(5), 6),
    rule(F_CVD_DECADE, FieldPreference::AtLeast(4), 4),
    rule(F_REALIZED_VOL, FieldPreference::AtLeast(2), 4),
    rule(F_BUYER_BREADTH, FieldPreference::AtLeast(2), 3),
    rule(F_ROUND_TRIP_COST, FieldPreference::AtMost(2), 5),
    rule(F_LIQUIDITY_DECADE, FieldPreference::AtLeast(3), 3),
];

/// Affinity table for [`StyleLens::Sniper`] (constitution 102).
///
/// The creation window: the earliest age bucket, on the curve, before breadth or
/// liquidity exist. Corroboration is structurally unavailable this early, which is
/// why the lens leans on what *is* observable at second thirty — the creator.
pub const SNIPER_RULES: &[AffinityRule] = &[
    rule(F_TOKEN_AGE, FieldPreference::Equals(0), 10),
    rule(F_VENUE_PHASE, FieldPreference::Equals(0), 5),
    rule(F_BUYER_BREADTH, FieldPreference::AtMost(1), 4),
    rule(F_LIQUIDITY_DECADE, FieldPreference::AtMost(2), 3),
    rule(F_META_SATURATION, FieldPreference::AtMost(1), 3),
    rule(F_ROUND_TRIP_COST, FieldPreference::AtMost(3), 3),
];

/// Affinity table for [`StyleLens::ConvictionSize`] (constitution 102).
///
/// Nine rules — the most of any lens, deliberately. This style's edge is *not*
/// taking most setups, so its signature is confluence: structure **and** flow
/// **and** breadth **and** authenticity **and** a meta with room left. The
/// authenticity floor is the highest in the table because size is what a
/// manufactured move is designed to attract.
pub const CONVICTION_SIZE_RULES: &[AffinityRule] = &[
    rule(F_TREND_STRUCTURE, FieldPreference::Equals(2), 5),
    rule(F_OFI, FieldPreference::AtLeast(4), 5),
    rule(F_BUYER_BREADTH, FieldPreference::AtLeast(3), 5),
    rule(F_AUTHENTICITY, FieldPreference::AtLeast(3), 6),
    rule(F_LIQUIDITY_DECADE, FieldPreference::AtLeast(3), 4),
    rule(F_HOLDER_GROWTH_ACCEL, FieldPreference::AtLeast(2), 4),
    rule(F_META_SATURATION, FieldPreference::AtMost(1), 4),
    rule(F_ATTENTION_VELOCITY, FieldPreference::AtLeast(2), 3),
    rule(F_REALIZED_VOL, FieldPreference::AtMost(3), 3),
];

const fn rule(field: usize, pref: FieldPreference, points: u32) -> AffinityRule {
    AffinityRule {
        field,
        pref,
        points,
    }
}

// ---------------------------------------------------------------------------
// Lenses
// ---------------------------------------------------------------------------

/// A named, measurable trading-style lens. See the module docs: these describe
/// *styles*, not people.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StyleLens {
    /// Meta emergence and attention velocity leading price; very early token age.
    EarlyRotation,
    /// Order flow, CVD and burst phase dominant; short holds, cost-sensitive.
    FlowScalper,
    /// The creation window; extreme earliness, thin corroboration.
    Sniper,
    /// Multi-factor confluence, higher authenticity floor, longer holds.
    ConvictionSize,
}

/// Every lens, in ordinal order. The canonical iteration order for anything that
/// scans all lenses.
pub const STYLE_LENSES: [StyleLens; LENS_COUNT] = [
    StyleLens::EarlyRotation,
    StyleLens::FlowScalper,
    StyleLens::Sniper,
    StyleLens::ConvictionSize,
];

impl StyleLens {
    /// Dense ordinal used for indexing, ordering, tie-breaks and any wire format.
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        match self {
            Self::EarlyRotation => 0,
            Self::FlowScalper => 1,
            Self::Sniper => 2,
            Self::ConvictionSize => 3,
        }
    }

    /// Inverse of [`StyleLens::ordinal`]; out-of-range input yields `None`.
    #[must_use]
    pub const fn from_ordinal(o: u8) -> Option<Self> {
        match o {
            0 => Some(Self::EarlyRotation),
            1 => Some(Self::FlowScalper),
            2 => Some(Self::Sniper),
            3 => Some(Self::ConvictionSize),
            _ => None,
        }
    }

    /// Stable machine name, used in diagnostics and operator listings.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::EarlyRotation => "early_rotation",
            Self::FlowScalper => "flow_scalper",
            Self::Sniper => "sniper",
            Self::ConvictionSize => "conviction_size",
        }
    }

    /// This lens's affinity table.
    #[must_use]
    pub const fn rules(self) -> &'static [AffinityRule] {
        match self {
            Self::EarlyRotation => EARLY_ROTATION_RULES,
            Self::FlowScalper => FLOW_SCALPER_RULES,
            Self::Sniper => SNIPER_RULES,
            Self::ConvictionSize => CONVICTION_SIZE_RULES,
        }
    }

    /// Total points available in this lens's table — the affinity denominator.
    #[must_use]
    pub fn points_possible(self) -> u32 {
        self.rules().iter().map(|r| r.points).sum()
    }

    /// The lens's stage-2 field weights.
    ///
    /// Note every lens keeps [`F_VENUE_PHASE`] at the maximum weight, matching
    /// [`crate::fingerprint::W_VENUE_PHASE`]: even though the recall filter already
    /// hard-partitions on phase (constitution 100), an accidentally unfiltered
    /// comparison must still never rank a cross-phase episode as near.
    #[must_use]
    pub fn weights(self) -> FeatureWeights {
        let mut w = [0u32; FIELD_COUNT];
        match self {
            Self::EarlyRotation => {
                w[F_OFI] = 3;
                w[F_CVD_DECADE] = 3;
                w[F_TREND_STRUCTURE] = 3;
                w[F_RANGE_STATE] = 2;
                w[F_BURST_PHASE] = 3;
                w[F_REALIZED_VOL] = 2;
                w[F_LIQUIDITY_DECADE] = 3;
                w[F_BUYER_BREADTH] = 4;
                w[F_TOKEN_AGE] = 8;
                w[F_VENUE_PHASE] = 10;
                w[F_ATTENTION_VELOCITY] = 10;
                w[F_NARRATIVE_CLASS] = 6;
                w[F_AUTHENTICITY] = 5;
                w[F_HOLDER_GROWTH_ACCEL] = 7;
                w[F_CREATOR_CLASS] = 2;
                w[F_META_CATEGORY] = 9;
                w[F_META_SATURATION] = 9;
                w[F_DESIGNATED_CALLER] = 2;
                w[F_ROUND_TRIP_COST] = 4;
                w[F_TIME_OF_DAY] = 1;
            }
            Self::FlowScalper => {
                w[F_OFI] = 10;
                w[F_CVD_DECADE] = 9;
                w[F_TREND_STRUCTURE] = 4;
                w[F_RANGE_STATE] = 5;
                w[F_BURST_PHASE] = 10;
                w[F_REALIZED_VOL] = 7;
                w[F_LIQUIDITY_DECADE] = 6;
                w[F_BUYER_BREADTH] = 6;
                w[F_TOKEN_AGE] = 2;
                w[F_VENUE_PHASE] = 10;
                w[F_ATTENTION_VELOCITY] = 2;
                w[F_NARRATIVE_CLASS] = 1;
                w[F_AUTHENTICITY] = 1;
                w[F_HOLDER_GROWTH_ACCEL] = 2;
                w[F_CREATOR_CLASS] = 1;
                w[F_META_CATEGORY] = 2;
                w[F_META_SATURATION] = 2;
                w[F_DESIGNATED_CALLER] = 1;
                w[F_ROUND_TRIP_COST] = 8;
                w[F_TIME_OF_DAY] = 1;
            }
            Self::Sniper => {
                w[F_OFI] = 3;
                w[F_CVD_DECADE] = 2;
                w[F_TREND_STRUCTURE] = 2;
                w[F_RANGE_STATE] = 2;
                w[F_BURST_PHASE] = 3;
                w[F_REALIZED_VOL] = 2;
                w[F_LIQUIDITY_DECADE] = 7;
                w[F_BUYER_BREADTH] = 6;
                w[F_TOKEN_AGE] = 12;
                w[F_VENUE_PHASE] = 12;
                w[F_ATTENTION_VELOCITY] = 3;
                w[F_NARRATIVE_CLASS] = 5;
                w[F_AUTHENTICITY] = 2;
                w[F_HOLDER_GROWTH_ACCEL] = 2;
                w[F_CREATOR_CLASS] = 9;
                w[F_META_CATEGORY] = 5;
                w[F_META_SATURATION] = 4;
                w[F_DESIGNATED_CALLER] = 3;
                w[F_ROUND_TRIP_COST] = 6;
                w[F_TIME_OF_DAY] = 1;
            }
            Self::ConvictionSize => {
                w[F_OFI] = 7;
                w[F_CVD_DECADE] = 5;
                w[F_TREND_STRUCTURE] = 8;
                w[F_RANGE_STATE] = 5;
                w[F_BURST_PHASE] = 4;
                w[F_REALIZED_VOL] = 5;
                w[F_LIQUIDITY_DECADE] = 7;
                w[F_BUYER_BREADTH] = 8;
                w[F_TOKEN_AGE] = 3;
                w[F_VENUE_PHASE] = 10;
                w[F_ATTENTION_VELOCITY] = 6;
                w[F_NARRATIVE_CLASS] = 5;
                w[F_AUTHENTICITY] = 9;
                w[F_HOLDER_GROWTH_ACCEL] = 7;
                w[F_CREATOR_CLASS] = 5;
                w[F_META_CATEGORY] = 6;
                w[F_META_SATURATION] = 7;
                w[F_DESIGNATED_CALLER] = 3;
                w[F_ROUND_TRIP_COST] = 5;
                w[F_TIME_OF_DAY] = 1;
            }
        }
        FeatureWeights { w }
    }

    /// [`RecallParams`] carrying this lens's weights — hand this to
    /// [`EpisodicIndex::recall_conditioned`] to ask "what happened last time, *read
    /// through this style*".
    #[must_use]
    pub fn recall_params(self) -> RecallParams {
        RecallParams {
            weights: self.weights(),
            ..RecallParams::default()
        }
    }

    /// The lens's recall-filter shape, on top of the mandatory venue-phase pin
    /// (constitution 100).
    ///
    /// * `EarlyRotation` pins the meta category: rotation is a question *about* a
    ///   meta, and pooling across metas would answer a different one.
    /// * `Sniper` pins [`DiscoveryLane::NewMint`]: the style only exists in the
    ///   creation window, and a token that reached us by any other lane is not a
    ///   snipe however young it is.
    /// * `FlowScalper` and `ConvictionSize` pin neither — flow structure and
    ///   confluence travel across metas and lanes, and pinning would starve the
    ///   sample for no gain in relevance.
    #[must_use]
    pub fn recall_filter(self, query: &SetupFingerprint, meta_category_id: u32) -> RecallFilter {
        let base = RecallFilter::for_query(query);
        match self {
            Self::EarlyRotation => base.with_meta_category(meta_category_id),
            Self::Sniper => base.with_discovery_lane(DiscoveryLane::NewMint),
            Self::FlowScalper | Self::ConvictionSize => base,
        }
    }

    /// The lens's canonical exemplar bucket vector: every ruled field set to its
    /// preference's representative bucket, every unruled field at zero.
    #[must_use]
    pub fn exemplar_buckets(self) -> [u8; FIELD_COUNT] {
        let mut buckets = [0u8; FIELD_COUNT];
        for r in self.rules() {
            if r.field < FIELD_COUNT {
                buckets[r.field] = r.pref.exemplar();
            }
        }
        buckets
    }

    /// The lens's exemplar fingerprint in a given phase — the "centre" of the style,
    /// used as the query anchor for [`archetype_performance`]'s nearest-neighbour
    /// diagnostics.
    #[must_use]
    pub fn exemplar(self, venue_phase: VenuePhase) -> SetupFingerprint {
        let mut buckets = self.exemplar_buckets();
        buckets[F_VENUE_PHASE] = venue_phase.ordinal();
        SetupFingerprint::from_buckets(buckets)
    }
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

/// How well a setup fits each style lens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchetypeAffinity {
    /// Affinity in basis points per lens, indexed by [`StyleLens::ordinal`].
    pub scores_bp: [u32; LENS_COUNT],
    /// Points met per lens, for audit.
    pub points_met: [u32; LENS_COUNT],
    /// Points available per lens, for audit.
    pub points_possible: [u32; LENS_COUNT],
    /// Whether each lens's [`ARCHETYPE_FIT_MIN_BP`] threshold was cleared.
    pub fits: [bool; LENS_COUNT],
    /// Number of lenses fitted.
    pub n_fits: u32,
    /// The highest-scoring lens; ties break to the lowest [`StyleLens::ordinal`].
    pub best: StyleLens,
    /// The best lens's affinity, basis points.
    pub best_score_bp: u32,
}

impl ArchetypeAffinity {
    /// This setup's affinity for one lens, basis points.
    #[must_use]
    pub fn score_bp(&self, lens: StyleLens) -> u32 {
        self.scores_bp[usize::from(lens.ordinal())]
    }

    /// Whether this setup fits one lens.
    #[must_use]
    pub fn fits(&self, lens: StyleLens) -> bool {
        self.fits[usize::from(lens.ordinal())]
    }

    /// Fitted lenses in ordinal order — the canonical deterministic listing.
    #[must_use]
    pub fn fitted_lenses(&self) -> Vec<StyleLens> {
        STYLE_LENSES
            .iter()
            .copied()
            .filter(|l| self.fits(*l))
            .collect()
    }
}

/// **Which style does this setup suit?**
///
/// Pure, total and deterministic: a fixed scan over four static rule tables, no
/// allocation on the scoring path, ties broken by lens ordinal.
#[must_use]
pub fn classify(query: &SetupFingerprint) -> ArchetypeAffinity {
    let buckets = query.buckets();
    let mut scores_bp = [0u32; LENS_COUNT];
    let mut points_met = [0u32; LENS_COUNT];
    let mut points_possible = [0u32; LENS_COUNT];
    let mut fits = [false; LENS_COUNT];
    let mut n_fits = 0u32;

    for lens in STYLE_LENSES {
        let i = usize::from(lens.ordinal());
        let mut met = 0u32;
        let mut possible = 0u32;
        for r in lens.rules() {
            possible += r.points;
            if r.field < FIELD_COUNT && r.pref.is_satisfied(buckets[r.field]) {
                met += r.points;
            }
        }
        let bp = if possible == 0 {
            0
        } else {
            ((u64::from(met) * u64::from(BPS_SCALE_U32)) / u64::from(possible)) as u32
        };
        points_met[i] = met;
        points_possible[i] = possible;
        scores_bp[i] = bp;
        if bp >= ARCHETYPE_FIT_MIN_BP {
            fits[i] = true;
            n_fits += 1;
        }
    }

    let mut best = StyleLens::EarlyRotation;
    let mut best_score_bp = scores_bp[0];
    for lens in STYLE_LENSES {
        let bp = scores_bp[usize::from(lens.ordinal())];
        // Strict `>` keeps the lowest ordinal on a tie — the deterministic rule.
        if bp > best_score_bp {
            best_score_bp = bp;
            best = lens;
        }
    }

    ArchetypeAffinity {
        scores_bp,
        points_met,
        points_possible,
        fits,
        n_fits,
        best,
        best_score_bp,
    }
}

// ---------------------------------------------------------------------------
// Per-lens realized performance
// ---------------------------------------------------------------------------

/// **Which style is actually paying for us right now?**
///
/// Realized-outcome statistics over every admitted episode in `venue_phase` whose
/// fingerprint *fits* `lens`, in exactly the [`RecallVerdict`] shape the rest of
/// the crate uses — so it is fail-closed by the same type, with the same
/// `stats() -> Option<&RecallStats>` accessor and the same structural inability to
/// hand out a number below `min_sample`.
///
/// The `nearest_*` diagnostics are measured against the lens's
/// [`StyleLens::exemplar`], since a cohort has no single query point. Note
/// [`RecallUnknown::NoCandidateInRadius`] is unreachable here: a lens is a *scope*
/// filter, not a radius, so "nothing fits the lens" surfaces as
/// [`RecallUnknown::NoEpisodeInScope`].
///
/// Cost is one linear pass over the index plus a bounded sort of the matched nets;
/// this is a reflection-path function, not the microsecond decision path.
#[must_use]
pub fn archetype_performance(
    index: &EpisodicIndex,
    lens: StyleLens,
    venue_phase: VenuePhase,
    min_sample: u32,
) -> RecallVerdict {
    if index.is_empty() {
        return RecallVerdict::Unknown(RecallUnknown::EmptyIndex);
    }
    let exemplar = lens.exemplar(venue_phase);
    let weights = lens.weights();

    let mut nets: Vec<i128> = Vec::new();
    let mut holds: Vec<u64> = Vec::new();
    let mut sum: i128 = 0;
    let mut win_count = 0u32;
    let mut loss_count = 0u32;
    let mut nearest_distance = u32::MAX;
    let mut nearest_weighted = u64::MAX;
    let mut nearest_episode_id = 0u64;

    for e in index.iter_oldest_first() {
        if e.context().venue_phase != venue_phase {
            continue;
        }
        if !e.outcome().was_admitted {
            continue;
        }
        if !classify(e.fingerprint()).fits(lens) {
            continue;
        }
        let d = unweighted_distance(&exemplar, e.fingerprint());
        let wd = weighted_distance(&exemplar, e.fingerprint(), &weights);
        // `(weighted, episode_id)` is a total order because ids are unique and
        // monotone, so the "nearest" episode never depends on ring position.
        if (wd, e.episode_id()) < (nearest_weighted, nearest_episode_id)
            || nearest_weighted == u64::MAX
        {
            nearest_weighted = wd;
            nearest_distance = d;
            nearest_episode_id = e.episode_id();
        }
        let net = e.outcome().realized_net_lamports;
        nets.push(net);
        holds.push(e.outcome().hold_duration_ns);
        sum = sum.saturating_add(net);
        if net > 0 {
            win_count += 1;
        } else if net < 0 {
            loss_count += 1;
        }
    }

    let n_matched = nets.len() as u32;
    if n_matched == 0 {
        return RecallVerdict::Unknown(RecallUnknown::NoEpisodeInScope);
    }
    if n_matched < min_sample {
        return RecallVerdict::Unknown(RecallUnknown::InsufficientSample {
            n_matched,
            min_sample,
        });
    }
    nets.sort_unstable();
    holds.sort_unstable();

    let decisive = win_count + loss_count;
    let win_rate_bp = if decisive == 0 {
        0
    } else {
        ((u64::from(win_count) * u64::from(BPS_SCALE_U32)) / u64::from(decisive)) as u32
    };

    RecallVerdict::Known(RecallStats {
        n_matched,
        median_net_lamports: order_stat_i128(&nets, P50),
        mean_net_lamports: sum / i128::from(n_matched),
        win_count,
        loss_count,
        win_rate_bp,
        p25_net_lamports: order_stat_i128(&nets, P25),
        p75_net_lamports: order_stat_i128(&nets, P75),
        median_hold_ns: order_stat_u64(&holds, P50),
        nearest_distance,
        nearest_weighted_distance: nearest_weighted,
        nearest_episode_id,
    })
}

/// Per-lens realized performance for every lens, in [`StyleLens::ordinal`] order.
///
/// The operator-facing scoreboard: four rows, each either a statistic or an
/// explicit refusal.
#[must_use]
pub fn lens_scoreboard(
    index: &EpisodicIndex,
    venue_phase: VenuePhase,
    min_sample: u32,
) -> Vec<(StyleLens, RecallVerdict)> {
    STYLE_LENSES
        .iter()
        .map(|l| {
            (
                *l,
                archetype_performance(index, *l, venue_phase, min_sample),
            )
        })
        .collect()
}

/// The lens paying us best right now, or `None` if no lens clears the sample floor.
///
/// Ranked by **median** realized net first — the robust statistic, so one outlier
/// cannot crown a style — then mean, then sample size, then ascending lens ordinal.
/// A lens whose median is non-positive is never returned: "least bad" is not
/// "paying", and reflection re-weighting toward a losing style is worse than
/// re-weighting toward nothing.
#[must_use]
pub fn best_paying_lens(
    index: &EpisodicIndex,
    venue_phase: VenuePhase,
    min_sample: u32,
) -> Option<(StyleLens, RecallStats)> {
    let mut best: Option<(StyleLens, RecallStats)> = None;
    for lens in STYLE_LENSES {
        let Some(stats) = archetype_performance(index, lens, venue_phase, min_sample)
            .stats()
            .copied()
        else {
            continue;
        };
        if stats.median_net_lamports <= 0 {
            continue;
        }
        let better = match &best {
            None => true,
            Some((_, b)) => {
                (
                    stats.median_net_lamports,
                    stats.mean_net_lamports,
                    stats.n_matched,
                ) > (b.median_net_lamports, b.mean_net_lamports, b.n_matched)
            }
        };
        if better {
            best = Some((lens, stats));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::episode::{Episode, EpisodeContext, EpisodeOutcome, ExitReason};
    use crate::fingerprint::{
        BurstPhase, CreatorClass, MetaSaturationState, NarrativeClass, RangeState, SetupInputs,
        TrendStructure,
    };

    const SEC: u64 = 1_000_000_000;

    fn fp(inputs: &SetupInputs) -> SetupFingerprint {
        SetupFingerprint::from_inputs(inputs)
    }

    /// A textbook early-rotation setup.
    fn early_rotation_setup() -> SetupInputs {
        SetupInputs {
            token_age_ns: 120 * SEC,
            attention_velocity_bps: 9_000,
            meta_saturation_state: MetaSaturationState::Emerging,
            holder_growth_accel_bps: 3_000,
            buyer_breadth: 10,
            authenticity_bps: 8_000,
            narrative_class: NarrativeClass::Animal,
            meta_category_id: 42,
            ..SetupInputs::default()
        }
    }

    /// A textbook flow-scalping setup.
    fn flow_scalper_setup() -> SetupInputs {
        SetupInputs {
            burst_phase: BurstPhase::Climax,
            ofi_bps: 3_000,
            cvd_decade: 10,
            realized_vol_bps: 800,
            buyer_breadth: 30,
            round_trip_cost_bps: 80,
            liquidity_decade: 12,
            token_age_ns: 3_600 * SEC,
            ..SetupInputs::default()
        }
    }

    /// A textbook snipe.
    fn sniper_setup() -> SetupInputs {
        SetupInputs {
            token_age_ns: 20 * SEC,
            venue_phase: VenuePhase::Curve,
            buyer_breadth: 2,
            liquidity_decade: 9,
            meta_saturation_state: MetaSaturationState::Emerging,
            round_trip_cost_bps: 300,
            creator_class: CreatorClass::Proven,
            ..SetupInputs::default()
        }
    }

    /// A textbook conviction-size setup.
    fn conviction_setup() -> SetupInputs {
        SetupInputs {
            trend_structure: TrendStructure::Up,
            ofi_bps: 800,
            buyer_breadth: 60,
            authenticity_bps: 9_500,
            liquidity_decade: 12,
            holder_growth_accel_bps: 700,
            meta_saturation_state: MetaSaturationState::Hot,
            attention_velocity_bps: 1_000,
            realized_vol_bps: 300,
            range_state: RangeState::Normal,
            ..SetupInputs::default()
        }
    }

    fn ep(id: u64, f: SetupFingerprint, net: i128, hold: u64, phase: VenuePhase) -> Episode {
        Episode::new(
            id,
            f,
            EpisodeContext {
                mint_id: id,
                venue_phase: phase,
                meta_category_id: 42,
                discovery_lane: DiscoveryLane::NewMint,
                info_time_ns: id * 1_000_000,
                slot: id,
            },
            EpisodeOutcome {
                realized_net_lamports: net,
                hold_duration_ns: hold,
                exit_reason: if net >= 0 {
                    ExitReason::TakeProfit
                } else {
                    ExitReason::StopLoss
                },
                mfe_bps: 100,
                mae_bps: -50,
                was_admitted: true,
            },
        )
    }

    // ------------------------------------------------------------- lens table

    #[test]
    fn lens_ordinals_round_trip_and_cover_the_table() {
        for (i, lens) in STYLE_LENSES.iter().enumerate() {
            assert_eq!(usize::from(lens.ordinal()), i);
            assert_eq!(StyleLens::from_ordinal(lens.ordinal()), Some(*lens));
            assert!(!lens.name().is_empty());
        }
        assert!(StyleLens::from_ordinal(LENS_COUNT as u8).is_none());
    }

    #[test]
    fn every_lens_rule_targets_a_real_field_with_reachable_buckets() {
        use crate::fingerprint::FIELD_SPECS;
        for lens in STYLE_LENSES {
            assert!(!lens.rules().is_empty(), "{} has no rules", lens.name());
            assert!(lens.points_possible() > 0);
            for r in lens.rules() {
                assert!(
                    r.field < FIELD_COUNT,
                    "{} targets a bogus field",
                    lens.name()
                );
                assert!(r.points > 0);
                let max = FIELD_SPECS[r.field].levels - 1;
                let probe = match r.pref {
                    FieldPreference::AtMost(x)
                    | FieldPreference::AtLeast(x)
                    | FieldPreference::Equals(x) => x,
                    FieldPreference::Between(lo, hi) => {
                        assert!(lo <= hi);
                        hi
                    }
                };
                assert!(
                    probe <= max,
                    "{} rule on field {} references bucket {} above the ladder max {}",
                    lens.name(),
                    r.field,
                    probe,
                    max
                );
            }
        }
    }

    #[test]
    fn every_lens_weights_venue_phase_at_the_maximum() {
        for lens in STYLE_LENSES {
            let w = lens.weights();
            let max = w.w.iter().copied().max().expect("non-empty");
            assert_eq!(
                w.w[F_VENUE_PHASE],
                max,
                "{} must never rank a cross-phase episode as near",
                lens.name()
            );
            assert!(w.total() > 0);
            assert!(
                w.w.iter().all(|x| *x > 0),
                "{} left a field at zero weight",
                lens.name()
            );
        }
    }

    #[test]
    fn lens_weight_profiles_are_distinct_and_emphasise_their_own_axis() {
        let er = StyleLens::EarlyRotation.weights();
        let fs = StyleLens::FlowScalper.weights();
        let sn = StyleLens::Sniper.weights();
        let cs = StyleLens::ConvictionSize.weights();
        assert_ne!(er, fs);
        assert_ne!(fs, sn);
        assert_ne!(sn, cs);
        assert!(er.w[F_ATTENTION_VELOCITY] > fs.w[F_ATTENTION_VELOCITY]);
        assert!(fs.w[F_OFI] > er.w[F_OFI]);
        assert!(fs.w[F_ROUND_TRIP_COST] > er.w[F_ROUND_TRIP_COST]);
        assert!(sn.w[F_TOKEN_AGE] > fs.w[F_TOKEN_AGE]);
        assert!(sn.w[F_CREATOR_CLASS] > cs.w[F_CREATOR_CLASS]);
        assert!(cs.w[F_AUTHENTICITY] > fs.w[F_AUTHENTICITY]);
    }

    #[test]
    fn recall_filter_shapes_match_the_documented_lens_semantics() {
        let q = fp(&early_rotation_setup());
        let er = StyleLens::EarlyRotation.recall_filter(&q, 42);
        assert_eq!(er.meta_category_id(), Some(42));
        assert_eq!(er.discovery_lane(), None);

        let sn = StyleLens::Sniper.recall_filter(&q, 42);
        assert_eq!(sn.discovery_lane(), Some(DiscoveryLane::NewMint));
        assert_eq!(sn.meta_category_id(), None);

        for lens in [StyleLens::FlowScalper, StyleLens::ConvictionSize] {
            let f = lens.recall_filter(&q, 42);
            assert_eq!(f.meta_category_id(), None);
            assert_eq!(f.discovery_lane(), None);
        }
        // The mandatory phase pin survives every shape (constitution 100).
        for lens in STYLE_LENSES {
            assert_eq!(lens.recall_filter(&q, 42).venue_phase(), q.venue_phase());
        }
    }

    #[test]
    fn recall_params_carry_the_lens_weights() {
        for lens in STYLE_LENSES {
            assert_eq!(lens.recall_params().weights, lens.weights());
        }
    }

    // ---------------------------------------------------------- classification

    #[test]
    fn each_textbook_setup_classifies_to_its_own_lens() {
        assert_eq!(
            classify(&fp(&early_rotation_setup())).best,
            StyleLens::EarlyRotation
        );
        assert_eq!(
            classify(&fp(&flow_scalper_setup())).best,
            StyleLens::FlowScalper
        );
        assert_eq!(classify(&fp(&sniper_setup())).best, StyleLens::Sniper);
        assert_eq!(
            classify(&fp(&conviction_setup())).best,
            StyleLens::ConvictionSize
        );
    }

    #[test]
    fn a_textbook_setup_saturates_its_own_lens() {
        let cases = [
            (early_rotation_setup(), StyleLens::EarlyRotation),
            (flow_scalper_setup(), StyleLens::FlowScalper),
            (sniper_setup(), StyleLens::Sniper),
            (conviction_setup(), StyleLens::ConvictionSize),
        ];
        for (inputs, lens) in cases {
            let a = classify(&fp(&inputs));
            assert_eq!(
                a.score_bp(lens),
                BPS_SCALE_U32,
                "{} did not saturate on its own textbook setup",
                lens.name()
            );
            assert!(a.fits(lens));
            assert_eq!(
                a.points_met[usize::from(lens.ordinal())],
                a.points_possible[usize::from(lens.ordinal())]
            );
        }
    }

    #[test]
    fn classification_is_deterministic() {
        let q = fp(&conviction_setup());
        let first = classify(&q);
        for _ in 0..64 {
            assert_eq!(classify(&q), first);
        }
    }

    #[test]
    fn classification_ties_break_to_the_lowest_ordinal() {
        // The all-zero default setup scores the same on nothing in particular;
        // whatever the scores are, `best` must be reproducible and must be the
        // lowest-ordinal lens among the maxima.
        let q = fp(&SetupInputs::default());
        let a = classify(&q);
        let max = a.scores_bp.iter().copied().max().expect("non-empty");
        let first_max = a
            .scores_bp
            .iter()
            .position(|s| *s == max)
            .expect("a maximum exists");
        assert_eq!(usize::from(a.best.ordinal()), first_max);
        assert_eq!(a.best_score_bp, max);
    }

    #[test]
    fn conviction_size_is_the_hardest_lens_to_fit() {
        assert!(
            StyleLens::ConvictionSize.rules().len() > StyleLens::Sniper.rules().len(),
            "the selective style must have the most conditions"
        );
        // A thin, inorganic, structureless burst is a perfect scalp and a terrible
        // thing to size into: it fits the scalper outright and misses conviction on
        // breadth, authenticity, structure and attention.
        let churn = SetupInputs {
            buyer_breadth: 10,
            authenticity_bps: 0,
            trend_structure: TrendStructure::Range,
            attention_velocity_bps: -100,
            ..flow_scalper_setup()
        };
        let a = classify(&fp(&churn));
        assert!(a.fits(StyleLens::FlowScalper));
        assert!(
            !a.fits(StyleLens::ConvictionSize),
            "conviction size must not fit a setup with no confluence: {} bp",
            a.score_bp(StyleLens::ConvictionSize)
        );
    }

    #[test]
    fn a_migrated_pool_setup_cannot_fit_the_sniper_lens_on_age_alone() {
        let inputs = SetupInputs {
            venue_phase: VenuePhase::Pool,
            ..sniper_setup()
        };
        let a = classify(&fp(&inputs));
        assert!(
            a.score_bp(StyleLens::Sniper) < BPS_SCALE_U32,
            "the curve rule must actually cost points in the pool"
        );
    }

    #[test]
    fn fitted_lenses_are_listed_in_ordinal_order() {
        let a = classify(&fp(&early_rotation_setup()));
        let fitted = a.fitted_lenses();
        assert_eq!(fitted.len(), a.n_fits as usize);
        for w in fitted.windows(2) {
            assert!(w[0].ordinal() < w[1].ordinal());
        }
    }

    #[test]
    fn field_preferences_are_exact_at_their_boundaries() {
        assert!(FieldPreference::AtMost(2).is_satisfied(2));
        assert!(!FieldPreference::AtMost(2).is_satisfied(3));
        assert!(FieldPreference::AtLeast(2).is_satisfied(2));
        assert!(!FieldPreference::AtLeast(2).is_satisfied(1));
        assert!(FieldPreference::Equals(0).is_satisfied(0));
        assert!(!FieldPreference::Equals(0).is_satisfied(1));
        assert!(FieldPreference::Between(1, 2).is_satisfied(1));
        assert!(FieldPreference::Between(1, 2).is_satisfied(2));
        assert!(!FieldPreference::Between(1, 2).is_satisfied(3));
        assert_eq!(FieldPreference::Between(1, 3).exemplar(), 2);
        assert_eq!(FieldPreference::AtMost(4).exemplar(), 4);
    }

    #[test]
    fn a_lens_exemplar_fits_its_own_lens_in_both_phases() {
        for lens in STYLE_LENSES {
            for phase in [VenuePhase::Curve, VenuePhase::Pool] {
                let e = lens.exemplar(phase);
                assert_eq!(e.venue_phase(), phase);
                if lens == StyleLens::Sniper && phase == VenuePhase::Pool {
                    // The Sniper lens *requires* the curve; its pool exemplar is
                    // deliberately a non-fit, which is the correct answer.
                    continue;
                }
                assert!(
                    classify(&e).fits(lens),
                    "{} exemplar does not fit its own lens in {phase:?}",
                    lens.name()
                );
            }
        }
    }

    // ------------------------------------------------------------ performance

    /// `n` admitted episodes of the given style, paying `net` each.
    fn index_of(style: &SetupInputs, n: u64, net: i128, phase: VenuePhase) -> EpisodicIndex {
        let mut idx = EpisodicIndex::with_capacity(512);
        let inputs = SetupInputs {
            venue_phase: phase,
            ..*style
        };
        for i in 1..=n {
            idx.push(ep(i, fp(&inputs), net, i * SEC, phase))
                .expect("monotone");
        }
        idx
    }

    #[test]
    fn archetype_performance_is_empty_index_unknown() {
        let idx = EpisodicIndex::with_capacity(8);
        let v = archetype_performance(
            &idx,
            StyleLens::FlowScalper,
            VenuePhase::Curve,
            ARCHETYPE_MIN_SAMPLE,
        );
        assert_eq!(v, RecallVerdict::Unknown(RecallUnknown::EmptyIndex));
        assert!(v.stats().is_none());
    }

    #[test]
    fn archetype_performance_respects_the_small_sample_floor() {
        let idx = index_of(&flow_scalper_setup(), 5, 1_000_000, VenuePhase::Curve);
        let v = archetype_performance(&idx, StyleLens::FlowScalper, VenuePhase::Curve, 8);
        assert_eq!(
            v,
            RecallVerdict::Unknown(RecallUnknown::InsufficientSample {
                n_matched: 5,
                min_sample: 8
            })
        );
        assert!(
            v.stats().is_none(),
            "no per-lens number may be readable below the floor"
        );
    }

    #[test]
    fn archetype_performance_appears_at_the_sample_floor() {
        let idx = index_of(&flow_scalper_setup(), 8, 3_000_000, VenuePhase::Curve);
        let s = *archetype_performance(&idx, StyleLens::FlowScalper, VenuePhase::Curve, 8)
            .stats()
            .expect("the eighth episode reaches the floor");
        assert_eq!(s.n_matched, 8);
        assert_eq!(s.median_net_lamports, 3_000_000);
        assert_eq!(s.mean_net_lamports, 3_000_000);
        assert_eq!(s.win_rate_bp, BPS_SCALE_U32);
        assert!(s.nearest_episode_id > 0);
    }

    #[test]
    fn archetype_performance_never_pools_across_venue_phases() {
        let mut idx = EpisodicIndex::with_capacity(512);
        let curve = SetupInputs {
            venue_phase: VenuePhase::Curve,
            ..flow_scalper_setup()
        };
        let pool = SetupInputs {
            venue_phase: VenuePhase::Pool,
            ..flow_scalper_setup()
        };
        let mut id = 1u64;
        for _ in 0..10 {
            idx.push(ep(id, fp(&curve), 5_000_000, SEC, VenuePhase::Curve))
                .expect("monotone");
            id += 1;
            idx.push(ep(id, fp(&pool), -5_000_000, SEC, VenuePhase::Pool))
                .expect("monotone");
            id += 1;
        }
        let c = *archetype_performance(&idx, StyleLens::FlowScalper, VenuePhase::Curve, 8)
            .stats()
            .expect("curve cohort");
        let p = *archetype_performance(&idx, StyleLens::FlowScalper, VenuePhase::Pool, 8)
            .stats()
            .expect("pool cohort");
        assert_eq!(c.n_matched, 10);
        assert_eq!(p.n_matched, 10);
        assert!(c.median_net_lamports > 0);
        assert!(p.median_net_lamports < 0);
        assert_eq!(
            c.median_net_lamports + p.median_net_lamports,
            0,
            "the two cohorts must be disjoint, not averaged (constitution 100)"
        );
    }

    #[test]
    fn archetype_performance_ignores_unadmitted_episodes() {
        let mut idx = EpisodicIndex::with_capacity(512);
        let inputs = flow_scalper_setup();
        for i in 1..=20u64 {
            let e = Episode::new(
                i,
                fp(&inputs),
                EpisodeContext {
                    mint_id: i,
                    venue_phase: VenuePhase::Curve,
                    meta_category_id: 42,
                    discovery_lane: DiscoveryLane::NewMint,
                    info_time_ns: i,
                    slot: i,
                },
                EpisodeOutcome::rejected(),
            );
            idx.push(e).expect("monotone");
        }
        let v = archetype_performance(&idx, StyleLens::FlowScalper, VenuePhase::Curve, 8);
        assert_eq!(v, RecallVerdict::Unknown(RecallUnknown::NoEpisodeInScope));
        assert!(v.stats().is_none());
    }

    #[test]
    fn a_lens_with_no_matching_episode_reports_no_episode_in_scope() {
        let idx = index_of(&flow_scalper_setup(), 30, 1_000_000, VenuePhase::Curve);
        let v = archetype_performance(&idx, StyleLens::Sniper, VenuePhase::Curve, 8);
        assert_eq!(v, RecallVerdict::Unknown(RecallUnknown::NoEpisodeInScope));
        assert!(v.stats().is_none());
    }

    #[test]
    fn archetype_performance_is_deterministic() {
        let idx = index_of(&conviction_setup(), 25, 7_000_000, VenuePhase::Pool);
        let first = archetype_performance(&idx, StyleLens::ConvictionSize, VenuePhase::Pool, 8);
        for _ in 0..32 {
            assert_eq!(
                archetype_performance(&idx, StyleLens::ConvictionSize, VenuePhase::Pool, 8),
                first
            );
        }
    }

    #[test]
    fn archetype_performance_computes_a_hand_checked_distribution() {
        let mut idx = EpisodicIndex::with_capacity(512);
        let inputs = SetupInputs {
            venue_phase: VenuePhase::Curve,
            ..flow_scalper_setup()
        };
        // Nets -3..4 million; one exact flat excluded from the win-rate denominator.
        let nets: [i128; 8] = [-3, -2, -1, 0, 1, 2, 3, 4];
        for (i, n) in nets.iter().enumerate() {
            idx.push(ep(
                i as u64 + 1,
                fp(&inputs),
                n * 1_000_000,
                (i as u64 + 1) * SEC,
                VenuePhase::Curve,
            ))
            .expect("monotone");
        }
        let s = *archetype_performance(&idx, StyleLens::FlowScalper, VenuePhase::Curve, 8)
            .stats()
            .expect("known");
        assert_eq!(s.n_matched, 8);
        assert_eq!(s.win_count, 4);
        assert_eq!(s.loss_count, 3);
        assert_eq!(s.win_rate_bp, (4 * 10_000) / 7);
        assert_eq!(s.median_net_lamports, 0);
        assert_eq!(s.mean_net_lamports, 500_000);
        assert_eq!(s.p25_net_lamports, -2_000_000);
        assert_eq!(s.p75_net_lamports, 2_000_000);
        assert_eq!(s.median_hold_ns, 4 * SEC);
    }

    // ------------------------------------------------------------- scoreboard

    #[test]
    fn the_scoreboard_lists_every_lens_in_ordinal_order() {
        let idx = index_of(&flow_scalper_setup(), 12, 4_000_000, VenuePhase::Curve);
        let board = lens_scoreboard(&idx, VenuePhase::Curve, 8);
        assert_eq!(board.len(), LENS_COUNT);
        for (i, (lens, _)) in board.iter().enumerate() {
            assert_eq!(usize::from(lens.ordinal()), i);
        }
        let scalper = board
            .iter()
            .find(|(l, _)| *l == StyleLens::FlowScalper)
            .expect("present");
        assert!(scalper.1.is_known());
        let sniper = board
            .iter()
            .find(|(l, _)| *l == StyleLens::Sniper)
            .expect("present");
        assert!(
            !sniper.1.is_known(),
            "a lens with no cohort must refuse, not report zero"
        );
    }

    #[test]
    fn best_paying_lens_picks_the_style_that_actually_pays_us() {
        let mut idx = EpisodicIndex::with_capacity(1_024);
        let scalp = SetupInputs {
            venue_phase: VenuePhase::Curve,
            ..flow_scalper_setup()
        };
        let rotate = SetupInputs {
            venue_phase: VenuePhase::Curve,
            ..early_rotation_setup()
        };
        let mut id = 1u64;
        for _ in 0..12 {
            idx.push(ep(id, fp(&scalp), -2_000_000, SEC, VenuePhase::Curve))
                .expect("monotone");
            id += 1;
            idx.push(ep(id, fp(&rotate), 6_000_000, SEC, VenuePhase::Curve))
                .expect("monotone");
            id += 1;
        }
        let (lens, stats) =
            best_paying_lens(&idx, VenuePhase::Curve, 8).expect("one lens is paying");
        assert_eq!(lens, StyleLens::EarlyRotation);
        assert_eq!(stats.median_net_lamports, 6_000_000);
    }

    #[test]
    fn best_paying_lens_refuses_to_crown_a_losing_style() {
        let idx = index_of(&flow_scalper_setup(), 20, -3_000_000, VenuePhase::Curve);
        assert!(
            best_paying_lens(&idx, VenuePhase::Curve, 8).is_none(),
            "least-bad is not paying"
        );
    }

    #[test]
    fn best_paying_lens_respects_the_sample_floor() {
        let idx = index_of(&flow_scalper_setup(), 6, 9_000_000, VenuePhase::Curve);
        assert!(best_paying_lens(&idx, VenuePhase::Curve, 8).is_none());
        assert!(best_paying_lens(&idx, VenuePhase::Curve, 6).is_some());
    }

    #[test]
    fn per_lens_stats_survive_ring_eviction_without_growing() {
        let mut idx = EpisodicIndex::with_capacity(16);
        let inputs = SetupInputs {
            venue_phase: VenuePhase::Curve,
            ..flow_scalper_setup()
        };
        for i in 1..=500u64 {
            idx.push(ep(i, fp(&inputs), 1_000_000, SEC, VenuePhase::Curve))
                .expect("monotone");
            assert!(idx.len() <= 16);
        }
        let s = *archetype_performance(&idx, StyleLens::FlowScalper, VenuePhase::Curve, 8)
            .stats()
            .expect("known");
        assert_eq!(s.n_matched, 16, "the cohort is bounded by the ring");
        assert!(idx.evicted_count() > 0);
    }
}
