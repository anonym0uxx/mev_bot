//! `DiscoveryLane::AlphaCall` (the 6th discovery lane) leaf tests.
//!
//! The designated-caller alpha lane (a curated X follow or a Discord alpha room)
//! must (a) be a first-class member of the bounded per-lane net-SOL ledger and
//! (b) attribute its realized net INDEPENDENTLY of the open social-caller
//! firehose, even though both present as the `CreationSniper` setup archetype —
//! §71 reflection integrity: a paid alpha room earns (or loses) its own SOL.

use pump_quant_watchlist::candidate::{DiscoveryLane, Lane};
use pump_quant_watchlist::lane_performance::DiscoveryLanePerformance;

#[test]
fn alpha_call_is_a_first_class_discovery_lane() {
    // Present in the enumeration and dense-index space, appended after ActiveMarket.
    assert_eq!(DiscoveryLane::COUNT, 6);
    assert!(DiscoveryLane::ALL.contains(&DiscoveryLane::AlphaCall));
    assert_eq!(DiscoveryLane::AlphaCall.index(), 5);
    // Indices are dense and cover 0..COUNT with no collision.
    let mut seen = [false; DiscoveryLane::COUNT];
    for lane in DiscoveryLane::ALL {
        let i = lane.index();
        assert!(i < DiscoveryLane::COUNT);
        assert!(!seen[i], "index {i} used twice");
        seen[i] = true;
    }
    assert!(seen.iter().all(|&s| s), "indices must cover 0..COUNT");
}

#[test]
fn alpha_call_shares_creation_archetype_but_keys_distinctly() {
    // Same setup archetype as the open social-caller firehose (ranking is keyed on
    // the archetype `Lane`, so the alpha lane ranks like a CreationSniper)…
    assert_eq!(DiscoveryLane::AlphaCall.setup_lane(), Lane::CreationSniper);
    assert_eq!(
        DiscoveryLane::SocialCaller.setup_lane(),
        Lane::CreationSniper
    );
    // …yet it occupies its OWN ledger slot, distinct from SocialCaller.
    assert_ne!(
        DiscoveryLane::AlphaCall.index(),
        DiscoveryLane::SocialCaller.index()
    );
}

#[test]
fn alpha_call_net_sol_does_not_contaminate_social_caller() {
    let mut lp = DiscoveryLanePerformance::new();
    // A profitable designated alpha room and a lossy open social-caller firehose.
    lp.record(DiscoveryLane::AlphaCall, 900_000);
    lp.record(DiscoveryLane::AlphaCall, 350_000);
    lp.record(DiscoveryLane::SocialCaller, -200_000);
    // Each lane carries exactly its own realized net + trade count — no crossover
    // despite sharing the CreationSniper archetype.
    assert_eq!(lp.net_sol(DiscoveryLane::AlphaCall), 1_250_000);
    assert_eq!(lp.trade_count(DiscoveryLane::AlphaCall), 2);
    assert_eq!(lp.net_sol(DiscoveryLane::SocialCaller), -200_000);
    assert_eq!(lp.trade_count(DiscoveryLane::SocialCaller), 1);
    // Untouched lanes remain zero.
    assert_eq!(lp.net_sol(DiscoveryLane::OnchainCreation), 0);
    // Total folds in the new lane's slot.
    assert_eq!(lp.total_net_sol(), 1_050_000);
}

#[test]
fn default_ledger_is_all_zero_including_alpha_call() {
    let lp = DiscoveryLanePerformance::default();
    for lane in DiscoveryLane::ALL {
        assert_eq!(lp.net_sol(lane), 0);
        assert_eq!(lp.trade_count(lane), 0);
    }
}
