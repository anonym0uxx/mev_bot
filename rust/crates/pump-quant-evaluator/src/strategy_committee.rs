//! `strategy_committee` — ensemble voting for execution decisions (Level 3).
//!
//! When multiple strategy types reach `ShadowValidated` or above, they form
//! a "committee" that votes on each trading opportunity. The committee
//! provides a consensus mechanism that combines validated strategies into
//! an ensemble, reducing the risk of any single strategy type's bias
//! dominating execution decisions.
//!
//! Constitution §56.4 (strategy committee), §13 (determinism), §22 (integer-only).
//! All values are integers (bps, lamports). No floats. No unsafe. Deterministic.

use crate::evaluator_state::LifecycleStage;

// ============================================================================
// Vote types
// ============================================================================

/// A committee member's vote on an opportunity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoteDecision {
    /// Vote to execute the trade.
    Yes,
    /// Vote against execution.
    No,
    /// Member is unsure or has insufficient evidence.
    Abstain,
}

/// One member's vote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemberVote {
    /// The strategy type id casting this vote.
    pub strategy_type_id: u64,
    /// The vote decision.
    pub decision: VoteDecision,
    /// Confidence in basis points (0-10000). Higher = more confident.
    pub confidence_bps: u32,
}

// ============================================================================
// Committee member
// ============================================================================

/// A member of the strategy committee.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    /// Strategy type id for this member.
    pub strategy_type_id: u64,
    /// Voting weight in basis points (0-10000). Higher = more influence.
    /// Weight is derived from Thompson posterior mean and lifecycle stage.
    pub weight_bps: u32,
    /// Current lifecycle stage (must be >= ShadowValidated to vote).
    pub lifecycle_stage: LifecycleStage,
}

impl Member {
    /// Check if this member is eligible to vote (must be ShadowValidated or above).
    #[must_use]
    pub fn can_vote(&self) -> bool {
        self.lifecycle_stage.ordinal() >= LifecycleStage::ShadowValidated.ordinal()
    }
}

// ============================================================================
// Committee verdict
// ============================================================================

/// The committee's combined verdict on an opportunity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitteeVerdict {
    /// True iff the committee approves execution (weighted majority yes).
    pub execute: bool,
    /// Number of yes votes (weighted).
    pub yes_count: u32,
    /// Number of no votes (weighted).
    pub no_count: u32,
    /// Number of abstentions (weighted).
    pub abstain_count: u32,
    /// Total weight that voted (yes + no + abstain).
    pub total_weight_bps: u32,
    /// Net confidence (yes_confidence - no_confidence) in bps.
    pub net_confidence_bps: i64,
    /// Summary string for logging.
    pub summary: String,
}

// ============================================================================
// Committee
// ============================================================================

/// The strategy committee — a collection of validated strategy types that
/// vote on each trading opportunity.
#[derive(Clone, Debug, Default)]
pub struct Committee {
    /// Members of the committee, keyed by strategy type id.
    pub members: Vec<Member>,
}

/// Minimum majority threshold for execution: >50% of voting weight.
const MAJORITY_THRESHOLD_BPS: u32 = 5_000;

impl Committee {
    /// Create a new empty committee.
    #[must_use]
    pub fn new() -> Self {
        Self { members: Vec::new() }
    }

    /// Add a member to the committee.
    pub fn add_member(&mut self, member: Member) {
        // Replace if already exists (same strategy_type_id)
        if let Some(existing) = self.members.iter_mut().find(|m| m.strategy_type_id == member.strategy_type_id) {
            *existing = member;
        } else {
            self.members.push(member);
        }
    }

    /// Conduct a vote on an opportunity.
    ///
    /// Each member that is eligible (ShadowValidated or above) and has a
    /// corresponding vote casts their vote. The verdict is determined by
    /// weighted majority: if the total weight of yes votes exceeds the
    /// total weight of no votes by more than the majority threshold,
    /// execution is approved.
    #[must_use]
    pub fn vote(&self, votes: &[MemberVote]) -> CommitteeVerdict {
        let mut yes_weight: u32 = 0;
        let mut no_weight: u32 = 0;
        let mut abstain_weight: u32 = 0;
        let mut yes_confidence: u64 = 0;
        let mut no_confidence: u64 = 0;

        for member in &self.members {
            if !member.can_vote() {
                continue;
            }
            // Find the vote for this member
            let vote = votes.iter().find(|v| v.strategy_type_id == member.strategy_type_id);
            match vote {
                Some(v) => {
                    let weighted_confidence = (v.confidence_bps as u64 * member.weight_bps as u64) / 10_000;
                    match v.decision {
                        VoteDecision::Yes => {
                            yes_weight += member.weight_bps;
                            yes_confidence += weighted_confidence;
                        }
                        VoteDecision::No => {
                            no_weight += member.weight_bps;
                            no_confidence += weighted_confidence;
                        }
                        VoteDecision::Abstain => {
                            abstain_weight += member.weight_bps;
                        }
                    }
                }
                None => {
                    // Member didn't vote — count as abstain
                    abstain_weight += member.weight_bps;
                }
            }
        }

        let total_weight = yes_weight + no_weight + abstain_weight;
        let net_confidence = yes_confidence as i64 - no_confidence as i64;

        // Execution requires weighted majority: yes_weight > no_weight
        // AND yes_weight > 50% of total voting weight.
        let execute = if total_weight == 0 {
            false
        } else {
            yes_weight > no_weight && yes_weight * 10_000 / total_weight > MAJORITY_THRESHOLD_BPS
        };

        CommitteeVerdict {
            execute,
            yes_count: yes_weight / 10_000,
            no_count: no_weight / 10_000,
            abstain_count: abstain_weight / 10_000,
            total_weight_bps: total_weight,
            net_confidence_bps: net_confidence,
            summary: format!(
                "yes={} no={} abstain={} total={} execute={}",
                yes_weight, no_weight, abstain_weight, total_weight, execute
            ),
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_committee_abstains() {
        let committee = Committee::new();
        let verdict = committee.vote(&[]);
        assert!(!verdict.execute);
        assert_eq!(verdict.yes_count, 0);
        assert_eq!(verdict.no_count, 0);
        assert_eq!(verdict.abstain_count, 0);
    }

    #[test]
    fn single_member_yes_executes() {
        let mut committee = Committee::new();
        committee.add_member(Member {
            strategy_type_id: 0,
            weight_bps: 10_000,
            lifecycle_stage: LifecycleStage::ShadowValidated,
        });
        let votes = vec![MemberVote {
            strategy_type_id: 0,
            decision: VoteDecision::Yes,
            confidence_bps: 8_000,
        }];
        let verdict = committee.vote(&votes);
        assert!(verdict.execute);
    }

    #[test]
    fn majority_required_for_execution() {
        let mut committee = Committee::new();
        committee.add_member(Member {
            strategy_type_id: 0,
            weight_bps: 10_000,
            lifecycle_stage: LifecycleStage::ShadowValidated,
        });
        committee.add_member(Member {
            strategy_type_id: 1,
            weight_bps: 10_000,
            lifecycle_stage: LifecycleStage::ShadowValidated,
        });
        // 1 yes, 1 no -> tie, no majority
        let votes = vec![
            MemberVote { strategy_type_id: 0, decision: VoteDecision::Yes, confidence_bps: 8_000 },
            MemberVote { strategy_type_id: 1, decision: VoteDecision::No, confidence_bps: 7_000 },
        ];
        let verdict = committee.vote(&votes);
        assert!(!verdict.execute);
        assert_eq!(verdict.yes_count, 1);
        assert_eq!(verdict.no_count, 1);
    }

    #[test]
    fn weighted_vote_uses_member_weight() {
        let mut committee = Committee::new();
        committee.add_member(Member {
            strategy_type_id: 0,
            weight_bps: 8_000,
            lifecycle_stage: LifecycleStage::LiveProbeValidated,
        });
        committee.add_member(Member {
            strategy_type_id: 1,
            weight_bps: 2_000,
            lifecycle_stage: LifecycleStage::ShadowValidated,
        });
        let votes = vec![
            MemberVote { strategy_type_id: 0, decision: VoteDecision::Yes, confidence_bps: 9_000 },
            MemberVote { strategy_type_id: 1, decision: VoteDecision::No, confidence_bps: 9_000 },
        ];
        let verdict = committee.vote(&votes);
        assert!(verdict.execute);
    }

    #[test]
    fn unvalidated_members_dont_vote() {
        let mut committee = Committee::new();
        committee.add_member(Member {
            strategy_type_id: 0,
            weight_bps: 10_000,
            lifecycle_stage: LifecycleStage::RegisteredChallenger,
        });
        let votes = vec![MemberVote {
            strategy_type_id: 0,
            decision: VoteDecision::Yes,
            confidence_bps: 8_000,
        }];
        let verdict = committee.vote(&votes);
        assert!(!verdict.execute);
    }
}
