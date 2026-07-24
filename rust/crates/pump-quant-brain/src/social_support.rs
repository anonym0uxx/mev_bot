//! `SocialSupportScore` — "does this coin actually have strong social support?"
//! (constitution 21.4 attention, 22 integer-only, 46 small-n, 57/99 bounded,
//! 102 named thresholds).
//!
//! # What "support" is, and what it is not
//!
//! The naive version of this question is "how many posts are there?", and it is
//! worse than useless — post count is the single cheapest quantity in the market to
//! manufacture. This module answers a harder, adversarially-aware version built
//! from five components, all computed from **our own** [`crate::social_recall`]
//! history and our own realized outcomes:
//!
//! **1. Distinct-originator breadth.** Entity-deduped: an author who posts eleven
//! times counts once, and a call relayed on [`Platform::Aggregator`] — documented
//! in [`crate::social_recall`] as *an amplifier, not an originator* — counts zero.
//! Echoes are not support; they are the same information arriving again.
//!
//! **2. Trust-weighted breadth.** Each originator is weighted by their **earned**
//! track record from [`crate::trust`] — realized net SOL, decayed, partially
//! pooled. An unproven account contributes
//! [`UNPROVEN_ORIGINATOR_WEIGHT_UNITS`] `= 1` out of a possible
//! [`ORIGINATOR_WEIGHT_UNIT`] `= 64`, so it takes sixty-four anonymous accounts to
//! equal one proven caller. A [`crate::trust::TrustTier::Demoted`] source
//! contributes exactly zero: a caller who has cost us money talking about a coin is
//! not evidence *for* the coin.
//!
//! **3. Cross-platform spread.** Support confined to one venue is a coordination
//! smell — one Telegram room can be bought outright. Corroboration that shows up
//! independently on X, a stream and a Discord is much harder to stage, so distinct
//! platform count modulates a documented fraction ([`PLATFORM_WEIGHT_BP`]) of the
//! score.
//!
//! **4. Velocity, not level.** A level without a derivative is a lagging
//! indicator: by the time the *count* of people talking about a coin is high, the
//! move is priced. The window is split into [`SUPPORT_SUBWINDOWS`] equal
//! sub-windows and the score is modulated by the change in trust-weighted breadth
//! from the first to the last, classified into [`SupportTrend`].
//!
//! **5. Echo / coordination penalty.** Three observable coordination signatures are
//! charged against the score: aggregator echo share, repeat-post spam, and
//! **temporal burst concentration** — the share of originators whose first call
//! landed inside a single [`ECHO_BURST_WINDOW_NS`] cluster. Forty "independent"
//! accounts that all discovered a coin within the same thirty seconds are one
//! account with a budget.
//!
//! ## What this module refuses to invent
//!
//! The brief asks for near-identical *content* detection. This crate is
//! integer-only and holds no post text — [`crate::social_recall::CallRecord`] has
//! no content field, by design. Rather than fabricate a content-similarity metric
//! out of fields that do not measure content, near-duplicate detection is driven by
//! an **optional caller-supplied** [`ContentEchoWitness`] side table. When it is
//! absent the verdict says so ([`SocialSupportScore::content_evidence`] is `false`)
//! and [`SocialSupport::support_inputs_needed`] emits
//! [`SupportInputNeed::ContentDigests`] so the Phase-B capture layer knows exactly
//! what to go and fetch. The brain states its information needs; it never
//! manufactures them.
//!
//! When digests *are* supplied, originators sharing a digest collapse into one
//! **content cluster** whose weight is that of its single best member — not the sum.
//! Twelve accounts posting the same sentence are one originator, and the honest
//! breadth count is the cluster count, which is what the small-n gate is applied to.
//!
//! # Fail-closed (constitution 46)
//!
//! [`SocialSupportVerdict`] mirrors [`crate::recall::RecallVerdict`]:
//! `Known(SocialSupportScore)` or `Unknown(SupportUnknown)`, and
//! [`SupportUnknown`] carries **counts and floors only** — no score field, no
//! partial estimate, no accessor that yields a number. Three gates produce
//! `Unknown`: nothing in the window, fewer than
//! [`SupportParams::min_originators`] effective originators, and every originator
//! carrying zero trust weight.
//!
//! # Determinism and bounds
//!
//! No wall clock (`as_of_ns` is caller-supplied information time), no RNG, no
//! floats. Originators are processed in ascending author-id order and every
//! returned `Vec` is in a documented total order. The originator table is capped at
//! [`SUPPORT_ORIGINATOR_CAP`]; overflow can only *lower* measured breadth, never
//! raise it, so the bound is conservative in the safe direction.

use crate::recall::BPS_SCALE_U32;
use crate::social_recall::{CallRecord, Platform, SocialRecallIndex};
use crate::trust::{SocialTrust, SourceExposure, TrustSnapshot, TrustTier, TrustVerdict};

// ---------------------------------------------------------------------------
// Named constants (constitution 102)
// ---------------------------------------------------------------------------

/// Default support window: 24 hours of information time (constitution 102).
/// A memecoin's social life is measured in hours, not weeks.
pub const SUPPORT_WINDOW_NS: u64 = 86_400 * 1_000_000_000;

/// Number of equal sub-windows the support window is split into for the velocity
/// derivative (constitution 102).
pub const SUPPORT_SUBWINDOWS: usize = 3;

/// Minimum **effective** distinct originators before support is scored at all
/// (constitution 46 small-n guard). Two accounts is a conversation, not support.
pub const SUPPORT_MIN_ORIGINATORS: u32 = 3;

/// Weight of one fully-trusted originator (constitution 102).
pub const ORIGINATOR_WEIGHT_UNIT: u64 = 64;

/// Weight of an originator whose track record is `Unknown` (constitution 46/102).
/// Deliberately near-nothing: an unproven account is a rumour with a handle.
pub const UNPROVEN_ORIGINATOR_WEIGHT_UNITS: u64 = 1;

/// Weight of a [`TrustTier::Watch`] originator (constitution 102).
pub const WATCH_ORIGINATOR_WEIGHT_UNITS: u64 = 8;

/// Floor weight of a [`TrustTier::Trusted`] originator (constitution 102); the
/// weight then scales linearly with trust score up to [`ORIGINATOR_WEIGHT_UNIT`].
pub const TRUSTED_ORIGINATOR_WEIGHT_FLOOR_UNITS: u64 = 16;

/// Trust-weighted breadth at which the base score saturates (constitution 102):
/// four fully-trusted originators.
pub const SUPPORT_SATURATION_UNITS: u64 = 4 * ORIGINATOR_WEIGHT_UNIT;

/// Basis points of score the cross-platform spread factor controls
/// (constitution 102). The remaining `10_000 - PLATFORM_WEIGHT_BP` is unconditional.
pub const PLATFORM_WEIGHT_BP: u32 = 4_000;

/// Basis points of platform-spread credit granted per distinct platform
/// (constitution 102). Four platforms saturate the spread term.
pub const PLATFORM_SPREAD_STEP_BP: u32 = 2_500;

/// Basis points of score the velocity term controls (constitution 102).
pub const VELOCITY_WEIGHT_BP: i64 = 3_000;

/// Maximum absolute basis-point swing the velocity term may apply
/// (constitution 102). Velocity modulates; it never dominates.
pub const VELOCITY_MAX_EFFECT_BP: i64 = 3_000;

/// Clamp on the raw window-over-window velocity reading (constitution 102).
pub const SUPPORT_VELOCITY_CLAMP_BP: i64 = 30_000;

/// Denominator floor for the velocity ratio (constitution 22): one `Watch`-tier
/// originator's worth of weight, so "from nothing to something" cannot divide by
/// almost-zero.
pub const SUPPORT_VELOCITY_FLOOR_UNITS: u64 = WATCH_ORIGINATOR_WEIGHT_UNITS;

/// Velocity at or above which support is [`SupportTrend::Accelerating`]
/// (constitution 102).
pub const SUPPORT_ACCELERATING_MIN_BP: i64 = 2_000;

/// Velocity at or below which support is [`SupportTrend::Decaying`]
/// (constitution 102).
pub const SUPPORT_DECAYING_MAX_BP: i64 = -2_000;

/// Width of the temporal cluster used for burst detection (constitution 102):
/// sixty seconds of information time.
pub const ECHO_BURST_WINDOW_NS: u64 = 60 * 1_000_000_000;

/// Burst concentration below which no burst penalty is charged
/// (constitution 102). Organic discovery does cluster somewhat.
pub const BURST_TOLERANCE_BP: u32 = 5_000;

/// Maximum penalty contribution from temporal burst concentration
/// (constitution 102).
pub const BURST_PENALTY_WEIGHT_BP: u32 = 4_000;

/// Maximum penalty contribution from near-duplicate content clustering
/// (constitution 102). The heaviest of the three: identical text from "independent"
/// accounts is the least ambiguous coordination signature there is.
pub const ECHO_DUP_PENALTY_WEIGHT_BP: u32 = 6_000;

/// Maximum penalty contribution from aggregator/repeat echo share
/// (constitution 102).
pub const ECHO_RELAY_PENALTY_WEIGHT_BP: u32 = 2_500;

/// Ceiling on the total coordination penalty (constitution 102). Even a maximally
/// coordinated burst leaves a residue, because the score is evidence, not a verdict.
pub const MAX_COORDINATION_PENALTY_BP: u32 = 8_000;

/// Capacity of the per-evaluation originator table (constitution 57/99). Overflow
/// only reduces measured breadth.
pub const SUPPORT_ORIGINATOR_CAP: usize = 256;

/// Capacity of the content-witness side table accepted per evaluation
/// (constitution 57/99).
pub const SUPPORT_WITNESS_CAP: usize = 4_096;

/// Cap on the number of information needs returned (constitution 57 bounded output).
pub const SUPPORT_NEEDS_CAP: usize = 16;

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

/// An externally-supplied near-duplicate content marker for one call.
///
/// The digest is an opaque integer produced *outside* this crate (a normalized-text
/// hash, a perceptual image hash, whatever the capture layer can honestly compute).
/// Equal digests mean "these two posts say the same thing"; this module never
/// interprets the value beyond equality, and it never invents one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentEchoWitness {
    /// The call this digest describes.
    pub call_id: u64,
    /// Opaque near-duplicate class identifier. Equality is the only operation.
    pub content_digest: u64,
}

/// Tunables for support scoring. Defaults are named consts (constitution 102).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupportParams {
    /// Lookback window in nanoseconds of information time.
    pub window_ns: u64,
    /// Minimum effective originators before a score is produced (constitution 46).
    pub min_originators: u32,
    /// Trust-weighted breadth at which the base score saturates.
    pub saturation_units: u64,
    /// Hard cap on the originator table.
    pub originator_cap: usize,
}

impl Default for SupportParams {
    fn default() -> Self {
        Self {
            window_ns: SUPPORT_WINDOW_NS,
            min_originators: SUPPORT_MIN_ORIGINATORS,
            saturation_units: SUPPORT_SATURATION_UNITS,
            originator_cap: SUPPORT_ORIGINATOR_CAP,
        }
    }
}

// ---------------------------------------------------------------------------
// Verdict
// ---------------------------------------------------------------------------

/// The sign of the support derivative (constitution 102 named thresholds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SupportTrend {
    /// Trust-weighted breadth is falling across the window.
    Decaying,
    /// Neither materially rising nor falling.
    Flat,
    /// Trust-weighted breadth is rising across the window.
    Accelerating,
}

impl SupportTrend {
    /// Dense ordinal used for ordering and any wire format.
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        match self {
            Self::Decaying => 0,
            Self::Flat => 1,
            Self::Accelerating => 2,
        }
    }

    /// Inverse of [`SupportTrend::ordinal`]; out-of-range input yields `None`.
    #[must_use]
    pub const fn from_ordinal(o: u8) -> Option<Self> {
        match o {
            0 => Some(Self::Decaying),
            1 => Some(Self::Flat),
            2 => Some(Self::Accelerating),
            _ => None,
        }
    }

    /// Classify a velocity reading against the named thresholds.
    #[must_use]
    pub const fn from_velocity_bp(velocity_bp: i64) -> Self {
        if velocity_bp >= SUPPORT_ACCELERATING_MIN_BP {
            Self::Accelerating
        } else if velocity_bp <= SUPPORT_DECAYING_MAX_BP {
            Self::Decaying
        } else {
            Self::Flat
        }
    }
}

/// Why support declined to score.
///
/// **Counts and floors only** — no score, no basis points of support, no partial
/// estimate (constitution 46).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportUnknown {
    /// No call at all for this mint in the window.
    NoCallsInWindow {
        /// The window that was searched.
        window_ns: u64,
    },
    /// Some originators, but below the breadth floor. Note the count is the
    /// *effective* (content-clustered, aggregator-excluded) one.
    InsufficientOriginators {
        /// Effective distinct originators found.
        n_originators: u32,
        /// The floor they failed to reach.
        min_originators: u32,
    },
    /// Originators exist, but every one of them carries zero trust weight — a
    /// crowd made entirely of sources we have demoted.
    NoTrustedOriginator {
        /// Effective distinct originators found.
        n_originators: u32,
    },
}

/// A scored social-support picture. Every field is a count, a ratio in basis
/// points, or an integer weight — nothing here is a float and nothing is a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocialSupportScore {
    /// The mint described.
    pub mint_id: u64,
    /// Calls of any kind seen in the window, including echoes.
    pub n_calls: u32,
    /// Calls attributed to amplification rather than origination: aggregator-relay
    /// posts plus an author's own repeats.
    pub n_echo_calls: u32,
    /// Distinct authors with at least one originating call.
    pub n_originators: u32,
    /// Distinct originators after near-duplicate content clustering. This is the
    /// number the small-n gate is applied to.
    pub n_effective_originators: u32,
    /// Originators that did not fit the bounded table (constitution 57/99).
    pub originators_dropped: u32,
    /// Sum of per-cluster trust weights, in [`ORIGINATOR_WEIGHT_UNIT`]s.
    pub trust_weighted_units: u64,
    /// Effective originators at [`TrustTier::Trusted`].
    pub n_trusted_originators: u32,
    /// Effective originators whose track record is `Unknown`.
    pub n_unproven_originators: u32,
    /// Distinct non-aggregator platforms carrying originating calls.
    pub distinct_platforms: u32,
    /// Cross-platform spread credit, basis points.
    pub platform_spread_bp: u32,
    /// Share of effective originators on the single most-used platform, basis
    /// points. High means concentration; concentration means stageable.
    pub platform_concentration_bp: u32,
    /// Share of effective originators whose first call fell inside the single
    /// tightest [`ECHO_BURST_WINDOW_NS`] cluster, basis points.
    pub burst_concentration_bp: u32,
    /// Share of raw originators eliminated by content clustering, basis points.
    /// Always `0` when no content evidence was supplied.
    pub duplicate_share_bp: u32,
    /// Total coordination penalty applied, basis points.
    pub coordination_penalty_bp: u32,
    /// Trust-weighted breadth per sub-window, oldest first.
    pub subwindow_units: [u64; SUPPORT_SUBWINDOWS],
    /// Window-over-window change in trust-weighted breadth, basis points, clamped.
    pub velocity_bp: i64,
    /// Classification of [`SocialSupportScore::velocity_bp`].
    pub trend: SupportTrend,
    /// Whether content digests were supplied. When `false`, near-duplicate
    /// detection was **off** and the score should be read as an upper bound.
    pub content_evidence: bool,
    /// The composite support score, `0..=10_000` basis points.
    pub support_score_bp: u32,
}

/// Social support, or an explicit refusal to guess (constitution 46).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocialSupportVerdict {
    /// Evidence was sufficient; here is the picture.
    Known(SocialSupportScore),
    /// Evidence was insufficient. No score exists, by construction.
    Unknown(SupportUnknown),
}

impl SocialSupportVerdict {
    /// `true` when a score is available.
    #[must_use]
    pub const fn is_known(&self) -> bool {
        matches!(self, Self::Known(_))
    }

    /// The score, or `None`. The **only** path to a number.
    #[must_use]
    pub const fn score(&self) -> Option<&SocialSupportScore> {
        match self {
            Self::Known(s) => Some(s),
            Self::Unknown(_) => None,
        }
    }

    /// Why support declined, or `None` if it did not.
    #[must_use]
    pub const fn unknown_reason(&self) -> Option<SupportUnknown> {
        match self {
            Self::Known(_) => None,
            Self::Unknown(u) => Some(*u),
        }
    }
}

/// A piece of **external** evidence that would sharpen the support estimate.
///
/// This is the brain telling the Phase-B capture layer what to go and fetch. It is
/// deliberately specific — a platform to poll, an author to build a record for —
/// rather than a vague "more data please".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SupportInputNeed {
    /// Below the breadth floor: corroboration from more independent originators is
    /// what would move this from `Unknown` to a score.
    MoreOriginators {
        /// Effective originators observed.
        n_originators: u32,
        /// The floor they must reach.
        min_originators: u32,
    },
    /// No content digests were supplied, so near-duplicate detection is off and the
    /// score is an upper bound. Supply digests for these calls.
    ContentDigests {
        /// Calls in the window lacking a digest.
        n_calls: u32,
    },
    /// No originating call was observed on this platform. Poll it: either the
    /// support really is single-venue (a coordination smell) or we are simply not
    /// looking there.
    PlatformCoverage {
        /// The platform to query.
        platform: Platform,
    },
    /// This originator has no usable track record. Attributing more markouts to
    /// them is what converts them from noise into evidence.
    AuthorTrackRecord {
        /// The author to build a record for.
        author_id: u64,
    },
    /// This originator scores as trusted but their crowding is unknown. Constitution
    /// 28 needs an operator judgement on how public they are before we lean on them.
    SourceExposure {
        /// The author whose exposure is unset.
        author_id: u64,
    },
}

// ---------------------------------------------------------------------------
// Internal working rows
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct OriginatorRow {
    author_id: u64,
    first_call_ns: u64,
    platform_mask: u8,
    subwindow_mask: u8,
    digest: Option<u64>,
    weight_units: u64,
    tier: TrustTier,
}

#[derive(Debug, Clone, Copy)]
struct ContentCluster {
    digest: Option<u64>,
    first_call_ns: u64,
    platform_mask: u8,
    subwindow_mask: u8,
    weight_units: u64,
    n_members: u32,
}

// ---------------------------------------------------------------------------
// The scorer
// ---------------------------------------------------------------------------

/// Computes [`SocialSupportScore`] from this crate's own social history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SocialSupport {
    params: SupportParams,
}

impl SocialSupport {
    /// A scorer at the default params.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A scorer with explicit params.
    #[must_use]
    pub const fn with_params(params: SupportParams) -> Self {
        Self { params }
    }

    /// The tunables in force.
    #[must_use]
    pub const fn params(&self) -> &SupportParams {
        &self.params
    }

    /// Score support with no content evidence, building the trust snapshot inline.
    ///
    /// Convenience wrapper; prefer [`SocialSupport::evaluate_with_content`] with a
    /// reused snapshot when scoring several mints at the same instant.
    #[must_use]
    pub fn evaluate(
        &self,
        social: &SocialRecallIndex,
        trust: &SocialTrust,
        mint_id: u64,
        as_of_ns: u64,
    ) -> SocialSupportVerdict {
        let snap = trust.snapshot(social, as_of_ns);
        self.evaluate_with_content(social, trust, &snap, mint_id, as_of_ns, &[])
    }

    /// Score support, optionally using externally-supplied content digests.
    ///
    /// `witnesses` may be in any order and may contain entries for calls outside the
    /// window; only the first [`SUPPORT_WITNESS_CAP`] are consulted, and duplicate
    /// `call_id`s resolve to the lowest-`content_digest` entry so the result does
    /// not depend on input order.
    #[must_use]
    pub fn evaluate_with_content(
        &self,
        social: &SocialRecallIndex,
        trust: &SocialTrust,
        snap: &TrustSnapshot,
        mint_id: u64,
        as_of_ns: u64,
        witnesses: &[ContentEchoWitness],
    ) -> SocialSupportVerdict {
        let calls = social.who_called(mint_id, as_of_ns, self.params.window_ns);
        if calls.is_empty() {
            return SocialSupportVerdict::Unknown(SupportUnknown::NoCallsInWindow {
                window_ns: self.params.window_ns,
            });
        }
        let digests = sorted_witnesses(witnesses);
        let content_evidence = !digests.is_empty();

        let (rows, n_echo_calls, originators_dropped) =
            self.collect_originators(&calls, trust, snap, as_of_ns, &digests);
        let n_calls = calls.len() as u32;
        let n_originators = rows.len() as u32;

        let clusters = cluster_by_content(&rows, content_evidence);
        let n_effective = clusters.len() as u32;

        if n_effective < self.params.min_originators {
            return SocialSupportVerdict::Unknown(SupportUnknown::InsufficientOriginators {
                n_originators: n_effective,
                min_originators: self.params.min_originators,
            });
        }

        let mut trust_weighted_units = 0u64;
        let mut platform_mask_all = 0u8;
        let mut subwindow_units = [0u64; SUPPORT_SUBWINDOWS];
        for c in &clusters {
            trust_weighted_units = trust_weighted_units.saturating_add(c.weight_units);
            platform_mask_all |= c.platform_mask;
            for (i, slot) in subwindow_units.iter_mut().enumerate() {
                if c.subwindow_mask & (1u8 << i) != 0 {
                    *slot = slot.saturating_add(c.weight_units);
                }
            }
        }
        if trust_weighted_units == 0 {
            return SocialSupportVerdict::Unknown(SupportUnknown::NoTrustedOriginator {
                n_originators: n_effective,
            });
        }

        let (n_trusted_originators, n_unproven_originators) = tier_counts(&rows, &clusters);
        let distinct_platforms = u32::from(platform_mask_all.count_ones() as u16);
        let platform_spread_bp =
            (distinct_platforms.saturating_mul(PLATFORM_SPREAD_STEP_BP)).min(BPS_SCALE_U32);
        let platform_concentration_bp = platform_concentration(&clusters);
        let burst_concentration_bp = burst_concentration(&clusters);
        let duplicate_share_bp = if content_evidence && n_originators > 0 {
            ((u64::from(n_originators - n_effective) * u64::from(BPS_SCALE_U32))
                / u64::from(n_originators)) as u32
        } else {
            0
        };
        let echo_share_bp = if n_calls > 0 {
            ((u64::from(n_echo_calls) * u64::from(BPS_SCALE_U32)) / u64::from(n_calls)) as u32
        } else {
            0
        };
        let coordination_penalty_bp =
            coordination_penalty(duplicate_share_bp, burst_concentration_bp, echo_share_bp);

        let velocity_bp = velocity_bp(&subwindow_units);
        let trend = SupportTrend::from_velocity_bp(velocity_bp);

        let support_score_bp = composite_score_bp(
            trust_weighted_units,
            self.params.saturation_units,
            platform_spread_bp,
            velocity_bp,
            coordination_penalty_bp,
        );

        SocialSupportVerdict::Known(SocialSupportScore {
            mint_id,
            n_calls,
            n_echo_calls,
            n_originators,
            n_effective_originators: n_effective,
            originators_dropped,
            trust_weighted_units,
            n_trusted_originators,
            n_unproven_originators,
            distinct_platforms,
            platform_spread_bp,
            platform_concentration_bp,
            burst_concentration_bp,
            duplicate_share_bp,
            coordination_penalty_bp,
            subwindow_units,
            velocity_bp,
            trend,
            content_evidence,
            support_score_bp,
        })
    }

    /// **What external evidence would sharpen this estimate?**
    ///
    /// Returned in a fixed, deterministic order — breadth need, then content
    /// digests, then uncovered platforms by ascending
    /// [`Platform::ordinal`], then unproven originators by ascending author id,
    /// then trusted-but-unclassified sources by ascending author id — and capped at
    /// [`SUPPORT_NEEDS_CAP`]. The ordering is the priority order: the earlier a need
    /// appears, the more it would move the verdict.
    ///
    /// [`Platform::Aggregator`] is never requested: more relay coverage adds echo,
    /// not evidence.
    #[must_use]
    pub fn support_inputs_needed(
        &self,
        social: &SocialRecallIndex,
        trust: &SocialTrust,
        snap: &TrustSnapshot,
        mint_id: u64,
        as_of_ns: u64,
        witnesses: &[ContentEchoWitness],
    ) -> Vec<SupportInputNeed> {
        let mut out: Vec<SupportInputNeed> = Vec::new();
        let calls = social.who_called(mint_id, as_of_ns, self.params.window_ns);
        if calls.is_empty() {
            return out;
        }
        let digests = sorted_witnesses(witnesses);
        let content_evidence = !digests.is_empty();
        let (rows, _echo, _dropped) =
            self.collect_originators(&calls, trust, snap, as_of_ns, &digests);
        let clusters = cluster_by_content(&rows, content_evidence);
        let n_effective = clusters.len() as u32;

        if n_effective < self.params.min_originators {
            out.push(SupportInputNeed::MoreOriginators {
                n_originators: n_effective,
                min_originators: self.params.min_originators,
            });
        }
        if !content_evidence {
            out.push(SupportInputNeed::ContentDigests {
                n_calls: calls.len() as u32,
            });
        }
        let mut covered = 0u8;
        for c in &clusters {
            covered |= c.platform_mask;
        }
        for o in 0u8..8 {
            let Some(p) = Platform::from_ordinal(o) else {
                continue;
            };
            if p == Platform::Aggregator {
                continue;
            }
            if covered & (1u8 << o) == 0 {
                out.push(SupportInputNeed::PlatformCoverage { platform: p });
            }
        }
        for r in &rows {
            if r.tier == TrustTier::Unproven {
                out.push(SupportInputNeed::AuthorTrackRecord {
                    author_id: r.author_id,
                });
            }
        }
        for r in &rows {
            if r.tier == TrustTier::Trusted
                && trust.exposure_of(r.author_id) == SourceExposure::Niche
            {
                out.push(SupportInputNeed::SourceExposure {
                    author_id: r.author_id,
                });
            }
        }
        out.truncate(SUPPORT_NEEDS_CAP);
        out
    }

    /// Fold the window's calls into one entity-deduped row per originating author.
    ///
    /// Returns `(rows ascending by author id, echo call count, originators dropped)`.
    fn collect_originators(
        &self,
        calls: &[CallRecord],
        trust: &SocialTrust,
        snap: &TrustSnapshot,
        as_of_ns: u64,
        digests: &[(u64, u64)],
    ) -> (Vec<OriginatorRow>, u32, u32) {
        let mut rows: Vec<OriginatorRow> = Vec::new();
        let mut n_echo_calls = 0u32;
        let mut originators_dropped = 0u32;
        let bounds = subwindow_bounds(as_of_ns, self.params.window_ns);

        for c in calls {
            // An aggregator relay is amplification by definition, never origination.
            if c.platform == Platform::Aggregator {
                n_echo_calls = n_echo_calls.saturating_add(1);
                continue;
            }
            let sub_bit = subwindow_bit(c.info_time_ns, &bounds);
            let digest = digests
                .binary_search_by_key(&c.call_id, |d| d.0)
                .ok()
                .map(|i| digests[i].1);
            match rows.binary_search_by_key(&c.author_id, |r| r.author_id) {
                Ok(i) => {
                    // The same author again is an echo of themselves, not breadth.
                    n_echo_calls = n_echo_calls.saturating_add(1);
                    let r = &mut rows[i];
                    r.platform_mask |= 1u8 << c.platform.ordinal();
                    r.subwindow_mask |= sub_bit;
                    if c.info_time_ns < r.first_call_ns {
                        r.first_call_ns = c.info_time_ns;
                    }
                    if r.digest.is_none() {
                        r.digest = digest;
                    }
                }
                Err(i) => {
                    if rows.len() >= self.params.originator_cap {
                        originators_dropped = originators_dropped.saturating_add(1);
                        continue;
                    }
                    let verdict = trust.trust_from_snapshot(snap, c.author_id);
                    rows.insert(
                        i,
                        OriginatorRow {
                            author_id: c.author_id,
                            first_call_ns: c.info_time_ns,
                            platform_mask: 1u8 << c.platform.ordinal(),
                            subwindow_mask: sub_bit,
                            digest,
                            weight_units: originator_weight_units(&verdict),
                            tier: verdict.tier(),
                        },
                    );
                }
            }
        }
        (rows, n_echo_calls, originators_dropped)
    }
}

// ---------------------------------------------------------------------------
// Free functions (public where a caller may reasonably want the same convention)
// ---------------------------------------------------------------------------

/// Trust weight of one originator, in [`ORIGINATOR_WEIGHT_UNIT`]s.
///
/// The whole adversarial argument of this module lives in this function: an
/// `Unknown` author is worth `1`, a `Watch` author `8`, a `Demoted` author `0`, and
/// only a `Trusted` author — one with decayed, partially-pooled, *realized* net SOL
/// behind them — scales up toward `64`.
#[must_use]
pub fn originator_weight_units(verdict: &TrustVerdict) -> u64 {
    match verdict.score() {
        None => UNPROVEN_ORIGINATOR_WEIGHT_UNITS,
        Some(s) => match s.tier {
            TrustTier::Demoted => 0,
            TrustTier::Unproven | TrustTier::Watch => WATCH_ORIGINATOR_WEIGHT_UNITS,
            TrustTier::Trusted => {
                let span = ORIGINATOR_WEIGHT_UNIT - TRUSTED_ORIGINATOR_WEIGHT_FLOOR_UNITS;
                let above = i64::from(s.trust_score_bp)
                    .saturating_sub(i64::from(crate::trust::TRUST_TRUSTED_MIN_BP))
                    .max(0) as u64;
                let head = (i64::from(BPS_SCALE_U32)
                    - i64::from(crate::trust::TRUST_TRUSTED_MIN_BP))
                .max(1) as u64;
                TRUSTED_ORIGINATOR_WEIGHT_FLOOR_UNITS + ((above.min(head) * span) / head)
            }
        },
    }
}

/// Sub-window boundaries for the velocity split: `SUPPORT_SUBWINDOWS + 1` ascending
/// information-time stamps, exact at both ends (no drift from integer division).
#[must_use]
pub fn subwindow_bounds(as_of_ns: u64, window_ns: u64) -> [u64; SUPPORT_SUBWINDOWS + 1] {
    let start = as_of_ns.saturating_sub(window_ns);
    let mut out = [start; SUPPORT_SUBWINDOWS + 1];
    for (i, slot) in out.iter_mut().enumerate() {
        let off = (window_ns / SUPPORT_SUBWINDOWS as u64) * i as u64
            + ((window_ns % SUPPORT_SUBWINDOWS as u64) * i as u64) / SUPPORT_SUBWINDOWS as u64;
        *slot = start.saturating_add(off);
    }
    out[SUPPORT_SUBWINDOWS] = as_of_ns;
    out
}

/// Which sub-window a call falls in, as a one-hot bit. Boundaries are half-open
/// `(lo, hi]` to match [`SocialRecallIndex::who_called`].
fn subwindow_bit(info_time_ns: u64, bounds: &[u64; SUPPORT_SUBWINDOWS + 1]) -> u8 {
    for i in 0..SUPPORT_SUBWINDOWS {
        if info_time_ns > bounds[i] && info_time_ns <= bounds[i + 1] {
            return 1u8 << i;
        }
    }
    // A call at exactly the window floor is excluded by `who_called`; anything else
    // that lands outside is attributed to the newest sub-window rather than dropped.
    1u8 << (SUPPORT_SUBWINDOWS - 1)
}

/// Normalize the witness table into a sorted, deduplicated `(call_id, digest)`
/// lookup. Duplicate `call_id`s resolve to the lowest digest, so the result is
/// independent of the caller's input order.
fn sorted_witnesses(witnesses: &[ContentEchoWitness]) -> Vec<(u64, u64)> {
    let mut v: Vec<(u64, u64)> = witnesses
        .iter()
        .take(SUPPORT_WITNESS_CAP)
        .map(|w| (w.call_id, w.content_digest))
        .collect();
    v.sort_unstable();
    v.dedup_by_key(|e| e.0);
    v
}

/// Collapse originators sharing a content digest into one cluster whose weight is
/// that of its **best** member — never the sum. Echoes are not support.
fn cluster_by_content(rows: &[OriginatorRow], content_evidence: bool) -> Vec<ContentCluster> {
    let mut out: Vec<ContentCluster> = Vec::with_capacity(rows.len());
    for r in rows {
        let key = if content_evidence { r.digest } else { None };
        let existing = key.and_then(|d| out.iter().position(|c| c.digest == Some(d)));
        match existing {
            Some(i) => {
                let c = &mut out[i];
                c.first_call_ns = c.first_call_ns.min(r.first_call_ns);
                c.platform_mask |= r.platform_mask;
                c.subwindow_mask |= r.subwindow_mask;
                c.weight_units = c.weight_units.max(r.weight_units);
                c.n_members = c.n_members.saturating_add(1);
            }
            None => out.push(ContentCluster {
                digest: key,
                first_call_ns: r.first_call_ns,
                platform_mask: r.platform_mask,
                subwindow_mask: r.subwindow_mask,
                weight_units: r.weight_units,
                n_members: 1,
            }),
        }
    }
    out
}

/// Trusted / unproven counts over the surviving clusters, attributed through the
/// rows that formed them.
fn tier_counts(rows: &[OriginatorRow], clusters: &[ContentCluster]) -> (u32, u32) {
    // A cluster inherits the best tier among its members; without content evidence
    // every row is its own cluster and this degenerates to a straight count.
    let n_clusters = clusters.len();
    if n_clusters == rows.len() {
        let mut trusted = 0u32;
        let mut unproven = 0u32;
        for r in rows {
            match r.tier {
                TrustTier::Trusted => trusted += 1,
                TrustTier::Unproven => unproven += 1,
                _ => {}
            }
        }
        return (trusted, unproven);
    }
    let mut trusted = 0u32;
    let mut unproven = 0u32;
    for c in clusters {
        let mut best = TrustTier::Unproven;
        let mut any_trusted = false;
        for r in rows {
            let key = c.digest;
            let matches = match key {
                Some(d) => r.digest == Some(d),
                None => r.digest.is_none() && r.first_call_ns == c.first_call_ns,
            };
            if matches {
                if r.tier == TrustTier::Trusted {
                    any_trusted = true;
                }
                if r.tier != TrustTier::Unproven {
                    best = r.tier;
                }
            }
        }
        if any_trusted {
            trusted += 1;
        } else if best == TrustTier::Unproven {
            unproven += 1;
        }
    }
    (trusted, unproven)
}

/// Share of clusters on the single most-used platform, basis points.
fn platform_concentration(clusters: &[ContentCluster]) -> u32 {
    if clusters.is_empty() {
        return 0;
    }
    let mut best = 0u32;
    for o in 0u8..8 {
        let bit = 1u8 << o;
        let count = clusters
            .iter()
            .filter(|c| c.platform_mask & bit != 0)
            .count() as u32;
        if count > best {
            best = count;
        }
    }
    ((u64::from(best) * u64::from(BPS_SCALE_U32)) / clusters.len() as u64) as u32
}

/// Share of clusters whose first call falls inside the single tightest
/// [`ECHO_BURST_WINDOW_NS`] cluster, basis points. Two-pointer over the sorted
/// first-call stamps: O(n log n) for the sort, O(n) for the sweep, no allocation
/// beyond the stamp vector.
fn burst_concentration(clusters: &[ContentCluster]) -> u32 {
    let n = clusters.len();
    if n == 0 {
        return 0;
    }
    let mut ts: Vec<u64> = clusters.iter().map(|c| c.first_call_ns).collect();
    ts.sort_unstable();
    let mut best = 1usize;
    let mut lo = 0usize;
    for hi in 0..n {
        while ts[hi].saturating_sub(ts[lo]) > ECHO_BURST_WINDOW_NS {
            lo += 1;
        }
        let span = hi - lo + 1;
        if span > best {
            best = span;
        }
    }
    ((best as u64 * u64::from(BPS_SCALE_U32)) / n as u64) as u32
}

/// Total coordination penalty in basis points, capped at
/// [`MAX_COORDINATION_PENALTY_BP`].
///
/// Three additive terms, each individually capped by its named weight:
/// near-duplicate content share, burst concentration **in excess of**
/// [`BURST_TOLERANCE_BP`] (organic discovery does cluster), and relay/repeat echo
/// share.
#[must_use]
pub fn coordination_penalty(
    duplicate_share_bp: u32,
    burst_concentration_bp: u32,
    echo_share_bp: u32,
) -> u32 {
    let full = u64::from(BPS_SCALE_U32);
    let dup = (u64::from(duplicate_share_bp) * u64::from(ECHO_DUP_PENALTY_WEIGHT_BP)) / full;
    let burst_excess = u64::from(burst_concentration_bp.saturating_sub(BURST_TOLERANCE_BP));
    let burst_head = full - u64::from(BURST_TOLERANCE_BP);
    let burst = (burst_excess * u64::from(BURST_PENALTY_WEIGHT_BP)) / burst_head.max(1);
    let relay = (u64::from(echo_share_bp) * u64::from(ECHO_RELAY_PENALTY_WEIGHT_BP)) / full;
    let total = dup + burst + relay;
    total.min(u64::from(MAX_COORDINATION_PENALTY_BP)) as u32
}

/// Window-over-window velocity of trust-weighted breadth, basis points, clamped to
/// `±`[`SUPPORT_VELOCITY_CLAMP_BP`].
#[must_use]
pub fn velocity_bp(subwindow_units: &[u64; SUPPORT_SUBWINDOWS]) -> i64 {
    let first = subwindow_units[0];
    let last = subwindow_units[SUPPORT_SUBWINDOWS - 1];
    if first == 0 && last == 0 {
        return 0;
    }
    let denom = first.max(SUPPORT_VELOCITY_FLOOR_UNITS) as i64;
    let delta = last as i64 - first as i64;
    let raw = delta.saturating_mul(i64::from(BPS_SCALE_U32)) / denom;
    raw.clamp(-SUPPORT_VELOCITY_CLAMP_BP, SUPPORT_VELOCITY_CLAMP_BP)
}

/// Fold the components into the composite `0..=10_000 bp` support score.
///
/// ```text
/// base    = min(10_000, trust_units * 10_000 / saturation_units)
/// spread  = (10_000 - PLATFORM_WEIGHT_BP) + PLATFORM_WEIGHT_BP * spread_bp / 10_000
/// vel     = 10_000 + clamp(velocity_bp * VELOCITY_WEIGHT_BP / 10_000, +-VELOCITY_MAX_EFFECT_BP)
/// score   = min(10_000, base * spread/10_000 * vel/10_000 * (10_000 - penalty)/10_000)
/// ```
///
/// Every step is integer with truncating division and the whole thing is monotone
/// non-decreasing in `trust_weighted_units` and in `platform_spread_bp`, and
/// monotone non-increasing in `coordination_penalty_bp` — properties the tests pin.
#[must_use]
pub fn composite_score_bp(
    trust_weighted_units: u64,
    saturation_units: u64,
    platform_spread_bp: u32,
    velocity_bp: i64,
    coordination_penalty_bp: u32,
) -> u32 {
    let full = u64::from(BPS_SCALE_U32);
    let sat = saturation_units.max(1);
    let base = ((trust_weighted_units.saturating_mul(full)) / sat).min(full);

    let spread_factor = u64::from(BPS_SCALE_U32 - PLATFORM_WEIGHT_BP)
        + (u64::from(PLATFORM_WEIGHT_BP) * u64::from(platform_spread_bp.min(BPS_SCALE_U32))) / full;

    let vel_contrib = ((velocity_bp.saturating_mul(VELOCITY_WEIGHT_BP)) / i64::from(BPS_SCALE_U32))
        .clamp(-VELOCITY_MAX_EFFECT_BP, VELOCITY_MAX_EFFECT_BP);
    let vel_factor = (i64::from(BPS_SCALE_U32) + vel_contrib).max(0) as u64;

    let keep = full.saturating_sub(u64::from(coordination_penalty_bp.min(BPS_SCALE_U32)));

    let mut acc = base;
    acc = (acc * spread_factor) / full;
    acc = (acc * vel_factor) / full;
    acc = (acc * keep) / full;
    acc.min(full) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::social_recall::CallMarkout;
    use crate::trust::{TrustParams, TRUST_TRUSTED_MIN_BP};

    const DAY: u64 = 86_400 * 1_000_000_000;
    const MIN: u64 = 60 * 1_000_000_000;
    const T0: u64 = 100 * DAY;

    fn call(id: u64, mint: u64, author: u64, t: u64, p: Platform) -> CallRecord {
        CallRecord {
            call_id: id,
            mint_id: mint,
            author_id: author,
            platform: p,
            info_time_ns: t,
            followers_decade: 9,
            was_designated: true,
        }
    }

    fn markout(call_id: u64, author: u64, net: i128, t: u64) -> CallMarkout {
        CallMarkout {
            call_id,
            author_id: author,
            realized_net_lamports: net,
            hold_duration_ns: MIN,
            info_time_ns: t,
        }
    }

    /// Give `author` a decisive realized record so they score `Trusted`.
    fn make_trusted(idx: &mut SocialRecallIndex, author: u64, base_id: u64) {
        for i in 0..60u64 {
            idx.record_markout(markout(base_id + i, author, 200_000_000, T0 - DAY))
                .expect("monotone");
        }
    }

    /// Give `author` a decisive losing record so they score `Demoted`.
    fn make_demoted(idx: &mut SocialRecallIndex, author: u64, base_id: u64) {
        for i in 0..60u64 {
            idx.record_markout(markout(base_id + i, author, -200_000_000, T0 - DAY))
                .expect("monotone");
        }
    }

    fn trust_model() -> SocialTrust {
        SocialTrust::with_params(TrustParams {
            population_min_sample: u32::MAX,
            ..TrustParams::default()
        })
    }

    // ------------------------------------------------------------ fail-closed

    #[test]
    fn no_calls_in_window_is_unknown_and_exposes_no_score() {
        let idx = SocialRecallIndex::with_capacity(64, 64);
        let t = trust_model();
        let v = SocialSupport::new().evaluate(&idx, &t, 1, T0);
        assert_eq!(
            v,
            SocialSupportVerdict::Unknown(SupportUnknown::NoCallsInWindow {
                window_ns: SUPPORT_WINDOW_NS
            })
        );
        assert!(v.score().is_none());
        assert!(!v.is_known());
    }

    #[test]
    fn two_originators_fail_the_breadth_floor_with_no_number_attached() {
        let mut idx = SocialRecallIndex::with_capacity(256, 256);
        idx.record_call(call(1, 5, 1, T0 - MIN, Platform::X))
            .expect("ok");
        idx.record_call(call(2, 5, 2, T0 - MIN, Platform::Telegram))
            .expect("ok");
        let t = trust_model();
        let v = SocialSupport::new().evaluate(&idx, &t, 5, T0);
        assert_eq!(
            v,
            SocialSupportVerdict::Unknown(SupportUnknown::InsufficientOriginators {
                n_originators: 2,
                min_originators: SUPPORT_MIN_ORIGINATORS
            })
        );
        assert!(v.score().is_none());
    }

    #[test]
    fn a_crowd_of_only_demoted_sources_scores_nothing() {
        let mut idx = SocialRecallIndex::with_capacity(1_024, 1_024);
        for a in 1..=4u64 {
            make_demoted(&mut idx, a, a * 1_000);
        }
        for a in 1..=4u64 {
            idx.record_call(call(a, 5, a, T0 - u64::from(a as u32) * MIN, Platform::X))
                .expect("ok");
        }
        let t = trust_model();
        let v = SocialSupport::new().evaluate(&idx, &t, 5, T0);
        assert_eq!(
            v,
            SocialSupportVerdict::Unknown(SupportUnknown::NoTrustedOriginator { n_originators: 4 })
        );
        assert!(v.score().is_none());
    }

    #[test]
    fn every_support_unknown_variant_exposes_no_score() {
        for u in [
            SupportUnknown::NoCallsInWindow { window_ns: 1 },
            SupportUnknown::InsufficientOriginators {
                n_originators: 1,
                min_originators: 3,
            },
            SupportUnknown::NoTrustedOriginator { n_originators: 5 },
        ] {
            let v = SocialSupportVerdict::Unknown(u);
            assert!(v.score().is_none());
            assert_eq!(v.unknown_reason(), Some(u));
        }
    }

    // ---------------------------------------------------------------- breadth

    #[test]
    fn breadth_counts_distinct_originators_not_echoes() {
        let mut idx = SocialRecallIndex::with_capacity(1_024, 1_024);
        // One author shouting eleven times, plus three genuine others.
        let mut id = 1u64;
        for k in 0..11u64 {
            idx.record_call(call(id, 5, 1, T0 - 10 * MIN + k, Platform::X))
                .expect("ok");
            id += 1;
        }
        for a in 2..=4u64 {
            idx.record_call(call(id, 5, a, T0 - 5 * MIN + a, Platform::X))
                .expect("ok");
            id += 1;
        }
        let t = trust_model();
        let s = *SocialSupport::new()
            .evaluate(&idx, &t, 5, T0)
            .score()
            .expect("four originators");
        assert_eq!(
            s.n_originators, 4,
            "eleven posts by one author is one voice"
        );
        assert_eq!(s.n_calls, 14);
        assert_eq!(s.n_echo_calls, 10, "the ten repeats are echo, not breadth");
    }

    #[test]
    fn aggregator_relays_are_not_originators() {
        let mut idx = SocialRecallIndex::with_capacity(1_024, 1_024);
        for a in 1..=3u64 {
            idx.record_call(call(a, 5, a, T0 - 10 * MIN + a, Platform::X))
                .expect("ok");
        }
        // Twenty relay bots piling on.
        for a in 100..120u64 {
            idx.record_call(call(a, 5, a, T0 - 5 * MIN + a, Platform::Aggregator))
                .expect("ok");
        }
        let t = trust_model();
        let s = *SocialSupport::new()
            .evaluate(&idx, &t, 5, T0)
            .score()
            .expect("known");
        assert_eq!(
            s.n_originators, 3,
            "an amplifier is not an originator (constitution 21.4)"
        );
        assert_eq!(s.n_echo_calls, 20);
    }

    #[test]
    fn originator_table_is_bounded_under_churn() {
        let mut idx = SocialRecallIndex::with_capacity(8_192, 64);
        for a in 1..=2_000u64 {
            idx.record_call(call(a, 5, a, T0 - 10 * MIN + a, Platform::X))
                .expect("ok");
        }
        let t = trust_model();
        let sup = SocialSupport::with_params(SupportParams {
            originator_cap: 32,
            ..SupportParams::default()
        });
        let s = *sup.evaluate(&idx, &t, 5, T0).score().expect("known");
        assert_eq!(s.n_originators, 32);
        assert!(s.originators_dropped > 0);
        assert!(
            s.support_score_bp <= BPS_SCALE_U32,
            "the score stays in range under overflow"
        );
    }

    // ----------------------------------------------------------- trust weight

    #[test]
    fn an_unproven_account_contributes_near_nothing() {
        let v = TrustVerdict::Unknown(crate::trust::TrustUnknown::NoHistory { min_sample: 8 });
        assert_eq!(
            originator_weight_units(&v),
            UNPROVEN_ORIGINATOR_WEIGHT_UNITS
        );
        const {
            assert!(UNPROVEN_ORIGINATOR_WEIGHT_UNITS * 60 < ORIGINATOR_WEIGHT_UNIT);
        }
    }

    #[test]
    fn a_demoted_source_contributes_exactly_zero() {
        let mut idx = SocialRecallIndex::with_capacity(1_024, 1_024);
        make_demoted(&mut idx, 1, 1);
        let t = trust_model();
        let snap = t.snapshot(&idx, T0);
        let v = t.trust_from_snapshot(&snap, 1);
        assert_eq!(v.tier(), TrustTier::Demoted);
        assert_eq!(originator_weight_units(&v), 0);
    }

    #[test]
    fn sixty_anonymous_accounts_do_not_outweigh_one_proven_caller() {
        let mut anon = SocialRecallIndex::with_capacity(8_192, 8_192);
        for a in 1..=60u64 {
            anon.record_call(call(a, 5, a, T0 - 10 * MIN + a, Platform::X))
                .expect("ok");
        }
        let mut proven = SocialRecallIndex::with_capacity(8_192, 8_192);
        for a in 1..=3u64 {
            proven
                .record_call(call(a, 5, a, T0 - 10 * MIN + a, Platform::X))
                .expect("ok");
        }
        for a in 1..=3u64 {
            make_trusted(&mut proven, a, a * 1_000);
        }
        let t = trust_model();
        let sup = SocialSupport::new();
        let anon_s = *sup.evaluate(&anon, &t, 5, T0).score().expect("known");
        let proven_s = *sup.evaluate(&proven, &t, 5, T0).score().expect("known");
        assert!(
            proven_s.trust_weighted_units > anon_s.trust_weighted_units,
            "three earned records must beat sixty anonymous handles: {} vs {}",
            proven_s.trust_weighted_units,
            anon_s.trust_weighted_units
        );
        assert!(proven_s.support_score_bp > anon_s.support_score_bp);
    }

    #[test]
    fn trusted_weight_scales_with_score_between_floor_and_unit() {
        let low = crate::trust::TrustScore {
            author_id: 1,
            n_markouts: 20,
            effective_weight_units: 1_000_000,
            realized_net_sum_lamports: 1,
            raw_mean_net_lamports: 1,
            prior_mean_net_lamports: 0,
            shrunk_mean_net_lamports: 1,
            pre_demotion_score_bp: TRUST_TRUSTED_MIN_BP,
            trust_score_bp: TRUST_TRUSTED_MIN_BP,
            exposure: SourceExposure::Niche,
            tier: TrustTier::Trusted,
        };
        let mut high = low;
        high.trust_score_bp = BPS_SCALE_U32 as i32;
        assert_eq!(
            originator_weight_units(&TrustVerdict::Known(low)),
            TRUSTED_ORIGINATOR_WEIGHT_FLOOR_UNITS
        );
        assert_eq!(
            originator_weight_units(&TrustVerdict::Known(high)),
            ORIGINATOR_WEIGHT_UNIT
        );
    }

    // ------------------------------------------------------- platform spread

    #[test]
    fn multi_platform_support_outscores_single_platform_support() {
        let t = trust_model();
        let sup = SocialSupport::new();
        let platforms = [
            Platform::X,
            Platform::Telegram,
            Platform::Discord,
            Platform::Stream,
        ];

        let mut single = SocialRecallIndex::with_capacity(1_024, 1_024);
        let mut multi = SocialRecallIndex::with_capacity(1_024, 1_024);
        for (i, p) in platforms.iter().enumerate() {
            let a = i as u64 + 1;
            single
                .record_call(call(a, 5, a, T0 - 10 * MIN + a, Platform::X))
                .expect("ok");
            multi
                .record_call(call(a, 5, a, T0 - 10 * MIN + a, *p))
                .expect("ok");
        }
        let ss = *sup.evaluate(&single, &t, 5, T0).score().expect("known");
        let ms = *sup.evaluate(&multi, &t, 5, T0).score().expect("known");
        assert_eq!(ss.distinct_platforms, 1);
        assert_eq!(ms.distinct_platforms, 4);
        assert_eq!(ss.platform_concentration_bp, BPS_SCALE_U32);
        assert_eq!(ms.platform_concentration_bp, 2_500);
        assert!(
            ms.support_score_bp > ss.support_score_bp,
            "cross-platform corroboration must beat single-venue concentration"
        );
    }

    // -------------------------------------------------------------- velocity

    #[test]
    fn velocity_reads_the_derivative_not_the_level() {
        let rising = velocity_bp(&[8, 16, 64]);
        let flat = velocity_bp(&[64, 64, 64]);
        let falling = velocity_bp(&[64, 16, 8]);
        assert!(rising > 0);
        assert_eq!(flat, 0);
        assert!(falling < 0);
        assert_eq!(
            SupportTrend::from_velocity_bp(rising),
            SupportTrend::Accelerating
        );
        assert_eq!(SupportTrend::from_velocity_bp(flat), SupportTrend::Flat);
        assert_eq!(
            SupportTrend::from_velocity_bp(falling),
            SupportTrend::Decaying
        );
    }

    #[test]
    fn velocity_is_clamped_and_never_divides_by_almost_zero() {
        assert_eq!(velocity_bp(&[0, 0, 0]), 0);
        assert_eq!(
            velocity_bp(&[0, 0, u64::from(u32::MAX)]),
            SUPPORT_VELOCITY_CLAMP_BP
        );
        // Total collapse is exactly -100%: support cannot fall by more than all of
        // itself, so the downside needs no clamp to stay bounded.
        assert_eq!(
            velocity_bp(&[u64::from(u32::MAX), 0, 0]),
            -i64::from(BPS_SCALE_U32)
        );
        assert_eq!(velocity_bp(&[64, 64, 0]), -i64::from(BPS_SCALE_U32));
    }

    #[test]
    fn accelerating_support_outscores_decaying_support_at_equal_breadth() {
        let t = trust_model();
        let sup = SocialSupport::new();
        // Same four originators, same platforms; only *when* they spoke differs.
        let mut early = SocialRecallIndex::with_capacity(1_024, 1_024);
        let mut late = SocialRecallIndex::with_capacity(1_024, 1_024);
        let hour = 60 * MIN;
        for a in 1..=4u64 {
            // Decaying: everyone spoke 23 hours ago, in the oldest third.
            early
                .record_call(call(a, 5, a, T0 - 23 * hour + a, Platform::X))
                .expect("ok");
            // Accelerating: the same four spoke one hour ago, in the newest third.
            late.record_call(call(a, 5, a, T0 - hour + a, Platform::X))
                .expect("ok");
        }
        let e = *sup.evaluate(&early, &t, 5, T0).score().expect("known");
        let l = *sup.evaluate(&late, &t, 5, T0).score().expect("known");
        assert_eq!(e.trend, SupportTrend::Decaying);
        assert_eq!(l.trend, SupportTrend::Accelerating);
        assert!(l.support_score_bp > e.support_score_bp);
    }

    #[test]
    fn subwindow_bounds_are_exact_at_both_ends_and_ascending() {
        let b = subwindow_bounds(T0, SUPPORT_WINDOW_NS);
        assert_eq!(b[0], T0 - SUPPORT_WINDOW_NS);
        assert_eq!(b[SUPPORT_SUBWINDOWS], T0);
        for w in b.windows(2) {
            assert!(w[1] > w[0]);
        }
        // A window that does not divide evenly still lands exactly on `as_of`.
        let b2 = subwindow_bounds(1_000, 1_000);
        assert_eq!(b2[0], 0);
        assert_eq!(b2[SUPPORT_SUBWINDOWS], 1_000);
    }

    // ---------------------------------------------------- coordination penalty

    #[test]
    fn a_simultaneous_burst_is_penalised() {
        let t = trust_model();
        let sup = SocialSupport::new();
        let mut burst = SocialRecallIndex::with_capacity(1_024, 1_024);
        let mut spread = SocialRecallIndex::with_capacity(1_024, 1_024);
        for a in 1..=8u64 {
            // Eight "independent" accounts inside one second.
            burst
                .record_call(call(a, 5, a, T0 - 30 * MIN + a, Platform::X))
                .expect("ok");
            // The same eight, spread over hours.
            spread
                .record_call(call(a, 5, a, T0 - a * 90 * MIN / 60, Platform::X))
                .expect("ok");
        }
        let b = *sup.evaluate(&burst, &t, 5, T0).score().expect("known");
        let s = *sup.evaluate(&spread, &t, 5, T0).score().expect("known");
        assert_eq!(b.burst_concentration_bp, BPS_SCALE_U32);
        assert!(s.burst_concentration_bp < b.burst_concentration_bp);
        assert!(b.coordination_penalty_bp > s.coordination_penalty_bp);
    }

    #[test]
    fn coordination_penalty_is_capped_and_monotone() {
        assert_eq!(coordination_penalty(0, 0, 0), 0);
        let a = coordination_penalty(0, BPS_SCALE_U32, 0);
        let b = coordination_penalty(BPS_SCALE_U32, BPS_SCALE_U32, BPS_SCALE_U32);
        assert!(b > a);
        assert_eq!(b, MAX_COORDINATION_PENALTY_BP);
        assert!(
            coordination_penalty(0, BURST_TOLERANCE_BP, 0) == 0,
            "tolerance is free"
        );
    }

    // ------------------------------------------------------- content clusters

    #[test]
    fn identical_content_collapses_originators_into_one_voice() {
        let mut idx = SocialRecallIndex::with_capacity(1_024, 1_024);
        for a in 1..=12u64 {
            idx.record_call(call(a, 5, a, T0 - 30 * MIN + a * 60, Platform::X))
                .expect("ok");
        }
        let t = trust_model();
        let snap = t.snapshot(&idx, T0);
        let sup = SocialSupport::new();

        let no_content = *sup
            .evaluate_with_content(&idx, &t, &snap, 5, T0, &[])
            .score()
            .expect("known");
        assert_eq!(no_content.n_effective_originators, 12);
        assert!(!no_content.content_evidence);

        // Eight of the twelve posted the same sentence; four spoke for themselves.
        let w: Vec<ContentEchoWitness> = (1..=12u64)
            .map(|id| ContentEchoWitness {
                call_id: id,
                content_digest: if id <= 8 { 777 } else { 1_000 + id },
            })
            .collect();
        let with_content = *sup
            .evaluate_with_content(&idx, &t, &snap, 5, T0, &w)
            .score()
            .expect("known");
        assert!(with_content.content_evidence);
        assert_eq!(with_content.n_originators, 12);
        assert_eq!(
            with_content.n_effective_originators, 5,
            "eight copies of one sentence are one originator"
        );
        assert!(with_content.duplicate_share_bp > 0);
        assert!(with_content.support_score_bp < no_content.support_score_bp);
    }

    #[test]
    fn content_clustering_can_push_a_raid_below_the_breadth_floor() {
        let mut idx = SocialRecallIndex::with_capacity(1_024, 1_024);
        for a in 1..=20u64 {
            idx.record_call(call(a, 5, a, T0 - 30 * MIN + a * 60, Platform::X))
                .expect("ok");
        }
        let t = trust_model();
        let snap = t.snapshot(&idx, T0);
        let w: Vec<ContentEchoWitness> = (1..=20u64)
            .map(|id| ContentEchoWitness {
                call_id: id,
                content_digest: 1,
            })
            .collect();
        let v = SocialSupport::new().evaluate_with_content(&idx, &t, &snap, 5, T0, &w);
        assert_eq!(
            v,
            SocialSupportVerdict::Unknown(SupportUnknown::InsufficientOriginators {
                n_originators: 1,
                min_originators: SUPPORT_MIN_ORIGINATORS
            }),
            "twenty copies of one post is one originator, which is below the floor"
        );
        assert!(v.score().is_none());
    }

    #[test]
    fn witness_order_does_not_change_the_verdict() {
        let mut idx = SocialRecallIndex::with_capacity(1_024, 1_024);
        for a in 1..=6u64 {
            idx.record_call(call(a, 5, a, T0 - 30 * MIN + a * 60, Platform::X))
                .expect("ok");
        }
        let t = trust_model();
        let snap = t.snapshot(&idx, T0);
        let mut w: Vec<ContentEchoWitness> = (1..=6u64)
            .map(|id| ContentEchoWitness {
                call_id: id,
                content_digest: if id <= 3 { 1 } else { id },
            })
            .collect();
        let sup = SocialSupport::new();
        let a = sup.evaluate_with_content(&idx, &t, &snap, 5, T0, &w);
        w.reverse();
        let b = sup.evaluate_with_content(&idx, &t, &snap, 5, T0, &w);
        assert_eq!(a, b);
    }

    // ------------------------------------------------------------ determinism

    #[test]
    fn support_evaluation_is_deterministic() {
        let mut idx = SocialRecallIndex::with_capacity(2_048, 2_048);
        for a in 1..=12u64 {
            let p = Platform::from_ordinal((a % 4) as u8).expect("in range");
            idx.record_call(call(a, 5, a, T0 - a * 30 * MIN, p))
                .expect("ok");
        }
        make_trusted(&mut idx, 3, 10_000);
        let t = trust_model();
        let sup = SocialSupport::new();
        let first = sup.evaluate(&idx, &t, 5, T0);
        for _ in 0..32 {
            assert_eq!(sup.evaluate(&idx, &t, 5, T0), first);
        }
    }

    #[test]
    fn composite_score_is_monotone_in_its_components() {
        let base = composite_score_bp(64, SUPPORT_SATURATION_UNITS, 5_000, 0, 0);
        assert!(composite_score_bp(128, SUPPORT_SATURATION_UNITS, 5_000, 0, 0) > base);
        assert!(composite_score_bp(64, SUPPORT_SATURATION_UNITS, 10_000, 0, 0) > base);
        assert!(composite_score_bp(64, SUPPORT_SATURATION_UNITS, 5_000, 0, 5_000) < base);
        assert!(composite_score_bp(64, SUPPORT_SATURATION_UNITS, 5_000, 30_000, 0) > base);
        assert!(composite_score_bp(64, SUPPORT_SATURATION_UNITS, 5_000, -30_000, 0) < base);
        assert_eq!(
            composite_score_bp(u64::MAX, SUPPORT_SATURATION_UNITS, 10_000, 30_000, 0),
            BPS_SCALE_U32,
            "the score is bounded above by 10_000 bp"
        );
    }

    // ------------------------------------------------------ information needs

    #[test]
    fn inputs_needed_names_the_platforms_and_authors_to_query() {
        let mut idx = SocialRecallIndex::with_capacity(1_024, 1_024);
        for a in 1..=3u64 {
            idx.record_call(call(a, 5, a, T0 - 10 * MIN + a, Platform::X))
                .expect("ok");
        }
        let t = trust_model();
        let snap = t.snapshot(&idx, T0);
        let needs = SocialSupport::new().support_inputs_needed(&idx, &t, &snap, 5, T0, &[]);

        assert!(needs.contains(&SupportInputNeed::ContentDigests { n_calls: 3 }));
        for p in [Platform::Telegram, Platform::Discord, Platform::Stream] {
            assert!(
                needs.contains(&SupportInputNeed::PlatformCoverage { platform: p }),
                "must ask for coverage on {p:?}"
            );
        }
        assert!(
            !needs.contains(&SupportInputNeed::PlatformCoverage {
                platform: Platform::Aggregator
            }),
            "never ask for more relay coverage"
        );
        for a in 1..=3u64 {
            assert!(needs.contains(&SupportInputNeed::AuthorTrackRecord { author_id: a }));
        }
        assert!(needs.len() <= SUPPORT_NEEDS_CAP);
    }

    #[test]
    fn inputs_needed_asks_for_exposure_on_a_trusted_source() {
        let mut idx = SocialRecallIndex::with_capacity(2_048, 2_048);
        for a in 1..=3u64 {
            idx.record_call(call(a, 5, a, T0 - 10 * MIN + a, Platform::X))
                .expect("ok");
        }
        make_trusted(&mut idx, 2, 10_000);
        let mut t = trust_model();
        let snap = t.snapshot(&idx, T0);
        let needs = SocialSupport::new().support_inputs_needed(&idx, &t, &snap, 5, T0, &[]);
        assert!(needs.contains(&SupportInputNeed::SourceExposure { author_id: 2 }));

        // Once the operator classifies them, the need disappears.
        t.set_exposure(2, SourceExposure::Crowded)
            .expect("capacity");
        let needs2 = SocialSupport::new().support_inputs_needed(&idx, &t, &snap, 5, T0, &[]);
        assert!(!needs2.contains(&SupportInputNeed::SourceExposure { author_id: 2 }));
    }

    #[test]
    fn inputs_needed_leads_with_the_breadth_gap_when_below_the_floor() {
        let mut idx = SocialRecallIndex::with_capacity(1_024, 1_024);
        idx.record_call(call(1, 5, 1, T0 - MIN, Platform::X))
            .expect("ok");
        let t = trust_model();
        let snap = t.snapshot(&idx, T0);
        let needs = SocialSupport::new().support_inputs_needed(&idx, &t, &snap, 5, T0, &[]);
        assert_eq!(
            needs.first(),
            Some(&SupportInputNeed::MoreOriginators {
                n_originators: 1,
                min_originators: SUPPORT_MIN_ORIGINATORS
            })
        );
    }

    #[test]
    fn inputs_needed_is_empty_when_there_is_nothing_to_ask_about() {
        let idx = SocialRecallIndex::with_capacity(64, 64);
        let t = trust_model();
        let snap = t.snapshot(&idx, T0);
        assert!(SocialSupport::new()
            .support_inputs_needed(&idx, &t, &snap, 5, T0, &[])
            .is_empty());
    }

    #[test]
    fn trend_ordinals_round_trip() {
        for o in 0u8..3 {
            assert_eq!(
                SupportTrend::from_ordinal(o).expect("in range").ordinal(),
                o
            );
        }
        assert!(SupportTrend::from_ordinal(3).is_none());
    }
}
