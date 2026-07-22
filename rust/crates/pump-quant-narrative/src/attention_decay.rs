//! §29.6 `AttentionDecayModel` — the complete 18-item attention-tracking fold.
//!
//! This module folds caller-supplied, timestamp-safe stream / comment / reply /
//! creator / post / high-quality-source events into the 18 tracked items of the
//! §29.6 AttentionDecayModel:
//!
//! 1. first mention              10. reply velocity
//! 2. first high-quality source  11. raid activity (wired input)
//! 3. first creator event        12. creator cadence
//! 4. first stream/comment event 13. streamer fatigue
//! 5. post velocity              14. narrative saturation (wired input)
//! 6. post acceleration          15. conversion to new wallets (wired input)
//! 7. semantic duplication (in)  16. conversion to independent breadth (wired)
//! 8. source diversity (input)   17. conversion to net flow (wired input)
//! 9. comment velocity           18. decay after peak
//!
//! Items 7, 8, 11, 14, 15, 16, 17 are produced by other crates (social copy-echo,
//! market-state breadth / meta net-flow, social raid determinant, the narrative
//! lifecycle stage); per the leaf plan they are *wired in as inputs* here rather
//! than recomputed. The remaining items are folded from the event stream.
//!
//! Hard invariants: §22 integer/fixed-point only; deterministic (all time enters
//! as caller-supplied integer nanosecond instants, never a wall clock); pure fold
//! over caller-owned slices (§99 bounded state — no growing state retained).

use crate::narrative::{sat_i64, sat_u64, FP_ONE};

/// Kind of a single timestamped attention event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// An original post/mention of the narrative.
    Post,
    /// A comment on an existing post.
    Comment,
    /// A reply to a comment.
    Reply,
    /// A creator action (dev post, update, event).
    CreatorEvent,
    /// A live-stream / stunt event.
    StreamEvent,
    /// A first appearance on a high-quality (curated) source.
    HighQualitySource,
}

/// One timestamp-safe attention event on a common monotonic integer clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttentionEvent {
    /// Instant in nanoseconds on the caller's monotonic clock (smaller earlier).
    pub ts_ns: u64,
    /// What kind of event this is.
    pub kind: EventKind,
}

/// Cross-crate signals wired into the decay model as inputs (§29.6 items
/// 7,8,11,14,15,16,17 plus the peak/current levels for item 18).
///
/// These are produced elsewhere (social copy-echo, market-state breadth/meta,
/// social raid determinant, narrative lifecycle) and are *not* recomputed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecayInputs {
    /// Item 7 — semantic-duplication density (bps), from social copy-echo.
    pub semantic_duplication_bps: u64,
    /// Item 8 — source diversity: count of independent sources.
    pub source_diversity: u32,
    /// Item 11 — raid activity: count of detected raid bursts (social D7).
    pub raid_activity: u32,
    /// Item 14 — narrative saturation (bps), from the lifecycle stage.
    pub narrative_saturation_bps: u64,
    /// Item 15 — conversion to new wallets (count of first-seen buyers).
    pub conversion_to_new_wallets: u64,
    /// Item 16 — conversion to independent buyer breadth (count).
    pub conversion_to_independent_breadth: u32,
    /// Item 17 — conversion to net flow (signed SOL, from market-state meta).
    pub conversion_to_net_flow: i64,
    /// Peak attention level observed for this narrative (item 18 numerator).
    pub peak_level: u64,
    /// Current attention level (item 18).
    pub current_level: u64,
}

/// The full 18-item AttentionDecayModel state (§29.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttentionDecayModel {
    /// 1 — first mention instant (min ts across all events), `None` if no events.
    pub first_mention_ns: Option<u64>,
    /// 2 — first high-quality-source instant.
    pub first_high_quality_source_ns: Option<u64>,
    /// 3 — first creator-event instant.
    pub first_creator_event_ns: Option<u64>,
    /// 4 — first stream-or-comment-event instant.
    pub first_stream_comment_event_ns: Option<u64>,
    /// 5 — post velocity: posts in the current window.
    pub post_velocity: u64,
    /// 6 — post acceleration: current-window posts − previous-window posts.
    pub post_acceleration: i64,
    /// 7 — semantic duplication (wired input, bps).
    pub semantic_duplication_bps: u64,
    /// 8 — source diversity (wired input).
    pub source_diversity: u32,
    /// 9 — comment velocity: comments in the current window.
    pub comment_velocity: u64,
    /// 10 — reply velocity: replies in the current window.
    pub reply_velocity: u64,
    /// 11 — raid activity (wired input).
    pub raid_activity: u32,
    /// 12 — creator cadence: creator events in the current window.
    pub creator_cadence: u64,
    /// 13 — streamer fatigue: previous-window stream events − current-window
    /// (positive == declining stream engagement).
    pub streamer_fatigue: i64,
    /// 14 — narrative saturation (wired input, bps).
    pub narrative_saturation_bps: u64,
    /// 15 — conversion to new wallets (wired input).
    pub conversion_to_new_wallets: u64,
    /// 16 — conversion to independent breadth (wired input).
    pub conversion_to_independent_breadth: u32,
    /// 17 — conversion to net flow (wired input).
    pub conversion_to_net_flow: i64,
    /// 18 — decay after peak (bps): `(peak − current) / peak`, `0` while at/above
    /// peak or when peak is unknown.
    pub decay_after_peak_bps: u64,
}

/// Count events of `kind` whose `ts_ns` falls in `(lo, hi]`.
fn count_in(events: &[AttentionEvent], kind: EventKind, lo: u64, hi: u64) -> u64 {
    let mut n: u64 = 0;
    for e in events {
        if e.kind == kind && e.ts_ns > lo && e.ts_ns <= hi {
            n = n.saturating_add(1);
        }
    }
    n
}

/// Minimum `ts_ns` among events matching `pred`, or `None` if none match.
fn first_ns(events: &[AttentionEvent], pred: impl Fn(EventKind) -> bool) -> Option<u64> {
    events
        .iter()
        .filter(|e| pred(e.kind))
        .map(|e| e.ts_ns)
        .min()
}

/// Fold events + wired inputs into the 18-item [`AttentionDecayModel`].
///
/// `now_ns` is the caller's evaluation instant and `window_ns` the velocity
/// window. The current window is `(now_ns − window_ns, now_ns]`; the previous
/// window is `(now_ns − 2·window_ns, now_ns − window_ns]`. Velocities count
/// events in the current window; accelerations/fatigue compare the two windows.
///
/// First-event instants are the minimum timestamp per kind (item 4 is the min of
/// stream *or* comment events). Decay-after-peak is a fixed-point fraction below
/// the supplied peak. Pure/total/deterministic; overflow saturates (§22).
pub fn nv_attention_decay(
    events: &[AttentionEvent],
    now_ns: u64,
    window_ns: u64,
    inputs: &DecayInputs,
) -> AttentionDecayModel {
    // Window bounds (saturating so a huge window cannot underflow, §22).
    let cur_lo = now_ns.saturating_sub(window_ns);
    let prev_lo = now_ns.saturating_sub(window_ns.saturating_mul(2));

    let post_cur = count_in(events, EventKind::Post, cur_lo, now_ns);
    let post_prev = count_in(events, EventKind::Post, prev_lo, cur_lo);
    // i128 difference then saturating narrow (§22).
    let post_acceleration = sat_i64(post_cur as i128 - post_prev as i128);

    let stream_cur = count_in(events, EventKind::StreamEvent, cur_lo, now_ns);
    let stream_prev = count_in(events, EventKind::StreamEvent, prev_lo, cur_lo);
    let streamer_fatigue = sat_i64(stream_prev as i128 - stream_cur as i128);

    let decay_after_peak_bps =
        if inputs.peak_level == 0 || inputs.current_level >= inputs.peak_level {
            0
        } else {
            let drop = inputs.peak_level - inputs.current_level; // current < peak here.
            sat_u64(drop as u128 * FP_ONE as u128 / inputs.peak_level as u128)
        };

    AttentionDecayModel {
        first_mention_ns: first_ns(events, |_| true),
        first_high_quality_source_ns: first_ns(events, |k| k == EventKind::HighQualitySource),
        first_creator_event_ns: first_ns(events, |k| k == EventKind::CreatorEvent),
        first_stream_comment_event_ns: first_ns(events, |k| {
            k == EventKind::StreamEvent || k == EventKind::Comment
        }),
        post_velocity: post_cur,
        post_acceleration,
        semantic_duplication_bps: inputs.semantic_duplication_bps,
        source_diversity: inputs.source_diversity,
        comment_velocity: count_in(events, EventKind::Comment, cur_lo, now_ns),
        reply_velocity: count_in(events, EventKind::Reply, cur_lo, now_ns),
        raid_activity: inputs.raid_activity,
        creator_cadence: count_in(events, EventKind::CreatorEvent, cur_lo, now_ns),
        streamer_fatigue,
        narrative_saturation_bps: inputs.narrative_saturation_bps,
        conversion_to_new_wallets: inputs.conversion_to_new_wallets,
        conversion_to_independent_breadth: inputs.conversion_to_independent_breadth,
        conversion_to_net_flow: inputs.conversion_to_net_flow,
        decay_after_peak_bps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> DecayInputs {
        DecayInputs {
            semantic_duplication_bps: 1_500,
            source_diversity: 12,
            raid_activity: 2,
            narrative_saturation_bps: 3_000,
            conversion_to_new_wallets: 40,
            conversion_to_independent_breadth: 9,
            conversion_to_net_flow: 25_000,
            peak_level: 1_000,
            current_level: 600,
        }
    }

    fn ev(ts_ns: u64, kind: EventKind) -> AttentionEvent {
        AttentionEvent { ts_ns, kind }
    }

    #[test]
    fn empty_events_yield_no_first_instants_and_zero_velocities() {
        let m = nv_attention_decay(&[], 10_000, 1_000, &inputs());
        assert_eq!(m.first_mention_ns, None);
        assert_eq!(m.first_high_quality_source_ns, None);
        assert_eq!(m.first_creator_event_ns, None);
        assert_eq!(m.first_stream_comment_event_ns, None);
        assert_eq!(m.post_velocity, 0);
        assert_eq!(m.post_acceleration, 0);
        assert_eq!(m.comment_velocity, 0);
        assert_eq!(m.reply_velocity, 0);
        // Wired inputs still surface.
        assert_eq!(m.semantic_duplication_bps, 1_500);
        assert_eq!(m.source_diversity, 12);
        assert_eq!(m.raid_activity, 2);
        assert_eq!(m.conversion_to_net_flow, 25_000);
    }

    #[test]
    fn first_instants_are_minimum_per_kind() {
        let events = [
            ev(500, EventKind::Post),
            ev(300, EventKind::HighQualitySource),
            ev(900, EventKind::HighQualitySource),
            ev(700, EventKind::CreatorEvent),
            ev(650, EventKind::Comment),
            ev(800, EventKind::StreamEvent),
        ];
        let m = nv_attention_decay(&events, 10_000, 1_000, &inputs());
        assert_eq!(m.first_mention_ns, Some(300)); // global min
        assert_eq!(m.first_high_quality_source_ns, Some(300));
        assert_eq!(m.first_creator_event_ns, Some(700));
        // min of stream(800) or comment(650) = 650.
        assert_eq!(m.first_stream_comment_event_ns, Some(650));
    }

    #[test]
    fn window_velocities_and_acceleration() {
        // now=10_000, window=1_000. current=(9_000,10_000], prev=(8_000,9_000].
        let events = [
            ev(9_500, EventKind::Post), // current
            ev(9_800, EventKind::Post), // current
            ev(9_900, EventKind::Post), // current
            ev(8_500, EventKind::Post), // prev
            ev(7_000, EventKind::Post), // older, ignored
            ev(9_600, EventKind::Comment),
            ev(9_700, EventKind::Reply),
            ev(9_100, EventKind::CreatorEvent),
        ];
        let m = nv_attention_decay(&events, 10_000, 1_000, &inputs());
        assert_eq!(m.post_velocity, 3);
        assert_eq!(m.post_acceleration, 3 - 1); // 2
        assert_eq!(m.comment_velocity, 1);
        assert_eq!(m.reply_velocity, 1);
        assert_eq!(m.creator_cadence, 1);
    }

    #[test]
    fn window_boundaries_are_half_open() {
        // Event exactly at now is included; exactly at cur_lo is excluded.
        let events = [
            ev(10_000, EventKind::Post), // == now -> included
            ev(9_000, EventKind::Post),  // == cur_lo -> excluded from current
        ];
        let m = nv_attention_decay(&events, 10_000, 1_000, &inputs());
        assert_eq!(m.post_velocity, 1);
        // The 9_000 event is the top of the previous window (8_000,9_000].
        assert_eq!(m.post_acceleration, 1 - 1);
    }

    #[test]
    fn streamer_fatigue_positive_when_declining() {
        // prev window has 2 stream events, current has 0 -> fatigue +2.
        let events = [
            ev(8_200, EventKind::StreamEvent),
            ev(8_800, EventKind::StreamEvent),
        ];
        let m = nv_attention_decay(&events, 10_000, 1_000, &inputs());
        assert_eq!(m.streamer_fatigue, 2);
    }

    #[test]
    fn decay_after_peak_fraction() {
        let mut inp = inputs();
        inp.peak_level = 1_000;
        inp.current_level = 600;
        let m = nv_attention_decay(&[], 10_000, 1_000, &inp);
        // (1000-600)/1000 = 0.4 -> 4_000 bps.
        assert_eq!(m.decay_after_peak_bps, 4_000);

        inp.current_level = 1_000; // at peak
        let m = nv_attention_decay(&[], 10_000, 1_000, &inp);
        assert_eq!(m.decay_after_peak_bps, 0);

        inp.current_level = 1_200; // above peak (fresh high)
        let m = nv_attention_decay(&[], 10_000, 1_000, &inp);
        assert_eq!(m.decay_after_peak_bps, 0);

        inp.peak_level = 0; // unknown peak
        let m = nv_attention_decay(&[], 10_000, 1_000, &inp);
        assert_eq!(m.decay_after_peak_bps, 0);
    }

    #[test]
    fn saturating_window_does_not_underflow() {
        // window larger than now must not panic; everything falls in current.
        let events = [ev(5, EventKind::Post)];
        let m = nv_attention_decay(&events, 10, 1_000_000, &inputs());
        assert_eq!(m.post_velocity, 1);
    }
}
