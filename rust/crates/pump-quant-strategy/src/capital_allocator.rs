//! # capital_allocator — reweight + detection-source guards (criterion 85)
//!
//! Three deterministic leaves for the CapitalAllocator:
//!
//! * [`reweight`] — distribute allocation across lanes strictly inside each lane's
//!   registered envelope `[min_bps, max_bps]`, and refuse (zero) any lane without a
//!   promoted policy: live capital cannot be deployed to a category that has not
//!   earned promotion.
//! * [`allocate_to_category`] — the standalone hard guard that refuses a category
//!   with no promoted policy.
//! * [`admit_rotation_trigger`] — the rotation-detection input-source guard:
//!   accept only on-chain-derived triggers, reject loss-triggered or social-led
//!   ones (rotation is detected from on-chain flow, never chased on a drawdown or
//!   a narrative).
//!
//! ## Constitution
//! §85 CapitalAllocator, §56.2 rotation detection. §22 integer bps; deterministic,
//! pure — the continuous-detection orchestration lives in the supervisor.

/// A lane's allocation request with its registered envelope and promotion status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaneRequest {
    /// Lane identifier.
    pub lane_id: u32,
    /// Requested weight in bps (before envelope clamping).
    pub requested_bps: u32,
    /// Registered envelope lower bound (bps).
    pub min_bps: u32,
    /// Registered envelope upper bound (bps).
    pub max_bps: u32,
    /// Whether this lane has a promoted policy (else it earns zero).
    pub has_promoted_policy: bool,
}

/// A granted allocation for one lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaneGrant {
    /// Lane identifier.
    pub lane_id: u32,
    /// Granted weight in bps.
    pub granted_bps: u32,
}

/// Why a reweight failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllocError {
    /// A lane's envelope is ill-formed (`min_bps > max_bps`).
    InvalidEnvelope {
        /// The offending lane.
        lane_id: u32,
    },
}

/// Why a category was refused live capital.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CategoryReject {
    /// No promoted policy — cannot deploy live capital in this category.
    NoPromotedPolicy,
}

/// The standalone promoted-policy guard (leaf helper for **ca_reweight**).
///
/// Refuses any category without a promoted policy; otherwise clamps the requested
/// weight into `[min_bps, max_bps]`.
pub fn allocate_to_category(
    has_promoted_policy: bool,
    requested_bps: u32,
    min_bps: u32,
    max_bps: u32,
) -> Result<u32, CategoryReject> {
    if !has_promoted_policy {
        return Err(CategoryReject::NoPromotedPolicy);
    }
    Ok(requested_bps.clamp(min_bps, max_bps))
}

/// Reweight allocation across lanes within envelopes (leaf **ca_reweight**).
///
/// For each lane: an ill-formed envelope aborts with [`AllocError::InvalidEnvelope`];
/// a lane without a promoted policy is granted `0`; otherwise the requested weight
/// is clamped into the lane's registered `[min_bps, max_bps]`. Output order mirrors
/// input order. Deterministic and allocation-envelope-bounded — no lane can be
/// granted outside its registered bounds.
pub fn reweight(lanes: &[LaneRequest]) -> Result<Vec<LaneGrant>, AllocError> {
    let mut grants = Vec::with_capacity(lanes.len());
    for l in lanes {
        if l.min_bps > l.max_bps {
            return Err(AllocError::InvalidEnvelope { lane_id: l.lane_id });
        }
        let granted_bps = if l.has_promoted_policy {
            l.requested_bps.clamp(l.min_bps, l.max_bps)
        } else {
            0
        };
        grants.push(LaneGrant {
            lane_id: l.lane_id,
            granted_bps,
        });
    }
    Ok(grants)
}

/// The source of a rotation-detection trigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerSource {
    /// Derived from on-chain flow — the only admissible source.
    OnChainDerived,
    /// Triggered by the system's own realized loss — rejected.
    LossTriggered,
    /// Led by a social/narrative signal — rejected.
    SocialLed,
}

/// Why a rotation trigger was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RotationReject {
    /// The trigger came from a realized loss, not on-chain evidence.
    LossTriggered,
    /// The trigger was social-led, not on-chain evidence.
    SocialLed,
}

/// The rotation-detection input-source guard (leaf **ca_rotation_source**).
///
/// Accepts only [`TriggerSource::OnChainDerived`]; loss-triggered and social-led
/// rotations are rejected with the matching reason. Pure and deterministic.
pub fn admit_rotation_trigger(source: TriggerSource) -> Result<(), RotationReject> {
    match source {
        TriggerSource::OnChainDerived => Ok(()),
        TriggerSource::LossTriggered => Err(RotationReject::LossTriggered),
        TriggerSource::SocialLed => Err(RotationReject::SocialLed),
    }
}
