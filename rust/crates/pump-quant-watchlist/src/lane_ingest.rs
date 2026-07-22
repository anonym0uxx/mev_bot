//! `wl_lane_ingest` leaf — union multi-lane intake with mint deduplication.
//!
//! Responsibility: take the UNION of candidate observations arriving from many
//! discovery lanes and collapse it to at most one record per mint, keeping the
//! **strongest lane evidence** for each mint. This is the choke point that
//! prevents a token discovered by three lanes from occupying three watchlist
//! slots.
//!
//! Dedup rule (total, deterministic — §22). For a given mint, keep the record
//! maximising, in order:
//!   1. `evidence_strength = discovery_score × per-lane weight_bp` (`u128`);
//!   2. then higher `weight_bp` (stronger lane prior) on ties;
//!   3. then **earlier** `discovered_at` (first sighting wins);
//!   4. then lower `lane` discriminant (stable final tie-break).
//!
//! Every comparison is integer; there is no path where two distinct records
//! compare equal, so the result is independent of input order.
//!
//! Constitution: §22 (deterministic, integer), §99 (output is bounded by the
//! number of distinct mints in the input; a `BTreeMap` gives ordered, non-random
//! iteration).

use crate::candidate::{Candidate, Mint};
use crate::rank::LaneWeights;
use core::cmp::Ordering;
use std::collections::BTreeMap;

/// Compare two candidates for the same mint by lane-evidence strength.
///
/// Returns [`Ordering::Greater`] when `a` is the stronger evidence to keep.
/// Total order per the module's documented rule. §22.
fn evidence_cmp(a: &Candidate, b: &Candidate, weights: &LaneWeights) -> Ordering {
    let sa = a.evidence_strength(weights.get(a.lane));
    let sb = b.evidence_strength(weights.get(b.lane));
    sa.cmp(&sb)
        // Higher lane weight wins on equal strength.
        .then_with(|| weights.get(a.lane).cmp(&weights.get(b.lane)))
        // Earlier discovery wins (reverse: smaller discovered_at => Greater).
        .then_with(|| b.discovered_at.cmp(&a.discovered_at))
        // Stable final tie-break: lower lane discriminant wins.
        .then_with(|| b.lane.cmp(&a.lane))
}

/// Union-ingest an iterator of raw candidates, deduplicating by mint and keeping
/// the strongest lane evidence per mint.
///
/// Returns a mint-keyed [`BTreeMap`] (deterministic iteration order, §22). The
/// output size equals the number of distinct mints — bounded by the input (§99);
/// callers wanting a hard cap feed the result into [`crate::state::WatchlistState`].
#[must_use]
pub fn ingest_union<I>(candidates: I, weights: &LaneWeights) -> BTreeMap<Mint, Candidate>
where
    I: IntoIterator<Item = Candidate>,
{
    let mut best: BTreeMap<Mint, Candidate> = BTreeMap::new();
    for cand in candidates {
        match best.get(&cand.mint) {
            Some(existing) => {
                if evidence_cmp(&cand, existing, weights) == Ordering::Greater {
                    best.insert(cand.mint, cand);
                }
            }
            None => {
                best.insert(cand.mint, cand);
            }
        }
    }
    best
}
