//! Leaf tests for Section 27 / §70.9 deployer-credibility features.
//!
//! Every expectation is computed independently by hand across multiple inputs
//! including point-in-time boundaries, serial-deploy windows, and the
//! verified-vs-self-claimed partnership split.

use pump_quant_wallet_graph::deployer_credibility::{
    compute_deployer_credibility, DeployerCredibilityConfig, PartnershipClaim, PriorLaunch,
    SocialReachInput,
};

fn cfg(window: u64, threshold: u32) -> DeployerCredibilityConfig {
    DeployerCredibilityConfig {
        serial_window_slots: window,
        serial_threshold: threshold,
    }
}

#[test]
fn point_in_time_excludes_future_launches() {
    // Launches at slots 100,150,200,300. Decision at slot 250.
    // Only 100,150,200 count (strictly < 250).
    let launches = [
        PriorLaunch { slot: 100 },
        PriorLaunch { slot: 150 },
        PriorLaunch { slot: 200 },
        PriorLaunch { slot: 300 },
    ];
    let out = compute_deployer_credibility(
        &launches,
        250,
        &[],
        &SocialReachInput::default(),
        &cfg(1000, 5),
    );
    assert_eq!(out.prior_ca_count, 3);
}

#[test]
fn launch_at_decision_slot_is_excluded() {
    // A launch exactly at the decision slot is not yet knowable.
    let launches = [PriorLaunch { slot: 250 }];
    let out = compute_deployer_credibility(
        &launches,
        250,
        &[],
        &SocialReachInput::default(),
        &cfg(10, 2),
    );
    assert_eq!(out.prior_ca_count, 0);
    assert!(!out.serial_deploy_flag);
    assert_eq!(out.max_launches_in_window, 0);
}

#[test]
fn serial_deploy_flag_fires_on_burst() {
    // Launches at slots 10,11,12,13 then a gap, then 500.
    // Window = 5 slots (span 4). Max occupancy: slots 10..13 all within span 4
    // of each other -> 4 launches. Threshold 4 -> flagged.
    let launches = [
        PriorLaunch { slot: 10 },
        PriorLaunch { slot: 11 },
        PriorLaunch { slot: 12 },
        PriorLaunch { slot: 13 },
        PriorLaunch { slot: 500 },
    ];
    let out = compute_deployer_credibility(
        &launches,
        1000,
        &[],
        &SocialReachInput::default(),
        &cfg(5, 4),
    );
    assert_eq!(out.prior_ca_count, 5);
    assert_eq!(out.max_launches_in_window, 4);
    assert!(out.serial_deploy_flag);
}

#[test]
fn serial_deploy_flag_does_not_fire_when_spread_out() {
    // Launches 100 slots apart, window 5 -> max occupancy 1, no flag.
    let launches = [
        PriorLaunch { slot: 100 },
        PriorLaunch { slot: 200 },
        PriorLaunch { slot: 300 },
    ];
    let out = compute_deployer_credibility(
        &launches,
        1000,
        &[],
        &SocialReachInput::default(),
        &cfg(5, 2),
    );
    assert_eq!(out.max_launches_in_window, 1);
    assert!(!out.serial_deploy_flag);
}

#[test]
fn serial_window_boundary_is_inclusive_span() {
    // Window = 3 slots => span 2 (covers [s, s+2] inclusive).
    // Launches at 10, 12, 13. From 10: 10 and 12 within span 2 -> 2; 13 is
    // span 3 from 10 -> evicted. From 12: 12,13 -> 2. Also 10,11? none.
    // Best window occupancy: {12,13} plus is 11? no. {10,12}=2, {12,13}=2.
    // Max = 2.
    let launches = [
        PriorLaunch { slot: 10 },
        PriorLaunch { slot: 12 },
        PriorLaunch { slot: 13 },
    ];
    let out = compute_deployer_credibility(
        &launches,
        1000,
        &[],
        &SocialReachInput::default(),
        &cfg(3, 3),
    );
    assert_eq!(out.max_launches_in_window, 2);
    assert!(!out.serial_deploy_flag); // threshold 3 not reached
}

#[test]
fn partnership_split_verified_vs_self_claimed() {
    // 2 verified, 3 self-claimed.
    let partnerships = [
        PartnershipClaim { verified: true },
        PartnershipClaim { verified: false },
        PartnershipClaim { verified: false },
        PartnershipClaim { verified: true },
        PartnershipClaim { verified: false },
    ];
    let out = compute_deployer_credibility(
        &[],
        100,
        &partnerships,
        &SocialReachInput::default(),
        &cfg(5, 2),
    );
    assert_eq!(out.verified_partnership_count, 2);
    assert_eq!(out.self_claimed_partnership_count, 3);
}

#[test]
fn social_reach_passthrough_is_kept_distinct() {
    let social = SocialReachInput {
        key_followers: 42,
        mutual_followers_with_reference: 7,
    };
    let out = compute_deployer_credibility(&[], 100, &[], &social, &cfg(5, 2));
    assert_eq!(out.key_follower_reach, 42);
    assert_eq!(out.mutual_follower_reach, 7);
}

#[test]
fn empty_inputs_produce_zeroed_features() {
    let out = compute_deployer_credibility(&[], 100, &[], &SocialReachInput::default(), &cfg(5, 2));
    assert_eq!(out.prior_ca_count, 0);
    assert!(!out.serial_deploy_flag);
    assert_eq!(out.max_launches_in_window, 0);
    assert_eq!(out.key_follower_reach, 0);
    assert_eq!(out.mutual_follower_reach, 0);
    assert_eq!(out.verified_partnership_count, 0);
    assert_eq!(out.self_claimed_partnership_count, 0);
}

#[test]
fn zero_threshold_never_flags() {
    // serial_threshold = 0 must never flag (guarded).
    let launches = [PriorLaunch { slot: 10 }, PriorLaunch { slot: 11 }];
    let out = compute_deployer_credibility(
        &launches,
        1000,
        &[],
        &SocialReachInput::default(),
        &cfg(5, 0),
    );
    assert!(!out.serial_deploy_flag);
}
