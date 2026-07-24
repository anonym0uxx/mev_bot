//! `follow_reco` — "should I be following someone I am not?" (constitution 22
//! integer-only, 46 small-n, 57/99 bounded, 102 named thresholds, 110 scope).
//!
//! # Scope boundary — read this first
//!
//! This module builds **recommendations only**: a ranked, bounded list of accounts
//! worth *following and monitoring*, and its inverse, a list of followed accounts
//! worth dropping. That is research — deciding what to read.
//!
//! It deliberately contains **no posting, engagement, amplification, promotion or
//! outreach capability of any kind**, and none may be added here. Constitution
//! criterion 110 forbids purchasing or instigating promotion for tokens we hold,
//! trade, or research; auto-posting, reply-farming, paid shilling or coordinated
//! amplification around a position is promotional market manipulation regardless of
//! how it is framed internally, and the fact that this module happens to know which
//! accounts have reach is exactly why the boundary is written here rather than
//! assumed. Reading the market is not the same act as talking to it. This module
//! only ever reads.
//!
//! # What is actually measured
//!
//! The question "is this account worth following?" has one honest answer available
//! to us: **did their call arrive before a setup we later made money on, and how
//! much before?** Everything else — their follower count, their claimed hit rate,
//! how often people quote them — is either purchasable or unverifiable.
//!
//! So for every admitted, decisive episode in our own [`EpisodicIndex`], this
//! module looks back for calls on the same mint that landed *before* our decision,
//! and attributes:
//!
//! ```text
//! attributed_i = realized_net_i * lead_weight_units(lead_i) / LEAD_WEIGHT_UNIT
//! ```
//!
//! ## Lead-time weighting is the whole point
//!
//! [`lead_weight_units`] is a trapezoid over lead time, and each of its four
//! segments encodes a separate market fact:
//!
//! * `lead < FOLLOW_MIN_LEAD_NS` → weight **0**. They called it at or after our
//!   entry. That is not a signal, that is a witness. Worthless.
//! * `MIN_LEAD .. FULL_LEAD` → linear ramp. Partially actionable: some of the move
//!   was already gone.
//! * `FULL_LEAD .. STALE_LEAD` → full weight. This is the band where following them
//!   would actually have changed our fill.
//! * `STALE_LEAD .. LOOKBACK` → linear decay to **0**. A call eleven hours before we
//!   traded did not cause the trade; crediting it would let a firehose account that
//!   mentions every mint on the chain harvest attribution from everything that ever
//!   worked. Beyond [`FollowParams::lookback_ns`] nothing is attributed at all.
//!
//! An author calling the same mint repeatedly before one episode is credited
//! **once**, on their earliest qualifying call (their best lead). Spamming a mint
//! cannot inflate attribution.
//!
//! # Fail-closed (constitution 46)
//!
//! [`FollowRecoVerdict`] mirrors [`crate::recall::RecallVerdict`]:
//! `Known(Vec<FollowRecommendation>)` or `Unknown(FollowRecoUnknown)` carrying
//! **counts and floors only**. An author below
//! [`FollowParams::min_attributed_calls`] is not returned with a small number next
//! to their name — they are not returned at all. Two well-timed calls is luck, and
//! the whole failure mode of "who should I follow" is promoting someone off a
//! two-call sample.
//!
//! A [`TrustTier::Demoted`] author is never recommended, whatever their attribution
//! arithmetic says: we already have decisive evidence that following them costs
//! money, and that evidence is drawn from a strictly larger sample than this
//! module's.
//!
//! # Determinism and bounds
//!
//! No wall clock, no RNG, no floats. Candidates rank by a documented **total**
//! order — attributed net descending, then attributed calls descending, then median
//! lead descending, then author id ascending — so ties never depend on iteration
//! order. Output is capped at [`FollowParams::max_recommendations`], the candidate
//! table at [`FOLLOW_AUTHOR_CAP`], the per-author lead sample at
//! [`FOLLOW_LEAD_SAMPLE_CAP`], and the follow set at [`FOLLOW_SET_CAP`].

use crate::episode::Episode;
use crate::fingerprint::VenuePhase;
use crate::recall::{order_stat_u64, EpisodicIndex, BPS_SCALE_U32, P50};
use crate::social_recall::{Platform, SocialRecallIndex};
use crate::trust::{SocialTrust, TrustSnapshot, TrustTier};

// ---------------------------------------------------------------------------
// Named constants (constitution 102)
// ---------------------------------------------------------------------------

/// Full lead-time weight (constitution 102). Weights are integer fractions of this.
pub const LEAD_WEIGHT_UNIT: u64 = 64;

/// Oldest lead that is attributed at all (constitution 102): 24 hours. Beyond this
/// a call did not cause the trade, it merely preceded it.
pub const FOLLOW_LOOKBACK_NS: u64 = 86_400 * 1_000_000_000;

/// Shortest lead that earns any weight (constitution 102): five seconds. Below
/// this the caller was not ahead of us in any way we could have acted on.
pub const FOLLOW_MIN_LEAD_NS: u64 = 5 * 1_000_000_000;

/// Lead at which weight reaches [`LEAD_WEIGHT_UNIT`] (constitution 102): ten
/// minutes — enough time to have read them, sized, and got a fill.
pub const FOLLOW_FULL_LEAD_NS: u64 = 600 * 1_000_000_000;

/// Lead beyond which weight begins decaying back to zero (constitution 102):
/// six hours. Past here the call is not a trigger, it is trivia.
pub const FOLLOW_STALE_LEAD_NS: u64 = 6 * 3_600 * 1_000_000_000;

/// Minimum attributed calls before an author may be recommended
/// (constitution 46 small-n guard).
pub const FOLLOW_MIN_ATTRIBUTED_CALLS: u32 = 5;

/// Attributed calls at which confidence saturates (constitution 102).
pub const FOLLOW_CONFIDENCE_SATURATION_CALLS: u32 = 20;

/// Confidence multiplier for a [`TrustTier::Trusted`] candidate (constitution 102).
pub const CONFIDENCE_TRUSTED_BP: u32 = 10_000;

/// Confidence multiplier for a [`TrustTier::Watch`] candidate (constitution 102).
pub const CONFIDENCE_WATCH_BP: u32 = 7_000;

/// Confidence multiplier for a candidate with no trust record at all
/// (constitution 102). Their lead-time attribution is real but uncorroborated.
pub const CONFIDENCE_UNPROVEN_BP: u32 = 3_000;

/// Attributed net at or below which a **followed** author becomes an unfollow
/// candidate (constitution 102). Strictly-negative attribution only.
pub const UNFOLLOW_MAX_NET_LAMPORTS: i128 = 0;

/// Cap on returned recommendations (constitution 57 bounded output).
pub const FOLLOW_RECO_CAP: usize = 16;

/// Capacity of the candidate table (constitution 57/99).
pub const FOLLOW_AUTHOR_CAP: usize = 1_024;

/// Per-author cap on retained lead samples (constitution 57/99). The median lead is
/// taken over the first this-many attributed leads, oldest episode first.
pub const FOLLOW_LEAD_SAMPLE_CAP: usize = 256;

/// Per-episode cap on distinct attributed authors (constitution 57/99). A mint that
/// more than this many tracked accounts called is a raid, not a signal.
pub const FOLLOW_EPISODE_AUTHOR_CAP: usize = 64;

/// Capacity of a [`FollowSet`] (constitution 57/99).
pub const FOLLOW_SET_CAP: usize = 512;

// ---------------------------------------------------------------------------
// Follow set
// ---------------------------------------------------------------------------

/// Why a follow-set mutation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowSetError {
    /// The bounded set is full. Nothing was evicted: silently dropping a followed
    /// author would make them reappear as a "new" recommendation, which is exactly
    /// the loop this module exists to avoid.
    CapacityExhausted {
        /// The capacity that was reached.
        capacity: usize,
    },
}

/// The bounded set of authors we already follow (constitution 57/99).
///
/// Held sorted so membership is a binary search and every derived listing is
/// deterministic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowSet {
    ids: Vec<u64>,
    capacity: usize,
}

impl Default for FollowSet {
    fn default() -> Self {
        Self::new()
    }
}

impl FollowSet {
    /// An empty set at [`FOLLOW_SET_CAP`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(FOLLOW_SET_CAP)
    }

    /// An empty set with an explicit capacity (clamped to at least 1).
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            ids: Vec::new(),
            capacity: capacity.max(1),
        }
    }

    /// Build from a slice; duplicates collapse, overflow is refused.
    pub fn from_ids(ids: &[u64], capacity: usize) -> Result<Self, FollowSetError> {
        let mut set = Self::with_capacity(capacity);
        for id in ids {
            set.follow(*id)?;
        }
        Ok(set)
    }

    /// Hard capacity.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Number of followed authors.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// `true` when nobody is followed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Followed author ids, ascending.
    #[must_use]
    pub fn ids(&self) -> &[u64] {
        &self.ids
    }

    /// `true` when this author is already followed.
    #[must_use]
    pub fn contains(&self, author_id: u64) -> bool {
        self.ids.binary_search(&author_id).is_ok()
    }

    /// Add an author. Returns `true` if they were newly added.
    pub fn follow(&mut self, author_id: u64) -> Result<bool, FollowSetError> {
        match self.ids.binary_search(&author_id) {
            Ok(_) => Ok(false),
            Err(i) => {
                if self.ids.len() >= self.capacity {
                    return Err(FollowSetError::CapacityExhausted {
                        capacity: self.capacity,
                    });
                }
                self.ids.insert(i, author_id);
                Ok(true)
            }
        }
    }

    /// Remove an author. Returns `true` if they were present.
    pub fn unfollow(&mut self, author_id: u64) -> bool {
        match self.ids.binary_search(&author_id) {
            Ok(i) => {
                self.ids.remove(i);
                true
            }
            Err(_) => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Params
// ---------------------------------------------------------------------------

/// Tunables for follow recommendation. Defaults are named consts (constitution 102).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FollowParams {
    /// Oldest lead attributed at all.
    pub lookback_ns: u64,
    /// Shortest lead earning any weight.
    pub min_lead_ns: u64,
    /// Lead at which weight is full.
    pub full_lead_ns: u64,
    /// Lead beyond which weight decays back to zero.
    pub stale_lead_ns: u64,
    /// Minimum attributed calls before an author may be surfaced (constitution 46).
    pub min_attributed_calls: u32,
    /// Cap on returned rows.
    pub max_recommendations: usize,
    /// Cap on the candidate table.
    pub author_cap: usize,
    /// Optional venue-phase restriction on the episodes that may be attributed
    /// (constitution 100). `None` attributes realized money from both phases, which
    /// is sound here because this is a P&L attribution, not a conditional estimate.
    pub venue_phase: Option<VenuePhase>,
}

impl Default for FollowParams {
    fn default() -> Self {
        Self {
            lookback_ns: FOLLOW_LOOKBACK_NS,
            min_lead_ns: FOLLOW_MIN_LEAD_NS,
            full_lead_ns: FOLLOW_FULL_LEAD_NS,
            stale_lead_ns: FOLLOW_STALE_LEAD_NS,
            min_attributed_calls: FOLLOW_MIN_ATTRIBUTED_CALLS,
            max_recommendations: FOLLOW_RECO_CAP,
            author_cap: FOLLOW_AUTHOR_CAP,
            venue_phase: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// An account worth following and monitoring — **not** an account to interact with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FollowRecommendation {
    /// The author.
    pub author_id: u64,
    /// The platform carrying most of their attributed calls; ties resolve to the
    /// lowest [`Platform::ordinal`].
    pub platform: Platform,
    /// Attributed calls backing the row.
    pub n_calls: u32,
    /// Lead-time-weighted realized net attributed to them, in lamports.
    pub realized_net_attributed_lamports: i128,
    /// Median lead from their call to our decision, nanoseconds (nearest rank).
    pub median_lead_ns: u64,
    /// Their earned trust tier from [`crate::trust`], for context. Never the reason
    /// they are ranked — the ranking is attribution.
    pub trust_tier: TrustTier,
    /// Confidence in the row, basis points: sample-size credit scaled by trust tier.
    pub confidence_bp: u32,
}

/// A followed account whose attributed contribution has gone negative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnfollowCandidate {
    /// The author.
    pub author_id: u64,
    /// The platform carrying most of their attributed calls.
    pub platform: Platform,
    /// Attributed calls backing the row.
    pub n_calls: u32,
    /// Lead-time-weighted realized net attributed to them — negative by definition
    /// of appearing here.
    pub realized_net_attributed_lamports: i128,
    /// Median lead, nanoseconds.
    pub median_lead_ns: u64,
    /// Their earned trust tier.
    pub trust_tier: TrustTier,
}

/// Why no recommendation could be made.
///
/// **Counts and floors only** — no partial ranking, no provisional scores
/// (constitution 46).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowRecoUnknown {
    /// The episodic index holds no admitted, decisive episode in scope.
    NoDecisiveEpisode {
        /// Episodes examined.
        n_episodes: u32,
    },
    /// Episodes exist, but no call preceded any of them inside the lookback.
    NoAttributedCall {
        /// Decisive episodes examined.
        n_episodes: u32,
        /// The lookback that was applied.
        lookback_ns: u64,
    },
    /// Candidates exist, but none cleared the sample floor or all were already
    /// followed / demoted.
    NoQualifiedCandidate {
        /// Distinct authors with at least one attributed call.
        n_candidates: u32,
        /// The floor they failed to reach.
        min_attributed_calls: u32,
    },
}

/// A ranked follow list, or an explicit refusal to guess (constitution 46).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FollowRecoVerdict {
    /// At least one candidate cleared every gate; ranked best first.
    Known(Vec<FollowRecommendation>),
    /// Nothing cleared the gates. No ranking exists, by construction.
    Unknown(FollowRecoUnknown),
}

impl FollowRecoVerdict {
    /// `true` when a ranking is available.
    #[must_use]
    pub const fn is_known(&self) -> bool {
        matches!(self, Self::Known(_))
    }

    /// The ranking, or `None`. The **only** path to a row.
    #[must_use]
    pub fn recommendations(&self) -> Option<&[FollowRecommendation]> {
        match self {
            Self::Known(v) => Some(v),
            Self::Unknown(_) => None,
        }
    }

    /// Why the engine declined, or `None` if it did not.
    #[must_use]
    pub const fn unknown_reason(&self) -> Option<FollowRecoUnknown> {
        match self {
            Self::Known(_) => None,
            Self::Unknown(u) => Some(*u),
        }
    }
}

// ---------------------------------------------------------------------------
// Lead weighting
// ---------------------------------------------------------------------------

/// Lead-time weight of one call, in [`LEAD_WEIGHT_UNIT`]s.
///
/// The trapezoid described in the module docs: zero below `min_lead`, a linear ramp
/// to full weight at `full_lead`, a plateau to `stale_lead`, then a linear decay to
/// zero at `lookback`, and zero beyond. Monotone non-decreasing on the ramp and
/// monotone non-increasing on the decay, with no discontinuity at any joint.
///
/// Degenerate parameterizations (non-ascending knots) are handled by clamping so the
/// function is total and never divides by zero.
#[must_use]
pub fn lead_weight_units(
    lead_ns: u64,
    min_lead_ns: u64,
    full_lead_ns: u64,
    stale_lead_ns: u64,
    lookback_ns: u64,
) -> u64 {
    let full_lead_ns = full_lead_ns.max(min_lead_ns.saturating_add(1));
    let stale_lead_ns = stale_lead_ns.max(full_lead_ns);
    let lookback_ns = lookback_ns.max(stale_lead_ns.saturating_add(1));

    if lead_ns < min_lead_ns || lead_ns >= lookback_ns {
        return 0;
    }
    if lead_ns < full_lead_ns {
        let span = full_lead_ns - min_lead_ns;
        return (u128::from(LEAD_WEIGHT_UNIT) * u128::from(lead_ns - min_lead_ns) / u128::from(span))
            as u64;
    }
    if lead_ns <= stale_lead_ns {
        return LEAD_WEIGHT_UNIT;
    }
    let span = lookback_ns - stale_lead_ns;
    (u128::from(LEAD_WEIGHT_UNIT) * u128::from(lookback_ns - lead_ns) / u128::from(span)) as u64
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Candidate {
    author_id: u64,
    n_calls: u32,
    attributed_net: i128,
    leads: Vec<u64>,
    platform_counts: [u32; 5],
}

impl Candidate {
    fn dominant_platform(&self) -> Platform {
        let mut best_ord = 0u8;
        let mut best = 0u32;
        for (o, count) in self.platform_counts.iter().enumerate() {
            if *count > best {
                best = *count;
                best_ord = o as u8;
            }
        }
        Platform::from_ordinal(best_ord).unwrap_or(Platform::X)
    }

    fn median_lead_ns(&self) -> u64 {
        let mut v = self.leads.clone();
        v.sort_unstable();
        order_stat_u64(&v, P50)
    }
}

/// Ranks authors by lead-time-weighted realized attribution over our own data.
///
/// Read-only by construction: it borrows the episodic index and the social index
/// and returns rows. See the module docs for the constitution-110 scope boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FollowRecommender {
    params: FollowParams,
}

impl FollowRecommender {
    /// A recommender at the default params.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A recommender with explicit params.
    #[must_use]
    pub const fn with_params(params: FollowParams) -> Self {
        Self { params }
    }

    /// The tunables in force.
    #[must_use]
    pub const fn params(&self) -> &FollowParams {
        &self.params
    }

    /// **Who should I be following that I am not?**
    ///
    /// Ranked by lead-time-weighted realized attribution descending, then attributed
    /// calls descending, then median lead descending, then author id ascending —
    /// a total order. Excludes everyone in `followed`, everyone below the sample
    /// floor, everyone with non-positive attribution, and every
    /// [`TrustTier::Demoted`] source.
    #[must_use]
    pub fn recommend_follows(
        &self,
        index: &EpisodicIndex,
        social: &SocialRecallIndex,
        trust: &SocialTrust,
        snap: &TrustSnapshot,
        followed: &FollowSet,
        as_of_ns: u64,
    ) -> FollowRecoVerdict {
        let (candidates, n_episodes, n_attributed) = self.attribute(index, social, as_of_ns);
        if n_episodes == 0 {
            return FollowRecoVerdict::Unknown(FollowRecoUnknown::NoDecisiveEpisode {
                n_episodes: 0,
            });
        }
        if n_attributed == 0 {
            return FollowRecoVerdict::Unknown(FollowRecoUnknown::NoAttributedCall {
                n_episodes,
                lookback_ns: self.params.lookback_ns,
            });
        }
        let n_candidates = candidates.len() as u32;

        let mut rows: Vec<FollowRecommendation> = Vec::new();
        for c in &candidates {
            if followed.contains(c.author_id) {
                continue;
            }
            if c.n_calls < self.params.min_attributed_calls {
                continue;
            }
            if c.attributed_net <= 0 {
                continue;
            }
            let tier = trust.trust_from_snapshot(snap, c.author_id).tier();
            if tier == TrustTier::Demoted {
                // Already-decisive evidence, over a strictly larger sample, that
                // following this source costs money. Attribution does not overrule it.
                continue;
            }
            rows.push(FollowRecommendation {
                author_id: c.author_id,
                platform: c.dominant_platform(),
                n_calls: c.n_calls,
                realized_net_attributed_lamports: c.attributed_net,
                median_lead_ns: c.median_lead_ns(),
                trust_tier: tier,
                confidence_bp: confidence_bp(c.n_calls, tier),
            });
        }
        if rows.is_empty() {
            return FollowRecoVerdict::Unknown(FollowRecoUnknown::NoQualifiedCandidate {
                n_candidates,
                min_attributed_calls: self.params.min_attributed_calls,
            });
        }
        rows.sort_unstable_by_key(|r| {
            (
                core::cmp::Reverse(r.realized_net_attributed_lamports),
                core::cmp::Reverse(r.n_calls),
                core::cmp::Reverse(r.median_lead_ns),
                r.author_id,
            )
        });
        rows.truncate(self.params.max_recommendations);
        FollowRecoVerdict::Known(rows)
    }

    /// **Who am I following that I should not be?**
    ///
    /// Followed authors whose lead-time-weighted attribution has gone strictly
    /// negative, worst first. Authors below the sample floor never appear: we do not
    /// fire a source on two bad calls any more than we hire one on two good ones.
    #[must_use]
    pub fn unfollow_candidates(
        &self,
        index: &EpisodicIndex,
        social: &SocialRecallIndex,
        trust: &SocialTrust,
        snap: &TrustSnapshot,
        followed: &FollowSet,
        as_of_ns: u64,
    ) -> Vec<UnfollowCandidate> {
        let (candidates, _n_episodes, _n_attributed) = self.attribute(index, social, as_of_ns);
        let mut rows: Vec<UnfollowCandidate> = Vec::new();
        for c in &candidates {
            if !followed.contains(c.author_id) {
                continue;
            }
            if c.n_calls < self.params.min_attributed_calls {
                continue;
            }
            if c.attributed_net >= UNFOLLOW_MAX_NET_LAMPORTS {
                continue;
            }
            rows.push(UnfollowCandidate {
                author_id: c.author_id,
                platform: c.dominant_platform(),
                n_calls: c.n_calls,
                realized_net_attributed_lamports: c.attributed_net,
                median_lead_ns: c.median_lead_ns(),
                trust_tier: trust.trust_from_snapshot(snap, c.author_id).tier(),
            });
        }
        rows.sort_unstable_by_key(|r| {
            (
                r.realized_net_attributed_lamports,
                core::cmp::Reverse(r.n_calls),
                r.author_id,
            )
        });
        rows.truncate(self.params.max_recommendations);
        rows
    }

    /// One attribution pass. Returns
    /// `(candidates ascending by author id, decisive episodes, attributed pairs)`.
    fn attribute(
        &self,
        index: &EpisodicIndex,
        social: &SocialRecallIndex,
        as_of_ns: u64,
    ) -> (Vec<Candidate>, u32, u64) {
        // Calls keyed by (mint, time, call_id) so an episode's lookback window is a
        // contiguous sub-slice found by two binary searches.
        let mut calls: Vec<(u64, u64, u64, u64, u8)> = social
            .iter_calls_oldest_first()
            .filter(|c| c.info_time_ns <= as_of_ns)
            .map(|c| {
                (
                    c.mint_id,
                    c.info_time_ns,
                    c.call_id,
                    c.author_id,
                    c.platform.ordinal(),
                )
            })
            .collect();
        calls.sort_unstable();

        let mut candidates: Vec<Candidate> = Vec::new();
        let mut n_episodes = 0u32;
        let mut n_attributed = 0u64;

        for e in index.iter_oldest_first() {
            if !self.episode_in_scope(e, as_of_ns) {
                continue;
            }
            n_episodes = n_episodes.saturating_add(1);
            let ep_t = e.context().info_time_ns;
            let mint = e.context().mint_id;
            let floor = ep_t.saturating_sub(self.params.lookback_ns);

            let lo = calls.partition_point(|c| (c.0, c.1) < (mint, floor));
            let hi = calls.partition_point(|c| (c.0, c.1) < (mint, ep_t));
            let mut seen: Vec<u64> = Vec::new();

            for c in &calls[lo..hi] {
                if c.0 != mint {
                    continue;
                }
                let author = c.3;
                // Sorted ascending by time, so the first sighting of an author in
                // this window is their earliest call — their best lead. Later calls
                // by the same author on the same episode are ignored: spamming a
                // mint cannot inflate attribution.
                if seen.binary_search(&author).is_ok() {
                    continue;
                }
                if seen.len() >= FOLLOW_EPISODE_AUTHOR_CAP {
                    continue;
                }
                let lead = ep_t.saturating_sub(c.1);
                let w = lead_weight_units(
                    lead,
                    self.params.min_lead_ns,
                    self.params.full_lead_ns,
                    self.params.stale_lead_ns,
                    self.params.lookback_ns,
                );
                if w == 0 {
                    continue;
                }
                if let Err(i) = seen.binary_search(&author) {
                    seen.insert(i, author);
                }
                let attributed = e
                    .outcome()
                    .realized_net_lamports
                    .saturating_mul(i128::from(w))
                    / i128::from(LEAD_WEIGHT_UNIT);
                n_attributed = n_attributed.saturating_add(1);

                match candidates.binary_search_by_key(&author, |x| x.author_id) {
                    Ok(i) => {
                        let cand = &mut candidates[i];
                        cand.n_calls = cand.n_calls.saturating_add(1);
                        cand.attributed_net = cand.attributed_net.saturating_add(attributed);
                        if cand.leads.len() < FOLLOW_LEAD_SAMPLE_CAP {
                            cand.leads.push(lead);
                        }
                        cand.platform_counts[usize::from(c.4).min(4)] += 1;
                    }
                    Err(i) => {
                        if candidates.len() >= self.params.author_cap {
                            continue;
                        }
                        let mut platform_counts = [0u32; 5];
                        platform_counts[usize::from(c.4).min(4)] = 1;
                        candidates.insert(
                            i,
                            Candidate {
                                author_id: author,
                                n_calls: 1,
                                attributed_net: attributed,
                                leads: vec![lead],
                                platform_counts,
                            },
                        );
                    }
                }
            }
        }
        (candidates, n_episodes, n_attributed)
    }

    /// An episode contributes attribution only if it was actually traded, actually
    /// decisive, not in the future, and inside any requested phase.
    fn episode_in_scope(&self, e: &Episode, as_of_ns: u64) -> bool {
        if !e.outcome().was_admitted {
            return false;
        }
        if e.outcome().realized_net_lamports == 0 {
            return false;
        }
        if e.context().info_time_ns > as_of_ns {
            return false;
        }
        match self.params.venue_phase {
            None => true,
            Some(p) => e.context().venue_phase == p,
        }
    }
}

/// Confidence in a recommendation row, basis points.
///
/// Sample-size credit — linear in attributed calls up to
/// [`FOLLOW_CONFIDENCE_SATURATION_CALLS`] — scaled by a trust-tier multiplier. An
/// author with a long lead-time record but no independently earned trust is capped
/// at [`CONFIDENCE_UNPROVEN_BP`]: attribution over our own trades is real evidence,
/// but it is one dataset, and one dataset is not corroboration.
#[must_use]
pub fn confidence_bp(n_calls: u32, tier: TrustTier) -> u32 {
    let sample = (u64::from(n_calls) * u64::from(BPS_SCALE_U32)
        / u64::from(FOLLOW_CONFIDENCE_SATURATION_CALLS.max(1)))
    .min(u64::from(BPS_SCALE_U32));
    let tier_bp = match tier {
        TrustTier::Trusted => CONFIDENCE_TRUSTED_BP,
        TrustTier::Watch => CONFIDENCE_WATCH_BP,
        TrustTier::Unproven => CONFIDENCE_UNPROVEN_BP,
        TrustTier::Demoted => 0,
    };
    ((sample * u64::from(tier_bp)) / u64::from(BPS_SCALE_U32)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::episode::{DiscoveryLane, EpisodeContext, EpisodeOutcome, ExitReason};
    use crate::fingerprint::{SetupFingerprint, SetupInputs};
    use crate::social_recall::{CallMarkout, CallRecord};
    use crate::trust::TrustParams;

    const SEC: u64 = 1_000_000_000;
    const MIN: u64 = 60 * SEC;
    const HOUR: u64 = 60 * MIN;
    const DAY: u64 = 24 * HOUR;
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

    fn episode(id: u64, mint: u64, t: u64, net: i128) -> Episode {
        Episode::new(
            id,
            SetupFingerprint::from_inputs(&SetupInputs::default()),
            EpisodeContext {
                mint_id: mint,
                venue_phase: VenuePhase::Curve,
                meta_category_id: 1,
                discovery_lane: DiscoveryLane::SocialCall,
                info_time_ns: t,
                slot: id,
            },
            EpisodeOutcome {
                realized_net_lamports: net,
                hold_duration_ns: 5 * MIN,
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

    fn trust_model() -> SocialTrust {
        SocialTrust::with_params(TrustParams {
            population_min_sample: u32::MAX,
            ..TrustParams::default()
        })
    }

    /// `n` episodes on mints `1..=n`, each preceded by a call from `author` at
    /// `lead` before the decision.
    struct World {
        index: EpisodicIndex,
        social: SocialRecallIndex,
        next_call: u64,
        next_ep: u64,
    }

    impl World {
        fn new() -> Self {
            Self {
                index: EpisodicIndex::with_capacity(1_024),
                social: SocialRecallIndex::with_capacity(8_192, 8_192),
                next_call: 1,
                next_ep: 1,
            }
        }

        /// One episode on `mint` at `t` paying `net`, called by each `(author,
        /// lead, platform)` beforehand.
        fn scene(&mut self, mint: u64, t: u64, net: i128, callers: &[(u64, u64, Platform)]) {
            for (author, lead, p) in callers {
                self.social
                    .record_call(call(self.next_call, mint, *author, t - *lead, *p))
                    .expect("monotone");
                self.next_call += 1;
            }
            self.index
                .push(episode(self.next_ep, mint, t, net))
                .expect("monotone");
            self.next_ep += 1;
        }
    }

    // ------------------------------------------------------- lead weighting

    #[test]
    fn a_call_after_our_entry_is_worth_exactly_nothing() {
        let w = lead_weight_units(
            0,
            FOLLOW_MIN_LEAD_NS,
            FOLLOW_FULL_LEAD_NS,
            FOLLOW_STALE_LEAD_NS,
            FOLLOW_LOOKBACK_NS,
        );
        assert_eq!(w, 0);
        assert_eq!(
            lead_weight_units(
                FOLLOW_MIN_LEAD_NS - 1,
                FOLLOW_MIN_LEAD_NS,
                FOLLOW_FULL_LEAD_NS,
                FOLLOW_STALE_LEAD_NS,
                FOLLOW_LOOKBACK_NS
            ),
            0
        );
    }

    #[test]
    fn lead_weight_is_a_trapezoid_with_no_discontinuity() {
        let f = |lead| {
            lead_weight_units(
                lead,
                FOLLOW_MIN_LEAD_NS,
                FOLLOW_FULL_LEAD_NS,
                FOLLOW_STALE_LEAD_NS,
                FOLLOW_LOOKBACK_NS,
            )
        };
        assert_eq!(f(FOLLOW_MIN_LEAD_NS), 0);
        assert_eq!(f(FOLLOW_FULL_LEAD_NS), LEAD_WEIGHT_UNIT);
        assert_eq!(f(FOLLOW_STALE_LEAD_NS), LEAD_WEIGHT_UNIT);
        assert_eq!(f(FOLLOW_LOOKBACK_NS), 0);
        assert_eq!(f(FOLLOW_LOOKBACK_NS + DAY), 0);
        // Ramp is monotone up; decay is monotone down.
        let mut prev = 0;
        for k in 0..200u64 {
            let lead = FOLLOW_MIN_LEAD_NS + (FOLLOW_FULL_LEAD_NS - FOLLOW_MIN_LEAD_NS) * k / 199;
            let w = f(lead);
            assert!(w >= prev);
            prev = w;
        }
        let mut prev = LEAD_WEIGHT_UNIT;
        for k in 0..200u64 {
            let lead = FOLLOW_STALE_LEAD_NS + (FOLLOW_LOOKBACK_NS - FOLLOW_STALE_LEAD_NS) * k / 199;
            let w = f(lead);
            assert!(w <= prev);
            prev = w;
        }
    }

    #[test]
    fn lead_weight_tolerates_degenerate_knots_without_dividing_by_zero() {
        assert_eq!(lead_weight_units(100, 0, 0, 0, 0), 0);
        assert_eq!(lead_weight_units(0, 10, 5, 1, 1), 0);
    }

    // ------------------------------------------------------------ fail-closed

    #[test]
    fn an_empty_index_yields_unknown_with_no_rows() {
        let w = World::new();
        let t = trust_model();
        let snap = t.snapshot(&w.social, T0);
        let v = FollowRecommender::new().recommend_follows(
            &w.index,
            &w.social,
            &t,
            &snap,
            &FollowSet::new(),
            T0,
        );
        assert_eq!(
            v,
            FollowRecoVerdict::Unknown(FollowRecoUnknown::NoDecisiveEpisode { n_episodes: 0 })
        );
        assert!(v.recommendations().is_none());
    }

    #[test]
    fn episodes_with_no_preceding_call_yield_unknown() {
        let mut w = World::new();
        for i in 1..=6u64 {
            w.scene(i, T0 - (10 - i) * HOUR, 1_000_000_000, &[]);
        }
        let t = trust_model();
        let snap = t.snapshot(&w.social, T0);
        let v = FollowRecommender::new().recommend_follows(
            &w.index,
            &w.social,
            &t,
            &snap,
            &FollowSet::new(),
            T0,
        );
        assert!(matches!(
            v.unknown_reason(),
            Some(FollowRecoUnknown::NoAttributedCall { .. })
        ));
        assert!(v.recommendations().is_none());
    }

    #[test]
    fn four_well_timed_calls_are_not_enough_to_recommend_anyone() {
        let mut w = World::new();
        for i in 1..=4u64 {
            w.scene(
                i,
                T0 - (10 - i) * HOUR,
                5_000_000_000,
                &[(77, 30 * MIN, Platform::X)],
            );
        }
        let t = trust_model();
        let snap = t.snapshot(&w.social, T0);
        let v = FollowRecommender::new().recommend_follows(
            &w.index,
            &w.social,
            &t,
            &snap,
            &FollowSet::new(),
            T0,
        );
        assert_eq!(
            v,
            FollowRecoVerdict::Unknown(FollowRecoUnknown::NoQualifiedCandidate {
                n_candidates: 1,
                min_attributed_calls: FOLLOW_MIN_ATTRIBUTED_CALLS
            })
        );
        assert!(
            v.recommendations().is_none(),
            "a thin sample must expose no row at all"
        );
    }

    #[test]
    fn the_fifth_attributed_call_opens_the_gate() {
        let mut w = World::new();
        for i in 1..=5u64 {
            w.scene(
                i,
                T0 - (10 - i) * HOUR,
                5_000_000_000,
                &[(77, 30 * MIN, Platform::X)],
            );
        }
        let t = trust_model();
        let snap = t.snapshot(&w.social, T0);
        let v = FollowRecommender::new().recommend_follows(
            &w.index,
            &w.social,
            &t,
            &snap,
            &FollowSet::new(),
            T0,
        );
        let rows = v.recommendations().expect("known");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].author_id, 77);
        assert_eq!(rows[0].n_calls, 5);
    }

    #[test]
    fn every_follow_unknown_variant_exposes_no_rows() {
        for u in [
            FollowRecoUnknown::NoDecisiveEpisode { n_episodes: 0 },
            FollowRecoUnknown::NoAttributedCall {
                n_episodes: 4,
                lookback_ns: 1,
            },
            FollowRecoUnknown::NoQualifiedCandidate {
                n_candidates: 3,
                min_attributed_calls: 5,
            },
        ] {
            let v = FollowRecoVerdict::Unknown(u);
            assert!(v.recommendations().is_none());
            assert_eq!(v.unknown_reason(), Some(u));
        }
    }

    // -------------------------------------------------------------- ranking

    #[test]
    fn ranking_prefers_the_earlier_caller_at_equal_realized_net() {
        let mut w = World::new();
        // Same six episodes, same P&L. Author 1 calls 30 minutes early (full
        // weight); author 2 calls one minute early (low on the ramp).
        for i in 1..=6u64 {
            w.scene(
                i,
                T0 - (10 - i) * HOUR,
                4_000_000_000,
                &[(1, 30 * MIN, Platform::X), (2, MIN, Platform::X)],
            );
        }
        let t = trust_model();
        let snap = t.snapshot(&w.social, T0);
        let v = FollowRecommender::new().recommend_follows(
            &w.index,
            &w.social,
            &t,
            &snap,
            &FollowSet::new(),
            T0,
        );
        let rows = v.recommendations().expect("known");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].author_id, 1, "the early caller ranks first");
        assert_eq!(rows[1].author_id, 2);
        assert!(
            rows[0].realized_net_attributed_lamports > rows[1].realized_net_attributed_lamports * 8,
            "lead time must dominate: {} vs {}",
            rows[0].realized_net_attributed_lamports,
            rows[1].realized_net_attributed_lamports
        );
    }

    #[test]
    fn a_caller_who_only_ever_arrives_after_us_earns_nothing() {
        let mut w = World::new();
        for i in 1..=10u64 {
            w.scene(
                i,
                T0 - (20 - i) * HOUR,
                4_000_000_000,
                &[(1, 30 * MIN, Platform::X), (9, SEC, Platform::X)],
            );
        }
        let t = trust_model();
        let snap = t.snapshot(&w.social, T0);
        let v = FollowRecommender::new().recommend_follows(
            &w.index,
            &w.social,
            &t,
            &snap,
            &FollowSet::new(),
            T0,
        );
        let rows = v.recommendations().expect("known");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].author_id, 1);
        assert!(
            !rows.iter().any(|r| r.author_id == 9),
            "a witness is not a signal"
        );
    }

    #[test]
    fn a_stale_firehose_caller_is_discounted_against_a_timely_one() {
        let mut w = World::new();
        for i in 1..=8u64 {
            w.scene(
                i,
                T0 - (30 - i) * HOUR,
                4_000_000_000,
                &[
                    (1, 30 * MIN, Platform::X),
                    (2, 20 * HOUR, Platform::Telegram),
                ],
            );
        }
        let t = trust_model();
        let snap = t.snapshot(&w.social, T0);
        let rows = FollowRecommender::new()
            .recommend_follows(&w.index, &w.social, &t, &snap, &FollowSet::new(), T0)
            .recommendations()
            .expect("known")
            .to_vec();
        let timely = rows.iter().find(|r| r.author_id == 1).expect("present");
        let stale = rows.iter().find(|r| r.author_id == 2).expect("present");
        assert!(
            timely.realized_net_attributed_lamports > stale.realized_net_attributed_lamports,
            "a call twenty hours early did not cause the trade"
        );
        assert_eq!(rows[0].author_id, 1);
        assert_eq!(stale.platform, Platform::Telegram);
    }

    #[test]
    fn spamming_a_mint_cannot_inflate_attribution() {
        let mut w = World::new();
        let mut spam = World::new();
        for i in 1..=6u64 {
            w.scene(
                i,
                T0 - (10 - i) * HOUR,
                4_000_000_000,
                &[(1, 30 * MIN, Platform::X)],
            );
            let callers: Vec<(u64, u64, Platform)> = (0..20)
                .map(|k| (1u64, 30 * MIN + k * SEC, Platform::X))
                .collect();
            spam.scene(i, T0 - (10 - i) * HOUR, 4_000_000_000, &callers);
        }
        let t = trust_model();
        let r = FollowRecommender::new();
        let snap_a = t.snapshot(&w.social, T0);
        let snap_b = t.snapshot(&spam.social, T0);
        let a = r
            .recommend_follows(&w.index, &w.social, &t, &snap_a, &FollowSet::new(), T0)
            .recommendations()
            .expect("known")
            .to_vec();
        let b = r
            .recommend_follows(
                &spam.index,
                &spam.social,
                &t,
                &snap_b,
                &FollowSet::new(),
                T0,
            )
            .recommendations()
            .expect("known")
            .to_vec();
        assert_eq!(a[0].n_calls, b[0].n_calls, "one credit per episode");
        assert_eq!(
            a[0].realized_net_attributed_lamports,
            b[0].realized_net_attributed_lamports
        );
    }

    #[test]
    fn already_followed_authors_are_excluded() {
        let mut w = World::new();
        for i in 1..=8u64 {
            w.scene(
                i,
                T0 - (12 - i) * HOUR,
                4_000_000_000,
                &[(1, 30 * MIN, Platform::X), (2, 25 * MIN, Platform::Discord)],
            );
        }
        let t = trust_model();
        let snap = t.snapshot(&w.social, T0);
        let r = FollowRecommender::new();

        let none_followed = r
            .recommend_follows(&w.index, &w.social, &t, &snap, &FollowSet::new(), T0)
            .recommendations()
            .expect("known")
            .to_vec();
        assert_eq!(none_followed.len(), 2);

        let mut set = FollowSet::new();
        set.follow(1).expect("capacity");
        let filtered = r
            .recommend_follows(&w.index, &w.social, &t, &snap, &set, T0)
            .recommendations()
            .expect("known")
            .to_vec();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].author_id, 2);
    }

    #[test]
    fn a_caller_who_preceded_only_losers_is_never_recommended() {
        let mut w = World::new();
        for i in 1..=10u64 {
            w.scene(
                i,
                T0 - (14 - i) * HOUR,
                -4_000_000_000,
                &[(3, 30 * MIN, Platform::X)],
            );
        }
        let t = trust_model();
        let snap = t.snapshot(&w.social, T0);
        let v = FollowRecommender::new().recommend_follows(
            &w.index,
            &w.social,
            &t,
            &snap,
            &FollowSet::new(),
            T0,
        );
        assert!(v.recommendations().is_none());
        assert!(matches!(
            v.unknown_reason(),
            Some(FollowRecoUnknown::NoQualifiedCandidate { .. })
        ));
    }

    #[test]
    fn a_demoted_source_is_never_recommended_however_good_the_attribution() {
        let mut w = World::new();
        for i in 1..=10u64 {
            w.scene(
                i,
                T0 - (14 - i) * HOUR,
                4_000_000_000,
                &[(4, 30 * MIN, Platform::X)],
            );
        }
        // Independently decisive evidence that following author 4 loses money.
        for i in 0..60u64 {
            w.social
                .record_markout(CallMarkout {
                    call_id: 900_000 + i,
                    author_id: 4,
                    realized_net_lamports: -300_000_000,
                    hold_duration_ns: MIN,
                    info_time_ns: T0 - DAY,
                })
                .expect("monotone");
        }
        let t = trust_model();
        let snap = t.snapshot(&w.social, T0);
        assert_eq!(t.trust_from_snapshot(&snap, 4).tier(), TrustTier::Demoted);
        let v = FollowRecommender::new().recommend_follows(
            &w.index,
            &w.social,
            &t,
            &snap,
            &FollowSet::new(),
            T0,
        );
        assert!(v.recommendations().is_none());
    }

    #[test]
    fn recommendations_are_bounded_and_deterministic() {
        let mut w = World::new();
        for i in 1..=40u64 {
            let callers: Vec<(u64, u64, Platform)> = (1..=30u64)
                .map(|a| (a, 20 * MIN + a * SEC, Platform::X))
                .collect();
            w.scene(i, T0 - (60 - i) * HOUR, 1_000_000_000, &callers);
        }
        let t = trust_model();
        let snap = t.snapshot(&w.social, T0);
        let r = FollowRecommender::new();
        let first = r.recommend_follows(&w.index, &w.social, &t, &snap, &FollowSet::new(), T0);
        let rows = first.recommendations().expect("known");
        assert!(rows.len() <= FOLLOW_RECO_CAP);
        for _ in 0..16 {
            assert_eq!(
                r.recommend_follows(&w.index, &w.social, &t, &snap, &FollowSet::new(), T0),
                first
            );
        }
        // The documented total order holds pairwise.
        for pair in rows.windows(2) {
            let a = &pair[0];
            let b = &pair[1];
            assert!(
                (a.realized_net_attributed_lamports, a.n_calls)
                    >= (b.realized_net_attributed_lamports, b.n_calls)
                    || a.realized_net_attributed_lamports > b.realized_net_attributed_lamports
            );
        }
    }

    #[test]
    fn attribution_never_looks_past_as_of() {
        let mut w = World::new();
        for i in 1..=10u64 {
            w.scene(
                i,
                T0 + i * HOUR,
                9_000_000_000,
                &[(5, 30 * MIN, Platform::X)],
            );
        }
        let t = trust_model();
        let snap = t.snapshot(&w.social, T0);
        let v = FollowRecommender::new().recommend_follows(
            &w.index,
            &w.social,
            &t,
            &snap,
            &FollowSet::new(),
            T0,
        );
        assert_eq!(
            v,
            FollowRecoVerdict::Unknown(FollowRecoUnknown::NoDecisiveEpisode { n_episodes: 0 }),
            "replay must not attribute the future"
        );
    }

    #[test]
    fn unadmitted_episodes_contribute_no_attribution() {
        let mut w = World::new();
        for i in 1..=10u64 {
            w.social
                .record_call(call(i, i, 6, T0 - (14 - i) * HOUR - 30 * MIN, Platform::X))
                .expect("monotone");
            let mut e = episode(i, i, T0 - (14 - i) * HOUR, 9_000_000_000);
            e = Episode::new(
                i,
                *e.fingerprint(),
                *e.context(),
                EpisodeOutcome::rejected(),
            );
            w.index.push(e).expect("monotone");
        }
        let t = trust_model();
        let snap = t.snapshot(&w.social, T0);
        let v = FollowRecommender::new().recommend_follows(
            &w.index,
            &w.social,
            &t,
            &snap,
            &FollowSet::new(),
            T0,
        );
        assert_eq!(
            v,
            FollowRecoVerdict::Unknown(FollowRecoUnknown::NoDecisiveEpisode { n_episodes: 0 }),
            "a profit on a trade never taken is not evidence about a caller"
        );
    }

    // -------------------------------------------------------------- unfollow

    #[test]
    fn unfollow_surfaces_a_decayed_source_and_leaves_the_good_one_alone() {
        let mut w = World::new();
        for i in 1..=10u64 {
            w.scene(
                i,
                T0 - (14 - i) * HOUR,
                4_000_000_000,
                &[(1, 30 * MIN, Platform::X)],
            );
        }
        for i in 11..=20u64 {
            w.scene(
                i,
                T0 - (14 - 10) * HOUR + (i - 10) * MIN,
                -6_000_000_000,
                &[(2, 30 * MIN, Platform::Stream)],
            );
        }
        let t = trust_model();
        let snap = t.snapshot(&w.social, T0);
        let mut set = FollowSet::new();
        set.follow(1).expect("cap");
        set.follow(2).expect("cap");
        let rows =
            FollowRecommender::new().unfollow_candidates(&w.index, &w.social, &t, &snap, &set, T0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].author_id, 2);
        assert!(rows[0].realized_net_attributed_lamports < 0);
        assert_eq!(rows[0].platform, Platform::Stream);
    }

    #[test]
    fn unfollow_never_fires_a_source_on_a_thin_sample() {
        let mut w = World::new();
        for i in 1..=3u64 {
            w.scene(
                i,
                T0 - (14 - i) * HOUR,
                -9_000_000_000,
                &[(2, 30 * MIN, Platform::X)],
            );
        }
        let t = trust_model();
        let snap = t.snapshot(&w.social, T0);
        let mut set = FollowSet::new();
        set.follow(2).expect("cap");
        let rows =
            FollowRecommender::new().unfollow_candidates(&w.index, &w.social, &t, &snap, &set, T0);
        assert!(
            rows.is_empty(),
            "three bad calls is not grounds to fire anyone"
        );
    }

    #[test]
    fn unfollow_only_considers_authors_we_actually_follow() {
        let mut w = World::new();
        for i in 1..=10u64 {
            w.scene(
                i,
                T0 - (14 - i) * HOUR,
                -6_000_000_000,
                &[(2, 30 * MIN, Platform::X)],
            );
        }
        let t = trust_model();
        let snap = t.snapshot(&w.social, T0);
        let rows = FollowRecommender::new().unfollow_candidates(
            &w.index,
            &w.social,
            &t,
            &snap,
            &FollowSet::new(),
            T0,
        );
        assert!(rows.is_empty());
    }

    // ------------------------------------------------------------ follow set

    #[test]
    fn follow_set_is_bounded_and_refuses_rather_than_evicting() {
        let mut set = FollowSet::with_capacity(3);
        for i in 1..=3u64 {
            assert!(set.follow(i).expect("capacity"));
        }
        assert!(!set.follow(2).expect("already present"));
        let err = set.follow(4).expect_err("full");
        assert_eq!(err, FollowSetError::CapacityExhausted { capacity: 3 });
        assert_eq!(set.ids(), &[1, 2, 3]);
        assert!(set.contains(1) && !set.contains(4));
        assert!(set.unfollow(2));
        assert!(!set.unfollow(2));
        assert_eq!(set.ids(), &[1, 3]);
        assert!(set.follow(4).expect("room freed"));
    }

    #[test]
    fn follow_set_from_ids_dedupes_and_sorts() {
        let set = FollowSet::from_ids(&[9, 3, 9, 1], 8).expect("capacity");
        assert_eq!(set.ids(), &[1, 3, 9]);
        assert_eq!(set.len(), 3);
        assert!(!set.is_empty());
        assert!(FollowSet::from_ids(&[1, 2, 3], 2).is_err());
    }

    // ------------------------------------------------------------ confidence

    #[test]
    fn confidence_grows_with_sample_and_is_capped_by_tier() {
        assert!(confidence_bp(5, TrustTier::Trusted) < confidence_bp(20, TrustTier::Trusted));
        assert_eq!(confidence_bp(20, TrustTier::Trusted), CONFIDENCE_TRUSTED_BP);
        assert_eq!(
            confidence_bp(100, TrustTier::Trusted),
            CONFIDENCE_TRUSTED_BP
        );
        assert_eq!(confidence_bp(20, TrustTier::Watch), CONFIDENCE_WATCH_BP);
        assert_eq!(
            confidence_bp(20, TrustTier::Unproven),
            CONFIDENCE_UNPROVEN_BP
        );
        assert_eq!(confidence_bp(20, TrustTier::Demoted), 0);
        assert_eq!(confidence_bp(0, TrustTier::Trusted), 0);
    }

    #[test]
    fn an_unproven_but_well_timed_caller_is_surfaced_at_low_confidence() {
        let mut w = World::new();
        for i in 1..=20u64 {
            w.scene(
                i,
                T0 - (30 - i) * HOUR,
                4_000_000_000,
                &[(8, 30 * MIN, Platform::X)],
            );
        }
        let t = trust_model();
        let snap = t.snapshot(&w.social, T0);
        let rows = FollowRecommender::new()
            .recommend_follows(&w.index, &w.social, &t, &snap, &FollowSet::new(), T0)
            .recommendations()
            .expect("known")
            .to_vec();
        assert_eq!(rows[0].author_id, 8);
        assert_eq!(rows[0].trust_tier, TrustTier::Unproven);
        assert_eq!(rows[0].confidence_bp, CONFIDENCE_UNPROVEN_BP);
    }
}
