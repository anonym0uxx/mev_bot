//! Amplification-graph edge scoring (constitution §29.8 D8).
//!
//! # Responsibility
//! Turn detected copy-echo relationships into a weighted directed amplification graph
//! (originator → echo), score each edge by semantic similarity and temporal
//! proximity, and expose the per-source originator share that D8 consumes. Echo
//! centrality is reach, not alpha (§29.8), so this module measures *who originates vs
//! who repeats*, never treating echo reach as an entry signal. Deterministic integer
//! (§22).

use crate::copy_echo::CopyEchoEdge;
use crate::fixedpoint::{clamp_i128_to_i64, ratio_bps, BPS_SCALE};

/// A weighted directed edge in the amplification graph: `from_source` originated
/// content that `to_source` amplified, with accumulated `weight_bps`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AmplificationEdge {
    /// Originating source.
    pub from_source: u64,
    /// Amplifying (echo) source.
    pub to_source: u64,
    /// Accumulated edge weight in bps (capped at [`BPS_SCALE`]).
    pub weight_bps: i64,
}

/// Score a single amplification observation: `similarity × temporal_proximity`.
///
/// Temporal proximity decays linearly from `BPS_SCALE` at zero lag to `0` at
/// `max_lag_ns` (and stays `0` beyond). So a near-instant, near-identical repost
/// scores near `10_000`; a slow, loosely-similar one scores low. `max_lag_ns == 0`
/// yields `0` (no window). Result is in bps.
#[must_use]
pub fn score_edge(similarity_bps: i64, lag_ns: u64, max_lag_ns: u64) -> i64 {
    if max_lag_ns == 0 {
        return 0;
    }
    let proximity = if lag_ns >= max_lag_ns {
        0
    } else {
        BPS_SCALE - ratio_bps(lag_ns, max_lag_ns)
    };
    let w = (similarity_bps as i128) * (proximity as i128) / (BPS_SCALE as i128);
    clamp_i128_to_i64(w)
}

/// Build the aggregated amplification graph from copy-echo edges.
///
/// Each copy-echo relationship contributes [`score_edge`] to the `(originator, echo)`
/// directed edge; repeated observations of the same pair accumulate (saturating,
/// capped at [`BPS_SCALE`]). Output edges are sorted deterministically by
/// `(from_source, to_source)`. Memory-bounded by the number of distinct directed
/// pairs, not by the raw observation count.
#[must_use]
pub fn build_amplification_graph(
    edges: &[CopyEchoEdge],
    max_lag_ns: u64,
) -> Vec<AmplificationEdge> {
    // Aggregate into a stable, sorted vec of ((from,to), weight). No hashing, so the
    // result is fully deterministic regardless of platform hash seeding.
    let mut acc: Vec<((u64, u64), i64)> = Vec::new();
    for e in edges {
        let w = score_edge(e.similarity_bps, e.lag_ns, max_lag_ns);
        let key = (e.originator, e.echo);
        match acc.binary_search_by(|probe| probe.0.cmp(&key)) {
            Ok(idx) => {
                acc[idx].1 = acc[idx].1.saturating_add(w).min(BPS_SCALE);
            }
            Err(idx) => acc.insert(idx, (key, w.min(BPS_SCALE))),
        }
    }
    acc.into_iter()
        .map(|((from_source, to_source), weight_bps)| AmplificationEdge {
            from_source,
            to_source,
            weight_bps,
        })
        .collect()
}

/// Originator share for a source in bps: `out-degree / (out-degree + in-degree)`,
/// counting distinct directed edges.
///
/// This is the D8 network-position measurement: a source that mostly *originates*
/// (high out-degree) scores high; a pure echo (high in-degree, low out-degree) scores
/// low. A source with no edges either way yields `0` (no evidence). Suitable to pass
/// as `originator_count` / `echo_count` to
/// [`crate::determinants::d8_originality`] — see also
/// [`originator_echo_counts`].
#[must_use]
pub fn originator_fraction_bps(source_id: u64, edges: &[AmplificationEdge]) -> i64 {
    let (orig, echo) = originator_echo_counts(source_id, edges);
    let total = orig.saturating_add(echo);
    ratio_bps(u64::from(orig), u64::from(total))
}

/// Distinct out-degree (times this source originated) and in-degree (times it echoed)
/// in the amplification graph. Feeds [`crate::determinants::d8_originality`].
#[must_use]
pub fn originator_echo_counts(source_id: u64, edges: &[AmplificationEdge]) -> (u32, u32) {
    let mut orig: u32 = 0;
    let mut echo: u32 = 0;
    for e in edges {
        if e.from_source == source_id {
            orig = orig.saturating_add(1);
        }
        if e.to_source == source_id {
            echo = echo.saturating_add(1);
        }
    }
    (orig, echo)
}
