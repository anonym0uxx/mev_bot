//! Section 27 / §70.9 **deployer-credibility features**.
//!
//! Point-in-time, distinct-component deployer features feeding the creator /
//! deployer risk model. Per Section 27 these are preserved as **distinct
//! components rather than an opaque creator score**, and every feature is
//! computed at *point-in-time* — future launch outcomes are never used at an
//! earlier decision time.
//!
//! Features produced:
//! * **prior-CA count** — how many contracts/launches this deployer shipped
//!   strictly before the decision slot;
//! * **serial-deploy flag** — whether the deployer's prior launches cluster
//!   into a rapid serial-deployment burst (a serial-rug / volume-farmer tell);
//! * **key / mutual-follower reach** — social reach measured as verified "key"
//!   followers and followers shared with a trusted reference set (these enter
//!   only as research/scoring inputs, never trade authority — Section 28);
//! * **verified-partnership vs self-claimed** — partnerships split into
//!   independently verified vs merely self-asserted counts, because "creator
//!   statements are not truth" (Section 27).
//!
//! All arithmetic is integer and deterministic.

/// A prior launch by the deployer, stamped with its creation slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PriorLaunch {
    /// Slot at which the launch was created.
    pub slot: u64,
}

/// A partnership claim attached to the deployer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartnershipClaim {
    /// Whether the partnership is independently verified (on-chain or otherwise
    /// externally corroborated) rather than merely self-claimed.
    pub verified: bool,
}

/// Social-graph reach inputs (all raw counts; kept distinct per Section 28's
/// "never collapse into one opaque score").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SocialReachInput {
    /// Number of followers who are themselves independently "key" / verified
    /// accounts (weighted reach that is hard to fabricate cheaply).
    pub key_followers: u64,
    /// Number of followers shared with a trusted reference set (mutual-follower
    /// reach into a known-good neighborhood).
    pub mutual_followers_with_reference: u64,
}

/// Configuration for the serial-deploy detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeployerCredibilityConfig {
    /// Width, in slots, of the sliding window used to detect a serial-deploy
    /// burst.
    pub serial_window_slots: u64,
    /// Minimum number of launches within any `serial_window_slots` window that
    /// flags serial deployment.
    pub serial_threshold: u32,
}

/// Point-in-time deployer-credibility feature bundle (distinct components).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeployerCredibility {
    /// Number of prior launches strictly before the decision slot.
    pub prior_ca_count: u32,
    /// Whether the prior launches contain a serial-deployment burst.
    pub serial_deploy_flag: bool,
    /// Largest number of prior launches falling inside any single
    /// `serial_window_slots` window (the value the flag is derived from).
    pub max_launches_in_window: u32,
    /// Verified "key" follower reach (passthrough of the input).
    pub key_follower_reach: u64,
    /// Mutual-follower reach into the trusted reference set.
    pub mutual_follower_reach: u64,
    /// Number of independently verified partnerships.
    pub verified_partnership_count: u32,
    /// Number of merely self-claimed (unverified) partnerships.
    pub self_claimed_partnership_count: u32,
}

/// Compute the deployer-credibility features as of `decision_slot`.
///
/// Only prior launches with `slot < decision_slot` are counted (strict
/// point-in-time discipline: a launch created at the decision slot or later is
/// not yet knowable). The serial-deploy detector runs a two-pointer sliding
/// window over the sorted prior-launch slots and reports the maximum window
/// occupancy; the flag is set when that maximum reaches `serial_threshold`.
/// Partnerships are split into verified vs self-claimed counts (self-claimed
/// carries no credibility, per Section 27).
#[must_use]
pub fn compute_deployer_credibility(
    prior_launches: &[PriorLaunch],
    decision_slot: u64,
    partnerships: &[PartnershipClaim],
    social: &SocialReachInput,
    cfg: &DeployerCredibilityConfig,
) -> DeployerCredibility {
    // Point-in-time filter, then sort ascending by slot.
    let mut slots: Vec<u64> = prior_launches
        .iter()
        .filter(|l| l.slot < decision_slot)
        .map(|l| l.slot)
        .collect();
    slots.sort_unstable();

    let prior_ca_count = u32::try_from(slots.len()).unwrap_or(u32::MAX);

    // Sliding-window max occupancy. Window covers [slots[left], slots[right]]
    // with span <= serial_window_slots - 1 (a window of W slots spans W
    // consecutive slot values inclusive). A zero-width window degenerates to
    // counting exact-slot collisions.
    let mut max_in_window: u32 = 0;
    if !slots.is_empty() {
        let span = cfg.serial_window_slots.saturating_sub(1);
        let mut left = 0usize;
        for right in 0..slots.len() {
            while slots[right].saturating_sub(slots[left]) > span {
                left += 1;
            }
            let count = u32::try_from(right - left + 1).unwrap_or(u32::MAX);
            if count > max_in_window {
                max_in_window = count;
            }
        }
    }
    let serial_deploy_flag = cfg.serial_threshold > 0 && max_in_window >= cfg.serial_threshold;

    let mut verified: u32 = 0;
    let mut self_claimed: u32 = 0;
    for p in partnerships {
        if p.verified {
            verified = verified.saturating_add(1);
        } else {
            self_claimed = self_claimed.saturating_add(1);
        }
    }

    DeployerCredibility {
        prior_ca_count,
        serial_deploy_flag,
        max_launches_in_window: max_in_window,
        key_follower_reach: social.key_followers,
        mutual_follower_reach: social.mutual_followers_with_reference,
        verified_partnership_count: verified,
        self_claimed_partnership_count: self_claimed,
    }
}
