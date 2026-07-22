//! `wl_promote` leaf — hand the strongest candidates to the scalp pipeline.
//!
//! Responsibility: select, from the bounded watchlist [`WatchlistState`], the
//! top-`k` candidates by rank at logical time `now` that also clear a minimum
//! rank floor, and return them strongest-first for the downstream scalp
//! pipeline. This is where the "eye" hands work to the "hands". It is a pure
//! read of state — it does not mutate the watchlist and it does not submit
//! anything (submission is out of scope [S]).
//!
//! Constitution: §22 (deterministic selection, integer rank), §99 (result
//! bounded by `k`).

use crate::candidate::Candidate;
use crate::state::WatchlistState;

/// Promote the top-`k` candidates whose rank at `now` is `>= min_rank`.
///
/// Returns candidates strongest-first with the deterministic mint tie-break of
/// [`WatchlistState::ranked`] (§22). Candidates whose recency has decayed to
/// rank 0 are filtered out unless `min_rank == 0`. The result holds at most `k`
/// elements (§99); `k == 0` yields an empty vector.
#[must_use]
pub fn promote_top(state: &WatchlistState, now: u64, k: usize, min_rank: u64) -> Vec<Candidate> {
    if k == 0 {
        return Vec::new();
    }
    state
        .ranked(now)
        .into_iter()
        .filter(|(rank, _)| *rank >= min_rank)
        .take(k)
        .map(|(_, cand)| cand)
        .collect()
}
