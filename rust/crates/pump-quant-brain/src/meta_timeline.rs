//! Meta-lifecycle history — "what is the state of the meta this week, and does it
//! rhyme with one I have already been through?" (constitution 21.4).
//!
//! A memecoin meta is not a static category, it is a *trajectory*: it emerges,
//! goes hot, saturates, and decays, and the money is made on a specific stretch of
//! that curve. Knowing "this is the dog meta" is worth nothing. Knowing "this is
//! the dog meta at hour 30, breadth is flattening, and the last four metas that
//! looked exactly like this gave back everything over the following day" is the
//! whole trade.
//!
//! This module keeps a bounded, append-only timeline of [`MetaSnapshot`]s and
//! answers three questions:
//!
//! * [`MetaTimeline::current_meta_state`] — where is this meta right now?
//! * [`MetaTimeline::match_past_meta`] — which past metas resembled this state, and
//!   **what happened to them afterwards**?
//! * [`MetaTimeline::meta_lifecycle_stats`] — how long do these things last, and
//!   what does the decay look like?
//!
//! # Determinism and boundedness
//!
//! The timeline is a ring of [`META_SNAPSHOT_CAP`] snapshots with oldest-first
//! eviction (constitution 57/99). Snapshots must arrive in non-decreasing
//! information time — an out-of-order snapshot is rejected rather than silently
//! reordered, because a lifecycle whose arrow of time can be rewritten is not a
//! lifecycle. Every returned `Vec` is sorted by an explicit total order, so no
//! result depends on iteration order.
//!
//! # Fail-closed matching (constitution 46)
//!
//! [`MetaMatchParams::min_snapshots`] excludes categories that have too little
//! history to have a shape at all. A past meta observed twice is a rumour, not a
//! precedent, and [`match_past_meta`](MetaTimeline::match_past_meta) will not
//! return it.

use crate::fingerprint::{signed_decade, MetaSaturationState};

/// Bounded capacity of the meta timeline (constitution 57/99). Oldest-first
/// eviction; at one snapshot per meta per few minutes this is weeks of history.
pub const META_SNAPSHOT_CAP: usize = 4_096;

/// Minimum snapshots a past meta needs before it can be offered as a precedent
/// (constitution 46 small-n guard).
pub const META_MIN_SNAPSHOTS_DEFAULT: u32 = 4;

/// Default maximum feature distance for a past-meta match (constitution 102).
pub const META_MAX_DISTANCE_DEFAULT: u32 = 6;

/// Default cap on returned matches (constitution 57 bounded output).
pub const META_MAX_MATCHES_DEFAULT: usize = 8;

/// Distance weight on the saturation-state gap (constitution 102). Highest,
/// because *where on the curve you are* dominates everything else.
pub const MW_SATURATION: u32 = 4;
/// Distance weight on the participant-breadth decade gap (constitution 102).
pub const MW_BREADTH_DECADE: u32 = 2;
/// Distance weight on the aggregate-net decade gap (constitution 102).
pub const MW_NET_DECADE: u32 = 1;
/// Distance weight on the episode-count decade gap (constitution 102).
pub const MW_EPISODE_DECADE: u32 = 1;

/// One observation of a meta at a point in information time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetaSnapshot {
    /// Exact meta-category identifier.
    pub meta_category_id: u32,
    /// Observation time, nanoseconds of *information time* (never a wall clock).
    pub info_time_ns: u64,
    /// Where the meta sits on its lifecycle curve.
    pub saturation: MetaSaturationState,
    /// **Cumulative** realized net lamports attributed to this meta up to this
    /// snapshot. Cumulative rather than incremental so that "what happened after
    /// the moment that looked like now" is a single subtraction.
    pub aggregate_net_lamports: i128,
    /// Distinct participating wallets observed in the meta at this time.
    pub participant_breadth: u32,
    /// Number of episodes recorded against this meta up to this snapshot.
    pub episode_count: u32,
}

/// Why a snapshot could not be admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaTimelineError {
    /// Information time went backwards. The arrow of time is not negotiable.
    NonMonotonicInfoTime {
        /// Time offered.
        offered: u64,
        /// Latest time already recorded.
        last: u64,
    },
}

/// Tunables for [`MetaTimeline::match_past_meta`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetaMatchParams {
    /// Maximum weighted feature distance for a match.
    pub max_distance: u32,
    /// Maximum number of matches returned.
    pub max_matches: usize,
    /// Minimum recorded snapshots before a category counts as a precedent
    /// (constitution 46).
    pub min_snapshots: u32,
}

impl Default for MetaMatchParams {
    fn default() -> Self {
        Self {
            max_distance: META_MAX_DISTANCE_DEFAULT,
            max_matches: META_MAX_MATCHES_DEFAULT,
            min_snapshots: META_MIN_SNAPSHOTS_DEFAULT,
        }
    }
}

/// A past meta that resembled the current one, and what it went on to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PastMetaMatch {
    /// The past meta's category id.
    pub meta_category_id: u32,
    /// Weighted integer feature distance from the query snapshot to that meta's
    /// most similar recorded snapshot.
    pub distance: u32,
    /// Information time of the matched snapshot.
    pub matched_at_ns: u64,
    /// The matched snapshot's saturation state.
    pub matched_saturation: MetaSaturationState,
    /// **What happened next**: cumulative realized net from the matched snapshot to
    /// the last snapshot of that meta. This is the number the operator wants.
    pub subsequent_net_lamports: i128,
    /// Information time from the matched snapshot to the meta's last snapshot.
    pub subsequent_duration_ns: u64,
    /// How many snapshots that meta has (the evidence weight behind the match).
    pub n_snapshots: u32,
}

/// Shape of a meta's whole life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetaLifecycleStats {
    /// The category described.
    pub meta_category_id: u32,
    /// Snapshots recorded for it (constitution 46 evidence weight).
    pub n_snapshots: u32,
    /// First observation time.
    pub first_seen_ns: u64,
    /// Last observation time.
    pub last_seen_ns: u64,
    /// `last_seen_ns - first_seen_ns`.
    pub duration_ns: u64,
    /// Time from first observation to the first [`MetaSaturationState::Saturated`]
    /// snapshot, if it ever saturated. `None` means it never got there — either it
    /// died early or it is still running.
    pub time_to_saturation_ns: Option<u64>,
    /// Time from first saturation to first decay — the "give it back" window.
    pub decay_onset_ns: Option<u64>,
    /// Highest participant breadth ever recorded.
    pub peak_participant_breadth: u32,
    /// Cumulative realized net at the last snapshot: what the meta paid in total.
    pub terminal_net_lamports: i128,
    /// Cumulative realized net at the *peak* snapshot: what it paid at its best.
    /// The gap between this and `terminal_net_lamports` is the give-back.
    pub peak_net_lamports: i128,
}

/// Bounded ring of meta snapshots.
#[derive(Debug, Clone)]
pub struct MetaTimeline {
    capacity: usize,
    snapshots: Vec<MetaSnapshot>,
    head: usize,
    last_info_time_ns: Option<u64>,
    evicted_count: u64,
}

impl Default for MetaTimeline {
    fn default() -> Self {
        Self::new()
    }
}

impl MetaTimeline {
    /// A new timeline at the default [`META_SNAPSHOT_CAP`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(META_SNAPSHOT_CAP)
    }

    /// A new timeline with an explicit capacity (clamped to at least 1).
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            capacity,
            snapshots: Vec::with_capacity(capacity),
            head: 0,
            last_info_time_ns: None,
            evicted_count: 0,
        }
    }

    /// Hard capacity.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Number of live snapshots.
    #[must_use]
    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    /// `true` when no snapshots are held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    /// How many snapshots the oldest-first ring policy has dropped.
    #[must_use]
    pub const fn evicted_count(&self) -> u64 {
        self.evicted_count
    }

    /// Append a snapshot. Rejects information time that goes backwards.
    pub fn push(&mut self, snapshot: MetaSnapshot) -> Result<(), MetaTimelineError> {
        if let Some(last) = self.last_info_time_ns {
            if snapshot.info_time_ns < last {
                return Err(MetaTimelineError::NonMonotonicInfoTime {
                    offered: snapshot.info_time_ns,
                    last,
                });
            }
        }
        self.last_info_time_ns = Some(snapshot.info_time_ns);
        let slot = if self.snapshots.len() < self.capacity {
            let slot = self.snapshots.len();
            self.snapshots.push(snapshot);
            slot
        } else {
            let slot = self.head;
            self.snapshots[slot] = snapshot;
            self.evicted_count += 1;
            slot
        };
        self.head = (slot + 1) % self.capacity;
        Ok(())
    }

    /// Iterate live snapshots oldest-first — the canonical deterministic order.
    pub fn iter_oldest_first(&self) -> impl Iterator<Item = &MetaSnapshot> + '_ {
        let n = self.snapshots.len();
        let start = if n < self.capacity { 0 } else { self.head };
        (0..n).map(move |k| &self.snapshots[(start + k) % n.max(1)])
    }

    /// The most recent snapshot for a category, or `None` if it has no history.
    #[must_use]
    pub fn current_meta_state(&self, meta_category_id: u32) -> Option<MetaSnapshot> {
        self.iter_oldest_first()
            .filter(|s| s.meta_category_id == meta_category_id)
            .last()
            .copied()
    }

    /// Every category with live history, ascending by id (deterministic order).
    #[must_use]
    pub fn categories(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self
            .iter_oldest_first()
            .map(|s| s.meta_category_id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// The latest snapshot of every live category, ascending by id.
    #[must_use]
    pub fn current_metas(&self) -> Vec<MetaSnapshot> {
        self.categories()
            .into_iter()
            .filter_map(|id| self.current_meta_state(id))
            .collect()
    }

    /// Full lifecycle shape of one category, or `None` if it has no history.
    #[must_use]
    pub fn meta_lifecycle_stats(&self, meta_category_id: u32) -> Option<MetaLifecycleStats> {
        let mut n = 0u32;
        let mut first_seen = 0u64;
        let mut last_seen = 0u64;
        let mut first_saturated: Option<u64> = None;
        let mut first_decaying: Option<u64> = None;
        let mut peak_breadth = 0u32;
        let mut terminal_net = 0i128;
        let mut peak_net = i128::MIN;

        for s in self
            .iter_oldest_first()
            .filter(|s| s.meta_category_id == meta_category_id)
        {
            if n == 0 {
                first_seen = s.info_time_ns;
            }
            n += 1;
            last_seen = s.info_time_ns;
            terminal_net = s.aggregate_net_lamports;
            if s.aggregate_net_lamports > peak_net {
                peak_net = s.aggregate_net_lamports;
            }
            if s.participant_breadth > peak_breadth {
                peak_breadth = s.participant_breadth;
            }
            if s.saturation == MetaSaturationState::Saturated && first_saturated.is_none() {
                first_saturated = Some(s.info_time_ns);
            }
            if s.saturation == MetaSaturationState::Decaying && first_decaying.is_none() {
                first_decaying = Some(s.info_time_ns);
            }
        }
        if n == 0 {
            return None;
        }
        let time_to_saturation_ns = first_saturated.map(|t| t.saturating_sub(first_seen));
        let decay_onset_ns = match (first_saturated, first_decaying) {
            (Some(sat), Some(dec)) => Some(dec.saturating_sub(sat)),
            _ => None,
        };
        Some(MetaLifecycleStats {
            meta_category_id,
            n_snapshots: n,
            first_seen_ns: first_seen,
            last_seen_ns: last_seen,
            duration_ns: last_seen.saturating_sub(first_seen),
            time_to_saturation_ns,
            decay_onset_ns,
            peak_participant_breadth: peak_breadth,
            terminal_net_lamports: terminal_net,
            peak_net_lamports: peak_net,
        })
    }

    /// Which past metas resembled `current`, and what they did next.
    ///
    /// The query's own category is excluded — a meta is not its own precedent.
    /// Results are sorted by `(distance, meta_category_id)` ascending, a total
    /// order, then truncated to [`MetaMatchParams::max_matches`].
    #[must_use]
    pub fn match_past_meta(
        &self,
        current: &MetaSnapshot,
        params: &MetaMatchParams,
    ) -> Vec<PastMetaMatch> {
        let mut out: Vec<PastMetaMatch> = Vec::new();
        for id in self.categories() {
            if id == current.meta_category_id {
                continue;
            }
            let Some(stats) = self.meta_lifecycle_stats(id) else {
                continue;
            };
            if stats.n_snapshots < params.min_snapshots {
                continue; // constitution 46: not enough history to be a precedent.
            }
            // Best-matching snapshot within that category, tie-broken by earliest
            // information time so the answer is a total order.
            let mut best: Option<(u32, MetaSnapshot)> = None;
            for s in self
                .iter_oldest_first()
                .filter(|s| s.meta_category_id == id)
            {
                let d = snapshot_distance(current, s);
                let better = match best {
                    None => true,
                    Some((bd, bs)) => d < bd || (d == bd && s.info_time_ns < bs.info_time_ns),
                };
                if better {
                    best = Some((d, *s));
                }
            }
            let Some((distance, matched)) = best else {
                continue;
            };
            if distance > params.max_distance {
                continue;
            }
            out.push(PastMetaMatch {
                meta_category_id: id,
                distance,
                matched_at_ns: matched.info_time_ns,
                matched_saturation: matched.saturation,
                subsequent_net_lamports: stats
                    .terminal_net_lamports
                    .saturating_sub(matched.aggregate_net_lamports),
                subsequent_duration_ns: stats.last_seen_ns.saturating_sub(matched.info_time_ns),
                n_snapshots: stats.n_snapshots,
            });
        }
        out.sort_unstable_by_key(|m| (m.distance, m.meta_category_id));
        out.truncate(params.max_matches);
        out
    }
}

/// Weighted integer distance between two meta snapshots.
///
/// All four terms are gaps between small integer ordinals or decades, so the
/// distance is scale-free: a meta with 40 participants and one with 4_000 are three
/// decades apart whether the numbers are wallets or dollars.
#[must_use]
pub fn snapshot_distance(a: &MetaSnapshot, b: &MetaSnapshot) -> u32 {
    let sat = u32::from(a.saturation.ordinal().abs_diff(b.saturation.ordinal()));
    let breadth = decade_gap(
        i128::from(a.participant_breadth),
        i128::from(b.participant_breadth),
    );
    let net = decade_gap(a.aggregate_net_lamports, b.aggregate_net_lamports);
    let eps = decade_gap(i128::from(a.episode_count), i128::from(b.episode_count));
    sat * MW_SATURATION
        + breadth * MW_BREADTH_DECADE
        + net * MW_NET_DECADE
        + eps * MW_EPISODE_DECADE
}

/// Absolute gap between the signed decades of two quantities.
#[must_use]
fn decade_gap(a: i128, b: i128) -> u32 {
    signed_decade(a).abs_diff(signed_decade(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(
        id: u32,
        t: u64,
        sat: MetaSaturationState,
        net: i128,
        breadth: u32,
        eps: u32,
    ) -> MetaSnapshot {
        MetaSnapshot {
            meta_category_id: id,
            info_time_ns: t,
            saturation: sat,
            aggregate_net_lamports: net,
            participant_breadth: breadth,
            episode_count: eps,
        }
    }

    /// A completed past meta (id 1) that peaked and gave it all back, plus a second
    /// completed meta (id 2) that held its gains.
    fn timeline_with_two_past_metas() -> MetaTimeline {
        let mut tl = MetaTimeline::with_capacity(64);
        let hour = 3_600_000_000_000u64;
        // Meta 1: emerges, hot, saturates, decays; gives back most of the peak.
        tl.push(snap(
            1,
            hour,
            MetaSaturationState::Emerging,
            1_000_000_000,
            20,
            5,
        ))
        .expect("ok");
        tl.push(snap(
            1,
            2 * hour,
            MetaSaturationState::Hot,
            8_000_000_000,
            200,
            40,
        ))
        .expect("ok");
        tl.push(snap(
            1,
            3 * hour,
            MetaSaturationState::Saturated,
            9_000_000_000,
            900,
            90,
        ))
        .expect("ok");
        tl.push(snap(
            1,
            5 * hour,
            MetaSaturationState::Decaying,
            1_000_000_000,
            400,
            120,
        ))
        .expect("ok");
        // Meta 2: same lifecycle shape, but it kept the money.
        tl.push(snap(
            2,
            6 * hour,
            MetaSaturationState::Emerging,
            1_000_000_000,
            20,
            5,
        ))
        .expect("ok");
        tl.push(snap(
            2,
            7 * hour,
            MetaSaturationState::Hot,
            7_000_000_000,
            200,
            40,
        ))
        .expect("ok");
        tl.push(snap(
            2,
            8 * hour,
            MetaSaturationState::Saturated,
            9_000_000_000,
            900,
            90,
        ))
        .expect("ok");
        tl.push(snap(
            2,
            9 * hour,
            MetaSaturationState::Decaying,
            8_000_000_000,
            400,
            120,
        ))
        .expect("ok");
        tl
    }

    #[test]
    fn push_rejects_backwards_information_time() {
        let mut tl = MetaTimeline::with_capacity(8);
        tl.push(snap(1, 1_000, MetaSaturationState::Hot, 0, 1, 1))
            .expect("ok");
        let err = tl
            .push(snap(1, 999, MetaSaturationState::Hot, 0, 1, 1))
            .expect_err("time must not go backwards");
        assert_eq!(
            err,
            MetaTimelineError::NonMonotonicInfoTime {
                offered: 999,
                last: 1_000
            }
        );
    }

    #[test]
    fn equal_information_time_is_accepted() {
        let mut tl = MetaTimeline::with_capacity(8);
        tl.push(snap(1, 1_000, MetaSaturationState::Hot, 0, 1, 1))
            .expect("ok");
        tl.push(snap(2, 1_000, MetaSaturationState::Hot, 0, 1, 1))
            .expect("same instant is fine");
        assert_eq!(tl.len(), 2);
    }

    #[test]
    fn timeline_is_bounded_and_evicts_oldest_first() {
        let cap = 8usize;
        let mut tl = MetaTimeline::with_capacity(cap);
        for i in 1..=200u64 {
            tl.push(snap(
                1,
                i * 1_000,
                MetaSaturationState::Hot,
                i as i128,
                1,
                1,
            ))
            .expect("ok");
            assert!(tl.len() <= cap);
        }
        assert_eq!(tl.len(), cap);
        assert_eq!(tl.evicted_count(), 200 - cap as u64);
        let times: Vec<u64> = tl.iter_oldest_first().map(|s| s.info_time_ns).collect();
        let expect: Vec<u64> = (200 - cap as u64 + 1..=200).map(|i| i * 1_000).collect();
        assert_eq!(times, expect);
    }

    #[test]
    fn current_meta_state_returns_the_latest_snapshot() {
        let tl = timeline_with_two_past_metas();
        let s = tl.current_meta_state(1).expect("meta 1 exists");
        assert_eq!(s.saturation, MetaSaturationState::Decaying);
        assert_eq!(s.aggregate_net_lamports, 1_000_000_000);
        assert!(tl.current_meta_state(99).is_none());
    }

    #[test]
    fn categories_and_current_metas_are_sorted_and_deduped() {
        let tl = timeline_with_two_past_metas();
        assert_eq!(tl.categories(), vec![1, 2]);
        let cur = tl.current_metas();
        assert_eq!(cur.len(), 2);
        assert_eq!(cur[0].meta_category_id, 1);
        assert_eq!(cur[1].meta_category_id, 2);
    }

    #[test]
    fn lifecycle_stats_capture_the_curve() {
        let tl = timeline_with_two_past_metas();
        let hour = 3_600_000_000_000u64;
        let s = tl.meta_lifecycle_stats(1).expect("meta 1");
        assert_eq!(s.n_snapshots, 4);
        assert_eq!(s.first_seen_ns, hour);
        assert_eq!(s.last_seen_ns, 5 * hour);
        assert_eq!(s.duration_ns, 4 * hour);
        assert_eq!(s.time_to_saturation_ns, Some(2 * hour));
        assert_eq!(s.decay_onset_ns, Some(2 * hour));
        assert_eq!(s.peak_participant_breadth, 900);
        assert_eq!(s.peak_net_lamports, 9_000_000_000);
        assert_eq!(s.terminal_net_lamports, 1_000_000_000);
        // The give-back is the whole story: peaked at 9 SOL, ended at 1 SOL.
        assert_eq!(s.peak_net_lamports - s.terminal_net_lamports, 8_000_000_000);
        assert!(tl.meta_lifecycle_stats(42).is_none());
    }

    #[test]
    fn lifecycle_stats_report_none_for_a_meta_that_never_saturated() {
        let mut tl = MetaTimeline::with_capacity(8);
        for i in 1..=3u64 {
            tl.push(snap(5, i * 1_000, MetaSaturationState::Emerging, 0, 1, 1))
                .expect("ok");
        }
        let s = tl.meta_lifecycle_stats(5).expect("meta 5");
        assert_eq!(s.time_to_saturation_ns, None);
        assert_eq!(s.decay_onset_ns, None);
    }

    #[test]
    fn match_past_meta_finds_the_precedent_and_what_happened_next() {
        let tl = timeline_with_two_past_metas();
        // Today's meta (id 9) looks exactly like the saturation point of metas 1 & 2.
        let current = snap(
            9,
            100 * 3_600_000_000_000,
            MetaSaturationState::Saturated,
            9_000_000_000,
            900,
            90,
        );
        let matches = tl.match_past_meta(&current, &MetaMatchParams::default());
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].distance, 0);
        // Sorted by (distance, id): meta 1 first.
        assert_eq!(matches[0].meta_category_id, 1);
        assert_eq!(matches[1].meta_category_id, 2);
        // Meta 1 gave back 8 SOL after this point; meta 2 gave back 1 SOL.
        assert_eq!(matches[0].subsequent_net_lamports, -8_000_000_000);
        assert_eq!(matches[1].subsequent_net_lamports, -1_000_000_000);
        assert_eq!(
            matches[0].matched_saturation,
            MetaSaturationState::Saturated
        );
        assert_eq!(matches[0].subsequent_duration_ns, 2 * 3_600_000_000_000);
    }

    #[test]
    fn match_past_meta_excludes_the_query_category_itself() {
        let tl = timeline_with_two_past_metas();
        let current = tl.current_meta_state(1).expect("meta 1");
        let matches = tl.match_past_meta(&current, &MetaMatchParams::default());
        assert!(matches.iter().all(|m| m.meta_category_id != 1));
    }

    #[test]
    fn match_past_meta_is_fail_closed_on_thin_history() {
        let mut tl = MetaTimeline::with_capacity(16);
        // Meta 3 has exactly two snapshots: a rumour, not a precedent.
        tl.push(snap(
            3,
            1_000,
            MetaSaturationState::Saturated,
            5_000_000_000,
            900,
            90,
        ))
        .expect("ok");
        tl.push(snap(
            3,
            2_000,
            MetaSaturationState::Decaying,
            1_000_000_000,
            400,
            100,
        ))
        .expect("ok");
        let current = snap(
            9,
            9_000,
            MetaSaturationState::Saturated,
            5_000_000_000,
            900,
            90,
        );
        assert!(tl
            .match_past_meta(&current, &MetaMatchParams::default())
            .is_empty());
        // Lowering the evidence floor makes it visible — the guard is a policy, not
        // an accident of the data.
        let loose = MetaMatchParams {
            min_snapshots: 2,
            ..MetaMatchParams::default()
        };
        assert_eq!(tl.match_past_meta(&current, &loose).len(), 1);
    }

    #[test]
    fn match_past_meta_respects_the_distance_radius_and_match_cap() {
        let tl = timeline_with_two_past_metas();
        let current = snap(9, 99_000, MetaSaturationState::Emerging, 1, 1, 1);
        let tight = MetaMatchParams {
            max_distance: 0,
            ..MetaMatchParams::default()
        };
        assert!(tl.match_past_meta(&current, &tight).is_empty());
        let capped = MetaMatchParams {
            max_distance: 100,
            max_matches: 1,
            ..MetaMatchParams::default()
        };
        assert_eq!(tl.match_past_meta(&current, &capped).len(), 1);
    }

    #[test]
    fn match_past_meta_is_deterministic() {
        let tl = timeline_with_two_past_metas();
        let current = snap(
            9,
            100_000,
            MetaSaturationState::Saturated,
            9_000_000_000,
            900,
            90,
        );
        let first = tl.match_past_meta(&current, &MetaMatchParams::default());
        for _ in 0..32 {
            assert_eq!(
                tl.match_past_meta(&current, &MetaMatchParams::default()),
                first
            );
        }
    }

    #[test]
    fn snapshot_distance_is_zero_to_self_and_symmetric() {
        let a = snap(1, 0, MetaSaturationState::Hot, 5_000, 10, 4);
        let b = snap(2, 0, MetaSaturationState::Decaying, 5_000_000, 900, 90);
        assert_eq!(snapshot_distance(&a, &a), 0);
        assert_eq!(snapshot_distance(&a, &b), snapshot_distance(&b, &a));
        assert!(snapshot_distance(&a, &b) > 0);
    }

    #[test]
    fn saturation_gap_dominates_the_distance() {
        let base = snap(1, 0, MetaSaturationState::Emerging, 1_000, 10, 10);
        let far_state = snap(2, 0, MetaSaturationState::Decaying, 1_000, 10, 10);
        let far_size = snap(3, 0, MetaSaturationState::Emerging, 10_000, 100, 100);
        // Three lifecycle steps outweigh a decade of size drift, by weight design.
        assert!(snapshot_distance(&base, &far_state) > snapshot_distance(&base, &far_size));
    }
}
