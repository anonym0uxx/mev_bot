//! Deterministic value-of-information (VOI) ranking of the open-hypothesis
//! research queue (§56.10).
//!
//! Responsibility: given the four VOI inputs the knowledge base records for each
//! open hypothesis — *expected net-SOL impact if true*, *probability given prior
//! evidence*, *cost to test*, and *edge half-life* — compute a single, totally
//! ordered priority score and rank the queue so research compute flows to the
//! highest expected-value questions first.
//!
//! The score is pure integer / fixed-point arithmetic with explicit overflow
//! handling (§22): no float, no wall clock, no RNG, and identical inputs always
//! produce an identical ranking.
//!
//! ## Model
//! Confirming a true edge lets the system capture value over the horizon the edge
//! remains exploitable, which is proportional to its half-life. So the expected
//! gross value of resolving a hypothesis is
//!
//! ```text
//! gross = expected_impact_lamports * prob_true_bps/10_000 * edge_half_life_secs/REF_HALF_LIFE_SECS
//! ```
//!
//! and the VOI is that gross value net of the cost to run the deciding
//! experiment:
//!
//! ```text
//! voi = gross - cost_to_test_lamports
//! ```
//!
//! Every division is integer (truncating toward zero); the reference half-life
//! [`REF_HALF_LIFE_SECS`] fixes the units so a hypothesis whose edge lasts exactly
//! the reference horizon is scored at its full probability-weighted impact.

use crate::rows::{Hypothesis, HypothesisId};

/// Basis-point denominator (1.0 == 10_000 bps).
pub const BPS_DENOM: i128 = 10_000;

/// Reference edge half-life in seconds (one day). An edge with this half-life is
/// scored at its full probability-weighted impact; shorter edges are discounted
/// and longer edges are credited, proportionally.
pub const REF_HALF_LIFE_SECS: i128 = 86_400;

/// Combined VOI denominator: `BPS_DENOM * REF_HALF_LIFE_SECS`. Small enough that
/// the division never overflows.
const VOI_DENOM: i128 = BPS_DENOM * REF_HALF_LIFE_SECS;

/// Compute the deterministic VOI score of a single hypothesis, in lamports.
///
/// Overflow contract (§22): the three-way product
/// `expected_impact * prob_bps * half_life` is formed with `checked_mul`; if it
/// would exceed `i128`, the gross term **saturates** to `i128::MAX` / `i128::MIN`
/// (preserving the sign of the impact), and the final subtraction of the cost is
/// `saturating_sub`. This is saturating-by-contract, chosen because a hypothesis
/// whose magnitudes overflow `i128` is, correctly, ranked at the extreme rather
/// than wrapping to a nonsense value.
///
/// Sign: a hypothesis with negative expected impact (a fade/avoid claim expressed
/// as negative net-SOL) yields a negative gross term, so its VOI is dominated by
/// its cost — it sinks in the ranking unless its avoidance value is expressed as
/// a positive impact by the caller.
#[must_use]
pub fn voi_score(h: &Hypothesis) -> i128 {
    let impact = h.expected_impact_lamports;
    let prob = i128::from(h.prob_true_bps);
    let half_life = i128::from(h.edge_half_life_secs);
    let cost = i128::from(h.cost_to_test_lamports);

    let gross = match impact
        .checked_mul(prob)
        .and_then(|v| v.checked_mul(half_life))
    {
        Some(numerator) => numerator / VOI_DENOM,
        None => {
            // Overflowed i128: saturate preserving the sign of the impact.
            if impact >= 0 {
                i128::MAX
            } else {
                i128::MIN
            }
        }
    };

    gross.saturating_sub(cost)
}

/// A hypothesis id paired with its computed VOI score, as produced by
/// [`rank`]. Public so tests and callers can inspect the exact scores, not just
/// the order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RankedHypothesis {
    /// The hypothesis.
    pub id: HypothesisId,
    /// Its VOI score in lamports (see [`voi_score`]).
    pub score: i128,
}

/// Rank the given hypotheses by descending VOI, breaking ties by ascending
/// [`HypothesisId`] so the ordering is total and deterministic (§22 — stable
/// iteration order). The input slice is not required to be pre-filtered; callers
/// wanting only open hypotheses should use [`rank_open`].
#[must_use]
pub fn rank(hypotheses: &[Hypothesis]) -> Vec<RankedHypothesis> {
    let mut ranked: Vec<RankedHypothesis> = hypotheses
        .iter()
        .map(|h| RankedHypothesis {
            id: h.id,
            score: voi_score(h),
        })
        .collect();
    // Descending score; ascending id on ties. Total order => deterministic.
    ranked.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
    ranked
}

/// Rank only the *open* hypotheses (§56.10): those whose inference state is still
/// unresolved (see [`crate::rows::InferenceState::is_open`]). Validated, rejected,
/// and expired hypotheses are excluded from the research queue.
#[must_use]
pub fn rank_open(hypotheses: &[Hypothesis]) -> Vec<RankedHypothesis> {
    let open: Vec<Hypothesis> = hypotheses
        .iter()
        .filter(|h| h.status.is_open())
        .cloned()
        .collect();
    rank(&open)
}
