//! Leaf sh_admit: Signal-Horizon Matching Law gate (criterion 96).

use pump_quant_strategy::signal_horizon::{
    admit_feature_to_lane, class_admissible_to, latency_beats_horizon, FeatureClass,
    HorizonVerdict, Lane,
};

#[test]
fn latency_compare_uses_margin() {
    // L + margin <= H.
    assert!(latency_beats_horizon(100, 500, 50)); // 150 <= 500
    assert!(latency_beats_horizon(450, 500, 50)); // 500 <= 500 boundary ok
    assert!(!latency_beats_horizon(451, 500, 50)); // 501 > 500
    assert!(!latency_beats_horizon(600, 500, 0)); // 600 > 500
}

#[test]
fn on_chain_flow_admissible_to_every_lane() {
    for lane in [
        Lane::CreationSniper,
        Lane::EarlyEntry,
        Lane::HoldExitContext,
        Lane::SourceQuality,
        Lane::MetaEmergence,
    ] {
        assert!(class_admissible_to(FeatureClass::OnChainFlow, lane));
    }
}

#[test]
fn tiktok_confined_to_context_lanes() {
    // Forbidden on entry lanes.
    assert!(!class_admissible_to(
        FeatureClass::TikTokVirality,
        Lane::CreationSniper
    ));
    assert!(!class_admissible_to(
        FeatureClass::TikTokVirality,
        Lane::EarlyEntry
    ));
    // Allowed on hold/exit, source-quality, meta.
    assert!(class_admissible_to(
        FeatureClass::TikTokVirality,
        Lane::HoldExitContext
    ));
    assert!(class_admissible_to(
        FeatureClass::TikTokVirality,
        Lane::SourceQuality
    ));
    assert!(class_admissible_to(
        FeatureClass::TikTokVirality,
        Lane::MetaEmergence
    ));
}

#[test]
fn launch_social_linkage_only_entry_and_source_quality() {
    assert!(class_admissible_to(
        FeatureClass::LaunchSocialLinkage,
        Lane::CreationSniper
    ));
    assert!(class_admissible_to(
        FeatureClass::LaunchSocialLinkage,
        Lane::EarlyEntry
    ));
    assert!(class_admissible_to(
        FeatureClass::LaunchSocialLinkage,
        Lane::SourceQuality
    ));
    assert!(!class_admissible_to(
        FeatureClass::LaunchSocialLinkage,
        Lane::HoldExitContext
    ));
    assert!(!class_admissible_to(
        FeatureClass::LaunchSocialLinkage,
        Lane::MetaEmergence
    ));
}

#[test]
fn class_forbidden_beats_latency() {
    // TikTok on an entry lane is ClassForbidden even with tiny latency.
    let v = admit_feature_to_lane(
        1,
        FeatureClass::TikTokVirality,
        Lane::CreationSniper,
        1_000_000,
        0,
    );
    assert_eq!(v, HorizonVerdict::ClassForbidden);
}

#[test]
fn allowed_class_but_too_slow() {
    // On-chain flow allowed, but latency exceeds horizon.
    let v = admit_feature_to_lane(
        2_000,
        FeatureClass::OnChainFlow,
        Lane::CreationSniper,
        500,
        10,
    );
    assert_eq!(v, HorizonVerdict::TooSlow);
}

#[test]
fn admissible_when_class_ok_and_fast_enough() {
    let v = admit_feature_to_lane(
        100,
        FeatureClass::OnChainFlow,
        Lane::CreationSniper,
        1_000,
        50,
    );
    assert_eq!(v, HorizonVerdict::Admissible);
    // Launch social linkage into an early-entry lane, fast enough.
    let v = admit_feature_to_lane(
        200,
        FeatureClass::LaunchSocialLinkage,
        Lane::EarlyEntry,
        1_000,
        100,
    );
    assert_eq!(v, HorizonVerdict::Admissible);
}
