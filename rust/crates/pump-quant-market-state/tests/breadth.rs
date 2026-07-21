//! Tests for the manipulation-/cluster-adjusted breadth decomposition reducer.
//!
//! Expectations are computed independently of the reducer's internals: each
//! test constructs a stream by hand, reasons about the correct counts on paper,
//! and asserts the snapshot matches. Multiple inputs and edge cases are covered.

use pump_quant_market_state::breadth::{
    BreadthConfig, BreadthReducer, BuyerFlags, FlowEvent, Side,
};
use pump_quant_market_state::common::Completeness;

/// Helper to build a plain buy event with no flags.
fn buy(idx: u64, wallet: u64, cluster: u64, quote: u64, tokens: u64) -> FlowEvent {
    FlowEvent {
        event_index: idx,
        side: Side::Buy,
        wallet,
        token_account: wallet + 1_000_000, // distinct per wallet by construction
        fee_payer: wallet,
        funding_root: wallet + 2_000_000,
        cluster,
        quote_lamports: quote,
        token_base_units: tokens,
        funded_net_new: false,
        flags: BuyerFlags::empty(),
    }
}

#[test]
fn empty_reducer_is_all_zero_and_complete() {
    let r = BreadthReducer::new(BreadthConfig::default());
    let s = r.snapshot();
    assert_eq!(s.raw_unique_buyers, 0);
    assert_eq!(s.cluster_adjusted_actors, 0);
    assert_eq!(s.genuine_net_exposure_breadth, 0);
    assert_eq!(s.genuine_to_raw_bps, None); // 0 buyers => ratio UNKNOWN
    assert_eq!(s.completeness, Completeness::Complete);
}

#[test]
fn raw_uniqueness_counts_distinct_dimensions() {
    let cfg = BreadthConfig::default();
    let mut r = BreadthReducer::new(cfg);

    // Two wallets, but they share ONE fee payer and ONE funding root (a sponsor
    // paying for two wallets in one cluster) -> raw buyers 2, fee payers 1,
    // funding roots 1, cluster-adjusted actors 1.
    let e1 = FlowEvent {
        event_index: 0,
        side: Side::Buy,
        wallet: 10,
        token_account: 100,
        fee_payer: 999,
        funding_root: 777,
        cluster: 42,
        quote_lamports: 5_000_000,
        token_base_units: 1_000,
        funded_net_new: false,
        flags: BuyerFlags::empty(),
    };
    let e2 = FlowEvent {
        event_index: 1,
        wallet: 11,
        token_account: 101,
        ..e1
    };
    r.ingest(&e1);
    r.ingest(&e2);
    let s = r.snapshot();
    assert_eq!(s.raw_unique_buyers, 2);
    assert_eq!(s.unique_token_accounts, 2);
    assert_eq!(s.unique_fee_payers, 1);
    assert_eq!(s.unique_funding_roots, 1);
    assert_eq!(s.cluster_adjusted_actors, 1);
    // raw wallet count (2) exceeds cluster-adjusted actors (1): the exact point
    // of the constitution's "raw wallet count is not organic breadth".
    assert!(s.raw_unique_buyers > s.cluster_adjusted_actors);
}

#[test]
fn repeat_buyer_and_net_inventory_math() {
    let cfg = BreadthConfig {
        meaningful_net_quote_lamports: 3_000_000,
        ..BreadthConfig::default()
    };
    let mut r = BreadthReducer::new(cfg);

    // Cluster 1: buys twice (repeat buyer), net tokens 300, net quote 4_000_000.
    r.ingest(&buy(0, 1, 1, 2_000_000, 200));
    r.ingest(&buy(1, 1, 1, 2_000_000, 200));
    // then sells 100 tokens for 0 quote credit-back
    r.ingest(&FlowEvent {
        event_index: 2,
        side: Side::Sell,
        wallet: 1,
        token_account: 100,
        fee_payer: 1,
        funding_root: 5,
        cluster: 1,
        quote_lamports: 0,
        token_base_units: 100,
        funded_net_new: false,
        flags: BuyerFlags::empty(),
    });
    // Net tokens = 200+200-100 = 300 (>0 positive inventory).
    // Net quote  = 2_000_000+2_000_000-0 = 4_000_000 (>= 3_000_000 meaningful).
    // Cluster 2: single buy, net quote 1_000_000 (< meaningful), tokens 50.
    r.ingest(&buy(3, 2, 2, 1_000_000, 50));

    let s = r.snapshot();
    assert_eq!(s.cluster_adjusted_actors, 2);
    assert_eq!(s.repeat_buyers, 1); // only cluster 1 bought twice
    assert_eq!(s.positive_net_inventory_buyers, 2); // both hold >0 tokens
    assert_eq!(s.meaningful_net_exposure_buyers, 1); // only cluster 1
                                                     // genuine breadth = positive inv AND meaningful AND unflagged -> cluster 1.
    assert_eq!(s.genuine_net_exposure_breadth, 1);
    assert_eq!(s.independent_buyer_expansion, 2); // both unflagged
                                                  // genuine_to_raw: 1 genuine / 2 raw wallets = 5000 bps.
    assert_eq!(s.genuine_to_raw_bps, Some(5000));
}

#[test]
fn manipulation_flags_are_counted_separately_and_excluded_from_genuine() {
    let cfg = BreadthConfig {
        meaningful_net_quote_lamports: 1_000_000,
        ..BreadthConfig::default()
    };
    let mut r = BreadthReducer::new(cfg);

    // Cluster 1: WASH flagged, big exposure, positive inventory -> NOT genuine.
    r.ingest(&FlowEvent {
        event_index: 0,
        side: Side::Buy,
        wallet: 1,
        token_account: 100,
        fee_payer: 1,
        funding_root: 5,
        cluster: 1,
        quote_lamports: 9_000_000,
        token_base_units: 900,
        funded_net_new: true,
        flags: BuyerFlags::WASH,
    });
    // Cluster 2: BUNDLE + SNIPER flagged.
    r.ingest(&FlowEvent {
        event_index: 1,
        side: Side::Buy,
        wallet: 2,
        token_account: 200,
        fee_payer: 2,
        funding_root: 6,
        cluster: 2,
        quote_lamports: 5_000_000,
        token_base_units: 500,
        funded_net_new: false,
        flags: BuyerFlags::BUNDLE | BuyerFlags::SNIPER,
    });
    // Cluster 3: clean, meaningful, positive inventory -> genuine.
    r.ingest(&buy(2, 3, 3, 4_000_000, 400));

    let s = r.snapshot();
    assert_eq!(s.suspected_wash_buyers, 1);
    assert_eq!(s.suspected_bundle_buyers, 1);
    assert_eq!(s.suspected_sniper_buyers, 1);
    assert_eq!(s.suspected_volume_bot_buyers, 0);
    assert_eq!(s.net_new_funded_buyers, 1); // only cluster 1 was net-new funded
                                            // genuine only cluster 3; independent expansion (unflagged) only cluster 3.
    assert_eq!(s.genuine_net_exposure_breadth, 1);
    assert_eq!(s.independent_buyer_expansion, 1);
    assert_eq!(s.cluster_adjusted_actors, 3);
}

#[test]
fn linked_and_known_cluster_flags_counted() {
    let mut r = BreadthReducer::new(BreadthConfig::default());
    let base = buy(0, 1, 1, 1_000_000, 100);
    r.ingest(&FlowEvent {
        flags: BuyerFlags::CREATOR_LINKED | BuyerFlags::BUNDLE_LINKED,
        ..base
    });
    r.ingest(&FlowEvent {
        event_index: 1,
        wallet: 2,
        cluster: 2,
        flags: BuyerFlags::RUG_CLUSTER,
        ..base
    });
    r.ingest(&FlowEvent {
        event_index: 2,
        wallet: 3,
        cluster: 3,
        flags: BuyerFlags::RUNNER_CLUSTER,
        ..base
    });
    let s = r.snapshot();
    assert_eq!(s.creator_linked_buyers, 1);
    assert_eq!(s.bundle_linked_buyers, 1);
    assert_eq!(s.known_rug_cluster_buyers, 1);
    assert_eq!(s.known_runner_cluster_buyers, 1);
    assert_eq!(s.independent_buyer_expansion, 0); // all flagged
}

#[test]
fn recent_independent_arrivals_tracks_decay_window() {
    // Small decay window so we can reason precisely.
    let cfg = BreadthConfig {
        decay_window_events: 2,
        ..BreadthConfig::default()
    };
    let mut r = BreadthReducer::new(cfg);
    // Independent clusters first-buy at indices 0,1,2,3,4.
    for i in 0..5u64 {
        r.ingest(&buy(i, 100 + i, 100 + i, 1_000_000, 100));
    }
    // last_event_index = 4, window_start = 4 - 2 = 2.
    // Clusters whose first buy index >= 2 => indices 2,3,4 => 3 arrivals.
    let s = r.snapshot();
    assert_eq!(s.independent_buyer_expansion, 5);
    assert_eq!(s.recent_independent_arrivals, 3);
}

#[test]
fn sell_only_cluster_is_not_a_buyer() {
    let mut r = BreadthReducer::new(BreadthConfig::default());
    // A cluster that only ever sells (e.g. someone who received an airdrop).
    r.ingest(&FlowEvent {
        event_index: 0,
        side: Side::Sell,
        wallet: 9,
        token_account: 90,
        fee_payer: 9,
        funding_root: 9,
        cluster: 9,
        quote_lamports: 1_000_000,
        token_base_units: 100,
        funded_net_new: false,
        flags: BuyerFlags::empty(),
    });
    let s = r.snapshot();
    assert_eq!(s.raw_unique_buyers, 0); // sells don't create buyer records
    assert_eq!(s.cluster_adjusted_actors, 0); // sell-only cluster excluded
}

#[test]
fn memory_bound_reports_incomplete() {
    // Capacity for only 2 clusters; ingest 3 distinct -> overflow.
    let cfg = BreadthConfig {
        max_tracked_clusters: 2,
        max_tracked_ids: 100,
        ..BreadthConfig::default()
    };
    let mut r = BreadthReducer::new(cfg);
    r.ingest(&buy(0, 1, 1, 1_000_000, 100));
    r.ingest(&buy(1, 2, 2, 1_000_000, 100));
    r.ingest(&buy(2, 3, 3, 1_000_000, 100)); // 3rd cluster rejected
    let s = r.snapshot();
    assert_eq!(s.cluster_adjusted_actors, 2); // lower bound
    assert_eq!(s.completeness, Completeness::Incomplete);
}

#[test]
fn property_genuine_breadth_never_exceeds_cluster_actors_over_many_inputs() {
    // Deterministic pseudo-stream (no RNG): vary flags, exposure, inventory.
    let cfg = BreadthConfig {
        meaningful_net_quote_lamports: 2_000_000,
        ..BreadthConfig::default()
    };
    for seed in 0..40u64 {
        let mut r = BreadthReducer::new(cfg);
        let n = 3 + (seed % 7);
        for k in 0..n {
            let cluster = 1 + k;
            // deterministic flag pattern
            let flags = match (seed + k) % 4 {
                0 => BuyerFlags::empty(),
                1 => BuyerFlags::WASH,
                2 => BuyerFlags::BUNDLE,
                _ => BuyerFlags::SNIPER,
            };
            let quote = 1_000_000 * (1 + (seed + k) % 5);
            let tokens = 10 * (1 + (seed + k) % 5);
            r.ingest(&FlowEvent {
                event_index: k,
                side: Side::Buy,
                wallet: 1000 + k,
                token_account: 2000 + k,
                fee_payer: 3000 + k,
                funding_root: 4000 + k,
                cluster,
                quote_lamports: quote,
                token_base_units: tokens,
                funded_net_new: (seed + k) % 2 == 0,
                flags,
            });
        }
        let s = r.snapshot();
        // Invariants that must hold for ANY input:
        assert!(s.genuine_net_exposure_breadth <= s.cluster_adjusted_actors);
        assert!(s.independent_buyer_expansion <= s.cluster_adjusted_actors);
        assert!(s.genuine_net_exposure_breadth <= s.independent_buyer_expansion);
        assert!(s.meaningful_net_exposure_buyers <= s.cluster_adjusted_actors);
        assert!(s.recent_independent_arrivals <= s.independent_buyer_expansion);
        // sum of a disjoint flag partition can't exceed actor count individually
        assert!(s.suspected_wash_buyers <= s.cluster_adjusted_actors);
    }
}
