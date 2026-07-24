//! `SocialTrust` — "can I actually trust the accounts saying this?"
//! (constitution 22 integer-only, 28 public-burned edge, 46 small-n, 57/99
//! bounded state, 102 named thresholds).
//!
//! # The load-bearing law: trust is earned in lamports, and only in lamports
//!
//! Every number this module produces is derived from **realized net SOL on calls
//! attributable to that author** — [`crate::social_recall::CallMarkout`] records —
//! and from nothing else. Follower count, engagement, verified badges, "10x
//! caller" bios, channel size, screenshot P&L: none of them appear anywhere in the
//! computation.
//!
//! That is not a stylistic preference, it is the entire point. Every one of those
//! signals is *purchasable*, and cheaply: followers, retweets, view counts and blue
//! ticks are line items with a market price, and a group whose business model is
//! selling exit liquidity will always buy them, because doing so is far cheaper
//! than actually being right. Any trust model that reads them is not measuring
//! skill, it is measuring marketing budget — and it will rank the most dangerous
//! counterparty in the market as the most trustworthy source in the book.
//!
//! ## The refusal is structural, not disciplinary
//!
//! [`crate::social_recall::CallRecord`] — the record of *who said what* — does
//! carry a `followers_decade` and a `was_designated` flag. **This module never
//! reads a `CallRecord`.** The trust path consumes only [`CallMarkout`], whose
//! entire field set is `call_id`, `author_id`, `realized_net_lamports`,
//! `hold_duration_ns` and `info_time_ns`. There is no popularity field on the type
//! this module reads, so popularity is not merely ignored — it is *unreachable*.
//! A future contributor cannot accidentally weight it in without first changing the
//! shape of the data, which is a reviewable act.
//!
//! `tests::popularity_cannot_buy_trust` and
//! `tests::follower_explosion_does_not_move_a_single_basis_point` pin it down from
//! the outside as well.
//!
//! # The model: integer partial pooling with information-time decay
//!
//! Three ideas, all integer, all replay-stable.
//!
//! **1. Decay (memecoin edge is perishable).** A markout attributed `age`
//! nanoseconds ago carries weight [`decay_weight_units`]`(age, half_life)`, a
//! monotone non-increasing integer approximation of `2^-age/half_life` scaled by
//! [`TRUST_WEIGHT_UNIT`]. A caller who was right through one regime and has not
//! been right since bleeds evidence mass at a documented half-life
//! ([`TRUST_HALF_LIFE_NS`]) until the prior is all that is left of them.
//!
//! **2. Partial pooling (two lucky calls are not an edge).** The author's decayed
//! weighted mean net is blended with a population prior in the exact conjugate
//! form — a precision-weighted average, where "precision" is evidence mass:
//!
//! ```text
//! shrunk = (sum_i w_i * net_i  +  prior_mean * PRIOR_MASS)
//!          / (sum_i w_i        +  PRIOR_MASS)
//!
//! PRIOR_MASS = prior_pseudo_samples * TRUST_WEIGHT_UNIT
//! ```
//!
//! With [`TRUST_PRIOR_PSEUDO_SAMPLES`] `= 12`, an author holding two fresh
//! markouts owns `2/14` of their own posterior and borrows `12/14` from the
//! population. Their two 5-SOL winners move the estimate by a seventh of what a
//! naive mean would claim. As evidence mass grows the author's own record
//! dominates and the prior fades — which is the whole content of "shrink small
//! samples toward the prior".
//!
//! **3. A bounded, signed score.** The shrunk mean is expressed against
//! [`TRUST_REFERENCE_NET_LAMPORTS`] — the per-call realized net that defines full
//! trust — and clamped to `±10_000` bp. Tiers are named thresholds over that.
//!
//! ## The prior may drag down without limit, but may only lift halfway
//!
//! The population prior is itself estimated from our own markout ring, so a lucky
//! fortnight across *all* callers would inflate every author's posterior at once.
//! [`TRUST_PRIOR_POSITIVE_CAP_LAMPORTS`] caps the prior's positive side at half of
//! reference while leaving its negative side unclamped (constitution 46: an
//! estimator is allowed to be pessimistic for free, never optimistic for free). And
//! below [`TRUST_POPULATION_MIN_SAMPLE`] markouts the prior is the neutral
//! [`TRUST_NEUTRAL_PRIOR_NET_LAMPORTS`] `= 0` — "a caller drawn at random makes us
//! nothing", which is both honest and conservative.
//!
//! # Public-burned presumption (constitution 28)
//!
//! A source that everybody reads has no edge left to give: by the time a widely
//! legible caller has spoken, the fill is gone and what remains is the privilege of
//! being someone's exit. That is expressed as [`SourceExposure`], an **explicit
//! caller-set flag** — this module does not and cannot infer how crowded a source
//! is, and it will not pretend to. Each level carries a documented demotion in
//! basis points, and [`SourceExposure::PublicBurned`] additionally caps the tier at
//! [`TrustTier::Watch`]: a burned source can be worth *monitoring*, never worth
//! *sizing on*.
//!
//! Demotion only ever removes positive trust. It never improves a negative score,
//! because "everyone follows them" is not an excuse for having lost us money.
//!
//! # Fail-closed (constitution 46)
//!
//! [`TrustVerdict`] mirrors [`crate::recall::RecallVerdict`] exactly:
//! `Known(TrustScore)` or `Unknown(TrustUnknown)`, where [`TrustUnknown`] carries
//! **counts and floors only** — no score, no mean, no lamports. There is no
//! accessor, no `unwrap_or`, no `Default` that hands out a number. Three gates
//! produce `Unknown`: no history at all, fewer than
//! [`TrustParams::min_sample`] in-scope markouts, and — the interesting one —
//! *enough markouts but all of them stale*, i.e. decayed evidence mass below
//! [`TrustParams::min_effective_weight_units`]. A caller proven in March is
//! unproven in July.
//!
//! # Determinism and bounds
//!
//! No wall clock (`as_of_ns` is caller-supplied information time), no RNG, no
//! floats, no unordered iteration. Markouts stamped *after* `as_of_ns` are excluded
//! so a replay cannot look ahead. The exposure registry is a fixed-capacity sorted
//! vector ([`TRUST_EXPOSURE_CAP`]) that **refuses new entries when full rather than
//! evicting** — silently evicting a `PublicBurned` marking would silently re-trust
//! a burned source, which is a safety regression, so the ring idiom used elsewhere
//! in this crate is deliberately *not* used here. [`TrustSnapshot`] is likewise
//! capacity-bounded ([`TRUST_AUTHOR_CAP`]) and an author beyond the bound comes
//! back `Unknown`, never guessed.

use crate::recall::BPS_SCALE_U32;
use crate::social_recall::{CallMarkout, SocialRecallIndex};

// ---------------------------------------------------------------------------
// Named constants (constitution 102)
// ---------------------------------------------------------------------------

/// Fixed-point unit of one *fresh* evidence sample (constitution 22). A markout of
/// age zero weighs exactly this; the decay curve is expressed as a fraction of it.
pub const TRUST_WEIGHT_UNIT: u64 = 1 << 16;

/// Number of halvings after which [`decay_weight_units`] returns exactly zero
/// (constitution 102). At `TRUST_WEIGHT_UNIT = 2^16` the weight has already
/// truncated to zero by the sixteenth halving, so this is exact rather than a cut-off.
pub const TRUST_DECAY_MAX_HALVINGS: u64 = 16;

/// Half-life of attributed evidence, nanoseconds of information time
/// (constitution 102). Fourteen days: long enough that a genuine caller keeps their
/// record across a slow week, short enough that a caller who was right in a dead
/// meta three months ago is back at the prior today.
pub const TRUST_HALF_LIFE_NS: u64 = 14 * 86_400 * 1_000_000_000;

/// Minimum in-scope attributed markouts before an author can be scored
/// (constitution 46 small-n guard).
pub const TRUST_MIN_SAMPLE: u32 = 8;

/// Minimum *decayed* evidence mass before an author can be scored
/// (constitution 46). Three fresh-equivalent samples. This is the gate that stops
/// an old track record from speaking for a caller who has gone quiet or gone cold.
pub const TRUST_MIN_EFFECTIVE_WEIGHT_UNITS: u64 = 3 * TRUST_WEIGHT_UNIT;

/// Strength of the population prior in pseudo-observations (constitution 102).
/// An author needs this much of their own fresh evidence mass before their record
/// outweighs the population's.
pub const TRUST_PRIOR_PSEUDO_SAMPLES: u32 = 12;

/// Per-call realized net that defines a full `+10_000 bp` trust score
/// (constitution 102): 0.05 SOL of attributed net per call, sustained.
pub const TRUST_REFERENCE_NET_LAMPORTS: i128 = 50_000_000;

/// Minimum markouts across the whole population before the prior is pooled from
/// data rather than set to neutral (constitution 46).
pub const TRUST_POPULATION_MIN_SAMPLE: u32 = 32;

/// The neutral prior (constitution 46): a caller drawn at random makes us nothing.
pub const TRUST_NEUTRAL_PRIOR_NET_LAMPORTS: i128 = 0;

/// Cap on the *positive* side of the pooled prior (constitution 46). The prior may
/// pull an author down without limit; it may only lift them to half of reference.
pub const TRUST_PRIOR_POSITIVE_CAP_LAMPORTS: i128 = TRUST_REFERENCE_NET_LAMPORTS / 2;

/// Score at or below which an author is [`TrustTier::Demoted`] (constitution 102).
pub const TRUST_DEMOTED_MAX_BP: i32 = -500;

/// Score at or above which an author reaches [`TrustTier::Watch`]
/// (constitution 102).
pub const TRUST_WATCH_MIN_BP: i32 = 500;

/// Score at or above which an author reaches [`TrustTier::Trusted`]
/// (constitution 102).
pub const TRUST_TRUSTED_MIN_BP: i32 = 2_500;

/// Demotion applied to a source that is legible to a broad audience
/// (constitution 28).
pub const CROWDED_DEMOTION_BP: u32 = 3_000;

/// Demotion applied to a source that is fully public and front-run by everyone
/// (constitution 28).
pub const PUBLIC_BURNED_DEMOTION_BP: u32 = 7_500;

/// Capacity of the exposure registry (constitution 57/99). **Refuses** rather than
/// evicts when full — see the module docs.
pub const TRUST_EXPOSURE_CAP: usize = 1_024;

/// Capacity of a [`TrustSnapshot`]'s author table (constitution 57/99). Authors
/// beyond the bound are dropped and score `Unknown`.
pub const TRUST_AUTHOR_CAP: usize = 4_096;

// ---------------------------------------------------------------------------
// Decay
// ---------------------------------------------------------------------------

/// Evidence weight of a markout observed `age_ns` ago, in units of
/// [`TRUST_WEIGHT_UNIT`].
///
/// An integer approximation of `TRUST_WEIGHT_UNIT * 2^(-age/half_life)`. Writing
/// `age = k * half_life + r`, the exact value is `(unit >> k) * 2^(-r/half_life)`;
/// the fractional factor is replaced by its chord, `(2*half_life - r) /
/// (2*half_life)`, which agrees with the exact curve at both ends of every halving
/// and therefore joins up continuously across segments. The result is:
///
/// * **monotone non-increasing** in `age_ns`, within a halving and across the
///   boundary (`tests::decay_is_monotone_non_increasing`);
/// * exactly `TRUST_WEIGHT_UNIT` at `age_ns == 0`;
/// * exactly `TRUST_WEIGHT_UNIT / 2^k` at `age_ns == k * half_life`;
/// * exactly `0` once `k >= TRUST_DECAY_MAX_HALVINGS`.
///
/// A `half_life_ns` of zero means "no memory": weight `TRUST_WEIGHT_UNIT` at age
/// zero and `0` thereafter.
#[must_use]
pub fn decay_weight_units(age_ns: u64, half_life_ns: u64) -> u64 {
    if half_life_ns == 0 {
        return if age_ns == 0 { TRUST_WEIGHT_UNIT } else { 0 };
    }
    let k = age_ns / half_life_ns;
    if k >= TRUST_DECAY_MAX_HALVINGS {
        return 0;
    }
    let r = age_ns % half_life_ns;
    let base = TRUST_WEIGHT_UNIT >> k;
    let hl = u128::from(half_life_ns);
    let num = u128::from(base) * ((2 * hl) - u128::from(r));
    let den = 2 * hl;
    // `num / den <= base <= TRUST_WEIGHT_UNIT`, so the cast cannot truncate.
    (num / den) as u64
}

// ---------------------------------------------------------------------------
// Exposure (constitution 28)
// ---------------------------------------------------------------------------

/// How legible a source is to the rest of the market — an **operator-set** fact,
/// never inferred (constitution 28).
///
/// The demotion ladder is the price of crowding: an edge that everyone can read is
/// an edge that has already been taken by the time you read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceExposure {
    /// Not known to be widely followed. No demotion. The default.
    Niche,
    /// Legible to a broad audience; partially front-run.
    Crowded,
    /// Fully public and reliably front-run (constitution 28). Demoted hard **and**
    /// capped at [`TrustTier::Watch`] whatever the realized record says.
    PublicBurned,
}

impl SourceExposure {
    /// Dense ordinal used for ordering and any wire format.
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        match self {
            Self::Niche => 0,
            Self::Crowded => 1,
            Self::PublicBurned => 2,
        }
    }

    /// Inverse of [`SourceExposure::ordinal`]; out-of-range input yields `None`.
    #[must_use]
    pub const fn from_ordinal(o: u8) -> Option<Self> {
        match o {
            0 => Some(Self::Niche),
            1 => Some(Self::Crowded),
            2 => Some(Self::PublicBurned),
            _ => None,
        }
    }

    /// Fraction of a *positive* trust score removed by this exposure level, in
    /// basis points (constitution 28/102).
    #[must_use]
    pub const fn demotion_bp(self) -> u32 {
        match self {
            Self::Niche => 0,
            Self::Crowded => CROWDED_DEMOTION_BP,
            Self::PublicBurned => PUBLIC_BURNED_DEMOTION_BP,
        }
    }
}

// ---------------------------------------------------------------------------
// Tiers
// ---------------------------------------------------------------------------

/// Coarse trust classification with named thresholds (constitution 102).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TrustTier {
    /// Not enough realized evidence to say anything. The only tier a
    /// [`TrustVerdict::Unknown`] can map to.
    Unproven,
    /// Scored, but with no demonstrated edge worth sizing on. Also the ceiling for
    /// a [`SourceExposure::PublicBurned`] source (constitution 28).
    Watch,
    /// Demonstrated, decay-adjusted, partially-pooled positive realized net.
    Trusted,
    /// Demonstrated *negative* realized attribution. Following this source has cost
    /// us money.
    Demoted,
}

impl TrustTier {
    /// Dense ordinal used for ordering and any wire format.
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        match self {
            Self::Unproven => 0,
            Self::Watch => 1,
            Self::Trusted => 2,
            Self::Demoted => 3,
        }
    }

    /// Inverse of [`TrustTier::ordinal`]; out-of-range input yields `None`.
    #[must_use]
    pub const fn from_ordinal(o: u8) -> Option<Self> {
        match o {
            0 => Some(Self::Unproven),
            1 => Some(Self::Watch),
            2 => Some(Self::Trusted),
            3 => Some(Self::Demoted),
            _ => None,
        }
    }

    /// `true` only for [`TrustTier::Trusted`] — the single tier this crate will let
    /// a sizing decision lean on.
    #[must_use]
    pub const fn is_sizable(self) -> bool {
        matches!(self, Self::Trusted)
    }

    /// Classify a post-demotion score against the named thresholds.
    #[must_use]
    pub const fn from_score_bp(score_bp: i32) -> Self {
        if score_bp <= TRUST_DEMOTED_MAX_BP {
            Self::Demoted
        } else if score_bp >= TRUST_TRUSTED_MIN_BP {
            Self::Trusted
        } else {
            Self::Watch
        }
    }
}

// ---------------------------------------------------------------------------
// Params
// ---------------------------------------------------------------------------

/// Tunables for the trust model. All defaults are named consts (constitution 102).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustParams {
    /// Minimum in-scope attributed markouts before scoring (constitution 46).
    pub min_sample: u32,
    /// Minimum decayed evidence mass before scoring (constitution 46).
    pub min_effective_weight_units: u64,
    /// Evidence half-life in nanoseconds of information time.
    pub half_life_ns: u64,
    /// Prior strength in pseudo-observations.
    pub prior_pseudo_samples: u32,
    /// Per-call realized net defining a full `+10_000 bp` score.
    pub reference_net_lamports: i128,
    /// Minimum population markouts before the prior is pooled from data.
    pub population_min_sample: u32,
    /// Cap on the positive side of the pooled prior.
    pub prior_positive_cap_lamports: i128,
}

impl Default for TrustParams {
    fn default() -> Self {
        Self {
            min_sample: TRUST_MIN_SAMPLE,
            min_effective_weight_units: TRUST_MIN_EFFECTIVE_WEIGHT_UNITS,
            half_life_ns: TRUST_HALF_LIFE_NS,
            prior_pseudo_samples: TRUST_PRIOR_PSEUDO_SAMPLES,
            reference_net_lamports: TRUST_REFERENCE_NET_LAMPORTS,
            population_min_sample: TRUST_POPULATION_MIN_SAMPLE,
            prior_positive_cap_lamports: TRUST_PRIOR_POSITIVE_CAP_LAMPORTS,
        }
    }
}

// ---------------------------------------------------------------------------
// Verdict
// ---------------------------------------------------------------------------

/// Why trust declined to score a source.
///
/// **Carries counts and floors only.** No lamports, no mean, no score, no tier
/// beyond [`TrustTier::Unproven`]. There is no way to coax a number out of this
/// type (constitution 46).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustUnknown {
    /// No attributed markout for this author at or before `as_of_ns`.
    NoHistory {
        /// The sample floor that was in force.
        min_sample: u32,
    },
    /// Some history, but below the sample floor.
    InsufficientSample {
        /// In-scope attributed markouts found.
        n_markouts: u32,
        /// The floor they failed to reach.
        min_sample: u32,
    },
    /// Enough markouts, but all of them too old to mean anything after decay —
    /// a caller proven in a regime that is over (constitution 28/46).
    StaleEvidence {
        /// In-scope attributed markouts found.
        n_markouts: u32,
        /// Decayed evidence mass, in [`TRUST_WEIGHT_UNIT`]s.
        effective_weight_units: u64,
        /// The mass floor it failed to reach.
        min_effective_weight_units: u64,
    },
    /// The author is not in the snapshot's bounded table (constitution 57/99).
    NotInSnapshot {
        /// How many authors the snapshot had to drop.
        authors_dropped: u64,
    },
}

/// A scored source. Every field is auditable back to realized lamports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustScore {
    /// The author described.
    pub author_id: u64,
    /// In-scope attributed markouts backing the score.
    pub n_markouts: u32,
    /// Decayed evidence mass in [`TRUST_WEIGHT_UNIT`]s.
    pub effective_weight_units: u64,
    /// Undecayed total realized net across in-scope markouts — the audit anchor.
    pub realized_net_sum_lamports: i128,
    /// Decay-weighted mean net per call, **before** pooling.
    pub raw_mean_net_lamports: i128,
    /// The population prior the author was pooled toward.
    pub prior_mean_net_lamports: i128,
    /// Decay-weighted mean net per call **after** partial pooling. This is the
    /// number the score is computed from.
    pub shrunk_mean_net_lamports: i128,
    /// Score in basis points of [`TrustParams::reference_net_lamports`], clamped to
    /// `±10_000`, **before** the exposure demotion.
    pub pre_demotion_score_bp: i32,
    /// Score after the constitution-28 exposure demotion. The published number.
    pub trust_score_bp: i32,
    /// The operator-set exposure applied.
    pub exposure: SourceExposure,
    /// Tier of [`TrustScore::trust_score_bp`], capped at [`TrustTier::Watch`] for a
    /// [`SourceExposure::PublicBurned`] source.
    pub tier: TrustTier,
}

/// An author's trust, or an explicit refusal to guess (constitution 46).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustVerdict {
    /// Evidence was sufficient; here is the score.
    Known(TrustScore),
    /// Evidence was insufficient. No score exists, by construction.
    Unknown(TrustUnknown),
}

impl TrustVerdict {
    /// `true` when a score is available.
    #[must_use]
    pub const fn is_known(&self) -> bool {
        matches!(self, Self::Known(_))
    }

    /// The score, or `None`. The **only** path to a number.
    #[must_use]
    pub const fn score(&self) -> Option<&TrustScore> {
        match self {
            Self::Known(s) => Some(s),
            Self::Unknown(_) => None,
        }
    }

    /// Why trust declined, or `None` if it did not.
    #[must_use]
    pub const fn unknown_reason(&self) -> Option<TrustUnknown> {
        match self {
            Self::Known(_) => None,
            Self::Unknown(u) => Some(*u),
        }
    }

    /// The tier, which is [`TrustTier::Unproven`] for every `Unknown`.
    #[must_use]
    pub const fn tier(&self) -> TrustTier {
        match self {
            Self::Known(s) => s.tier,
            Self::Unknown(_) => TrustTier::Unproven,
        }
    }
}

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

/// The population prior the shrinkage pools toward.
///
/// `pooled == false` means the population itself was below
/// [`TrustParams::population_min_sample`] and the prior is the neutral
/// [`TRUST_NEUTRAL_PRIOR_NET_LAMPORTS`] (constitution 46).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PopulationPrior {
    /// Prior mean net per call, after the positive-side cap.
    pub mean_net_lamports: i128,
    /// Decayed population evidence mass.
    pub weight_units: u64,
    /// In-scope population markouts.
    pub n_markouts: u32,
    /// Whether the prior was pooled from data rather than set neutral.
    pub pooled: bool,
}

/// One author's accumulated, decayed evidence mass. Pure counts and sums — this is
/// *evidence*, not an estimate, and it is deliberately not a score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorMass {
    /// The author.
    pub author_id: u64,
    /// In-scope attributed markouts.
    pub n_markouts: u32,
    /// Decayed evidence mass in [`TRUST_WEIGHT_UNIT`]s.
    pub weight_units: u64,
    /// `sum_i w_i * net_i` in lamport-weight-units.
    pub weighted_net_lamports: i128,
    /// Undecayed total realized net.
    pub realized_net_sum_lamports: i128,
}

/// A single-pass, capacity-bounded view of every author's evidence mass as of one
/// information-time instant (constitution 57/99).
///
/// Built once and reused, so scoring twenty callers on a mint costs one pass over
/// the markout ring rather than twenty. Authors are held sorted by id, so every
/// derived listing is deterministic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustSnapshot {
    as_of_ns: u64,
    prior: PopulationPrior,
    authors: Vec<AuthorMass>,
    capacity: usize,
    authors_dropped: u64,
}

impl TrustSnapshot {
    /// The information-time instant this snapshot describes.
    #[must_use]
    pub const fn as_of_ns(&self) -> u64 {
        self.as_of_ns
    }

    /// The population prior in force.
    #[must_use]
    pub const fn prior(&self) -> PopulationPrior {
        self.prior
    }

    /// Every author's evidence mass, ascending by author id.
    #[must_use]
    pub fn authors(&self) -> &[AuthorMass] {
        &self.authors
    }

    /// Hard author capacity.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Authors that did not fit the bounded table. They score `Unknown`.
    #[must_use]
    pub const fn authors_dropped(&self) -> u64 {
        self.authors_dropped
    }

    /// One author's evidence mass, or `None`.
    #[must_use]
    pub fn author_mass(&self, author_id: u64) -> Option<AuthorMass> {
        self.authors
            .binary_search_by_key(&author_id, |a| a.author_id)
            .ok()
            .map(|i| self.authors[i])
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why an exposure marking could not be stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustError {
    /// The bounded exposure registry is full. **Nothing was evicted** — see the
    /// module docs on why silently dropping a demotion is a safety regression.
    ExposureCapacityExhausted {
        /// The capacity that was reached.
        capacity: usize,
    },
}

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

/// Per-author, per-source trust earned exclusively from realized net SOL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocialTrust {
    params: TrustParams,
    exposure: Vec<(u64, SourceExposure)>,
    exposure_capacity: usize,
    author_capacity: usize,
}

impl Default for SocialTrust {
    fn default() -> Self {
        Self::new()
    }
}

impl SocialTrust {
    /// A model at the default params and capacities.
    #[must_use]
    pub fn new() -> Self {
        Self::with_params(TrustParams::default())
    }

    /// A model with explicit params at the default capacities.
    #[must_use]
    pub fn with_params(params: TrustParams) -> Self {
        Self::with_capacity(params, TRUST_EXPOSURE_CAP, TRUST_AUTHOR_CAP)
    }

    /// A model with explicit params and capacities (each clamped to at least 1).
    #[must_use]
    pub fn with_capacity(
        params: TrustParams,
        exposure_capacity: usize,
        author_capacity: usize,
    ) -> Self {
        let exposure_capacity = exposure_capacity.max(1);
        let author_capacity = author_capacity.max(1);
        Self {
            params,
            exposure: Vec::with_capacity(exposure_capacity.min(64)),
            exposure_capacity,
            author_capacity,
        }
    }

    /// The tunables in force.
    #[must_use]
    pub const fn params(&self) -> &TrustParams {
        &self.params
    }

    /// Hard capacity of the exposure registry.
    #[must_use]
    pub const fn exposure_capacity(&self) -> usize {
        self.exposure_capacity
    }

    /// Live exposure markings.
    #[must_use]
    pub fn exposure_len(&self) -> usize {
        self.exposure.len()
    }

    /// Hard capacity of a snapshot's author table.
    #[must_use]
    pub const fn author_capacity(&self) -> usize {
        self.author_capacity
    }

    /// Record how legible a source is (constitution 28). Returns the previous
    /// marking, if any. Overwriting an existing author never consumes capacity.
    pub fn set_exposure(
        &mut self,
        author_id: u64,
        exposure: SourceExposure,
    ) -> Result<Option<SourceExposure>, TrustError> {
        match self.exposure.binary_search_by_key(&author_id, |e| e.0) {
            Ok(i) => {
                let prev = self.exposure[i].1;
                self.exposure[i].1 = exposure;
                Ok(Some(prev))
            }
            Err(i) => {
                if self.exposure.len() >= self.exposure_capacity {
                    return Err(TrustError::ExposureCapacityExhausted {
                        capacity: self.exposure_capacity,
                    });
                }
                self.exposure.insert(i, (author_id, exposure));
                Ok(None)
            }
        }
    }

    /// The exposure marking for an author, defaulting to [`SourceExposure::Niche`].
    #[must_use]
    pub fn exposure_of(&self, author_id: u64) -> SourceExposure {
        self.exposure
            .binary_search_by_key(&author_id, |e| e.0)
            .map_or(SourceExposure::Niche, |i| self.exposure[i].1)
    }

    /// Build a one-pass, bounded view of every author's decayed evidence mass.
    ///
    /// Markouts stamped after `as_of_ns` are excluded: replay must not see the
    /// future. Cost is one pass over the markout ring plus a bounded sorted insert
    /// per new author.
    #[must_use]
    pub fn snapshot(&self, social: &SocialRecallIndex, as_of_ns: u64) -> TrustSnapshot {
        let mut authors: Vec<AuthorMass> = Vec::new();
        let mut authors_dropped = 0u64;
        let mut pop_weight = 0u64;
        let mut pop_weighted_net = 0i128;
        let mut pop_n = 0u32;

        for m in social.iter_markouts_oldest_first() {
            if m.info_time_ns > as_of_ns {
                continue;
            }
            let w = self.markout_weight(m, as_of_ns);
            pop_weight = pop_weight.saturating_add(w);
            pop_weighted_net =
                pop_weighted_net.saturating_add(i128::from(w) * m.realized_net_lamports);
            pop_n = pop_n.saturating_add(1);

            match authors.binary_search_by_key(&m.author_id, |a| a.author_id) {
                Ok(i) => {
                    let a = &mut authors[i];
                    a.n_markouts = a.n_markouts.saturating_add(1);
                    a.weight_units = a.weight_units.saturating_add(w);
                    a.weighted_net_lamports = a
                        .weighted_net_lamports
                        .saturating_add(i128::from(w) * m.realized_net_lamports);
                    a.realized_net_sum_lamports = a
                        .realized_net_sum_lamports
                        .saturating_add(m.realized_net_lamports);
                }
                Err(i) => {
                    if authors.len() >= self.author_capacity {
                        authors_dropped = authors_dropped.saturating_add(1);
                        continue;
                    }
                    authors.insert(
                        i,
                        AuthorMass {
                            author_id: m.author_id,
                            n_markouts: 1,
                            weight_units: w,
                            weighted_net_lamports: i128::from(w) * m.realized_net_lamports,
                            realized_net_sum_lamports: m.realized_net_lamports,
                        },
                    );
                }
            }
        }

        let prior = if pop_n < self.params.population_min_sample || pop_weight == 0 {
            PopulationPrior {
                mean_net_lamports: TRUST_NEUTRAL_PRIOR_NET_LAMPORTS,
                weight_units: pop_weight,
                n_markouts: pop_n,
                pooled: false,
            }
        } else {
            let raw = pop_weighted_net / i128::from(pop_weight);
            let capped = if raw > self.params.prior_positive_cap_lamports {
                self.params.prior_positive_cap_lamports
            } else {
                raw
            };
            PopulationPrior {
                mean_net_lamports: capped,
                weight_units: pop_weight,
                n_markouts: pop_n,
                pooled: true,
            }
        };

        TrustSnapshot {
            as_of_ns,
            prior,
            authors,
            capacity: self.author_capacity,
            authors_dropped,
        }
    }

    /// Score one author against a prebuilt snapshot (constitution 46 fail-closed).
    #[must_use]
    pub fn trust_from_snapshot(&self, snap: &TrustSnapshot, author_id: u64) -> TrustVerdict {
        let Some(mass) = snap.author_mass(author_id) else {
            if snap.authors_dropped > 0 && snap.authors.len() >= snap.capacity {
                return TrustVerdict::Unknown(TrustUnknown::NotInSnapshot {
                    authors_dropped: snap.authors_dropped,
                });
            }
            return TrustVerdict::Unknown(TrustUnknown::NoHistory {
                min_sample: self.params.min_sample,
            });
        };
        self.score_mass(&mass, &snap.prior)
    }

    /// Score one author directly. Convenience wrapper over
    /// [`SocialTrust::snapshot`]; prefer the snapshot form when scoring many
    /// authors at the same instant.
    #[must_use]
    pub fn author_trust(
        &self,
        social: &SocialRecallIndex,
        author_id: u64,
        as_of_ns: u64,
    ) -> TrustVerdict {
        let snap = self.snapshot(social, as_of_ns);
        self.trust_from_snapshot(&snap, author_id)
    }

    /// Score a list of authors against one snapshot, returned in the caller's order
    /// with duplicates preserved (the caller owns the ordering contract).
    #[must_use]
    pub fn rank_from_snapshot(
        &self,
        snap: &TrustSnapshot,
        authors: &[u64],
    ) -> Vec<(u64, TrustVerdict)> {
        authors
            .iter()
            .map(|id| (*id, self.trust_from_snapshot(snap, *id)))
            .collect()
    }

    /// Decayed weight of one markout as of `as_of_ns`.
    fn markout_weight(&self, m: &CallMarkout, as_of_ns: u64) -> u64 {
        let age = as_of_ns.saturating_sub(m.info_time_ns);
        decay_weight_units(age, self.params.half_life_ns)
    }

    /// The integer partial-pooling core. See the module docs for the algebra.
    fn score_mass(&self, mass: &AuthorMass, prior: &PopulationPrior) -> TrustVerdict {
        if mass.n_markouts == 0 {
            return TrustVerdict::Unknown(TrustUnknown::NoHistory {
                min_sample: self.params.min_sample,
            });
        }
        if mass.n_markouts < self.params.min_sample {
            return TrustVerdict::Unknown(TrustUnknown::InsufficientSample {
                n_markouts: mass.n_markouts,
                min_sample: self.params.min_sample,
            });
        }
        if mass.weight_units < self.params.min_effective_weight_units {
            return TrustVerdict::Unknown(TrustUnknown::StaleEvidence {
                n_markouts: mass.n_markouts,
                effective_weight_units: mass.weight_units,
                min_effective_weight_units: self.params.min_effective_weight_units,
            });
        }

        // Raw decay-weighted mean, truncating toward zero (constitution 22: the
        // rounding rule is stated, not implied).
        let raw_mean = mass.weighted_net_lamports / i128::from(mass.weight_units);

        // Precision-weighted blend of the author's own evidence and the prior.
        let prior_mass =
            i128::from(self.params.prior_pseudo_samples) * i128::from(TRUST_WEIGHT_UNIT);
        let num = mass
            .weighted_net_lamports
            .saturating_add(prior.mean_net_lamports.saturating_mul(prior_mass));
        let den = i128::from(mass.weight_units).saturating_add(prior_mass);
        let shrunk = if den == 0 { 0 } else { num / den };

        let pre_demotion_score_bp = score_bp_of(shrunk, self.params.reference_net_lamports);
        let exposure = self.exposure_of(mass.author_id);
        let trust_score_bp = apply_demotion(pre_demotion_score_bp, exposure);

        let mut tier = TrustTier::from_score_bp(trust_score_bp);
        if exposure == SourceExposure::PublicBurned && tier == TrustTier::Trusted {
            // Constitution 28: a source everyone reads cannot be sized on, however
            // good its realized record looks.
            tier = TrustTier::Watch;
        }

        TrustVerdict::Known(TrustScore {
            author_id: mass.author_id,
            n_markouts: mass.n_markouts,
            effective_weight_units: mass.weight_units,
            realized_net_sum_lamports: mass.realized_net_sum_lamports,
            raw_mean_net_lamports: raw_mean,
            prior_mean_net_lamports: prior.mean_net_lamports,
            shrunk_mean_net_lamports: shrunk,
            pre_demotion_score_bp,
            trust_score_bp,
            exposure,
            tier,
        })
    }
}

/// Express a per-call net against the reference net, in basis points clamped to
/// `±10_000` (constitution 22 fixed-point, 102 named scale).
#[must_use]
pub fn score_bp_of(net_lamports: i128, reference_net_lamports: i128) -> i32 {
    if reference_net_lamports <= 0 {
        return 0;
    }
    let full = i128::from(BPS_SCALE_U32);
    let raw = net_lamports.saturating_mul(full) / reference_net_lamports;
    if raw > full {
        BPS_SCALE_U32 as i32
    } else if raw < -full {
        -(BPS_SCALE_U32 as i32)
    } else {
        raw as i32
    }
}

/// Apply the constitution-28 exposure demotion.
///
/// Removes a documented fraction of a **positive** score and leaves a negative one
/// untouched: being crowded is not a defence against having lost us money.
#[must_use]
pub fn apply_demotion(score_bp: i32, exposure: SourceExposure) -> i32 {
    if score_bp <= 0 {
        return score_bp;
    }
    let keep = i64::from(BPS_SCALE_U32) - i64::from(exposure.demotion_bp());
    let keep = keep.max(0);
    ((i64::from(score_bp) * keep) / i64::from(BPS_SCALE_U32)) as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::social_recall::{CallRecord, Platform};

    const DAY: u64 = 86_400 * 1_000_000_000;

    fn markout(call_id: u64, author: u64, net: i128, t: u64) -> CallMarkout {
        CallMarkout {
            call_id,
            author_id: author,
            realized_net_lamports: net,
            hold_duration_ns: 60 * 1_000_000_000,
            info_time_ns: t,
        }
    }

    fn call(id: u64, mint: u64, author: u64, t: u64, followers_decade: i32) -> CallRecord {
        CallRecord {
            call_id: id,
            mint_id: mint,
            author_id: author,
            platform: Platform::X,
            info_time_ns: t,
            followers_decade,
            was_designated: false,
        }
    }

    /// `n` markouts of `net` for `author`, all stamped at `t`.
    fn seed(idx: &mut SocialRecallIndex, author: u64, n: u32, net: i128, t: u64, base_id: u64) {
        for i in 0..u64::from(n) {
            idx.record_markout(markout(base_id + i, author, net, t))
                .expect("monotone");
        }
    }

    fn params_neutral_prior() -> TrustParams {
        // Force the neutral prior so shrinkage arithmetic is hand-checkable.
        TrustParams {
            population_min_sample: u32::MAX,
            ..TrustParams::default()
        }
    }

    // ------------------------------------------------------------------ decay

    #[test]
    fn decay_is_exact_at_zero_and_at_each_half_life() {
        let hl = TRUST_HALF_LIFE_NS;
        assert_eq!(decay_weight_units(0, hl), TRUST_WEIGHT_UNIT);
        assert_eq!(decay_weight_units(hl, hl), TRUST_WEIGHT_UNIT / 2);
        assert_eq!(decay_weight_units(2 * hl, hl), TRUST_WEIGHT_UNIT / 4);
        assert_eq!(decay_weight_units(3 * hl, hl), TRUST_WEIGHT_UNIT / 8);
        assert_eq!(
            decay_weight_units(TRUST_DECAY_MAX_HALVINGS * hl, hl),
            0,
            "evidence must reach exactly zero, not an epsilon"
        );
    }

    #[test]
    fn decay_is_monotone_non_increasing() {
        let hl = 1_000_000u64;
        let mut prev = u64::MAX;
        for step in 0..4_000u64 {
            let w = decay_weight_units(step * (hl / 97), hl);
            assert!(w <= prev, "decay rose at step {step}: {prev} -> {w}");
            prev = w;
        }
        assert_eq!(prev, 0);
    }

    #[test]
    fn decay_with_zero_half_life_is_memoryless() {
        assert_eq!(decay_weight_units(0, 0), TRUST_WEIGHT_UNIT);
        assert_eq!(decay_weight_units(1, 0), 0);
    }

    #[test]
    fn decay_is_deterministic() {
        let a = decay_weight_units(7 * DAY + 12_345, TRUST_HALF_LIFE_NS);
        for _ in 0..64 {
            assert_eq!(decay_weight_units(7 * DAY + 12_345, TRUST_HALF_LIFE_NS), a);
        }
    }

    // ----------------------------------------------------------- fail-closed

    #[test]
    fn trust_is_unknown_with_no_history_and_exposes_no_number() {
        let idx = SocialRecallIndex::with_capacity(64, 64);
        let t = SocialTrust::new();
        let v = t.author_trust(&idx, 42, 1_000);
        assert_eq!(
            v,
            TrustVerdict::Unknown(TrustUnknown::NoHistory {
                min_sample: TRUST_MIN_SAMPLE
            })
        );
        assert!(v.score().is_none(), "Unknown must expose no score");
        assert!(!v.is_known());
        assert_eq!(v.tier(), TrustTier::Unproven);
    }

    #[test]
    fn two_lucky_calls_do_not_mint_a_trusted_source() {
        let mut idx = SocialRecallIndex::with_capacity(64, 64);
        // Two enormous winners — the classic small-n trap.
        seed(&mut idx, 7, 2, 5_000_000_000, 10 * DAY, 1);
        let t = SocialTrust::new();
        let v = t.author_trust(&idx, 7, 10 * DAY);
        assert_eq!(
            v,
            TrustVerdict::Unknown(TrustUnknown::InsufficientSample {
                n_markouts: 2,
                min_sample: TRUST_MIN_SAMPLE
            })
        );
        assert!(v.score().is_none());
        assert_eq!(v.tier(), TrustTier::Unproven);
    }

    #[test]
    fn trust_appears_exactly_at_the_sample_floor() {
        let mut idx = SocialRecallIndex::with_capacity(64, 64);
        seed(&mut idx, 7, 7, 60_000_000, 10 * DAY, 1);
        let t = SocialTrust::with_params(params_neutral_prior());
        assert!(!t.author_trust(&idx, 7, 10 * DAY).is_known());
        seed(&mut idx, 7, 1, 60_000_000, 10 * DAY, 100);
        let v = t.author_trust(&idx, 7, 10 * DAY);
        let s = v
            .score()
            .copied()
            .expect("eighth markout reaches the floor");
        assert_eq!(s.n_markouts, 8);
    }

    #[test]
    fn a_long_but_stale_record_fails_closed_rather_than_speaking() {
        let mut idx = SocialRecallIndex::with_capacity(256, 256);
        // Twenty markouts, all a year old at a fourteen-day half-life.
        seed(&mut idx, 9, 20, 80_000_000, 1_000, 1);
        let t = SocialTrust::new();
        let v = t.author_trust(&idx, 9, 1_000 + 365 * DAY);
        match v {
            TrustVerdict::Unknown(TrustUnknown::StaleEvidence {
                n_markouts,
                effective_weight_units,
                min_effective_weight_units,
            }) => {
                assert_eq!(n_markouts, 20);
                assert!(effective_weight_units < min_effective_weight_units);
            }
            other => panic!("expected StaleEvidence, got {other:?}"),
        }
        assert!(v.score().is_none());
    }

    #[test]
    fn every_unknown_variant_exposes_no_score() {
        let unknowns = [
            TrustUnknown::NoHistory { min_sample: 8 },
            TrustUnknown::InsufficientSample {
                n_markouts: 3,
                min_sample: 8,
            },
            TrustUnknown::StaleEvidence {
                n_markouts: 20,
                effective_weight_units: 10,
                min_effective_weight_units: 1_000,
            },
            TrustUnknown::NotInSnapshot {
                authors_dropped: 12,
            },
        ];
        for u in unknowns {
            let v = TrustVerdict::Unknown(u);
            assert!(v.score().is_none());
            assert_eq!(v.tier(), TrustTier::Unproven);
            assert_eq!(v.unknown_reason(), Some(u));
        }
    }

    // ------------------------------------------------------------- shrinkage

    #[test]
    fn shrinkage_pulls_a_thin_sample_toward_the_prior() {
        let mut idx = SocialRecallIndex::with_capacity(256, 256);
        let t0 = 100 * DAY;
        // Exactly eight fresh markouts at 1 SOL of attributed net each.
        seed(&mut idx, 5, 8, 1_000_000_000, t0, 1);
        let t = SocialTrust::with_params(params_neutral_prior());
        let s = *t.author_trust(&idx, 5, t0).score().expect("known");

        assert_eq!(s.raw_mean_net_lamports, 1_000_000_000);
        assert_eq!(s.prior_mean_net_lamports, 0);
        // 8 fresh units of own mass against 12 pseudo-samples of prior mass.
        let expect = 1_000_000_000i128 * 8 / (8 + i128::from(TRUST_PRIOR_PSEUDO_SAMPLES));
        assert_eq!(s.shrunk_mean_net_lamports, expect);
        assert!(
            s.shrunk_mean_net_lamports < s.raw_mean_net_lamports,
            "a thin sample must be pulled toward the prior"
        );
    }

    #[test]
    fn shrinkage_weakens_as_evidence_mass_grows() {
        let t0 = 100 * DAY;
        let t = SocialTrust::with_params(params_neutral_prior());
        let mut ratios: Vec<i128> = Vec::new();
        for n in [8u32, 32, 128, 512] {
            let mut idx = SocialRecallIndex::with_capacity(4_096, 4_096);
            seed(&mut idx, 5, n, 1_000_000_000, t0, 1);
            let s = *t.author_trust(&idx, 5, t0).score().expect("known");
            ratios.push(s.shrunk_mean_net_lamports);
        }
        for w in ratios.windows(2) {
            assert!(
                w[1] > w[0],
                "more evidence must move the posterior toward the record: {w:?}"
            );
        }
        // With 512 samples the prior is nearly irrelevant.
        assert!(*ratios.last().expect("non-empty") > 950_000_000);
    }

    #[test]
    fn decay_pulls_an_aging_record_back_toward_the_prior() {
        let mut idx = SocialRecallIndex::with_capacity(256, 256);
        let t0 = 100 * DAY;
        // Net per call chosen below the reference so the score is not clamped and
        // the decay is visible in basis points as well as in lamports.
        seed(&mut idx, 5, 40, 30_000_000, t0, 1);
        let t = SocialTrust::with_params(params_neutral_prior());
        let fresh = *t.author_trust(&idx, 5, t0).score().expect("known");
        let aged = *t
            .author_trust(&idx, 5, t0 + 2 * TRUST_HALF_LIFE_NS)
            .score()
            .expect("still above the mass floor");
        assert_eq!(fresh.n_markouts, aged.n_markouts);
        assert!(
            aged.effective_weight_units < fresh.effective_weight_units,
            "evidence mass must decay"
        );
        assert!(
            aged.shrunk_mean_net_lamports < fresh.shrunk_mean_net_lamports,
            "an aging record must decay toward the prior: {} -> {}",
            fresh.shrunk_mean_net_lamports,
            aged.shrunk_mean_net_lamports
        );
        assert!(aged.trust_score_bp < fresh.trust_score_bp);
    }

    #[test]
    fn the_prior_is_neutral_below_the_population_floor() {
        let mut idx = SocialRecallIndex::with_capacity(256, 256);
        seed(&mut idx, 5, 10, 1_000_000_000, 100 * DAY, 1);
        let t = SocialTrust::new();
        let snap = t.snapshot(&idx, 100 * DAY);
        assert!(!snap.prior().pooled);
        assert_eq!(
            snap.prior().mean_net_lamports,
            TRUST_NEUTRAL_PRIOR_NET_LAMPORTS
        );
    }

    #[test]
    fn the_pooled_prior_cannot_lift_beyond_half_of_reference() {
        let mut idx = SocialRecallIndex::with_capacity(1_024, 1_024);
        // A wildly lucky population: every caller printing 10 SOL a call.
        for a in 0..10u64 {
            seed(&mut idx, a, 10, 10_000_000_000, 100 * DAY, a * 100 + 1);
        }
        let t = SocialTrust::new();
        let snap = t.snapshot(&idx, 100 * DAY);
        assert!(snap.prior().pooled);
        assert_eq!(
            snap.prior().mean_net_lamports,
            TRUST_PRIOR_POSITIVE_CAP_LAMPORTS,
            "the prior's positive side must be capped"
        );
    }

    #[test]
    fn the_pooled_prior_drags_a_negative_population_down_without_limit() {
        let mut idx = SocialRecallIndex::with_capacity(1_024, 1_024);
        for a in 0..10u64 {
            seed(&mut idx, a, 10, -10_000_000_000, 100 * DAY, a * 100 + 1);
        }
        let t = SocialTrust::new();
        let snap = t.snapshot(&idx, 100 * DAY);
        assert!(snap.prior().pooled);
        assert_eq!(snap.prior().mean_net_lamports, -10_000_000_000);
    }

    // ------------------------------------------------- popularity is not trust

    #[test]
    fn popularity_cannot_buy_trust() {
        let mut idx = SocialRecallIndex::with_capacity(4_096, 4_096);
        // A "huge following" author: ten thousand calls, a nine-decade follower
        // count, on the designated list — and not one attributed markout.
        for i in 1..=2_000u64 {
            let mut c = call(i, i, 999, i * 1_000, 9);
            c.was_designated = true;
            idx.record_call(c).expect("monotone");
        }
        let t = SocialTrust::new();
        let v = t.author_trust(&idx, 999, 10_000_000);
        assert!(
            v.score().is_none(),
            "a purchasable audience must buy exactly zero trust"
        );
        assert_eq!(v.tier(), TrustTier::Unproven);
    }

    #[test]
    fn follower_explosion_does_not_move_a_single_basis_point() {
        let t0 = 100 * DAY;
        let mut small = SocialRecallIndex::with_capacity(4_096, 4_096);
        let mut huge = SocialRecallIndex::with_capacity(4_096, 4_096);
        for i in 1..=40u64 {
            small.record_call(call(i, i, 3, i * 1_000, 1)).expect("ok");
            let mut c = call(i, i, 3, i * 1_000, 9);
            c.was_designated = true;
            huge.record_call(c).expect("ok");
        }
        seed(&mut small, 3, 20, 30_000_000, t0, 10_000);
        seed(&mut huge, 3, 20, 30_000_000, t0, 10_000);

        let t = SocialTrust::new();
        let a = t.author_trust(&small, 3, t0);
        let b = t.author_trust(&huge, 3, t0);
        assert_eq!(
            a, b,
            "identical realized records must score identically regardless of audience"
        );
    }

    #[test]
    fn a_losing_source_with_a_massive_audience_is_demoted() {
        let mut idx = SocialRecallIndex::with_capacity(4_096, 4_096);
        let t0 = 100 * DAY;
        for i in 1..=50u64 {
            let mut c = call(i, i, 4, i * 1_000, 9);
            c.was_designated = true;
            idx.record_call(c).expect("ok");
        }
        seed(&mut idx, 4, 20, -40_000_000, t0, 10_000);
        let t = SocialTrust::with_params(params_neutral_prior());
        let s = *t.author_trust(&idx, 4, t0).score().expect("known");
        assert_eq!(s.tier, TrustTier::Demoted);
        assert!(s.trust_score_bp < TRUST_DEMOTED_MAX_BP);
    }

    // --------------------------------------------------------- exposure (§28)

    #[test]
    fn public_burned_demotes_and_caps_the_tier_at_watch() {
        let mut idx = SocialRecallIndex::with_capacity(1_024, 1_024);
        let t0 = 100 * DAY;
        seed(&mut idx, 11, 200, 200_000_000, t0, 1);
        let mut t = SocialTrust::with_params(params_neutral_prior());

        let clean = *t.author_trust(&idx, 11, t0).score().expect("known");
        assert_eq!(clean.tier, TrustTier::Trusted);
        assert_eq!(clean.exposure, SourceExposure::Niche);
        assert_eq!(clean.trust_score_bp, clean.pre_demotion_score_bp);

        t.set_exposure(11, SourceExposure::PublicBurned)
            .expect("capacity");
        let burned = *t.author_trust(&idx, 11, t0).score().expect("known");
        assert_eq!(burned.pre_demotion_score_bp, clean.pre_demotion_score_bp);
        assert!(burned.trust_score_bp < clean.trust_score_bp);
        assert_eq!(
            burned.tier,
            TrustTier::Watch,
            "a source everyone reads is never sizable (constitution 28)"
        );
        assert!(!burned.tier.is_sizable());
    }

    #[test]
    fn crowded_demotes_less_than_public_burned() {
        let mut idx = SocialRecallIndex::with_capacity(1_024, 1_024);
        let t0 = 100 * DAY;
        seed(&mut idx, 12, 200, 200_000_000, t0, 1);
        let mut t = SocialTrust::with_params(params_neutral_prior());
        let niche = *t.author_trust(&idx, 12, t0).score().expect("known");
        t.set_exposure(12, SourceExposure::Crowded).expect("cap");
        let crowded = *t.author_trust(&idx, 12, t0).score().expect("known");
        t.set_exposure(12, SourceExposure::PublicBurned)
            .expect("cap");
        let burned = *t.author_trust(&idx, 12, t0).score().expect("known");
        assert!(niche.trust_score_bp > crowded.trust_score_bp);
        assert!(crowded.trust_score_bp > burned.trust_score_bp);
    }

    #[test]
    fn demotion_never_rehabilitates_a_negative_score() {
        for e in [
            SourceExposure::Niche,
            SourceExposure::Crowded,
            SourceExposure::PublicBurned,
        ] {
            assert_eq!(apply_demotion(-4_000, e), -4_000);
        }
        assert_eq!(apply_demotion(0, SourceExposure::PublicBurned), 0);
    }

    #[test]
    fn exposure_registry_refuses_rather_than_evicting_a_demotion() {
        let mut t = SocialTrust::with_capacity(TrustParams::default(), 2, 16);
        t.set_exposure(1, SourceExposure::PublicBurned)
            .expect("first");
        t.set_exposure(2, SourceExposure::Crowded).expect("second");
        let err = t
            .set_exposure(3, SourceExposure::PublicBurned)
            .expect_err("full");
        assert_eq!(
            err,
            TrustError::ExposureCapacityExhausted { capacity: 2 },
            "a full registry must refuse, never silently un-burn a source"
        );
        assert_eq!(t.exposure_of(1), SourceExposure::PublicBurned);
        assert_eq!(t.exposure_of(2), SourceExposure::Crowded);
        assert_eq!(t.exposure_of(3), SourceExposure::Niche);
        assert_eq!(t.exposure_len(), 2);
    }

    #[test]
    fn overwriting_an_exposure_does_not_consume_capacity() {
        let mut t = SocialTrust::with_capacity(TrustParams::default(), 2, 16);
        t.set_exposure(1, SourceExposure::Crowded).expect("first");
        let prev = t
            .set_exposure(1, SourceExposure::PublicBurned)
            .expect("overwrite");
        assert_eq!(prev, Some(SourceExposure::Crowded));
        assert_eq!(t.exposure_len(), 1);
        t.set_exposure(2, SourceExposure::Niche).expect("room left");
    }

    // -------------------------------------------------------------- snapshot

    #[test]
    fn snapshot_excludes_the_future() {
        let mut idx = SocialRecallIndex::with_capacity(256, 256);
        seed(&mut idx, 5, 10, 1_000_000, 1_000, 1);
        seed(&mut idx, 5, 10, 9_000_000_000, 5_000, 100);
        let t = SocialTrust::new();
        let snap = t.snapshot(&idx, 1_000);
        let mass = snap.author_mass(5).expect("present");
        assert_eq!(mass.n_markouts, 10, "replay must not see the future");
        assert_eq!(mass.realized_net_sum_lamports, 10_000_000);
    }

    #[test]
    fn snapshot_authors_are_sorted_and_deterministic() {
        let mut idx = SocialRecallIndex::with_capacity(1_024, 1_024);
        for (i, a) in [9u64, 3, 7, 1, 5].iter().enumerate() {
            seed(&mut idx, *a, 4, 1_000_000, 1_000, i as u64 * 100 + 1);
        }
        let t = SocialTrust::new();
        let snap = t.snapshot(&idx, 10_000);
        let ids: Vec<u64> = snap.authors().iter().map(|a| a.author_id).collect();
        assert_eq!(ids, vec![1, 3, 5, 7, 9]);
        for _ in 0..16 {
            assert_eq!(t.snapshot(&idx, 10_000), snap);
        }
    }

    #[test]
    fn snapshot_author_table_is_bounded_and_overflow_fails_closed() {
        let mut idx = SocialRecallIndex::with_capacity(4_096, 4_096);
        for a in 0..40u64 {
            seed(&mut idx, a, 10, 500_000_000, 100 * DAY, a * 100 + 1);
        }
        let t = SocialTrust::with_capacity(TrustParams::default(), 16, 4);
        let snap = t.snapshot(&idx, 100 * DAY);
        assert_eq!(snap.authors().len(), 4);
        assert!(snap.authors_dropped() > 0);
        // Author 0 fitted; author 39 did not, and must not be guessed at.
        assert!(t.trust_from_snapshot(&snap, 0).is_known());
        let v = t.trust_from_snapshot(&snap, 39);
        assert!(v.score().is_none());
        assert!(matches!(
            v.unknown_reason(),
            Some(TrustUnknown::NotInSnapshot { .. })
        ));
    }

    #[test]
    fn author_trust_is_deterministic_across_repeated_calls() {
        let mut idx = SocialRecallIndex::with_capacity(1_024, 1_024);
        let t0 = 100 * DAY;
        for i in 0..30u64 {
            idx.record_markout(markout(
                i + 1,
                6,
                (i as i128 - 15) * 20_000_000,
                t0 - (30 - i) * 3_600_000_000_000,
            ))
            .expect("monotone");
        }
        let t = SocialTrust::new();
        let first = t.author_trust(&idx, 6, t0);
        for _ in 0..32 {
            assert_eq!(t.author_trust(&idx, 6, t0), first);
        }
    }

    #[test]
    fn rank_from_snapshot_preserves_caller_order() {
        let mut idx = SocialRecallIndex::with_capacity(1_024, 1_024);
        seed(&mut idx, 2, 10, 100_000_000, 100 * DAY, 1);
        seed(&mut idx, 8, 10, -100_000_000, 100 * DAY, 100);
        let t = SocialTrust::with_params(params_neutral_prior());
        let snap = t.snapshot(&idx, 100 * DAY);
        let rows = t.rank_from_snapshot(&snap, &[8, 2, 99]);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].0, 8);
        assert_eq!(rows[1].0, 2);
        assert_eq!(rows[2].0, 99);
        assert_eq!(rows[0].1.tier(), TrustTier::Demoted);
        assert_eq!(rows[1].1.tier(), TrustTier::Trusted);
        assert_eq!(rows[2].1.tier(), TrustTier::Unproven);
    }

    // ---------------------------------------------------------- score mapping

    #[test]
    fn score_bp_is_clamped_and_signed() {
        assert_eq!(
            score_bp_of(TRUST_REFERENCE_NET_LAMPORTS, TRUST_REFERENCE_NET_LAMPORTS),
            10_000
        );
        assert_eq!(
            score_bp_of(
                TRUST_REFERENCE_NET_LAMPORTS * 100,
                TRUST_REFERENCE_NET_LAMPORTS
            ),
            10_000
        );
        assert_eq!(
            score_bp_of(
                -TRUST_REFERENCE_NET_LAMPORTS * 100,
                TRUST_REFERENCE_NET_LAMPORTS
            ),
            -10_000
        );
        assert_eq!(score_bp_of(0, TRUST_REFERENCE_NET_LAMPORTS), 0);
        assert_eq!(score_bp_of(1_000, 0), 0);
    }

    #[test]
    fn tier_thresholds_are_the_named_ones() {
        assert_eq!(
            TrustTier::from_score_bp(TRUST_DEMOTED_MAX_BP),
            TrustTier::Demoted
        );
        assert_eq!(
            TrustTier::from_score_bp(TRUST_DEMOTED_MAX_BP + 1),
            TrustTier::Watch
        );
        assert_eq!(
            TrustTier::from_score_bp(TRUST_TRUSTED_MIN_BP - 1),
            TrustTier::Watch
        );
        assert_eq!(
            TrustTier::from_score_bp(TRUST_TRUSTED_MIN_BP),
            TrustTier::Trusted
        );
        assert!(TrustTier::Trusted.is_sizable());
        assert!(!TrustTier::Watch.is_sizable());
        assert!(!TrustTier::Unproven.is_sizable());
        assert!(!TrustTier::Demoted.is_sizable());
    }

    #[test]
    fn enum_ordinals_round_trip() {
        for o in 0u8..3 {
            assert_eq!(
                SourceExposure::from_ordinal(o).expect("in range").ordinal(),
                o
            );
        }
        assert!(SourceExposure::from_ordinal(3).is_none());
        for o in 0u8..4 {
            assert_eq!(TrustTier::from_ordinal(o).expect("in range").ordinal(), o);
        }
        assert!(TrustTier::from_ordinal(4).is_none());
    }
}
