//! Social recall — "who was tweeting about this, when, and do they actually make
//! money?" (constitution 29.9 social-call and source-quality ledger).
//!
//! Two bounded, time-ordered rings that are deliberately *separate*:
//!
//! * [`CallRecord`] — an immutable "author X called mint Y on platform Z at
//!   information time T". Appended the moment the call is observed, when the
//!   outcome is by definition unknown.
//! * [`CallMarkout`] — the realized net attributed to a call, appended later when
//!   the position that call fed has actually closed.
//!
//! Keeping them apart is what preserves append-only immutability. The alternative —
//! one record whose outcome field is patched in afterwards — would mean the social
//! history is mutable, and a mutable track record is one that quietly improves
//! every time you look at it.
//!
//! # Fail-closed author scoring (constitution 46)
//!
//! [`AuthorTrackRecord`] is the same shape as
//! [`crate::recall::RecallVerdict`]: `Known(AuthorStats)` or `Unknown` with **no
//! outcome numbers attached**. An author with three calls has no track record, and
//! this module will not manufacture one. Callers with a big following are exactly
//! the population where small-n flattery is most expensive — three lucky calls and
//! a bot farm is not an edge.

use crate::recall::{nearest_rank_index, order_stat_i128, BPS_SCALE_U32, P50};

/// Bounded capacity of the call ring (constitution 57/99), oldest-first eviction.
pub const SOCIAL_CALL_CAP: usize = 32_768;

/// Bounded capacity of the markout ring (constitution 57/99), oldest-first eviction.
pub const SOCIAL_MARKOUT_CAP: usize = 32_768;

/// Minimum attributed markouts before an author gets a track record
/// (constitution 46 small-n guard).
pub const AUTHOR_MIN_SAMPLE: u32 = 8;

/// Default lookback window for [`SocialRecallIndex::who_called`]: seven days of
/// information time in nanoseconds (constitution 102). This is the literal
/// "who was tweeting about it last week" window.
pub const DEFAULT_CALL_WINDOW_NS: u64 = 7 * 86_400 * 1_000_000_000;

/// Social platform a call was made on. Nominal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Platform {
    /// X / Twitter.
    X,
    /// Telegram channel or group.
    Telegram,
    /// Discord server.
    Discord,
    /// Live stream (Twitch / Pump live).
    Stream,
    /// Aggregator or bot relay — an amplifier, not an originator.
    Aggregator,
}

impl Platform {
    /// Dense ordinal used in ordering and the wire format.
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        match self {
            Self::X => 0,
            Self::Telegram => 1,
            Self::Discord => 2,
            Self::Stream => 3,
            Self::Aggregator => 4,
        }
    }

    /// Inverse of [`Platform::ordinal`]; out-of-range input yields `None`.
    #[must_use]
    pub const fn from_ordinal(o: u8) -> Option<Self> {
        match o {
            0 => Some(Self::X),
            1 => Some(Self::Telegram),
            2 => Some(Self::Discord),
            3 => Some(Self::Stream),
            4 => Some(Self::Aggregator),
            _ => None,
        }
    }
}

/// An observed social call. Immutable once appended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallRecord {
    /// Monotone call identifier; also the deterministic tie-break key.
    pub call_id: u64,
    /// Dense internal mint identifier the call was about.
    pub mint_id: u64,
    /// Dense internal author identifier.
    pub author_id: u64,
    /// Where the call was made.
    pub platform: Platform,
    /// Call time, nanoseconds of *information time*.
    pub info_time_ns: u64,
    /// Signed decade of the author's follower count — scale without float.
    pub followers_decade: i32,
    /// Whether this author is on the designated-caller list at call time.
    pub was_designated: bool,
}

/// Realized net attributed to a specific call, appended after the fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallMarkout {
    /// The call this outcome belongs to.
    pub call_id: u64,
    /// Denormalised author id so track records do not need a join.
    pub author_id: u64,
    /// Realized net lamports attributed to the call, after all costs.
    pub realized_net_lamports: i128,
    /// Time from call to attribution, nanoseconds.
    pub hold_duration_ns: u64,
    /// Attribution time, nanoseconds of information time.
    pub info_time_ns: u64,
}

/// A scored author.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorStats {
    /// The author described.
    pub author_id: u64,
    /// Number of attributed markouts backing these numbers.
    pub n_markouts: u32,
    /// Total realized net lamports across all attributed calls — the only number
    /// that survives contact with reality.
    pub realized_net_sum_lamports: i128,
    /// Median attributed net (nearest rank; no interpolation).
    pub median_net_lamports: i128,
    /// Mean attributed net, integer division truncating toward zero.
    pub mean_net_lamports: i128,
    /// Calls with strictly positive attributed net.
    pub win_count: u32,
    /// Calls with strictly negative attributed net.
    pub loss_count: u32,
    /// Win rate in basis points over decisive calls only.
    pub win_rate_bp: u32,
}

/// An author's track record, or an explicit refusal to guess.
///
/// `Unknown` carries the sample count and the floor it missed — and nothing else.
/// No net, no win rate, no "provisional" estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorTrackRecord {
    /// Enough attributed calls to say something.
    Known(AuthorStats),
    /// Not enough evidence (constitution 46).
    Unknown {
        /// Attributed markouts found.
        n_markouts: u32,
        /// The floor they failed to reach.
        min_sample: u32,
    },
}

impl AuthorTrackRecord {
    /// The statistics, or `None`. The only path to a number.
    #[must_use]
    pub const fn stats(&self) -> Option<&AuthorStats> {
        match self {
            Self::Known(s) => Some(s),
            Self::Unknown { .. } => None,
        }
    }

    /// `true` when a track record exists.
    #[must_use]
    pub const fn is_known(&self) -> bool {
        matches!(self, Self::Known(_))
    }
}

/// Why a record could not be appended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocialIndexError {
    /// Call ids must be strictly increasing (deterministic total ordering).
    NonMonotonicCallId {
        /// The id offered.
        offered: u64,
        /// The last id already stored.
        last: u64,
    },
    /// Information time went backwards on the markout ring.
    NonMonotonicInfoTime {
        /// The time offered.
        offered: u64,
        /// The latest time already stored.
        last: u64,
    },
}

/// Bounded, time-windowed index of social calls and their attributed outcomes.
#[derive(Debug, Clone)]
pub struct SocialRecallIndex {
    call_capacity: usize,
    markout_capacity: usize,
    calls: Vec<CallRecord>,
    markouts: Vec<CallMarkout>,
    call_head: usize,
    markout_head: usize,
    last_call_id: Option<u64>,
    last_markout_time_ns: Option<u64>,
    calls_evicted: u64,
    markouts_evicted: u64,
}

impl Default for SocialRecallIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl SocialRecallIndex {
    /// A new index at the default capacities.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(SOCIAL_CALL_CAP, SOCIAL_MARKOUT_CAP)
    }

    /// A new index with explicit capacities (each clamped to at least 1).
    #[must_use]
    pub fn with_capacity(call_capacity: usize, markout_capacity: usize) -> Self {
        let call_capacity = call_capacity.max(1);
        let markout_capacity = markout_capacity.max(1);
        Self {
            call_capacity,
            markout_capacity,
            calls: Vec::with_capacity(call_capacity),
            markouts: Vec::with_capacity(markout_capacity),
            call_head: 0,
            markout_head: 0,
            last_call_id: None,
            last_markout_time_ns: None,
            calls_evicted: 0,
            markouts_evicted: 0,
        }
    }

    /// Live call count.
    #[must_use]
    pub fn call_len(&self) -> usize {
        self.calls.len()
    }

    /// Live markout count.
    #[must_use]
    pub fn markout_len(&self) -> usize {
        self.markouts.len()
    }

    /// Hard call capacity.
    #[must_use]
    pub const fn call_capacity(&self) -> usize {
        self.call_capacity
    }

    /// Hard markout capacity.
    #[must_use]
    pub const fn markout_capacity(&self) -> usize {
        self.markout_capacity
    }

    /// Calls dropped by the oldest-first ring policy.
    #[must_use]
    pub const fn calls_evicted(&self) -> u64 {
        self.calls_evicted
    }

    /// Markouts dropped by the oldest-first ring policy.
    #[must_use]
    pub const fn markouts_evicted(&self) -> u64 {
        self.markouts_evicted
    }

    /// Append an observed call. Ids must be strictly increasing.
    pub fn record_call(&mut self, call: CallRecord) -> Result<(), SocialIndexError> {
        if let Some(last) = self.last_call_id {
            if call.call_id <= last {
                return Err(SocialIndexError::NonMonotonicCallId {
                    offered: call.call_id,
                    last,
                });
            }
        }
        self.last_call_id = Some(call.call_id);
        let slot = if self.calls.len() < self.call_capacity {
            let slot = self.calls.len();
            self.calls.push(call);
            slot
        } else {
            let slot = self.call_head;
            self.calls[slot] = call;
            self.calls_evicted += 1;
            slot
        };
        self.call_head = (slot + 1) % self.call_capacity;
        Ok(())
    }

    /// Append an attributed outcome for a call already observed.
    pub fn record_markout(&mut self, markout: CallMarkout) -> Result<(), SocialIndexError> {
        if let Some(last) = self.last_markout_time_ns {
            if markout.info_time_ns < last {
                return Err(SocialIndexError::NonMonotonicInfoTime {
                    offered: markout.info_time_ns,
                    last,
                });
            }
        }
        self.last_markout_time_ns = Some(markout.info_time_ns);
        let slot = if self.markouts.len() < self.markout_capacity {
            let slot = self.markouts.len();
            self.markouts.push(markout);
            slot
        } else {
            let slot = self.markout_head;
            self.markouts[slot] = markout;
            self.markouts_evicted += 1;
            slot
        };
        self.markout_head = (slot + 1) % self.markout_capacity;
        Ok(())
    }

    /// Iterate live calls oldest-first — the canonical deterministic order.
    pub fn iter_calls_oldest_first(&self) -> impl Iterator<Item = &CallRecord> + '_ {
        let n = self.calls.len();
        let start = if n < self.call_capacity {
            0
        } else {
            self.call_head
        };
        (0..n).map(move |k| &self.calls[(start + k) % n.max(1)])
    }

    /// Iterate live markouts oldest-first.
    pub fn iter_markouts_oldest_first(&self) -> impl Iterator<Item = &CallMarkout> + '_ {
        let n = self.markouts.len();
        let start = if n < self.markout_capacity {
            0
        } else {
            self.markout_head
        };
        (0..n).map(move |k| &self.markouts[(start + k) % n.max(1)])
    }

    /// **Who was tweeting about this mint, and when.**
    ///
    /// Returns calls for `mint_id` whose information time lies in the half-open
    /// window `(as_of_ns - window_ns, as_of_ns]`, sorted by
    /// `(info_time_ns, call_id)` ascending — a total order, because call ids are
    /// unique. Window arithmetic saturates, so a window wider than the epoch simply
    /// means "everything".
    #[must_use]
    pub fn who_called(&self, mint_id: u64, as_of_ns: u64, window_ns: u64) -> Vec<CallRecord> {
        let floor = as_of_ns.saturating_sub(window_ns);
        let mut out: Vec<CallRecord> = self
            .iter_calls_oldest_first()
            .filter(|c| {
                c.mint_id == mint_id && c.info_time_ns > floor && c.info_time_ns <= as_of_ns
            })
            .copied()
            .collect();
        out.sort_unstable_by_key(|c| (c.info_time_ns, c.call_id));
        out
    }

    /// Every call in `(from_ns, to_ns]` regardless of mint, sorted by
    /// `(info_time_ns, call_id)` ascending.
    #[must_use]
    pub fn calls_in_window(&self, from_ns: u64, to_ns: u64) -> Vec<CallRecord> {
        let mut out: Vec<CallRecord> = self
            .iter_calls_oldest_first()
            .filter(|c| c.info_time_ns > from_ns && c.info_time_ns <= to_ns)
            .copied()
            .collect();
        out.sort_unstable_by_key(|c| (c.info_time_ns, c.call_id));
        out
    }

    /// Distinct authors who called a mint in the window, ascending by id.
    #[must_use]
    pub fn authors_of(&self, mint_id: u64, as_of_ns: u64, window_ns: u64) -> Vec<u64> {
        let mut ids: Vec<u64> = self
            .who_called(mint_id, as_of_ns, window_ns)
            .into_iter()
            .map(|c| c.author_id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// **Does this author actually make money?**
    ///
    /// Fail-closed below `min_sample` attributed markouts (constitution 46).
    #[must_use]
    pub fn author_track_record(&self, author_id: u64, min_sample: u32) -> AuthorTrackRecord {
        let mut nets: Vec<i128> = self
            .iter_markouts_oldest_first()
            .filter(|m| m.author_id == author_id)
            .map(|m| m.realized_net_lamports)
            .collect();
        let n = nets.len() as u32;
        if n < min_sample || n == 0 {
            return AuthorTrackRecord::Unknown {
                n_markouts: n,
                min_sample,
            };
        }
        nets.sort_unstable();

        let mut sum = 0i128;
        let mut win_count = 0u32;
        let mut loss_count = 0u32;
        for net in &nets {
            sum = sum.saturating_add(*net);
            if *net > 0 {
                win_count += 1;
            } else if *net < 0 {
                loss_count += 1;
            }
        }
        let decisive = win_count + loss_count;
        let win_rate_bp = if decisive == 0 {
            0
        } else {
            ((u64::from(win_count) * u64::from(BPS_SCALE_U32)) / u64::from(decisive)) as u32
        };
        AuthorTrackRecord::Known(AuthorStats {
            author_id,
            n_markouts: n,
            realized_net_sum_lamports: sum,
            median_net_lamports: order_stat_i128(&nets, P50),
            mean_net_lamports: sum / i128::from(n),
            win_count,
            loss_count,
            win_rate_bp,
        })
    }

    /// Track records for everyone who called a mint in the window, ordered by
    /// author id ascending. Authors without enough history come back `Unknown` —
    /// they are still *listed*, because "a big account called this and I have no
    /// idea whether they are any good" is itself actionable information.
    #[must_use]
    pub fn callers_with_records(
        &self,
        mint_id: u64,
        as_of_ns: u64,
        window_ns: u64,
        min_sample: u32,
    ) -> Vec<(u64, AuthorTrackRecord)> {
        self.authors_of(mint_id, as_of_ns, window_ns)
            .into_iter()
            .map(|id| (id, self.author_track_record(id, min_sample)))
            .collect()
    }
}

/// Median of an already-collected sample of attributed nets, nearest rank.
/// Exposed so callers can build ad-hoc cohort views without re-deriving the
/// order-statistic convention (which must stay identical everywhere).
#[must_use]
pub fn median_net(sorted_nets: &[i128]) -> i128 {
    if sorted_nets.is_empty() {
        return 0;
    }
    sorted_nets[nearest_rank_index(sorted_nets.len(), P50)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: u64, mint: u64, author: u64, t: u64) -> CallRecord {
        CallRecord {
            call_id: id,
            mint_id: mint,
            author_id: author,
            platform: Platform::X,
            info_time_ns: t,
            followers_decade: 5,
            was_designated: false,
        }
    }

    fn markout(call_id: u64, author: u64, net: i128, t: u64) -> CallMarkout {
        CallMarkout {
            call_id,
            author_id: author,
            realized_net_lamports: net,
            hold_duration_ns: 60_000_000_000,
            info_time_ns: t,
        }
    }

    #[test]
    fn record_call_rejects_non_monotone_ids() {
        let mut idx = SocialRecallIndex::with_capacity(8, 8);
        idx.record_call(call(1, 1, 1, 100)).expect("ok");
        let err = idx
            .record_call(call(1, 1, 1, 200))
            .expect_err("duplicate id");
        assert_eq!(
            err,
            SocialIndexError::NonMonotonicCallId {
                offered: 1,
                last: 1
            }
        );
    }

    #[test]
    fn record_markout_rejects_backwards_time() {
        let mut idx = SocialRecallIndex::with_capacity(8, 8);
        idx.record_markout(markout(1, 1, 0, 500)).expect("ok");
        let err = idx
            .record_markout(markout(2, 1, 0, 499))
            .expect_err("backwards");
        assert_eq!(
            err,
            SocialIndexError::NonMonotonicInfoTime {
                offered: 499,
                last: 500
            }
        );
    }

    #[test]
    fn call_ring_is_bounded_and_evicts_oldest_first() {
        let cap = 8usize;
        let mut idx = SocialRecallIndex::with_capacity(cap, 4);
        for i in 1..=100u64 {
            idx.record_call(call(i, 1, 1, i * 10)).expect("ok");
            assert!(idx.call_len() <= cap);
        }
        assert_eq!(idx.call_len(), cap);
        assert_eq!(idx.calls_evicted(), 100 - cap as u64);
        let ids: Vec<u64> = idx.iter_calls_oldest_first().map(|c| c.call_id).collect();
        assert_eq!(ids, (100 - cap as u64 + 1..=100).collect::<Vec<u64>>());
    }

    #[test]
    fn markout_ring_is_bounded_and_evicts_oldest_first() {
        let cap = 4usize;
        let mut idx = SocialRecallIndex::with_capacity(4, cap);
        for i in 1..=50u64 {
            idx.record_markout(markout(i, 1, i as i128, i * 10))
                .expect("ok");
            assert!(idx.markout_len() <= cap);
        }
        assert_eq!(idx.markout_len(), cap);
        assert_eq!(idx.markouts_evicted(), 50 - cap as u64);
    }

    #[test]
    fn who_called_returns_the_window_in_deterministic_order() {
        let mut idx = SocialRecallIndex::with_capacity(64, 64);
        idx.record_call(call(1, 100, 7, 1_000)).expect("ok");
        idx.record_call(call(2, 100, 9, 2_000)).expect("ok");
        idx.record_call(call(3, 200, 7, 2_500)).expect("ok"); // different mint
        idx.record_call(call(4, 100, 7, 5_000)).expect("ok");

        let out = idx.who_called(100, 5_000, 4_000);
        // Window is (1_000, 5_000]: the call at exactly 1_000 is excluded, the one
        // at exactly 5_000 is included.
        let ids: Vec<u64> = out.iter().map(|c| c.call_id).collect();
        assert_eq!(ids, vec![2, 4]);
        for _ in 0..16 {
            assert_eq!(idx.who_called(100, 5_000, 4_000), out);
        }
    }

    #[test]
    fn who_called_window_boundaries_are_half_open() {
        let mut idx = SocialRecallIndex::with_capacity(16, 16);
        idx.record_call(call(1, 1, 1, 1_000)).expect("ok");
        idx.record_call(call(2, 1, 1, 1_001)).expect("ok");
        // as_of = 2_000, window = 1_000 -> floor 1_000, half-open (1_000, 2_000].
        let out = idx.who_called(1, 2_000, 1_000);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].call_id, 2);
    }

    #[test]
    fn who_called_window_saturates_instead_of_underflowing() {
        let mut idx = SocialRecallIndex::with_capacity(16, 16);
        idx.record_call(call(1, 1, 1, 1)).expect("ok");
        let out = idx.who_called(1, 1_000, u64::MAX);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn calls_in_window_spans_every_mint() {
        let mut idx = SocialRecallIndex::with_capacity(64, 64);
        idx.record_call(call(1, 100, 7, 1_500)).expect("ok");
        idx.record_call(call(2, 200, 8, 1_600)).expect("ok");
        idx.record_call(call(3, 300, 9, 9_000)).expect("ok");
        let out = idx.calls_in_window(1_000, 2_000);
        assert_eq!(
            out.iter().map(|c| c.call_id).collect::<Vec<u64>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn authors_of_is_sorted_and_deduped() {
        let mut idx = SocialRecallIndex::with_capacity(64, 64);
        idx.record_call(call(1, 100, 9, 1_000)).expect("ok");
        idx.record_call(call(2, 100, 3, 1_100)).expect("ok");
        idx.record_call(call(3, 100, 9, 1_200)).expect("ok");
        assert_eq!(idx.authors_of(100, 2_000, 2_000), vec![3, 9]);
    }

    #[test]
    fn author_track_record_is_fail_closed_below_min_sample() {
        let mut idx = SocialRecallIndex::with_capacity(64, 64);
        // Seven glorious winners: the classic small-n trap.
        for i in 1..=7u64 {
            idx.record_markout(markout(i, 42, 10_000_000, i * 1_000))
                .expect("ok");
        }
        let tr = idx.author_track_record(42, AUTHOR_MIN_SAMPLE);
        assert_eq!(
            tr,
            AuthorTrackRecord::Unknown {
                n_markouts: 7,
                min_sample: AUTHOR_MIN_SAMPLE
            }
        );
        assert!(
            tr.stats().is_none(),
            "no estimate may be readable at n < min_sample"
        );
        assert!(!tr.is_known());
    }

    #[test]
    fn author_track_record_appears_at_the_sample_floor() {
        let mut idx = SocialRecallIndex::with_capacity(64, 64);
        for i in 1..=8u64 {
            idx.record_markout(markout(i, 42, 10_000_000, i * 1_000))
                .expect("ok");
        }
        let tr = idx.author_track_record(42, AUTHOR_MIN_SAMPLE);
        let s = tr.stats().expect("eighth markout reaches the floor");
        assert_eq!(s.author_id, 42);
        assert_eq!(s.n_markouts, 8);
        assert_eq!(s.realized_net_sum_lamports, 80_000_000);
        assert_eq!(s.median_net_lamports, 10_000_000);
        assert_eq!(s.mean_net_lamports, 10_000_000);
        assert_eq!(s.win_rate_bp, 10_000);
    }

    #[test]
    fn author_track_record_computes_the_hand_checked_distribution() {
        let mut idx = SocialRecallIndex::with_capacity(64, 64);
        // Nets: -3, -2, -1, 0, 1, 2, 3, 4 (millions). Sum = 4M.
        let nets: [i128; 8] = [-3, -2, -1, 0, 1, 2, 3, 4];
        for (i, n) in nets.iter().enumerate() {
            idx.record_markout(markout(
                i as u64 + 1,
                7,
                n * 1_000_000,
                (i as u64 + 1) * 1_000,
            ))
            .expect("ok");
        }
        let s = idx
            .author_track_record(7, 4)
            .stats()
            .copied()
            .expect("known");
        assert_eq!(s.n_markouts, 8);
        assert_eq!(s.realized_net_sum_lamports, 4_000_000);
        assert_eq!(s.win_count, 4);
        assert_eq!(s.loss_count, 3);
        // The exact-zero markout is excluded from the win-rate denominator.
        assert_eq!(s.win_rate_bp, (4 * 10_000) / 7);
        // Lower median of eight samples is index 3 -> 0.
        assert_eq!(s.median_net_lamports, 0);
        // 4_000_000 / 8 = 500_000.
        assert_eq!(s.mean_net_lamports, 500_000);
    }

    #[test]
    fn unknown_author_reports_zero_markouts_and_no_numbers() {
        let idx = SocialRecallIndex::with_capacity(8, 8);
        let tr = idx.author_track_record(1_234, AUTHOR_MIN_SAMPLE);
        assert_eq!(
            tr,
            AuthorTrackRecord::Unknown {
                n_markouts: 0,
                min_sample: AUTHOR_MIN_SAMPLE
            }
        );
        assert!(tr.stats().is_none());
    }

    #[test]
    fn callers_with_records_lists_unknown_authors_too() {
        let mut idx = SocialRecallIndex::with_capacity(64, 64);
        idx.record_call(call(1, 500, 11, 1_000)).expect("ok");
        idx.record_call(call(2, 500, 22, 1_100)).expect("ok");
        for i in 1..=10u64 {
            idx.record_markout(markout(i, 11, 5_000_000, i * 100))
                .expect("ok");
        }
        let rows = idx.callers_with_records(500, 2_000, 2_000, AUTHOR_MIN_SAMPLE);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, 11);
        assert!(rows[0].1.is_known());
        assert_eq!(rows[1].0, 22);
        assert!(
            !rows[1].1.is_known(),
            "an unscored caller must not get a number"
        );
    }

    #[test]
    fn author_track_record_is_deterministic() {
        let mut idx = SocialRecallIndex::with_capacity(64, 64);
        for i in 1..=12u64 {
            let net = (i as i128 - 6) * 1_000_000;
            idx.record_markout(markout(i, 3, net, i * 1_000))
                .expect("ok");
        }
        let first = idx.author_track_record(3, AUTHOR_MIN_SAMPLE);
        for _ in 0..32 {
            assert_eq!(idx.author_track_record(3, AUTHOR_MIN_SAMPLE), first);
        }
    }

    #[test]
    fn platform_ordinals_round_trip() {
        for o in 0u8..5 {
            assert_eq!(Platform::from_ordinal(o).expect("in range").ordinal(), o);
        }
        assert!(Platform::from_ordinal(5).is_none());
    }

    #[test]
    fn median_net_helper_matches_the_order_statistic_convention() {
        assert_eq!(median_net(&[]), 0);
        assert_eq!(median_net(&[5]), 5);
        assert_eq!(median_net(&[1, 2, 3, 4]), 2);
        assert_eq!(median_net(&[1, 2, 3, 4, 5]), 3);
    }
}
