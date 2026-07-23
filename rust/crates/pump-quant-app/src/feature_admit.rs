//! §46 Signal-Horizon Matching Law: feature admission (LAW 19).
//!
//! A feature is admissible to a decision lane only when two conditions hold: its
//! horizon *class* is permitted for the lane (launch-time social linkage is an
//! entry-lane input; TikTok virality is confined to context/meta; on-chain flow is
//! admissible everywhere), AND its measured end-to-end detection+capture latency
//! `L` beats the lane's decision horizon `H` with margin (`L + margin <= H`). Slow
//! intelligence can inform holds/exits/sizing/meta but is structurally excluded
//! from any entry lane whose horizon it cannot beat.
//!
//! This module is the app-side admission request: it carries the measured latency
//! and the target lane's horizon alongside the feature's class and lane, and calls
//! the frozen `pump_quant_strategy::signal_horizon` verdict — the matching law is
//! consulted, never re-implemented. Report-plane / additive: nothing here mutates
//! the decision journal, so a run that never admits a feature is byte-identical.

use pump_quant_strategy::signal_horizon::{
    admit_feature_to_lane, FeatureClass, HorizonVerdict, Lane,
};

/// A feature-admission request: which feature (its horizon class), the lane it is
/// proposed for (with that lane's decision horizon), the feature's measured
/// end-to-end latency, and the safety margin the latency must clear the horizon by.
/// All times are nanoseconds (§22 integer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeatureAdmissionRequest {
    /// The feature's horizon class (drives the classification-table check).
    pub class: FeatureClass,
    /// The lane the feature is proposed to inform.
    pub lane: Lane,
    /// Measured end-to-end detection + capture latency of the feature, ns.
    pub feature_latency_ns: u64,
    /// The lane's natural decision horizon, ns.
    pub lane_horizon_ns: u64,
    /// The margin the latency must beat the horizon by, ns.
    pub margin_ns: u64,
}

impl FeatureAdmissionRequest {
    /// Construct a request.
    #[must_use]
    pub fn new(
        class: FeatureClass,
        lane: Lane,
        feature_latency_ns: u64,
        lane_horizon_ns: u64,
        margin_ns: u64,
    ) -> Self {
        FeatureAdmissionRequest {
            class,
            lane,
            feature_latency_ns,
            lane_horizon_ns,
            margin_ns,
        }
    }
}

/// Admit (or reject) a feature to its proposed lane under the §46 Signal-Horizon
/// Matching Law by consulting the frozen `signal_horizon` verdict. A feature whose
/// class is forbidden for the lane, or whose measured latency does not beat the
/// lane horizon with margin, is rejected; only a class-permitted, fast-enough
/// feature is [`HorizonVerdict::Admissible`]. Pure and deterministic.
#[must_use]
pub fn admit_feature(req: &FeatureAdmissionRequest) -> HorizonVerdict {
    admit_feature_to_lane(
        req.feature_latency_ns,
        req.class,
        req.lane,
        req.lane_horizon_ns,
        req.margin_ns,
    )
}

/// Whether a request is admissible (convenience over [`admit_feature`]).
#[must_use]
pub fn is_admissible(req: &FeatureAdmissionRequest) -> bool {
    matches!(admit_feature(req), HorizonVerdict::Admissible)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fast on-chain-flow feature into an entry lane whose horizon it beats is
    /// admitted.
    #[test]
    fn fast_onchain_flow_into_entry_lane_is_admitted() {
        let req = FeatureAdmissionRequest::new(
            FeatureClass::OnChainFlow,
            Lane::CreationSniper,
            100_000_000,   // 100ms latency
            1_000_000_000, // 1s horizon
            50_000_000,    // 50ms margin
        );
        assert_eq!(admit_feature(&req), HorizonVerdict::Admissible);
        assert!(is_admissible(&req));
    }

    /// A slow feature whose latency does not beat the entry-lane horizon with
    /// margin is rejected as too slow — a horizon mismatch.
    #[test]
    fn slow_feature_mismatches_entry_lane_horizon() {
        let req = FeatureAdmissionRequest::new(
            FeatureClass::XText,
            Lane::CreationSniper,
            2_000_000_000, // 2s latency
            1_000_000_000, // 1s horizon — cannot be beaten
            0,
        );
        assert_eq!(admit_feature(&req), HorizonVerdict::TooSlow);
        assert!(!is_admissible(&req));
    }

    /// A structurally-late feature class (TikTok virality) is class-forbidden from
    /// an entry lane even if it were somehow fast enough.
    #[test]
    fn late_class_is_forbidden_from_entry_lane() {
        let req = FeatureAdmissionRequest::new(
            FeatureClass::TikTokVirality,
            Lane::CreationSniper,
            1, // trivially fast, but the class table forbids it here
            u64::MAX,
            0,
        );
        assert_eq!(admit_feature(&req), HorizonVerdict::ClassForbidden);
    }
}
