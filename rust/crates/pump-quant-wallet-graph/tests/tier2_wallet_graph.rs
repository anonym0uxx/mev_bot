//! Leaf tests for Section 28 Tier-2 union-find wallet graph, family grouping,
//! Section 53 family holdout, and Section 46 activity-matched placebo.
//!
//! Expected component groupings, fold assignments, and placebo matchings are
//! all computed independently by hand across multiple inputs and edge cases.

use pump_quant_wallet_graph::integer_median;
use pump_quant_wallet_graph::tier2_wallet_graph::{
    build_activity_matched_placebo, EdgeKind, FamilyHoldout, UnionFind, WalletGraph,
};

#[test]
fn union_find_components_are_canonical() {
    // 6 nodes. Unions: 0-1, 2-3, 3-4. Components: {0,1},{2,3,4},{5}.
    let mut uf = UnionFind::new(6);
    assert!(uf.union(0, 1));
    assert!(uf.union(2, 3));
    assert!(uf.union(3, 4));
    assert!(!uf.union(2, 4)); // already connected
    let comps = uf.components();
    assert_eq!(comps, vec![vec![0, 1], vec![2, 3, 4], vec![5]]);
    assert!(uf.connected(2, 4));
    assert!(!uf.connected(0, 2));
    assert_eq!(uf.component_size(3), 3);
    assert_eq!(uf.component_size(5), 1);
}

#[test]
fn union_find_single_and_empty() {
    let mut uf = UnionFind::new(1);
    assert_eq!(uf.components(), vec![vec![0]]);
    let mut empty = UnionFind::new(0);
    assert!(empty.is_empty());
    assert_eq!(empty.components(), Vec::<Vec<usize>>::new());
}

#[test]
fn families_by_kind_isolate_provenance() {
    // Nodes 0..5.
    // Creator edges: 0-1 (SameCreator), 1-2 (SameDeployer).
    // Funding edges: 3-4 (Funding).
    // A transfer edge 2-3 links creator+funding families only in the operator
    // (all-edge) view.
    let mut g = WalletGraph::new(6);
    g.add_edge(0, 1, EdgeKind::SameCreator, 10);
    g.add_edge(1, 2, EdgeKind::SameDeployer, 11);
    g.add_edge(3, 4, EdgeKind::Funding, 12);
    g.add_edge(2, 3, EdgeKind::Transfer, 13);

    // Creator families (SameCreator + SameDeployer): {0,1,2}, plus singletons.
    let creator = g.families_by_kinds(&EdgeKind::creator_family_kinds());
    assert_eq!(creator, vec![vec![0, 1, 2], vec![3], vec![4], vec![5]]);

    // Funding families: {3,4}, plus singletons.
    let funding = g.families_by_kinds(&EdgeKind::funding_family_kinds());
    assert_eq!(
        funding,
        vec![vec![0], vec![1], vec![2], vec![3, 4], vec![5]]
    );

    // Operator families (all edges): {0,1,2,3,4}, singleton {5}.
    let operator = g.operator_families();
    assert_eq!(operator, vec![vec![0, 1, 2, 3, 4], vec![5]]);
}

#[test]
fn families_as_of_respects_discovery_time() {
    // Edge 0-1 discovered at slot 5, edge 1-2 discovered at slot 20.
    let mut g = WalletGraph::new(3);
    g.add_edge(0, 1, EdgeKind::SameCreator, 5);
    g.add_edge(1, 2, EdgeKind::SameCreator, 20);

    // As of slot 10, only the first edge is visible: {0,1},{2}.
    let early = g.families_as_of(&EdgeKind::creator_family_kinds(), 10);
    assert_eq!(early, vec![vec![0, 1], vec![2]]);

    // As of slot 25, both visible: {0,1,2}.
    let late = g.families_as_of(&EdgeKind::creator_family_kinds(), 25);
    assert_eq!(late, vec![vec![0, 1, 2]]);
}

#[test]
fn family_holdout_keeps_families_intact_and_is_leakage_free() {
    // Build families over an all-edge graph.
    let mut g = WalletGraph::new(8);
    g.add_edge(0, 1, EdgeKind::SameCreator, 1);
    g.add_edge(1, 2, EdgeKind::Funding, 2); // family {0,1,2}
    g.add_edge(3, 4, EdgeKind::SameDeployer, 3); // family {3,4}
    g.add_edge(5, 6, EdgeKind::CoBuySameBlock, 4); // family {5,6}
                                                   // node 7 singleton
    let families = g.operator_families();
    assert_eq!(
        families,
        vec![vec![0, 1, 2], vec![3, 4], vec![5, 6], vec![7]]
    );

    let folds = 3u32;
    let ho = FamilyHoldout::assign(&families, 8, folds);
    // Fold of a family = min_member % folds.
    // {0,1,2}: 0%3=0. {3,4}: 3%3=0. {5,6}: 5%3=2. {7}: 7%3=1.
    for n in [0, 1, 2] {
        assert_eq!(ho.fold_of(n), Some(0));
    }
    for n in [3, 4] {
        assert_eq!(ho.fold_of(n), Some(0));
    }
    for n in [5, 6] {
        assert_eq!(ho.fold_of(n), Some(2));
    }
    assert_eq!(ho.fold_of(7), Some(1));

    // No intra-family edge crosses a fold boundary -> leakage-free.
    assert!(ho.verify_no_leakage(g.edges()));
}

#[test]
fn family_holdout_detects_leakage_from_a_foreign_edge() {
    // Two separate families assigned to different folds; a spurious edge
    // linking them across folds must be flagged as leakage.
    let mut g = WalletGraph::new(4);
    g.add_edge(0, 1, EdgeKind::SameCreator, 1); // family {0,1} fold 0%2=0
    g.add_edge(2, 3, EdgeKind::SameCreator, 1); // family {2,3} fold 2%2=0
    let families = g.operator_families();
    let ho = FamilyHoldout::assign(&families, 4, 2);
    // Both families land in fold 0 here (0%2=0, 2%2=0); use folds=3 to split:
    let ho3 = FamilyHoldout::assign(&families, 4, 3);
    // {0,1}: 0%3=0 ; {2,3}: 2%3=2 -> different folds.
    assert_eq!(ho3.fold_of(1), Some(0));
    assert_eq!(ho3.fold_of(2), Some(2));
    // A cross-family edge would be leakage under ho3.
    let cross = [pump_quant_wallet_graph::tier2_wallet_graph::Edge {
        a: 1,
        b: 2,
        kind: EdgeKind::Transfer,
        discovery_slot: 9,
    }];
    assert!(!ho3.verify_no_leakage(&cross));
    // The original intra-family edges are still leakage-free under ho (folds=2).
    assert!(ho.verify_no_leakage(g.edges()));
}

#[test]
fn activity_matched_placebo_picks_closest_then_lowest_id() {
    // Treatment wallets with activity levels.
    let treatment = [(100usize, 50u64), (101, 90)];
    // Pool: control candidates.
    // For t=100 (act 50): pool acts -> gaps: node200(48)->2, node201(55)->5,
    //   node202(50)->0 (exact). Closest is node202 (gap 0).
    // For t=101 (act 90): remaining pool node200(48)->42, node201(55)->35.
    //   Closest is node201 (gap 35).
    let pool = [(200usize, 48u64), (201, 55), (202, 50)];
    let pairs = build_activity_matched_placebo(&treatment, &pool);
    assert_eq!(pairs.len(), 2);
    assert_eq!(pairs[0].treatment, 100);
    assert_eq!(pairs[0].control, 202);
    assert_eq!(pairs[0].activity_gap, 0);
    assert_eq!(pairs[1].treatment, 101);
    assert_eq!(pairs[1].control, 201);
    assert_eq!(pairs[1].activity_gap, 35);
}

#[test]
fn activity_matched_placebo_tie_breaks_by_smallest_control_id() {
    // Two pool wallets equidistant from the treatment activity -> lower id wins.
    let treatment = [(1usize, 100u64)];
    let pool = [(9usize, 110u64), (5usize, 90u64)]; // both gap 10
    let pairs = build_activity_matched_placebo(&treatment, &pool);
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0].control, 5); // smaller id
    assert_eq!(pairs[0].activity_gap, 10);
}

#[test]
fn activity_matched_placebo_exhausts_pool() {
    // More treatment than pool -> only as many pairs as pool wallets.
    let treatment = [(1usize, 10u64), (2, 20), (3, 30)];
    let pool = [(9usize, 12u64)];
    let pairs = build_activity_matched_placebo(&treatment, &pool);
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0].treatment, 1);
    assert_eq!(pairs[0].control, 9);
}

#[test]
fn integer_median_odd_even_and_empty() {
    assert_eq!(integer_median(&[]), None);
    assert_eq!(integer_median(&[7]), Some(7));
    assert_eq!(integer_median(&[3, 1, 2]), Some(2)); // sorted 1,2,3 -> 2
    assert_eq!(integer_median(&[4, 1, 3, 2]), Some(2)); // (2+3)/2 = 2 (toward 0)
    assert_eq!(integer_median(&[-5, 5]), Some(0));
}
