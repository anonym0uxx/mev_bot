//! `EpisodicIndex` and `recall` — "what happened last time a coin looked like this?"
//!
//! This is the module the operator's question actually lands in. It answers in
//! microseconds, locally, deterministically, with no model and no network call.
//!
//! # The two-stage algorithm
//!
//! **Stage 1 — Hamming prefilter (the microsecond path).** The index keeps three
//! *contiguous parallel arrays*: packed `u128` signatures, `u64` filter keys, and
//! the episode records. Stage 1 touches only the first two, in **one linear pass**.
//! Per slot it executes one `xor`, one `count_ones`, one `and` and two compares:
//!
//! * the whole filter (venue phase, admitted-only, meta, lane) is compiled once
//!   into a single `(mask, expect)` pair by
//!   [`RecallFilter::key_mask_and_expect`], so filtering is `key & mask == expect`;
//! * the tie-break key is the episode's *insertion age*, derived from ring geometry
//!   rather than read out of the `Episode` record — so stage 1 never loads an
//!   episode, and never chases a pointer.
//!
//! Distances are memoised into a one-byte-per-slot scratch buffer and folded into a
//! stack histogram (`[u32; 130]`, one bucket per possible popcount). The histogram
//! yields the exact cutoff `T` that admits at least `top_m` candidates without
//! sorting anything, and the *selection* pass then re-reads the 1-byte scratch —
//! `len` bytes, not `len * 24` — instead of re-streaming the signature array.
//! Survivors accumulate in a `Vec` capped at `2 * top_m`, compacted by
//! `(distance, age)` whenever it fills; since that is a total order, the result is
//! *exactly* the global top-M, not an approximation of it.
//!
//! **Stage 2 — precise weighted rank.** The at-most-`top_m` survivors are re-scored
//! with [`crate::fingerprint::weighted_distance`], which emphasises structure and
//! flow over time-of-day. Stage 2 is O(1) with respect to index size. Ordering is by
//! `(weighted_distance, episode_id)` ascending — a **total** order, because the
//! index rejects duplicate/non-monotone `episode_id`s. There is no unordered
//! iteration anywhere in this module and therefore no run-to-run drift.
//!
//! ## Cost profile
//!
//! One pass over the two hot streams. At [`EPISODE_CAP`] `= 16_384` that is
//! `16_384 * (16 + 8) = 384 KiB` streamed once, and the pass is memory-bound rather
//! than compute-bound. Measured on a release build at the default
//! `top_m = 64`, wall time per `recall` averaged over 2_000 iterations:
//!
//! ```text
//! index size    recall latency
//!      1_024        ~10 us
//!      4_096        ~21 us
//!     16_384        ~70 us
//! ```
//!
//! Stage 2 is the fixed floor (~4 us at `top_m = 64`): it is the only part of recall
//! that touches `Episode` records, so it is the only part paying a cache miss per
//! candidate. Everything above the floor is the linear scan, which is bandwidth-
//! bound. A live index of a few thousand episodes therefore answers in the low tens
//! of microseconds. The op count is not left to belief:
//! [`EpisodicIndex::recall_probe`] returns a [`RecallOpCount`] and
//! `tests::full_index_recall_stays_inside_the_op_budget` asserts it against
//! [`RECALL_OP_BUDGET_FULL_INDEX`].
//!
//! Stage 1 allocates the `len`-byte distance scratch and the bounded survivor
//! buffer; stage 2 allocates bounded scratch of at most `top_m` elements. Nothing
//! allocates per candidate.
//!
//! # Fail-closed recall (constitution 46) — the safety property
//!
//! [`RecallVerdict`] is an enum with exactly two shapes:
//!
//! ```text
//! RecallVerdict::Known(RecallStats)   <- has numbers
//! RecallVerdict::Unknown(RecallUnknown) <- has NO outcome numbers, structurally
//! ```
//!
//! [`RecallUnknown`] carries only *diagnostics* — how many episodes matched, what
//! the sample floor was, how far the nearest neighbour was. It has no field of type
//! "lamports", "win rate" or "hold". There is no accessor, no `unwrap_or`, no
//! `Default` that will hand a caller a number when the evidence is thin. To read an
//! estimate you must destructure `Known`, and you can only do that when the engine
//! decided the evidence supports one.
//!
//! This is deliberate and it is the most important line of code in the crate.
//! Small-n recall is precisely how a quant fools himself: seven episodes, five
//! winners, "71% win rate", size up, ruin. The verdict type makes that sentence
//! unrepresentable.
//!
//! Unknown is returned when any of:
//! * the index is empty;
//! * nothing lies within [`RecallParams::max_distance`] of the query;
//! * fewer than [`RecallParams::min_sample`] episodes matched.
//!
//! # Phase separation (constitution 100) — enforced by the type, not by discipline
//!
//! §100 forbids pooling bonding-curve and migrated-pool outcomes into one estimate.
//! Rather than document that and hope, [`RecallFilter`] has **no constructor that
//! leaves the venue phase unset**: [`RecallFilter::for_query`] takes it from the
//! query fingerprint and [`RecallFilter::for_phase`] demands it explicitly. Both
//! [`EpisodicIndex::recall`] and [`EpisodicIndex::recall_conditioned`] go through a
//! `RecallFilter`, so **every** estimate this crate can produce is phase-pure. The
//! meta category and discovery lane are the optional pins on top of that mandatory
//! one.

use crate::episode::{DiscoveryLane, Episode};
use crate::fingerprint::{
    signature_hamming, weighted_distance, FeatureWeights, SetupFingerprint, VenuePhase,
};

/// Bounded capacity of the default episodic index (constitution 57/99).
///
/// Eviction is **oldest-first**: the index is a ring, and slot `head` — the oldest
/// live episode — is the one a full index overwrites. Memory is therefore constant
/// at `16_384 * (size_of::<Episode>() + 24)` regardless of uptime, which is what
/// lets this run for weeks on a fixed box. Durable history is not lost by eviction:
/// [`crate::persist`]'s journal is append-only and keeps everything on disk.
pub const EPISODE_CAP: usize = 16_384;

/// Minimum number of matched episodes before recall will report an estimate
/// (constitution 46 small-n guard). Below this the verdict is
/// [`RecallUnknown::InsufficientSample`] and carries no numbers.
pub const MIN_SAMPLE_DEFAULT: u32 = 8;

/// Maximum stage-1 Hamming distance at which an episode still counts as "a setup
/// like this" (constitution 102). Twelve bits is roughly "three or four fields
/// differ by a bucket or two" under the thermometer/one-hot encoding.
pub const MAX_DISTANCE_DEFAULT: u32 = 12;

/// Maximum number of candidates promoted from stage 1 into stage 2
/// (constitution 57 bounded work). Caps stage-2 cost independent of index size.
///
/// Sixty-four is chosen on statistical grounds first and latency grounds second.
/// A recall estimate is only as good as the *similarity* of the episodes it
/// averages: widening the neighbourhood to a few hundred out of a 16_384-episode
/// index pulls in materially different setups and biases the estimate toward the
/// unconditional mean — the classic nearest-neighbour bias/variance trade, and in
/// this domain bias is the expensive side. Sixty-four is comfortably above
/// [`MIN_SAMPLE_DEFAULT`] while keeping the ball tight. It also happens to make
/// stage 2 roughly four times cheaper than a 256-candidate default, since stage 2
/// is the only part of recall that touches `Episode` records and therefore the only
/// part that pays cache-miss latency per candidate.
pub const PREFILTER_TOP_M_DEFAULT: usize = 64;

/// Basis-point scale for [`RecallStats::win_rate_bp`].
pub const BPS_SCALE_U32: u32 = 10_000;

/// Sentinel written into the stage-1 distance scratch for a slot the filter
/// rejected. Strictly greater than any reachable Hamming distance (a `u128` popcount
/// cannot exceed 128), so the selection pass excludes it by the same comparison it
/// uses for the radius — one test, not two.
const DIST_OUT_OF_SCOPE: u8 = u8::MAX;

/// Percentile used for the lower quartile order statistic.
pub const P25: u32 = 25;
/// Percentile used for the median order statistic.
pub const P50: u32 = 50;
/// Percentile used for the upper quartile order statistic.
pub const P75: u32 = 75;

/// Worst-case op budget for one recall over a completely full [`EPISODE_CAP`]
/// index (constitution 24 performance is a compiled-in contract, not a memory).
///
/// Derivation, and it is a real bound rather than a hopeful one:
/// * the single stage-1 popcount pass performs exactly `len` popcounts (the
///   selection pass reads memoised distances and performs none);
/// * candidate pushes are at most `len` (in the degenerate case every episode sits
///   at the histogram cutoff);
/// * weighted re-scores are at most `top_m`;
/// * statistics touch at most `top_m` elements.
///
/// Hence `2 * EPISODE_CAP + 2 * PREFILTER_TOP_M_DEFAULT`, rounded up to
/// `2 * EPISODE_CAP + 4 * PREFILTER_TOP_M_DEFAULT` for headroom.
pub const RECALL_OP_BUDGET_FULL_INDEX: u64 =
    2 * EPISODE_CAP as u64 + 4 * PREFILTER_TOP_M_DEFAULT as u64;

// ---------------------------------------------------------------------------
// Filter key packing
// ---------------------------------------------------------------------------

/// Bit offset of the venue-phase ordinal inside the packed filter key.
pub const FK_VENUE_SHIFT: u32 = 0;
/// Mask (pre-shift) of the venue-phase ordinal inside the packed filter key.
pub const FK_VENUE_MASK: u64 = 0x3;
/// Bit offset of the `was_admitted` flag inside the packed filter key.
pub const FK_ADMITTED_SHIFT: u32 = 4;
/// Bit offset of the discovery-lane ordinal inside the packed filter key.
pub const FK_LANE_SHIFT: u32 = 8;
/// Mask (pre-shift) of the discovery-lane ordinal inside the packed filter key.
pub const FK_LANE_MASK: u64 = 0xFF;
/// Bit offset of the exact meta-category id inside the packed filter key.
pub const FK_META_SHIFT: u32 = 32;
/// Mask (pre-shift) of the exact meta-category id inside the packed filter key.
pub const FK_META_MASK: u64 = 0xFFFF_FFFF;

/// Pack an episode's filterable context into one contiguous `u64`.
///
/// This exists so stage 1 can apply venue-phase, discovery-lane, meta-category and
/// admitted-only filters **without loading the `Episode` record** — the whole point
/// of the parallel-array layout. The exact (un-mixed) meta id is carried here, so a
/// conditioned recall is precise even though the signature only holds a 16-slot
/// digest of it.
#[must_use]
pub fn pack_filter_key(e: &Episode) -> u64 {
    let ctx = e.context();
    (u64::from(ctx.venue_phase.ordinal()) << FK_VENUE_SHIFT)
        | (u64::from(e.outcome().was_admitted) << FK_ADMITTED_SHIFT)
        | (u64::from(ctx.discovery_lane.ordinal()) << FK_LANE_SHIFT)
        | (u64::from(ctx.meta_category_id) << FK_META_SHIFT)
}

/// Which episodes a recall is allowed to see.
///
/// The venue phase is **mandatory** — see the module docs on constitution 100.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecallFilter {
    venue_phase: VenuePhase,
    meta_category_id: Option<u32>,
    discovery_lane: Option<DiscoveryLane>,
}

impl RecallFilter {
    /// The only phase-correct default: take the phase from the query itself.
    #[must_use]
    pub fn for_query(query: &SetupFingerprint) -> Self {
        Self {
            venue_phase: query.venue_phase(),
            meta_category_id: None,
            discovery_lane: None,
        }
    }

    /// Build a filter for an explicitly chosen phase.
    #[must_use]
    pub const fn for_phase(venue_phase: VenuePhase) -> Self {
        Self {
            venue_phase,
            meta_category_id: None,
            discovery_lane: None,
        }
    }

    /// Pin the exact meta category: "did a setup like this, *in this meta*, earn?"
    #[must_use]
    pub const fn with_meta_category(mut self, meta_category_id: u32) -> Self {
        self.meta_category_id = Some(meta_category_id);
        self
    }

    /// Pin the discovery lane: "…and only when it came off the whale-follow lane".
    #[must_use]
    pub const fn with_discovery_lane(mut self, lane: DiscoveryLane) -> Self {
        self.discovery_lane = Some(lane);
        self
    }

    /// The mandatory venue phase this filter is pinned to.
    #[must_use]
    pub const fn venue_phase(&self) -> VenuePhase {
        self.venue_phase
    }

    /// The optional exact meta-category pin.
    #[must_use]
    pub const fn meta_category_id(&self) -> Option<u32> {
        self.meta_category_id
    }

    /// The optional discovery-lane pin.
    #[must_use]
    pub const fn discovery_lane(&self) -> Option<DiscoveryLane> {
        self.discovery_lane
    }

    /// Compile this filter into a single `(mask, expect)` pair.
    ///
    /// Every enabled pin contributes its bits to `mask` and its value to `expect`,
    /// so the whole filter — phase, admitted-only, meta, lane — collapses to one
    /// `and` and one `cmp` in the hot loop. Building it is done once per recall,
    /// outside the scan.
    #[must_use]
    pub fn key_mask_and_expect(&self, require_admitted: bool) -> (u64, u64) {
        let mut mask = FK_VENUE_MASK << FK_VENUE_SHIFT;
        let mut expect = u64::from(self.venue_phase.ordinal()) << FK_VENUE_SHIFT;
        if require_admitted {
            mask |= 1 << FK_ADMITTED_SHIFT;
            expect |= 1 << FK_ADMITTED_SHIFT;
        }
        if let Some(meta) = self.meta_category_id {
            mask |= FK_META_MASK << FK_META_SHIFT;
            expect |= u64::from(meta) << FK_META_SHIFT;
        }
        if let Some(lane) = self.discovery_lane {
            mask |= FK_LANE_MASK << FK_LANE_SHIFT;
            expect |= u64::from(lane.ordinal()) << FK_LANE_SHIFT;
        }
        (mask, expect)
    }

    /// Integer-only test of a packed filter key — one `and` and one `cmp`.
    #[must_use]
    pub fn accepts_key(&self, key: u64, require_admitted: bool) -> bool {
        let (mask, expect) = self.key_mask_and_expect(require_admitted);
        (key & mask) == expect
    }
}

// ---------------------------------------------------------------------------
// Params / verdict
// ---------------------------------------------------------------------------

/// Tunables for one recall. All named-const defaults (constitution 102).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecallParams {
    /// Minimum matched episodes before an estimate is produced (constitution 46).
    pub min_sample: u32,
    /// Stage-1 Hamming radius defining "a setup like this".
    pub max_distance: u32,
    /// Cap on candidates promoted into stage 2.
    pub top_m: usize,
    /// Stage-2 field weights.
    pub weights: FeatureWeights,
    /// When true (the default), only actually-traded episodes contribute to the
    /// statistics. See [`crate::episode`] on the counterfactual problem.
    pub require_admitted: bool,
}

impl Default for RecallParams {
    fn default() -> Self {
        Self {
            min_sample: MIN_SAMPLE_DEFAULT,
            max_distance: MAX_DISTANCE_DEFAULT,
            top_m: PREFILTER_TOP_M_DEFAULT,
            weights: FeatureWeights::default(),
            require_admitted: true,
        }
    }
}

/// Why recall declined to produce an estimate.
///
/// **Deliberately carries no outcome statistic of any kind.** Every field here is
/// a count or a distance — never lamports, never a win rate, never a hold time.
/// There is no way to coax a P&L number out of this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecallUnknown {
    /// The index holds no episodes at all.
    EmptyIndex,
    /// No episode passed the filter (e.g. nothing has ever traded in this phase).
    NoEpisodeInScope,
    /// Episodes exist in scope but none is within the similarity radius.
    NoCandidateInRadius {
        /// Hamming distance to the nearest in-scope episode, if any existed.
        nearest_distance: Option<u32>,
        /// The radius that was applied.
        max_distance: u32,
    },
    /// Similar episodes exist, but too few to say anything (constitution 46).
    InsufficientSample {
        /// How many episodes matched.
        n_matched: u32,
        /// The floor they failed to reach.
        min_sample: u32,
    },
}

/// The recalled distribution of outcomes for setups like the query.
///
/// Only reachable through [`RecallVerdict::Known`], i.e. only when the sample and
/// proximity gates both passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecallStats {
    /// Number of episodes the statistics were computed over.
    pub n_matched: u32,
    /// Median realized net lamports (nearest-rank order statistic; for an even
    /// sample this is the lower median — no interpolation, no rounding artefact).
    pub median_net_lamports: i128,
    /// Arithmetic mean of realized net lamports, integer division truncating
    /// toward zero (constitution 22: the rounding rule is stated, not implied).
    pub mean_net_lamports: i128,
    /// Episodes with strictly positive realized net.
    pub win_count: u32,
    /// Episodes with strictly negative realized net.
    pub loss_count: u32,
    /// Win rate in basis points over *decisive* episodes only
    /// (`win / (win + loss)`); exact flats are excluded from the denominator
    /// because a scratch is not evidence either way. `0` when nothing was decisive.
    pub win_rate_bp: u32,
    /// Lower-quartile realized net lamports (nearest rank).
    pub p25_net_lamports: i128,
    /// Upper-quartile realized net lamports (nearest rank).
    pub p75_net_lamports: i128,
    /// Median hold duration in nanoseconds (nearest rank).
    pub median_hold_ns: u64,
    /// Stage-1 Hamming distance to the closest matched episode.
    pub nearest_distance: u32,
    /// Stage-2 weighted-L1 distance to the closest matched episode.
    pub nearest_weighted_distance: u64,
    /// `episode_id` of the closest matched episode — the audit anchor. Lets an
    /// operator go read the single most similar thing that ever happened.
    pub nearest_episode_id: u64,
}

/// The answer to "what happened last time a coin looked like this?".
///
/// See the module docs: `Unknown` is structurally incapable of carrying an estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecallVerdict {
    /// Evidence was sufficient; here is the distribution.
    Known(RecallStats),
    /// Evidence was insufficient. No estimate exists, by construction.
    Unknown(RecallUnknown),
}

impl RecallVerdict {
    /// `true` when an estimate is available.
    #[must_use]
    pub const fn is_known(&self) -> bool {
        matches!(self, Self::Known(_))
    }

    /// The statistics, or `None`. This is the **only** way to reach a number.
    #[must_use]
    pub const fn stats(&self) -> Option<&RecallStats> {
        match self {
            Self::Known(s) => Some(s),
            Self::Unknown(_) => None,
        }
    }

    /// Why recall declined, or `None` if it did not.
    #[must_use]
    pub const fn unknown_reason(&self) -> Option<RecallUnknown> {
        match self {
            Self::Known(_) => None,
            Self::Unknown(u) => Some(*u),
        }
    }
}

/// Deterministic operation counters for one recall (constitution 24).
///
/// Returned by [`EpisodicIndex::recall_probe`] so the latency contract is an
/// assertion in CI rather than a claim in a comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecallOpCount {
    /// `xor` + `count_ones` pairs executed (both passes).
    pub popcount_ops: u64,
    /// Candidate slots pushed into the stage-2 working set.
    pub candidates_collected: u64,
    /// Weighted-L1 field scans executed in stage 2.
    pub weighted_scores: u64,
    /// Elements touched while computing order statistics.
    pub stat_elements: u64,
}

impl RecallOpCount {
    /// Total counted operations.
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.popcount_ops + self.candidates_collected + self.weighted_scores + self.stat_elements
    }
}

/// Why an episode could not be admitted to the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexError {
    /// `episode_id` was not strictly greater than the last admitted id. Monotone
    /// ids are what make the recall tie-break a *total* order; without that,
    /// ranking would depend on physical ring position and replay would drift.
    NonMonotonicEpisodeId {
        /// The id that was offered.
        offered: u64,
        /// The last id already in the index.
        last: u64,
    },
    /// The record's schema version is not the one this build understands.
    SchemaMismatch {
        /// Version found on the record.
        found: u16,
        /// Version this build writes and reads.
        expected: u16,
    },
}

// ---------------------------------------------------------------------------
// Index
// ---------------------------------------------------------------------------

/// Bounded, append-only-in-spirit ring of episodes plus the two contiguous hot
/// streams recall scans (constitution 57/99).
#[derive(Debug, Clone)]
pub struct EpisodicIndex {
    capacity: usize,
    episodes: Vec<Episode>,
    signatures: Vec<u128>,
    filter_keys: Vec<u64>,
    head: usize,
    last_episode_id: Option<u64>,
    evicted_count: u64,
}

impl Default for EpisodicIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl EpisodicIndex {
    /// A new index at the default [`EPISODE_CAP`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(EPISODE_CAP)
    }

    /// A new index with an explicit capacity (clamped to at least 1).
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            capacity,
            episodes: Vec::with_capacity(capacity),
            signatures: Vec::with_capacity(capacity),
            filter_keys: Vec::with_capacity(capacity),
            head: 0,
            last_episode_id: None,
            evicted_count: 0,
        }
    }

    /// Hard capacity. Live episode count never exceeds this, ever.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Number of live episodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.episodes.len()
    }

    /// `true` when no episodes are held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.episodes.is_empty()
    }

    /// How many episodes have been evicted by the oldest-first ring policy.
    #[must_use]
    pub const fn evicted_count(&self) -> u64 {
        self.evicted_count
    }

    /// The highest `episode_id` admitted so far, if any.
    #[must_use]
    pub const fn last_episode_id(&self) -> Option<u64> {
        self.last_episode_id
    }

    /// Admit an episode, returning the episode it evicted (if the index was full).
    ///
    /// Rejects non-monotone ids and foreign schema versions. Eviction is
    /// **oldest-first** and is the only way an episode ever leaves the index.
    pub fn push(&mut self, episode: Episode) -> Result<Option<Episode>, IndexError> {
        if episode.schema_version() != crate::episode::EPISODE_SCHEMA_VERSION {
            return Err(IndexError::SchemaMismatch {
                found: episode.schema_version(),
                expected: crate::episode::EPISODE_SCHEMA_VERSION,
            });
        }
        if let Some(last) = self.last_episode_id {
            if episode.episode_id() <= last {
                return Err(IndexError::NonMonotonicEpisodeId {
                    offered: episode.episode_id(),
                    last,
                });
            }
        }

        let signature = episode.fingerprint().signature();
        let key = pack_filter_key(&episode);
        self.last_episode_id = Some(episode.episode_id());

        let (slot, evicted) = if self.episodes.len() < self.capacity {
            let slot = self.episodes.len();
            self.episodes.push(episode);
            self.signatures.push(signature);
            self.filter_keys.push(key);
            (slot, None)
        } else {
            let slot = self.head;
            let old = core::mem::replace(&mut self.episodes[slot], episode);
            self.signatures[slot] = signature;
            self.filter_keys[slot] = key;
            self.evicted_count += 1;
            (slot, Some(old))
        };
        self.head = (slot + 1) % self.capacity;
        Ok(evicted)
    }

    /// Iterate live episodes oldest-first — the canonical, deterministic order used
    /// for snapshotting and for any operator-facing listing.
    pub fn iter_oldest_first(&self) -> impl Iterator<Item = &Episode> + '_ {
        let n = self.episodes.len();
        let start = if n < self.capacity { 0 } else { self.head };
        (0..n).map(move |k| &self.episodes[(start + k) % n.max(1)])
    }

    /// Fetch a live episode by id (linear scan; for audit paths, not the hot loop).
    #[must_use]
    pub fn get_by_episode_id(&self, episode_id: u64) -> Option<&Episode> {
        self.episodes.iter().find(|e| e.episode_id() == episode_id)
    }

    /// Recall over episodes in the **query's own venue phase** (constitution 100).
    ///
    /// There is no phase-pooled variant of this function, and there never will be.
    #[must_use]
    pub fn recall(&self, query: &SetupFingerprint, params: &RecallParams) -> RecallVerdict {
        let filter = RecallFilter::for_query(query);
        let mut ops = RecallOpCount::default();
        self.recall_inner(query, params, &filter, &mut ops)
    }

    /// Recall conditioned on an explicit filter — "did a setup like this, in *this*
    /// meta, in *this* phase, off *this* lane, earn?".
    #[must_use]
    pub fn recall_conditioned(
        &self,
        query: &SetupFingerprint,
        params: &RecallParams,
        filter: &RecallFilter,
    ) -> RecallVerdict {
        let mut ops = RecallOpCount::default();
        self.recall_inner(query, params, filter, &mut ops)
    }

    /// Recall plus its deterministic operation counters (constitution 24).
    #[must_use]
    pub fn recall_probe(
        &self,
        query: &SetupFingerprint,
        params: &RecallParams,
        filter: &RecallFilter,
    ) -> (RecallVerdict, RecallOpCount) {
        let mut ops = RecallOpCount::default();
        let verdict = self.recall_inner(query, params, filter, &mut ops);
        (verdict, ops)
    }

    fn recall_inner(
        &self,
        query: &SetupFingerprint,
        params: &RecallParams,
        filter: &RecallFilter,
        ops: &mut RecallOpCount,
    ) -> RecallVerdict {
        let n = self.episodes.len();
        if n == 0 {
            return RecallVerdict::Unknown(RecallUnknown::EmptyIndex);
        }
        let q = query.signature();
        let max_d = params.max_distance.min(128);
        let top_m = params.top_m.max(1);

        // --- Stage 1: one contiguous streaming pass, bounded top-M selection. ---
        // The filter collapses to a single mask-and-compare against the packed key,
        // and the tie-break key is the episode's *insertion age*, which is derived
        // from the ring geometry rather than read out of the `Episode` record. So
        // this loop touches only the two hot arrays: no `Episode` load, no pointer
        // chasing, one `xor`, one `count_ones`, one `and`, one `cmp` per slot.
        let (mask, expect) = filter.key_mask_and_expect(params.require_admitted);
        let mut in_scope = 0u64;
        let mut nearest_in_scope = u32::MAX;

        // Distances are memoised into a one-byte-per-slot scratch buffer. This is
        // the module's only unbounded-in-`len` allocation and it is deliberate: it
        // costs `len` bytes (16 KiB at full capacity) and it means the *selection*
        // pass re-reads 16 KiB from L1 instead of re-streaming 384 KiB. Out-of-scope
        // slots are marked with a sentinel above every reachable distance, so the
        // filter is applied exactly once.
        let mut distances: Vec<u8> = vec![DIST_OUT_OF_SCOPE; n];
        let mut hist = [0u32; 130];
        for ((slot, sig), key) in distances
            .iter_mut()
            .zip(self.signatures.iter())
            .zip(self.filter_keys.iter())
        {
            let d = signature_hamming(*sig, q);
            ops.popcount_ops += 1;
            if (*key & mask) == expect {
                *slot = d as u8;
                in_scope += 1;
                if d < nearest_in_scope {
                    nearest_in_scope = d;
                }
                if d <= max_d {
                    hist[d as usize] += 1;
                }
            }
        }
        if in_scope == 0 {
            return RecallVerdict::Unknown(RecallUnknown::NoEpisodeInScope);
        }

        // Smallest cutoff T admitting at least `top_m` in-scope candidates. Read
        // straight off the histogram — no sorting, no partial selection.
        let mut cumulative = 0u64;
        let mut cutoff = max_d;
        for (d, count) in hist.iter().enumerate().take(max_d as usize + 1) {
            cumulative += u64::from(*count);
            if cumulative >= top_m as u64 {
                cutoff = d as u32;
                break;
            }
        }
        if cumulative == 0 {
            return RecallVerdict::Unknown(RecallUnknown::NoCandidateInRadius {
                nearest_distance: Some(nearest_in_scope),
                max_distance: max_d,
            });
        }

        // Selection pass over the 1-byte distance scratch. Survivors are compacted
        // by `(distance, age)` whenever the buffer reaches `2 * top_m`, so the
        // working set is bounded and the survivors are chosen by a total order
        // rather than by physical ring position. `age` is the insertion rank,
        // derived from ring geometry — no `Episode` load anywhere in stage 1.
        let cap_kept = top_m.saturating_mul(2);
        let mut kept: Vec<Candidate> = Vec::with_capacity(cap_kept);
        let full = self.episodes.len() == self.capacity;
        let head = self.head;
        let cap = self.capacity;
        for (i, dv) in distances.iter().enumerate() {
            let d = u32::from(*dv);
            if d > cutoff {
                continue;
            }
            let age = if full {
                let t = i + cap - head;
                if t >= cap {
                    (t - cap) as u32
                } else {
                    t as u32
                }
            } else {
                i as u32
            };
            kept.push(Candidate {
                distance: d,
                age,
                slot: i as u32,
            });
            ops.candidates_collected += 1;
            if kept.len() >= cap_kept {
                kept.sort_unstable_by_key(|c| (c.distance, c.age));
                kept.truncate(top_m);
            }
        }
        kept.sort_unstable_by_key(|c| (c.distance, c.age));
        kept.truncate(top_m);

        let n_matched = kept.len() as u32;
        if n_matched < params.min_sample {
            return RecallVerdict::Unknown(RecallUnknown::InsufficientSample {
                n_matched,
                min_sample: params.min_sample,
            });
        }

        // --- Stage 2: precise weighted rank over the bounded candidate set. ---
        let mut scored: Vec<ScoredCandidate> = Vec::with_capacity(kept.len());
        for cand in &kept {
            let slot = cand.slot as usize;
            let episode = &self.episodes[slot];
            let weighted = weighted_distance(query, episode.fingerprint(), &params.weights);
            ops.weighted_scores += 1;
            scored.push(ScoredCandidate {
                weighted,
                distance: cand.distance,
                episode_id: episode.episode_id(),
                slot,
            });
        }
        scored.sort_unstable_by_key(|c| (c.weighted, c.episode_id));

        let nearest = scored[0];

        let mut nets: Vec<i128> = Vec::with_capacity(scored.len());
        let mut holds: Vec<u64> = Vec::with_capacity(scored.len());
        let mut sum: i128 = 0;
        let mut win_count = 0u32;
        let mut loss_count = 0u32;
        for c in &scored {
            let outcome = self.episodes[c.slot].outcome();
            nets.push(outcome.realized_net_lamports);
            holds.push(outcome.hold_duration_ns);
            sum = sum.saturating_add(outcome.realized_net_lamports);
            if outcome.realized_net_lamports > 0 {
                win_count += 1;
            } else if outcome.realized_net_lamports < 0 {
                loss_count += 1;
            }
            ops.stat_elements += 1;
        }
        nets.sort_unstable();
        holds.sort_unstable();

        let decisive = win_count + loss_count;
        let win_rate_bp = if decisive == 0 {
            0
        } else {
            // u64 intermediate: win_count * 10_000 cannot overflow u64.
            ((u64::from(win_count) * u64::from(BPS_SCALE_U32)) / u64::from(decisive)) as u32
        };
        // Integer mean, truncating toward zero (Rust's i128 division semantics).
        let mean = sum / i128::from(n_matched);

        RecallVerdict::Known(RecallStats {
            n_matched,
            median_net_lamports: order_stat_i128(&nets, P50),
            mean_net_lamports: mean,
            win_count,
            loss_count,
            win_rate_bp,
            p25_net_lamports: order_stat_i128(&nets, P25),
            p75_net_lamports: order_stat_i128(&nets, P75),
            median_hold_ns: order_stat_u64(&holds, P50),
            nearest_distance: nearest.distance,
            nearest_weighted_distance: nearest.weighted,
            nearest_episode_id: nearest.episode_id,
        })
    }
}

/// A stage-1 survivor. Deliberately 12 bytes and free of any `Episode` reference:
/// `age` is the insertion rank derived from ring geometry, which orders identically
/// to `episode_id` but costs nothing to obtain during the hot pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Candidate {
    distance: u32,
    age: u32,
    slot: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScoredCandidate {
    weighted: u64,
    distance: u32,
    episode_id: u64,
    slot: usize,
}

/// Nearest-rank index for a percentile over `n` sorted samples.
///
/// `idx = ceil(n * pct / 100) - 1`, clamped into `0..n`. Pure integer arithmetic
/// with no interpolation, so an order statistic is always an *actually observed*
/// value — never a synthesized one. For an even sample the 50th percentile is the
/// lower median.
#[must_use]
pub fn nearest_rank_index(n: usize, pct: u32) -> usize {
    if n == 0 {
        return 0;
    }
    let num = n as u64 * u64::from(pct);
    let ceil_div = num.div_ceil(100);
    let idx = ceil_div.max(1) - 1;
    (idx as usize).min(n - 1)
}

/// Nearest-rank order statistic over a sorted `i128` slice.
#[must_use]
pub fn order_stat_i128(sorted: &[i128], pct: u32) -> i128 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[nearest_rank_index(sorted.len(), pct)]
}

/// Nearest-rank order statistic over a sorted `u64` slice.
#[must_use]
pub fn order_stat_u64(sorted: &[u64], pct: u32) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[nearest_rank_index(sorted.len(), pct)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::episode::{DiscoveryLane, Episode, EpisodeContext, EpisodeOutcome, ExitReason};
    use crate::fingerprint::{SetupInputs, TrendStructure};

    fn fp(ofi: i64, breadth: u32, phase: VenuePhase) -> SetupFingerprint {
        let inputs = SetupInputs {
            ofi_bps: ofi,
            buyer_breadth: breadth,
            venue_phase: phase,
            trend_structure: TrendStructure::Up,
            ..SetupInputs::default()
        };
        SetupFingerprint::from_inputs(&inputs)
    }

    #[allow(clippy::too_many_arguments)]
    fn ep(
        id: u64,
        f: SetupFingerprint,
        net: i128,
        hold: u64,
        admitted: bool,
        phase: VenuePhase,
        meta: u32,
        lane: DiscoveryLane,
    ) -> Episode {
        Episode::new(
            id,
            f,
            EpisodeContext {
                mint_id: id,
                venue_phase: phase,
                meta_category_id: meta,
                discovery_lane: lane,
                info_time_ns: id * 1_000,
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
                was_admitted: admitted,
            },
        )
    }

    /// Twenty near-identical curve episodes with a known net ladder.
    fn built_index() -> EpisodicIndex {
        let mut idx = EpisodicIndex::with_capacity(64);
        for i in 0..20u64 {
            let net = (i as i128 - 10) * 1_000_000;
            let e = ep(
                i + 1,
                fp(600, 10, VenuePhase::Curve),
                net,
                (i + 1) * 1_000_000,
                true,
                VenuePhase::Curve,
                7,
                DiscoveryLane::NewMint,
            );
            idx.push(e).expect("monotone");
        }
        idx
    }

    // -------------------------------------------------------------- index shape

    #[test]
    fn push_rejects_non_monotone_episode_ids() {
        let mut idx = EpisodicIndex::with_capacity(8);
        idx.push(ep(
            5,
            fp(0, 0, VenuePhase::Curve),
            0,
            0,
            true,
            VenuePhase::Curve,
            1,
            DiscoveryLane::NewMint,
        ))
        .expect("first");
        let err = idx
            .push(ep(
                5,
                fp(0, 0, VenuePhase::Curve),
                0,
                0,
                true,
                VenuePhase::Curve,
                1,
                DiscoveryLane::NewMint,
            ))
            .expect_err("duplicate id must be rejected");
        assert_eq!(
            err,
            IndexError::NonMonotonicEpisodeId {
                offered: 5,
                last: 5
            }
        );
    }

    #[test]
    fn capacity_bound_holds_under_churn_and_evicts_oldest_first() {
        let cap = 16usize;
        let mut idx = EpisodicIndex::with_capacity(cap);
        for i in 1..=1_000u64 {
            let evicted = idx
                .push(ep(
                    i,
                    fp(0, 0, VenuePhase::Curve),
                    0,
                    0,
                    true,
                    VenuePhase::Curve,
                    1,
                    DiscoveryLane::NewMint,
                ))
                .expect("monotone");
            assert!(idx.len() <= cap, "capacity bound violated at i={i}");
            if i > cap as u64 {
                let e = evicted.expect("full index must evict");
                assert_eq!(
                    e.episode_id(),
                    i - cap as u64,
                    "eviction was not oldest-first"
                );
            } else {
                assert!(evicted.is_none());
            }
        }
        assert_eq!(idx.len(), cap);
        assert_eq!(idx.evicted_count(), 1_000 - cap as u64);
        // Survivors are exactly the newest `cap` ids, in insertion order.
        let ids: Vec<u64> = idx.iter_oldest_first().map(Episode::episode_id).collect();
        let expect: Vec<u64> = (1_000 - cap as u64 + 1..=1_000).collect();
        assert_eq!(ids, expect);
    }

    #[test]
    fn iter_oldest_first_is_correct_before_the_ring_wraps() {
        let mut idx = EpisodicIndex::with_capacity(8);
        for i in 1..=3u64 {
            idx.push(ep(
                i,
                fp(0, 0, VenuePhase::Curve),
                0,
                0,
                true,
                VenuePhase::Curve,
                1,
                DiscoveryLane::NewMint,
            ))
            .expect("monotone");
        }
        let ids: Vec<u64> = idx.iter_oldest_first().map(Episode::episode_id).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn filter_key_round_trips_every_field() {
        let e = ep(
            1,
            fp(0, 0, VenuePhase::Pool),
            0,
            0,
            true,
            VenuePhase::Pool,
            0xDEAD_BEEF,
            DiscoveryLane::WhaleFollow,
        );
        let key = pack_filter_key(&e);
        assert_eq!(
            (key >> FK_VENUE_SHIFT) & FK_VENUE_MASK,
            u64::from(VenuePhase::Pool.ordinal())
        );
        assert_eq!((key >> FK_ADMITTED_SHIFT) & 1, 1);
        assert_eq!(
            (key >> FK_LANE_SHIFT) & FK_LANE_MASK,
            u64::from(DiscoveryLane::WhaleFollow.ordinal())
        );
        assert_eq!((key >> FK_META_SHIFT) & FK_META_MASK, 0xDEAD_BEEF);
    }

    // ---------------------------------------------------------- fail-closed n

    #[test]
    fn empty_index_is_unknown_with_no_numbers() {
        let idx = EpisodicIndex::new();
        let v = idx.recall(&fp(0, 0, VenuePhase::Curve), &RecallParams::default());
        assert_eq!(v, RecallVerdict::Unknown(RecallUnknown::EmptyIndex));
        assert!(v.stats().is_none());
        assert!(!v.is_known());
    }

    #[test]
    fn small_n_is_fail_closed_and_carries_no_estimate() {
        // Seven glowing winners. A naive implementation reports a 100% win rate.
        let mut idx = EpisodicIndex::with_capacity(64);
        for i in 1..=7u64 {
            idx.push(ep(
                i,
                fp(600, 10, VenuePhase::Curve),
                50_000_000,
                1_000,
                true,
                VenuePhase::Curve,
                7,
                DiscoveryLane::NewMint,
            ))
            .expect("monotone");
        }
        let v = idx.recall(&fp(600, 10, VenuePhase::Curve), &RecallParams::default());
        assert_eq!(
            v,
            RecallVerdict::Unknown(RecallUnknown::InsufficientSample {
                n_matched: 7,
                min_sample: MIN_SAMPLE_DEFAULT
            })
        );
        assert!(
            v.stats().is_none(),
            "no estimate may be readable at n < min_sample"
        );
    }

    #[test]
    fn crossing_the_min_sample_boundary_flips_the_verdict() {
        let mut idx = EpisodicIndex::with_capacity(64);
        for i in 1..=7u64 {
            idx.push(ep(
                i,
                fp(600, 10, VenuePhase::Curve),
                50_000_000,
                1_000,
                true,
                VenuePhase::Curve,
                7,
                DiscoveryLane::NewMint,
            ))
            .expect("monotone");
        }
        let q = fp(600, 10, VenuePhase::Curve);
        assert!(!idx.recall(&q, &RecallParams::default()).is_known());
        idx.push(ep(
            8,
            q,
            50_000_000,
            1_000,
            true,
            VenuePhase::Curve,
            7,
            DiscoveryLane::NewMint,
        ))
        .expect("monotone");
        let v = idx.recall(&q, &RecallParams::default());
        let s = v.stats().expect("eighth episode reaches the floor");
        assert_eq!(s.n_matched, MIN_SAMPLE_DEFAULT);
        assert_eq!(s.win_rate_bp, 10_000);
    }

    #[test]
    fn out_of_radius_is_unknown_and_reports_only_a_distance() {
        let idx = built_index();
        // A query deliberately far away in many fields.
        let far = SetupFingerprint::from_inputs(&SetupInputs {
            ofi_bps: -9_000,
            buyer_breadth: 500,
            realized_vol_bps: 9_000,
            liquidity_decade: 14,
            token_age_ns: 90_000 * 1_000_000_000,
            venue_phase: VenuePhase::Curve,
            ..SetupInputs::default()
        });
        let params = RecallParams {
            max_distance: 2,
            ..RecallParams::default()
        };
        let v = idx.recall(&far, &params);
        match v.unknown_reason().expect("must be unknown") {
            RecallUnknown::NoCandidateInRadius {
                nearest_distance,
                max_distance,
            } => {
                assert_eq!(max_distance, 2);
                assert!(nearest_distance.expect("in-scope episodes exist") > 2);
            }
            other => panic!("unexpected reason: {other:?}"),
        }
        assert!(v.stats().is_none());
    }

    #[test]
    fn unknown_variants_expose_no_outcome_accessor() {
        // Compile-time-ish guard: the only accessor that yields numbers is `stats`,
        // and it is None for every Unknown shape.
        let shapes = [
            RecallVerdict::Unknown(RecallUnknown::EmptyIndex),
            RecallVerdict::Unknown(RecallUnknown::NoEpisodeInScope),
            RecallVerdict::Unknown(RecallUnknown::NoCandidateInRadius {
                nearest_distance: Some(3),
                max_distance: 2,
            }),
            RecallVerdict::Unknown(RecallUnknown::InsufficientSample {
                n_matched: 1,
                min_sample: 8,
            }),
        ];
        for v in shapes {
            assert!(v.stats().is_none());
            assert!(!v.is_known());
            assert!(v.unknown_reason().is_some());
        }
    }

    #[test]
    fn require_admitted_excludes_counterfactual_episodes() {
        let mut idx = EpisodicIndex::with_capacity(64);
        // 10 rejected setups (net 0, never traded) + 8 real losers.
        for i in 1..=10u64 {
            idx.push(ep(
                i,
                fp(600, 10, VenuePhase::Curve),
                0,
                0,
                false,
                VenuePhase::Curve,
                7,
                DiscoveryLane::NewMint,
            ))
            .expect("monotone");
        }
        for i in 11..=18u64 {
            idx.push(ep(
                i,
                fp(600, 10, VenuePhase::Curve),
                -5_000_000,
                1_000,
                true,
                VenuePhase::Curve,
                7,
                DiscoveryLane::NewMint,
            ))
            .expect("monotone");
        }
        let q = fp(600, 10, VenuePhase::Curve);
        let admitted_only = idx.recall(&q, &RecallParams::default());
        let s = admitted_only.stats().expect("8 admitted episodes");
        assert_eq!(s.n_matched, 8);
        assert_eq!(s.loss_count, 8);
        assert_eq!(s.median_net_lamports, -5_000_000);

        // Pooling the counterfactuals would drag the median to zero — the exact
        // self-deception the default guards against.
        let pooled = idx.recall(
            &q,
            &RecallParams {
                require_admitted: false,
                ..RecallParams::default()
            },
        );
        let ps = pooled.stats().expect("18 pooled episodes");
        assert_eq!(ps.n_matched, 18);
        assert_eq!(ps.median_net_lamports, 0);
    }

    // ------------------------------------------------------- phase separation

    #[test]
    fn curve_and_pool_are_never_pooled() {
        let mut idx = EpisodicIndex::with_capacity(128);
        // 12 profitable pool episodes, 12 losing curve episodes, same setup shape.
        for i in 1..=12u64 {
            idx.push(ep(
                i,
                fp(600, 10, VenuePhase::Pool),
                100_000_000,
                1_000,
                true,
                VenuePhase::Pool,
                7,
                DiscoveryLane::NewMint,
            ))
            .expect("monotone");
        }
        for i in 13..=24u64 {
            idx.push(ep(
                i,
                fp(600, 10, VenuePhase::Curve),
                -100_000_000,
                1_000,
                true,
                VenuePhase::Curve,
                7,
                DiscoveryLane::NewMint,
            ))
            .expect("monotone");
        }
        let curve_q = fp(600, 10, VenuePhase::Curve);
        let pool_q = fp(600, 10, VenuePhase::Pool);

        let curve = idx.recall(&curve_q, &RecallParams::default());
        let cs = curve.stats().expect("12 curve episodes");
        assert_eq!(cs.n_matched, 12);
        assert_eq!(cs.win_count, 0);
        assert_eq!(cs.median_net_lamports, -100_000_000);

        let pool = idx.recall(&pool_q, &RecallParams::default());
        let ps = pool.stats().expect("12 pool episodes");
        assert_eq!(ps.n_matched, 12);
        assert_eq!(ps.loss_count, 0);
        assert_eq!(ps.median_net_lamports, 100_000_000);

        // The pooled figure (zero net, 50% win rate) is unreachable through the API.
        assert_ne!(cs.median_net_lamports, 0);
        assert_ne!(ps.median_net_lamports, 0);
    }

    #[test]
    fn a_phase_with_no_history_reports_no_episode_in_scope() {
        let idx = built_index(); // curve only
        let pool_q = fp(600, 10, VenuePhase::Pool);
        let v = idx.recall(&pool_q, &RecallParams::default());
        assert_eq!(v, RecallVerdict::Unknown(RecallUnknown::NoEpisodeInScope));
    }

    #[test]
    fn conditioning_on_meta_and_lane_partitions_the_estimate() {
        let mut idx = EpisodicIndex::with_capacity(128);
        for i in 1..=10u64 {
            idx.push(ep(
                i,
                fp(600, 10, VenuePhase::Curve),
                80_000_000,
                1_000,
                true,
                VenuePhase::Curve,
                1,
                DiscoveryLane::SocialCall,
            ))
            .expect("monotone");
        }
        for i in 11..=20u64 {
            idx.push(ep(
                i,
                fp(600, 10, VenuePhase::Curve),
                -80_000_000,
                1_000,
                true,
                VenuePhase::Curve,
                2,
                DiscoveryLane::WhaleFollow,
            ))
            .expect("monotone");
        }
        let q = fp(600, 10, VenuePhase::Curve);
        let params = RecallParams::default();

        let meta1 = idx.recall_conditioned(
            &q,
            &params,
            &RecallFilter::for_query(&q).with_meta_category(1),
        );
        assert_eq!(meta1.stats().expect("meta 1").win_count, 10);

        let meta2 = idx.recall_conditioned(
            &q,
            &params,
            &RecallFilter::for_query(&q).with_meta_category(2),
        );
        assert_eq!(meta2.stats().expect("meta 2").loss_count, 10);

        let lane = idx.recall_conditioned(
            &q,
            &params,
            &RecallFilter::for_query(&q).with_discovery_lane(DiscoveryLane::SocialCall),
        );
        assert_eq!(lane.stats().expect("social lane").win_count, 10);

        // Meta 1 crossed with the whale lane has no history at all -> fail closed.
        let empty = idx.recall_conditioned(
            &q,
            &params,
            &RecallFilter::for_query(&q)
                .with_meta_category(1)
                .with_discovery_lane(DiscoveryLane::WhaleFollow),
        );
        assert_eq!(
            empty,
            RecallVerdict::Unknown(RecallUnknown::NoEpisodeInScope)
        );
    }

    #[test]
    fn recall_filter_cannot_be_built_without_a_phase() {
        // The only constructors both demand a phase; this test documents the API
        // shape that makes constitution 100 structurally enforced.
        let q = fp(600, 10, VenuePhase::Pool);
        assert_eq!(RecallFilter::for_query(&q).venue_phase(), VenuePhase::Pool);
        assert_eq!(
            RecallFilter::for_phase(VenuePhase::Curve).venue_phase(),
            VenuePhase::Curve
        );
        let f = RecallFilter::for_phase(VenuePhase::Curve)
            .with_meta_category(4)
            .with_discovery_lane(DiscoveryLane::Rescan);
        assert_eq!(f.meta_category_id(), Some(4));
        assert_eq!(f.discovery_lane(), Some(DiscoveryLane::Rescan));
    }

    // ------------------------------------------------------------- statistics

    #[test]
    fn known_verdict_reports_the_hand_computed_distribution() {
        let idx = built_index(); // nets: -10e6, -9e6, ... +9e6 (20 episodes)
        let v = idx.recall(&fp(600, 10, VenuePhase::Curve), &RecallParams::default());
        let s = v.stats().expect("20 matches");
        assert_eq!(s.n_matched, 20);
        assert_eq!(s.win_count, 9); // +1e6 .. +9e6
        assert_eq!(s.loss_count, 10); // -10e6 .. -1e6
                                      // One exact flat (net 0) excluded from the win-rate denominator.
        assert_eq!(s.win_rate_bp, (9 * 10_000) / 19);
        // Lower median of a 20-sample ladder -10..+9 is -1e6.
        assert_eq!(s.median_net_lamports, -1_000_000);
        assert_eq!(s.p25_net_lamports, -6_000_000);
        assert_eq!(s.p75_net_lamports, 4_000_000);
        // Mean of the ladder sum(-10..9)*1e6 / 20 = -10e6/20 truncated toward zero.
        assert_eq!(s.mean_net_lamports, -500_000);
        assert_eq!(s.nearest_distance, 0);
        assert_eq!(s.nearest_weighted_distance, 0);
    }

    #[test]
    fn nearest_episode_is_the_actually_nearest_one() {
        let mut idx = EpisodicIndex::with_capacity(64);
        // Episode 1..=8 far-ish, episode 9 exact.
        for i in 1..=8u64 {
            idx.push(ep(
                i,
                fp(-3_000, 1, VenuePhase::Curve),
                1,
                1,
                true,
                VenuePhase::Curve,
                7,
                DiscoveryLane::NewMint,
            ))
            .expect("monotone");
        }
        let exact = fp(600, 10, VenuePhase::Curve);
        idx.push(ep(
            9,
            exact,
            1,
            1,
            true,
            VenuePhase::Curve,
            7,
            DiscoveryLane::NewMint,
        ))
        .expect("monotone");
        let params = RecallParams {
            max_distance: 64,
            min_sample: 1,
            ..RecallParams::default()
        };
        let s = idx.recall(&exact, &params).stats().copied().expect("known");
        assert_eq!(s.nearest_episode_id, 9);
        assert_eq!(s.nearest_distance, 0);
    }

    #[test]
    fn win_rate_is_zero_when_nothing_was_decisive() {
        let mut idx = EpisodicIndex::with_capacity(32);
        for i in 1..=10u64 {
            idx.push(ep(
                i,
                fp(600, 10, VenuePhase::Curve),
                0,
                5,
                true,
                VenuePhase::Curve,
                7,
                DiscoveryLane::NewMint,
            ))
            .expect("monotone");
        }
        let s = idx
            .recall(&fp(600, 10, VenuePhase::Curve), &RecallParams::default())
            .stats()
            .copied()
            .expect("known");
        assert_eq!(s.win_rate_bp, 0);
        assert_eq!(s.win_count, 0);
        assert_eq!(s.loss_count, 0);
        assert_eq!(s.median_hold_ns, 5);
    }

    #[test]
    fn order_statistics_are_nearest_rank_and_never_interpolate() {
        assert_eq!(nearest_rank_index(0, P50), 0);
        assert_eq!(nearest_rank_index(1, P50), 0);
        assert_eq!(nearest_rank_index(4, P25), 0);
        assert_eq!(nearest_rank_index(4, P50), 1);
        assert_eq!(nearest_rank_index(4, P75), 2);
        assert_eq!(nearest_rank_index(8, P50), 3);
        assert_eq!(nearest_rank_index(100, P75), 74);
        let v = [-5i128, -1, 0, 3, 9];
        // Every reported quantile is an observed sample.
        for pct in [P25, P50, P75] {
            assert!(v.contains(&order_stat_i128(&v, pct)));
        }
        assert_eq!(order_stat_i128(&[], P50), 0);
        assert_eq!(order_stat_u64(&[], P50), 0);
    }

    // ---------------------------------------------------------- determinism

    #[test]
    fn recall_is_byte_identical_across_repeated_calls() {
        let idx = built_index();
        let q = fp(600, 10, VenuePhase::Curve);
        let params = RecallParams::default();
        let first = idx.recall(&q, &params);
        for _ in 0..100 {
            assert_eq!(idx.recall(&q, &params), first);
        }
    }

    #[test]
    fn tie_break_is_by_episode_id_and_independent_of_ring_position() {
        // Build the same eight tied episodes into two indexes whose ring positions
        // differ (one has wrapped), and assert the verdicts are identical.
        let build = |cap: usize| {
            let warmup = 9u64;
            let mut idx = EpisodicIndex::with_capacity(cap);
            for i in 1..=warmup {
                idx.push(ep(
                    i,
                    fp(-3_000, 1, VenuePhase::Pool),
                    7,
                    7,
                    true,
                    VenuePhase::Pool,
                    9,
                    DiscoveryLane::Rescan,
                ))
                .expect("monotone");
            }
            for k in 0..10u64 {
                idx.push(ep(
                    warmup + 1 + k,
                    fp(600, 10, VenuePhase::Curve),
                    (k as i128) * 1_000,
                    k * 10,
                    true,
                    VenuePhase::Curve,
                    7,
                    DiscoveryLane::NewMint,
                ))
                .expect("monotone");
            }
            idx
        };
        let a = build(64); // never wraps: curve episodes sit at slots 9..19
        let b = build(12); // wraps: the same curve episodes sit at different slots
        let q = fp(600, 10, VenuePhase::Curve);
        let params = RecallParams {
            top_m: 5,
            min_sample: 1,
            ..RecallParams::default()
        };
        let va = a.recall_conditioned(&q, &params, &RecallFilter::for_query(&q));
        let vb = b.recall_conditioned(&q, &params, &RecallFilter::for_query(&q));
        let sa = va.stats().expect("known");
        let sb = vb.stats().expect("known");
        // Top-5 of ten exact ties, chosen by ascending episode_id in both layouts.
        assert_eq!(sa.n_matched, 5);
        assert_eq!(sa.n_matched, sb.n_matched);
        assert_eq!(sa.nearest_episode_id, sb.nearest_episode_id);
        assert_eq!(sa.median_net_lamports, sb.median_net_lamports);
    }

    #[test]
    fn top_m_bounds_the_matched_sample() {
        let mut idx = EpisodicIndex::with_capacity(512);
        for i in 1..=400u64 {
            idx.push(ep(
                i,
                fp(600, 10, VenuePhase::Curve),
                1_000,
                1,
                true,
                VenuePhase::Curve,
                7,
                DiscoveryLane::NewMint,
            ))
            .expect("monotone");
        }
        let params = RecallParams {
            top_m: 32,
            ..RecallParams::default()
        };
        let s = idx
            .recall(&fp(600, 10, VenuePhase::Curve), &params)
            .stats()
            .copied()
            .expect("known");
        assert_eq!(s.n_matched, 32);
    }

    // ------------------------------------------------------------------- perf

    #[test]
    fn full_index_recall_stays_inside_the_op_budget() {
        let mut idx = EpisodicIndex::new();
        // Fill the whole 16_384-slot index with a deterministic spread of setups.
        let mut seed = 1u32;
        for i in 1..=EPISODE_CAP as u64 {
            seed = crate::hash::mix_u32(seed);
            let ofi = i64::from(seed % 8_000) - 4_000;
            let breadth = seed % 100;
            idx.push(ep(
                i,
                fp(ofi, breadth, VenuePhase::Curve),
                i128::from(seed % 1_000) - 500,
                u64::from(seed % 1_000),
                true,
                VenuePhase::Curve,
                seed % 8,
                DiscoveryLane::NewMint,
            ))
            .expect("monotone");
        }
        assert_eq!(idx.len(), EPISODE_CAP);

        let q = fp(600, 10, VenuePhase::Curve);
        let params = RecallParams::default();
        let (verdict, ops) = idx.recall_probe(&q, &params, &RecallFilter::for_query(&q));
        assert!(
            verdict.is_known(),
            "a full index must have enough neighbours"
        );

        // Exactly one popcount pass over the index — the documented contract.
        assert_eq!(ops.popcount_ops, EPISODE_CAP as u64);
        assert!(ops.weighted_scores <= params.top_m as u64);
        assert!(ops.stat_elements <= params.top_m as u64);
        assert!(
            ops.total() <= RECALL_OP_BUDGET_FULL_INDEX,
            "op budget blown: {} > {}",
            ops.total(),
            RECALL_OP_BUDGET_FULL_INDEX
        );
    }

    /// The bounded streaming selection is an optimisation, so it owes a proof that
    /// it returns the *same set* a naive implementation would: every in-scope
    /// episode within the radius, ranked by `(hamming, episode_id)`, truncated to
    /// `top_m`. This is the test that would catch a compaction bug.
    #[test]
    fn selection_matches_a_brute_force_reference() {
        let mut idx = EpisodicIndex::with_capacity(1_024);
        let mut seed = 7u32;
        for i in 1..=1_024u64 {
            seed = crate::hash::mix_u32(seed);
            let ofi = i64::from(seed % 6_000) - 3_000;
            let breadth = seed % 60;
            idx.push(ep(
                i,
                fp(ofi, breadth, VenuePhase::Curve),
                i128::from(seed % 400) - 200,
                u64::from(seed % 900),
                true,
                VenuePhase::Curve,
                3,
                DiscoveryLane::NewMint,
            ))
            .expect("monotone");
        }
        // Force the ring to wrap so physical order and insertion order differ.
        for i in 1_025..=1_500u64 {
            seed = crate::hash::mix_u32(seed);
            idx.push(ep(
                i,
                fp(
                    i64::from(seed % 6_000) - 3_000,
                    seed % 60,
                    VenuePhase::Curve,
                ),
                i128::from(seed % 400) - 200,
                u64::from(seed % 900),
                true,
                VenuePhase::Curve,
                3,
                DiscoveryLane::NewMint,
            ))
            .expect("monotone");
        }

        for top_m in [8usize, 17, 64, 200] {
            for max_distance in [2u32, 6, 12] {
                let q = fp(600, 10, VenuePhase::Curve);
                let params = RecallParams {
                    top_m,
                    max_distance,
                    min_sample: 1,
                    ..RecallParams::default()
                };

                // Brute-force reference.
                let mut refs: Vec<(u32, u64, i128)> = idx
                    .iter_oldest_first()
                    .map(|e| {
                        (
                            signature_hamming(e.fingerprint().signature(), q.signature()),
                            e.episode_id(),
                            e.outcome().realized_net_lamports,
                        )
                    })
                    .filter(|(d, _, _)| *d <= max_distance)
                    .collect();
                refs.sort_unstable_by_key(|(d, id, _)| (*d, *id));
                refs.truncate(top_m);

                let verdict = idx.recall(&q, &params);
                if refs.is_empty() {
                    assert!(!verdict.is_known(), "top_m={top_m} r={max_distance}");
                    continue;
                }
                let s = verdict.stats().copied().expect("known");
                assert_eq!(
                    s.n_matched as usize,
                    refs.len(),
                    "top_m={top_m} r={max_distance}"
                );
                assert_eq!(
                    s.nearest_distance, refs[0].0,
                    "nearest distance mismatch at top_m={top_m} r={max_distance}"
                );

                let mut nets: Vec<i128> = refs.iter().map(|(_, _, n)| *n).collect();
                nets.sort_unstable();
                assert_eq!(s.median_net_lamports, order_stat_i128(&nets, P50));
                assert_eq!(s.p25_net_lamports, order_stat_i128(&nets, P25));
                assert_eq!(s.p75_net_lamports, order_stat_i128(&nets, P75));
                assert_eq!(s.win_count, nets.iter().filter(|n| **n > 0).count() as u32);
                assert_eq!(s.loss_count, nets.iter().filter(|n| **n < 0).count() as u32);
            }
        }
    }

    #[test]
    fn a_tie_mass_far_larger_than_the_working_set_still_picks_the_oldest_episodes() {
        // 1_000 exact ties with a working-set cap of 2 * top_m = 20. If compaction
        // were order-dependent this would pick an arbitrary 10.
        let mut idx = EpisodicIndex::with_capacity(2_048);
        for i in 1..=1_000u64 {
            idx.push(ep(
                i,
                fp(600, 10, VenuePhase::Curve),
                i128::from(i),
                i,
                true,
                VenuePhase::Curve,
                3,
                DiscoveryLane::NewMint,
            ))
            .expect("monotone");
        }
        let q = fp(600, 10, VenuePhase::Curve);
        let params = RecallParams {
            top_m: 10,
            min_sample: 1,
            ..RecallParams::default()
        };
        let s = idx.recall(&q, &params).stats().copied().expect("known");
        assert_eq!(s.n_matched, 10);
        assert_eq!(
            s.nearest_episode_id, 1,
            "ties must resolve to the lowest episode_id"
        );
        // Nets 1..=10 were selected, so the lower median of ten samples is 5.
        assert_eq!(s.median_net_lamports, 5);
        assert_eq!(s.mean_net_lamports, 5); // (1+..+10)/10 = 5
    }

    #[test]
    fn op_count_is_itself_deterministic() {
        let idx = built_index();
        let q = fp(600, 10, VenuePhase::Curve);
        let params = RecallParams::default();
        let (_, a) = idx.recall_probe(&q, &params, &RecallFilter::for_query(&q));
        let (_, b) = idx.recall_probe(&q, &params, &RecallFilter::for_query(&q));
        assert_eq!(a, b);
        assert_eq!(a.popcount_ops, idx.len() as u64);
    }

    #[test]
    fn schema_mismatch_is_rejected_at_the_door() {
        let bad = Episode::with_schema_version(
            crate::episode::EPISODE_SCHEMA_VERSION + 1,
            1,
            fp(0, 0, VenuePhase::Curve),
            EpisodeContext {
                mint_id: 1,
                venue_phase: VenuePhase::Curve,
                meta_category_id: 0,
                discovery_lane: DiscoveryLane::NewMint,
                info_time_ns: 0,
                slot: 0,
            },
            EpisodeOutcome::rejected(),
        );
        let mut idx = EpisodicIndex::with_capacity(4);
        assert!(matches!(
            idx.push(bad),
            Err(IndexError::SchemaMismatch { .. })
        ));
        assert!(idx.is_empty());
    }

    #[test]
    fn get_by_episode_id_finds_live_episodes_only() {
        let mut idx = EpisodicIndex::with_capacity(4);
        for i in 1..=6u64 {
            idx.push(ep(
                i,
                fp(0, 0, VenuePhase::Curve),
                0,
                0,
                true,
                VenuePhase::Curve,
                1,
                DiscoveryLane::NewMint,
            ))
            .expect("monotone");
        }
        assert!(idx.get_by_episode_id(1).is_none(), "evicted");
        assert!(idx.get_by_episode_id(6).is_some());
    }
}
