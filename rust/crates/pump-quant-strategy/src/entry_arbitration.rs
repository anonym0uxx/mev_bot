//! # entry_arbitration — §23 candidate arbitration / concurrent-slot allocation
//!
//! When more `EntryEligible` candidates compete than there are free concurrent
//! position slots (or than remaining exposure allows), the entry side must *rank
//! and choose*, not paper-scalp each candidate independently. This leaf mirrors
//! the exit-side optimal-stopping comparator
//! ([`scalp_position::should_exit_on_rate`](crate::scalp_position::should_exit_on_rate))
//! on the entry side:
//!
//! * candidates are ranked by their **conditional expected net SOL** — the
//!   per-candidate expectancy already conditioned on `EntryMode`, archetype,
//!   regime, and the economic cost floor, supplied by the caller;
//! * the scarce concurrent-position slots (and a bounded total exposure) are
//!   awarded to the highest-ranked candidates that clear the cost floor;
//! * the **forgone entry-stage opportunity cost** — the summed conditional
//!   expected net SOL of eligible candidates that lost the arbitration — is
//!   recorded, satisfying §23's "entry-stage opportunity cost" preservation.
//!
//! ## Constitution
//! §22/§99: integer/fixed-point only, bounded state (output is bounded by the
//! candidate slice), deterministic. Ranking is a total order — ties break on the
//! candidate id ascending, so input order never changes the result. Conditional
//! expected net SOL is an *input* (computed upstream from the simulator's
//! expectancy and the economic gate's cost floor); this leaf only arbitrates.

// ---------------------------------------------------------------------------
// Candidate + parameters
// ---------------------------------------------------------------------------

/// One `EntryEligible` candidate competing for a concurrent-position slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryCandidate {
    /// Stable candidate identity (deterministic tie-break key).
    pub candidate_id: u64,
    /// Entry mode the expectancy is conditioned on.
    pub entry_mode: u16,
    /// Setup archetype the expectancy is conditioned on.
    pub archetype: u16,
    /// Market-regime id the expectancy is conditioned on.
    pub regime: u16,
    /// Conditional expected net SOL in lamports (signed; already net of the cost
    /// floor / fees / impact, conditioned on mode+archetype+regime).
    pub expected_net_sol_lamports: i64,
    /// Size this candidate would consume against the exposure budget (lamports).
    pub size_lamports: u64,
}

/// Arbitration limits: the scarce resources slots compete for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArbitrationParams {
    /// Number of free concurrent-position slots to allocate.
    pub max_slots: u32,
    /// Total exposure budget across all awarded slots (lamports).
    pub exposure_cap_lamports: u64,
    /// Cost floor: a candidate must have conditional expected net SOL strictly
    /// greater than this to be eligible (else it is rejected below floor). Set to
    /// `0` to admit any positive-expectancy candidate.
    pub min_expected_net_lamports: i64,
}

/// A slot awarded to a candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotAward {
    /// The winning candidate.
    pub candidate_id: u64,
    /// Size committed against the exposure budget.
    pub size_lamports: u64,
    /// The conditional expected net SOL that won it the slot.
    pub expected_net_sol_lamports: i64,
}

/// The full arbitration result (§23).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArbitrationOutcome {
    /// Awarded slots, highest expected-net-SOL first.
    pub awarded: Vec<SlotAward>,
    /// Total exposure committed across awarded slots.
    pub used_exposure_lamports: u64,
    /// Summed conditional expected net SOL of eligible candidates that were **not**
    /// awarded a slot — the recorded entry-stage opportunity cost (§23).
    pub forgone_opportunity_cost_lamports: i64,
    /// Count of candidates rejected because they did not clear the cost floor.
    pub rejected_below_floor: u32,
}

// ---------------------------------------------------------------------------
// Ranking (leaf helper)
// ---------------------------------------------------------------------------

/// Deterministic total order over candidates: higher conditional expected net SOL
/// first, ties broken by the lower `candidate_id`.
///
/// Returns the eligible candidates (those clearing `min_expected_net_lamports`)
/// sorted best-first, and the count rejected below the floor. Input order does not
/// affect the result.
pub fn rank_eligible(
    candidates: &[EntryCandidate],
    min_expected_net_lamports: i64,
) -> (Vec<EntryCandidate>, u32) {
    let mut eligible: Vec<EntryCandidate> = Vec::with_capacity(candidates.len());
    let mut rejected: u32 = 0;
    for c in candidates {
        if c.expected_net_sol_lamports > min_expected_net_lamports {
            eligible.push(*c);
        } else {
            rejected = rejected.saturating_add(1);
        }
    }
    eligible.sort_by(|a, b| {
        b.expected_net_sol_lamports
            .cmp(&a.expected_net_sol_lamports)
            .then(a.candidate_id.cmp(&b.candidate_id))
    });
    (eligible, rejected)
}

// ---------------------------------------------------------------------------
// Arbitration (leaf: ea_arbitrate)
// ---------------------------------------------------------------------------

/// Arbitrate scarce concurrent-position slots among competing candidates (§23).
///
/// Candidates are ranked by [`rank_eligible`]; slots are then awarded greedily in
/// rank order while a free slot remains **and** the candidate's size fits inside
/// the remaining exposure budget. A candidate whose size would breach the exposure
/// cap is skipped (it becomes forgone) and arbitration continues to the next
/// ranked candidate — a smaller one may still fit. Every eligible candidate that
/// does not receive a slot contributes its conditional expected net SOL to
/// `forgone_opportunity_cost_lamports`.
///
/// Pure integer, deterministic: identical inputs (in any order) yield an identical
/// outcome. Bounded state: the output vector never exceeds `max_slots` entries.
pub fn arbitrate(candidates: &[EntryCandidate], params: &ArbitrationParams) -> ArbitrationOutcome {
    let (ranked, rejected_below_floor) =
        rank_eligible(candidates, params.min_expected_net_lamports);

    let mut awarded: Vec<SlotAward> = Vec::new();
    let mut used_exposure: u64 = 0;
    let mut forgone: i64 = 0;

    for c in &ranked {
        let has_slot = (awarded.len() as u64) < params.max_slots as u64;
        let remaining = params.exposure_cap_lamports.saturating_sub(used_exposure);
        if has_slot && c.size_lamports <= remaining {
            used_exposure = used_exposure.saturating_add(c.size_lamports);
            awarded.push(SlotAward {
                candidate_id: c.candidate_id,
                size_lamports: c.size_lamports,
                expected_net_sol_lamports: c.expected_net_sol_lamports,
            });
        } else {
            // Lost the arbitration: record the forgone entry-stage opportunity cost.
            forgone = forgone.saturating_add(c.expected_net_sol_lamports);
        }
    }

    ArbitrationOutcome {
        awarded,
        used_exposure_lamports: used_exposure,
        forgone_opportunity_cost_lamports: forgone,
        rejected_below_floor,
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: u64, net: i64, size: u64) -> EntryCandidate {
        EntryCandidate {
            candidate_id: id,
            entry_mode: 1,
            archetype: 2,
            regime: 3,
            expected_net_sol_lamports: net,
            size_lamports: size,
        }
    }

    #[test]
    fn ranks_by_expected_net_then_id() {
        let cands = [cand(5, 100, 10), cand(2, 300, 10), cand(9, 100, 10)];
        let (ranked, rejected) = rank_eligible(&cands, 0);
        assert_eq!(rejected, 0);
        // 300 first; then the two 100s tie-broken by ascending id (5 before 9).
        let ids: Vec<u64> = ranked.iter().map(|c| c.candidate_id).collect();
        assert_eq!(ids, vec![2, 5, 9]);
    }

    #[test]
    fn below_floor_candidates_rejected_not_forgone() {
        let cands = [cand(1, 500, 10), cand(2, 0, 10), cand(3, -50, 10)];
        let params = ArbitrationParams {
            max_slots: 4,
            exposure_cap_lamports: 1_000,
            min_expected_net_lamports: 0,
        };
        let out = arbitrate(&cands, &params);
        // Only candidate 1 clears the floor (>0). 2 and 3 are rejected below floor.
        assert_eq!(out.rejected_below_floor, 2);
        assert_eq!(out.awarded.len(), 1);
        assert_eq!(out.awarded[0].candidate_id, 1);
        // Nothing eligible was left unfunded, so no forgone cost.
        assert_eq!(out.forgone_opportunity_cost_lamports, 0);
    }

    #[test]
    fn scarce_slots_go_to_the_top_ranked_and_record_forgone() {
        let cands = [
            cand(1, 100, 10),
            cand(2, 400, 10),
            cand(3, 300, 10),
            cand(4, 200, 10),
        ];
        let params = ArbitrationParams {
            max_slots: 2,
            exposure_cap_lamports: 1_000,
            min_expected_net_lamports: 0,
        };
        let out = arbitrate(&cands, &params);
        // Top two by expected net: 400 (id 2) then 300 (id 3).
        assert_eq!(out.awarded.len(), 2);
        assert_eq!(out.awarded[0].candidate_id, 2);
        assert_eq!(out.awarded[1].candidate_id, 3);
        assert_eq!(out.used_exposure_lamports, 20);
        // Forgone = the two that lost: 200 + 100 = 300.
        assert_eq!(out.forgone_opportunity_cost_lamports, 300);
        assert_eq!(out.rejected_below_floor, 0);
    }

    #[test]
    fn exposure_cap_binds_before_slots() {
        let cands = [cand(1, 500, 700), cand(2, 400, 700), cand(3, 300, 200)];
        let params = ArbitrationParams {
            max_slots: 3,
            exposure_cap_lamports: 1_000,
            min_expected_net_lamports: 0,
        };
        let out = arbitrate(&cands, &params);
        // 1 takes 700 (used=700). 2 needs 700 > 300 remaining -> skipped/forgone.
        // 3 needs 200 <= 300 remaining -> awarded (used=900).
        let ids: Vec<u64> = out.awarded.iter().map(|a| a.candidate_id).collect();
        assert_eq!(ids, vec![1, 3]);
        assert_eq!(out.used_exposure_lamports, 900);
        // Only candidate 2 was forgone.
        assert_eq!(out.forgone_opportunity_cost_lamports, 400);
    }

    #[test]
    fn order_independent() {
        let a = [cand(1, 100, 10), cand(2, 400, 10), cand(3, 300, 10)];
        let b = [cand(3, 300, 10), cand(1, 100, 10), cand(2, 400, 10)];
        let params = ArbitrationParams {
            max_slots: 1,
            exposure_cap_lamports: 1_000,
            min_expected_net_lamports: 0,
        };
        assert_eq!(arbitrate(&a, &params), arbitrate(&b, &params));
    }

    #[test]
    fn empty_and_zero_slots() {
        let params = ArbitrationParams {
            max_slots: 0,
            exposure_cap_lamports: 1_000,
            min_expected_net_lamports: 0,
        };
        // No candidates.
        let empty = arbitrate(&[], &params);
        assert!(empty.awarded.is_empty());
        assert_eq!(empty.forgone_opportunity_cost_lamports, 0);
        // Candidates but zero slots -> all eligible are forgone.
        let out = arbitrate(&[cand(1, 100, 10), cand(2, 50, 10)], &params);
        assert!(out.awarded.is_empty());
        assert_eq!(out.forgone_opportunity_cost_lamports, 150);
    }
}
