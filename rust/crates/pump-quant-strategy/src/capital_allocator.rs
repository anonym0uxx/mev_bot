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

use crate::probe_ladder::LadderConfig;

// ---------------------------------------------------------------------------
// Deployable-capital sizing derivation (leaf: ca_derive_sizing, §1)
// ---------------------------------------------------------------------------

/// Basis-points scale for the sizing derivation (`10_000 bps == 100%`).
const SIZING_BPS_SCALE: u128 = 10_000;

/// Operator-tunable fractions (bps of deployable capital) that shape the derived
/// sizing envelope. Every field is a *fraction of the single verified deployable
/// figure* — never an absolute lamport number — so §1 ("all probe tiers,
/// calibration caps, exposure limits, and the MinimumEconomicTradeGate derive
/// from the current verified deployable capital, never from any number written in
/// this document") holds by construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SizingParams {
    /// Rung-0 base probe size as a fraction of deployable capital (bps).
    pub base_probe_bps: u32,
    /// Highest rung index the derived ladder may occupy.
    pub max_rung: u8,
    /// Absolute per-position cap as a fraction of deployable capital (bps).
    pub per_position_cap_bps: u32,
    /// Total concurrent exposure cap as a fraction of deployable capital (bps).
    pub exposure_cap_bps: u32,
    /// Calibration-probe budget as a fraction of deployable capital (bps).
    pub calibration_cap_bps: u32,
    /// MinimumEconomicTradeGate floor as a fraction of deployable capital (bps).
    pub min_economic_bps: u32,
}

impl SizingParams {
    /// A deterministic fixture: 1% base probe, 4-rung ladder, 8% per-position cap,
    /// 40% concurrent exposure, 5% calibration budget, 0.5% economic floor.
    pub fn test() -> Self {
        SizingParams {
            base_probe_bps: 100,
            max_rung: 4,
            per_position_cap_bps: 800,
            exposure_cap_bps: 4_000,
            calibration_cap_bps: 500,
            min_economic_bps: 50,
        }
    }
}

/// The full sizing envelope derived from one verified deployable-capital figure.
///
/// [`probe_ladder`](crate::probe_ladder) consumes `ladder` in place of the
/// hardcoded `LadderConfig::test()` fixture; the caps below bound exposure,
/// calibration spend, and the economic gate. On any verified capital change the
/// caller re-runs [`derive_sizing`] and every number here moves with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeployableSizing {
    /// Probe ladder whose base and per-position cap track deployable capital.
    pub ladder: LadderConfig,
    /// Maximum total concurrent exposure in lamports.
    pub exposure_cap_lamports: u64,
    /// Maximum calibration-probe spend in lamports.
    pub calibration_cap_lamports: u64,
    /// MinimumEconomicTradeGate floor in lamports (smallest economically viable size).
    pub min_economic_floor_lamports: u64,
}

/// Derive the full sizing envelope from the current verified deployable capital
/// (leaf **ca_derive_sizing**, §1).
///
/// Every output is `deployable_capital_lamports * fraction_bps / 10_000` computed
/// in `u128` (saturating to `u64::MAX`). The ladder's rung-0 base is additionally
/// clamped to never exceed the per-position cap, preserving the
/// `base <= max_total` invariant `LadderConfig` relies on. No absolute lamport
/// constant appears anywhere in the result — the numbers are a pure function of
/// the one verified figure. Deterministic integer arithmetic.
pub fn derive_sizing(deployable_capital_lamports: u64, params: &SizingParams) -> DeployableSizing {
    let frac = |bps: u32| -> u64 {
        ((deployable_capital_lamports as u128 * bps as u128) / SIZING_BPS_SCALE)
            .min(u64::MAX as u128) as u64
    };
    let per_position_cap = frac(params.per_position_cap_bps);
    // Base probe can never exceed the per-position cap (LadderConfig invariant).
    let base = frac(params.base_probe_bps).min(per_position_cap);
    DeployableSizing {
        ladder: LadderConfig {
            base_probe_lamports: base,
            max_rung: params.max_rung,
            max_total_lamports: per_position_cap,
        },
        exposure_cap_lamports: frac(params.exposure_cap_bps),
        calibration_cap_lamports: frac(params.calibration_cap_bps),
        min_economic_floor_lamports: frac(params.min_economic_bps),
    }
}

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

// ===========================================================================
// Tests — deployable-capital sizing derivation (leaf: ca_derive_sizing)
// ===========================================================================

#[cfg(test)]
mod derive_sizing_tests {
    use super::*;

    const SOL: u64 = 1_000_000_000;

    #[test]
    fn every_cap_is_a_fraction_of_deployable() {
        let deployable = 100 * SOL;
        let s = derive_sizing(deployable, &SizingParams::test());
        // 1% base, 8% per-position cap, 40% exposure, 5% calibration, 0.5% economic.
        assert_eq!(s.ladder.base_probe_lamports, SOL); // 1%
        assert_eq!(s.ladder.max_total_lamports, 8 * SOL); // 8%
        assert_eq!(s.ladder.max_rung, 4);
        assert_eq!(s.exposure_cap_lamports, 40 * SOL); // 40%
        assert_eq!(s.calibration_cap_lamports, 5 * SOL); // 5%
        assert_eq!(s.min_economic_floor_lamports, SOL / 2); // 0.5%
    }

    #[test]
    fn caps_scale_linearly_with_capital() {
        let a = derive_sizing(50 * SOL, &SizingParams::test());
        let b = derive_sizing(100 * SOL, &SizingParams::test());
        // Doubling deployable capital doubles every derived cap.
        assert_eq!(b.exposure_cap_lamports, 2 * a.exposure_cap_lamports);
        assert_eq!(
            b.ladder.base_probe_lamports,
            2 * a.ladder.base_probe_lamports
        );
        assert_eq!(
            b.min_economic_floor_lamports,
            2 * a.min_economic_floor_lamports
        );
    }

    #[test]
    fn base_never_exceeds_per_position_cap() {
        // Pathological params: base fraction > per-position cap fraction.
        let params = SizingParams {
            base_probe_bps: 9_000,
            max_rung: 2,
            per_position_cap_bps: 1_000,
            exposure_cap_bps: 5_000,
            calibration_cap_bps: 100,
            min_economic_bps: 10,
        };
        let s = derive_sizing(100 * SOL, &params);
        assert_eq!(s.ladder.max_total_lamports, 10 * SOL);
        // base clamped down to the per-position cap.
        assert_eq!(s.ladder.base_probe_lamports, 10 * SOL);
        assert!(s.ladder.base_probe_lamports <= s.ladder.max_total_lamports);
    }

    #[test]
    fn zero_deployable_yields_zero_everywhere() {
        let s = derive_sizing(0, &SizingParams::test());
        assert_eq!(s.ladder.base_probe_lamports, 0);
        assert_eq!(s.ladder.max_total_lamports, 0);
        assert_eq!(s.exposure_cap_lamports, 0);
        assert_eq!(s.calibration_cap_lamports, 0);
        assert_eq!(s.min_economic_floor_lamports, 0);
    }

    #[test]
    fn derived_ladder_feeds_probe_ladder_schedule() {
        let s = derive_sizing(100 * SOL, &SizingParams::test());
        // Rung schedule uses the derived base and cap: 1 SOL, 2, 4, 8, clamp at 8.
        assert_eq!(s.ladder.size_at_rung(0), SOL);
        assert_eq!(s.ladder.size_at_rung(1), 2 * SOL);
        assert_eq!(s.ladder.size_at_rung(3), 8 * SOL);
        assert_eq!(s.ladder.size_at_rung(4), 8 * SOL); // clamped at per-position cap
    }

    #[test]
    fn large_capital_does_not_overflow() {
        let s = derive_sizing(u64::MAX, &SizingParams::test());
        // 40% of u64::MAX is a valid u64; no panic / wrap.
        assert!(s.exposure_cap_lamports < u64::MAX);
        assert!(s.exposure_cap_lamports > 0);
    }
}
