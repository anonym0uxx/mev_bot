//! §29.6 `AttentionStateReducer` — the full `AttentionState` struct + the six
//! required attention-state distinctions.
//!
//! [`AttentionSeries`](crate::AttentionSeries) mirrors only two of the ten
//! §29.6 AttentionState fields; this module folds timestamp-safe mention
//! observations into the complete ten-field state and classifies it into the six
//! attention-state distinctions, including the two that the lifecycle stage does
//! not cover — copycat attention and late exit-liquidity promotion.
//!
//! Hard invariants: §22 integer/fixed-point only; deterministic (time enters as
//! caller-supplied integer nanosecond instants); §99 bounded state — distinct
//! source/community counting uses a fixed-capacity ([`MAX_TRACKED`]) fold, never
//! a growing allocation. Beyond the cap, counts saturate and concentration is
//! reported over the tracked prefix (documented, never fabricated — §29.5).

use crate::narrative::{nv_attention_series, sat_u64, FP_ONE};

/// Fixed capacity for distinct source/community tracking (§99 bounded state).
///
/// The reducer allocates nothing on the heap; distinct ids are tracked in two
/// fixed arrays of this length. Streams with more than `MAX_TRACKED` distinct
/// ids saturate their unique counts at `MAX_TRACKED` and compute concentration
/// over the tracked prefix.
pub const MAX_TRACKED: usize = 64;

/// One timestamp-safe mention observation feeding the reducer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mention {
    /// Instant in nanoseconds on the caller's monotonic clock (smaller earlier).
    pub ts_ns: u64,
    /// Opaque source id (author/account).
    pub source_id: u64,
    /// Opaque community id (server/group).
    pub community_id: u64,
    /// Engagement weight of this mention (e.g. reach × quality), integer.
    pub weight: u64,
    /// Whether this mention is a detected copycat of an earlier one.
    pub copycat: bool,
}

/// The complete §29.6 ten-field `AttentionState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttentionState {
    /// Distinct sources observed (saturating at [`MAX_TRACKED`]).
    pub unique_sources: u32,
    /// Distinct communities observed (saturating at [`MAX_TRACKED`]).
    pub unique_communities: u32,
    /// Sum of weights of mentions in the trailing 1-minute window.
    pub weighted_mentions_1m: u64,
    /// Sum of weights of mentions in the trailing 5-minute window.
    pub weighted_mentions_5m: u64,
    /// Engagement velocity (first difference of the level series), saturating.
    pub engagement_velocity: i64,
    /// Engagement acceleration (second difference), saturating.
    pub engagement_acceleration: i64,
    /// Source concentration in `[0, FP_ONE]`: top source's weight share.
    pub source_concentration: u64,
    /// Narrative age in ns: `now − first_mention` (saturating), `0` if no data.
    pub narrative_age_ns: u64,
    /// Count of copycat-flagged mentions.
    pub copycat_count: u32,
    /// Freshness in `[0, FP_ONE]`: `FP_ONE` at age 0, linearly to `0` at
    /// `freshness_full_ns` (and beyond).
    pub freshness: u64,
}

/// Fixed-capacity distinct-id tracker with optional per-slot weight accumulation.
struct DistinctTracker {
    ids: [u64; MAX_TRACKED],
    weights: [u64; MAX_TRACKED],
    len: usize,
    /// Total weight seen (including ids beyond the cap).
    total_weight: u128,
}

impl DistinctTracker {
    fn new() -> Self {
        Self {
            ids: [0; MAX_TRACKED],
            weights: [0; MAX_TRACKED],
            len: 0,
            total_weight: 0,
        }
    }

    /// Record `id` with `weight`; adds a slot if new and capacity remains.
    fn observe(&mut self, id: u64, weight: u64) {
        self.total_weight += weight as u128;
        for i in 0..self.len {
            if self.ids[i] == id {
                self.weights[i] = self.weights[i].saturating_add(weight);
                return;
            }
        }
        if self.len < MAX_TRACKED {
            self.ids[self.len] = id;
            self.weights[self.len] = weight;
            self.len += 1;
        }
        // Beyond capacity: id is not tracked as distinct (count saturates), but
        // its weight is still in total_weight so concentration stays a lower
        // bound rather than an overstatement.
    }

    fn unique(&self) -> u32 {
        self.len as u32
    }

    /// Top-slot weight share in `[0, FP_ONE]` (0 when no weight observed).
    fn concentration_fp(&self) -> u64 {
        if self.total_weight == 0 {
            return 0;
        }
        let top = self.weights[..self.len].iter().copied().max().unwrap_or(0);
        sat_u64(top as u128 * FP_ONE as u128 / self.total_weight).min(FP_ONE)
    }
}

/// Fold mention observations + a level series into the full [`AttentionState`].
///
/// `now_ns` is the evaluation instant; `window_1m_ns` / `window_5m_ns` are the
/// trailing weighted-mention windows (each counts mentions with
/// `ts_ns > now_ns − window` and `ts_ns <= now_ns`). `level_series` (oldest→
/// newest) with `series_window` feeds [`nv_attention_series`] for velocity /
/// acceleration; when the series is too short both are `0` (§29.5 no fabrication).
/// `freshness_full_ns` is the age at which freshness decays to `0` (`0` disables
/// freshness, yielding `0`). Pure/total; bounded memory (§99). Overflow saturates.
pub fn nv_attention_state(
    mentions: &[Mention],
    now_ns: u64,
    window_1m_ns: u64,
    window_5m_ns: u64,
    level_series: &[u64],
    series_window: usize,
    freshness_full_ns: u64,
) -> AttentionState {
    let lo_1m = now_ns.saturating_sub(window_1m_ns);
    let lo_5m = now_ns.saturating_sub(window_5m_ns);

    let mut sources = DistinctTracker::new();
    let mut communities = DistinctTracker::new();
    let mut w1: u128 = 0;
    let mut w5: u128 = 0;
    let mut copycat_count: u32 = 0;
    let mut first_ns: Option<u64> = None;

    for m in mentions {
        sources.observe(m.source_id, m.weight);
        communities.observe(m.community_id, 0);
        if m.ts_ns > lo_1m && m.ts_ns <= now_ns {
            w1 += m.weight as u128;
        }
        if m.ts_ns > lo_5m && m.ts_ns <= now_ns {
            w5 += m.weight as u128;
        }
        if m.copycat {
            copycat_count = copycat_count.saturating_add(1);
        }
        first_ns = Some(match first_ns {
            Some(f) => f.min(m.ts_ns),
            None => m.ts_ns,
        });
    }

    let (engagement_velocity, engagement_acceleration) =
        match nv_attention_series(level_series, series_window) {
            Some(s) => (s.velocity, s.acceleration),
            None => (0, 0),
        };

    let narrative_age_ns = match first_ns {
        Some(f) => now_ns.saturating_sub(f),
        None => 0,
    };

    let freshness = if freshness_full_ns == 0 || first_ns.is_none() {
        // No data (§29.5) or disabled scale => no fabricated freshness.
        0
    } else {
        // Linear decay: FP_ONE at age 0, 0 at freshness_full_ns.
        let penalty =
            sat_u64(narrative_age_ns as u128 * FP_ONE as u128 / freshness_full_ns as u128);
        FP_ONE.saturating_sub(penalty.min(FP_ONE))
    };

    AttentionState {
        unique_sources: sources.unique(),
        unique_communities: communities.unique(),
        weighted_mentions_1m: sat_u64(w1),
        weighted_mentions_5m: sat_u64(w5),
        engagement_velocity,
        engagement_acceleration,
        source_concentration: sources.concentration_fp(),
        narrative_age_ns,
        copycat_count,
        freshness,
    }
}

/// The six required §29.6 attention-state distinctions.
///
/// Extends [`LifecycleStage`](crate::LifecycleStage) with the two distinctions it
/// cannot express: copycat attention and late exit-liquidity promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionDistinction {
    /// No mentions observed — §29.5 fade-first no-data state.
    NoSignal,
    /// Broad, independent, rising attention — genuine organic emergence.
    OrganicEmergence,
    /// Attention dominated by copycat/duplicate mentions.
    CopycatAttention,
    /// Attention rising while net flow is exiting — exit-liquidity promotion.
    LateExitLiquidityPromotion,
    /// Attention no longer rising but not yet falling — saturating.
    SaturatedAttention,
    /// Engagement velocity negative — decaying.
    DecayingAttention,
}

/// Classify an [`AttentionState`] into one of the six distinctions.
///
/// Ordered, total (first match wins); `net_flow` is the concurrent on-chain net
/// SOL flow (sign matters); `copycat_share_threshold_fp` is the copycat fraction
/// (`copycat_count / total_mentions`, fixed-point) at/above which the state is
/// copycat-dominated; `total_mentions` is the observation count backing the
/// state; `min_sources` is the breadth required to call rising attention organic.
/// Rules:
/// 1. `total_mentions == 0` → [`AttentionDistinction::NoSignal`].
/// 2. velocity `> 0` and `net_flow < 0` → [`AttentionDistinction::LateExitLiquidityPromotion`].
/// 3. copycat share ≥ threshold → [`AttentionDistinction::CopycatAttention`].
/// 4. velocity `< 0` → [`AttentionDistinction::DecayingAttention`].
/// 5. velocity `> 0` and `unique_sources ≥ min_sources` →
///    [`AttentionDistinction::OrganicEmergence`].
/// 6. otherwise → [`AttentionDistinction::SaturatedAttention`].
pub fn nv_attention_distinction(
    st: &AttentionState,
    net_flow: i64,
    copycat_share_threshold_fp: u64,
    total_mentions: u32,
    min_sources: u32,
) -> AttentionDistinction {
    if total_mentions == 0 {
        return AttentionDistinction::NoSignal;
    }
    if st.engagement_velocity > 0 && net_flow < 0 {
        return AttentionDistinction::LateExitLiquidityPromotion;
    }
    // copycat_count / total_mentions in fixed-point (u128 to avoid overflow).
    let copycat_share = st.copycat_count as u128 * FP_ONE as u128 / total_mentions as u128;
    if copycat_share >= copycat_share_threshold_fp as u128 {
        return AttentionDistinction::CopycatAttention;
    }
    if st.engagement_velocity < 0 {
        return AttentionDistinction::DecayingAttention;
    }
    if st.engagement_velocity > 0 && st.unique_sources >= min_sources {
        return AttentionDistinction::OrganicEmergence;
    }
    AttentionDistinction::SaturatedAttention
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(ts_ns: u64, source_id: u64, community_id: u64, weight: u64, copycat: bool) -> Mention {
        Mention {
            ts_ns,
            source_id,
            community_id,
            weight,
            copycat,
        }
    }

    #[test]
    fn empty_state_is_neutral() {
        let st = nv_attention_state(&[], 1_000, 60, 300, &[], 1, 1_000);
        assert_eq!(st.unique_sources, 0);
        assert_eq!(st.unique_communities, 0);
        assert_eq!(st.weighted_mentions_1m, 0);
        assert_eq!(st.weighted_mentions_5m, 0);
        assert_eq!(st.engagement_velocity, 0);
        assert_eq!(st.engagement_acceleration, 0);
        assert_eq!(st.source_concentration, 0);
        assert_eq!(st.narrative_age_ns, 0);
        assert_eq!(st.copycat_count, 0);
        assert_eq!(st.freshness, 0); // no first mention -> age 0 -> but no data
    }

    #[test]
    fn unique_counts_and_concentration() {
        let mentions = [
            m(100, 1, 10, 70, false),
            m(110, 1, 10, 10, false), // same source accumulates
            m(120, 2, 11, 10, true),
            m(130, 3, 11, 10, false),
        ];
        // now=200; windows large enough to include all.
        let st = nv_attention_state(&mentions, 200, 1_000, 1_000, &[], 1, 10_000);
        assert_eq!(st.unique_sources, 3);
        assert_eq!(st.unique_communities, 2);
        assert_eq!(st.copycat_count, 1);
        // total weight 100, top source (id 1) weight 80 -> 8_000 bps.
        assert_eq!(st.source_concentration, 8_000);
        assert_eq!(st.weighted_mentions_1m, 100);
        assert_eq!(st.narrative_age_ns, 200 - 100);
    }

    #[test]
    fn windows_partition_weighted_mentions() {
        // now=1_000. 1m window=(940,1000], 5m window=(700,1000].
        let mentions = [
            m(990, 1, 1, 5, false),  // in both
            m(950, 2, 1, 7, false),  // in both
            m(800, 3, 1, 11, false), // only 5m
            m(600, 4, 1, 13, false), // in neither
        ];
        let st = nv_attention_state(&mentions, 1_000, 60, 300, &[], 1, 10_000);
        assert_eq!(st.weighted_mentions_1m, 5 + 7);
        assert_eq!(st.weighted_mentions_5m, 5 + 7 + 11);
    }

    #[test]
    fn velocity_from_level_series() {
        // 2*window+1 = 5 samples; window=2. last=50, mid=30, first=10.
        let levels = [10, 20, 30, 40, 50];
        let st = nv_attention_state(&[m(1, 1, 1, 1, false)], 100, 60, 300, &levels, 2, 10_000);
        assert_eq!(st.engagement_velocity, 20); // 50-30
        assert_eq!(st.engagement_acceleration, 0); // (50-30)-(30-10)
    }

    #[test]
    fn freshness_linear_decay() {
        // age = now - first. first=100, now=600 -> age 500. full=1000.
        let st = nv_attention_state(&[m(100, 1, 1, 1, false)], 600, 60, 300, &[], 1, 1_000);
        // penalty = 500*FP_ONE/1000 = 5_000; freshness = FP_ONE-5_000 = 5_000.
        assert_eq!(st.freshness, 5_000);
    }

    #[test]
    fn freshness_zero_when_beyond_full_or_disabled() {
        let st = nv_attention_state(&[m(0, 1, 1, 1, false)], 5_000, 60, 300, &[], 1, 1_000);
        assert_eq!(st.freshness, 0); // age 5000 > full 1000
        let st = nv_attention_state(&[m(0, 1, 1, 1, false)], 100, 60, 300, &[], 1, 0);
        assert_eq!(st.freshness, 0); // disabled
    }

    #[test]
    fn distinct_tracker_saturates_at_cap() {
        // MAX_TRACKED+10 distinct sources -> unique saturates at MAX_TRACKED.
        let mut mentions = Vec::new();
        for i in 0..(MAX_TRACKED as u64 + 10) {
            mentions.push(m(i, i, 0, 1, false));
        }
        let st = nv_attention_state(&mentions, 1_000_000, 10, 10, &[], 1, 10_000);
        assert_eq!(st.unique_sources as usize, MAX_TRACKED);
        assert_eq!(st.unique_communities, 1);
    }

    fn state_with(velocity: i64, copycat_count: u32, unique_sources: u32) -> AttentionState {
        AttentionState {
            unique_sources,
            unique_communities: 1,
            weighted_mentions_1m: 0,
            weighted_mentions_5m: 0,
            engagement_velocity: velocity,
            engagement_acceleration: 0,
            source_concentration: 0,
            narrative_age_ns: 0,
            copycat_count,
            freshness: 0,
        }
    }

    #[test]
    fn distinction_no_signal() {
        let st = state_with(5, 0, 10);
        assert_eq!(
            nv_attention_distinction(&st, 100, 3_000, 0, 3),
            AttentionDistinction::NoSignal
        );
    }

    #[test]
    fn distinction_late_exit_beats_copycat() {
        // rising + net outflow, even with heavy copycat share -> late exit.
        let st = state_with(5, 100, 10);
        assert_eq!(
            nv_attention_distinction(&st, -1, 3_000, 100, 3),
            AttentionDistinction::LateExitLiquidityPromotion
        );
    }

    #[test]
    fn distinction_copycat() {
        // rising, positive flow, copycat share 50% >= 30% threshold.
        let st = state_with(5, 5, 10);
        assert_eq!(
            nv_attention_distinction(&st, 100, 3_000, 10, 3),
            AttentionDistinction::CopycatAttention
        );
    }

    #[test]
    fn distinction_decaying_organic_saturated() {
        // decaying
        let st = state_with(-5, 0, 10);
        assert_eq!(
            nv_attention_distinction(&st, 100, 3_000, 10, 3),
            AttentionDistinction::DecayingAttention
        );
        // organic: rising, broad, low copycat
        let st = state_with(5, 0, 10);
        assert_eq!(
            nv_attention_distinction(&st, 100, 3_000, 10, 3),
            AttentionDistinction::OrganicEmergence
        );
        // rising but too narrow -> saturated bucket
        let st = state_with(5, 0, 1);
        assert_eq!(
            nv_attention_distinction(&st, 100, 3_000, 10, 3),
            AttentionDistinction::SaturatedAttention
        );
        // flat velocity -> saturated
        let st = state_with(0, 0, 10);
        assert_eq!(
            nv_attention_distinction(&st, 100, 3_000, 10, 3),
            AttentionDistinction::SaturatedAttention
        );
    }
}
