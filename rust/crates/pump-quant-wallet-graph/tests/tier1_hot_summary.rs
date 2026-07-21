//! Leaf tests for Section 28 Tier-1 bounded hot summaries.
//!
//! Every expected value below is computed independently by hand from the event
//! stream, across multiple inputs including edge cases (capacity overflow,
//! window truncation, empty streams).

use pump_quant_wallet_graph::tier1_hot_summary::{BuyEvent, HotSummaryReducer, SellEvent};
use pump_quant_wallet_graph::{BoundedIdSet, WalletId};

fn buy(w: u64, slot: u64) -> BuyEvent {
    BuyEvent {
        wallet: WalletId(w),
        slot,
    }
}
fn sell(w: u64, slot: u64) -> SellEvent {
    SellEvent {
        wallet: WalletId(w),
        slot,
    }
}

#[test]
fn distinct_buyers_and_first_n_cooccurrence() {
    // Buyers: 10,11,10,12,11,13 across slots. Distinct = {10,11,12,13} = 4.
    // First-3 distinct in arrival order = [10, 11, 12].
    let mut r = HotSummaryReducer::new(3, 64, 5, 64);
    for (w, s) in [(10, 1), (11, 1), (10, 2), (12, 2), (11, 3), (13, 3)] {
        r.on_buy(buy(w, s));
    }
    let empty = BoundedIdSet::with_capacity(4);
    let sum = r.summary(&empty);
    assert_eq!(sum.unique_buyers, 4);
    assert!(!sum.unique_buyers_overflowed);
    assert_eq!(
        sum.first_n_buyers,
        vec![WalletId(10), WalletId(11), WalletId(12)]
    );
}

#[test]
fn same_slot_cobuy_peak() {
    // Slot 1: buyers 1,2,3 -> co-buy 3. Slot 2: buyer 4 -> 1. Slot 3: 5,6 -> 2.
    // Peak co-buy = 3.
    let mut r = HotSummaryReducer::new(10, 64, 5, 64);
    for (w, s) in [(1, 1), (2, 1), (3, 1), (4, 2), (5, 3), (6, 3)] {
        r.on_buy(buy(w, s));
    }
    let empty = BoundedIdSet::with_capacity(1);
    assert_eq!(r.summary(&empty).max_same_slot_cobuy, 3);
}

#[test]
fn cluster_adjusted_breadth_subtracts_cluster_members() {
    // Distinct buyers {1,2,3,4,5}. Cluster members {2,4}. Adjusted = 3.
    let mut r = HotSummaryReducer::new(10, 64, 5, 64);
    for w in 1..=5u64 {
        r.on_buy(buy(w, w));
    }
    let mut cluster = BoundedIdSet::with_capacity(8);
    cluster.insert(2);
    cluster.insert(4);
    let sum = r.summary(&cluster);
    assert_eq!(sum.unique_buyers, 5);
    assert_eq!(sum.cluster_adjusted_breadth, 3);
}

#[test]
fn synchronized_sell_window_peak() {
    // Window = 3 slots inclusive. Sells at slots 10,11,12,20,21,22,23.
    // For window [s-2, s]:
    //  slot10 -> {10}          size1
    //  slot11 -> {10,11}       size2
    //  slot12 -> {10,11,12}    size3   <- peak so far
    //  slot20 -> {20}          size1 (10,11,12 evicted)
    //  slot21 -> {20,21}       size2
    //  slot22 -> {20,21,22}    size3
    //  slot23 -> {21,22,23}    size3 (20 evicted since 20 < 23-2=21)
    // Peak = 3.
    let mut r = HotSummaryReducer::new(4, 64, 3, 64);
    for s in [10, 11, 12, 20, 21, 22, 23] {
        r.on_sell(sell(99, s));
    }
    let empty = BoundedIdSet::with_capacity(1);
    let sum = r.summary(&empty);
    assert_eq!(sum.synchronized_sell_peak, 3);
    assert!(!sum.sync_sell_truncated);
}

#[test]
fn synchronized_sell_dense_burst_full_window() {
    // 5 sells all in the same slot, window 3 -> peak 5 (all inside window).
    let mut r = HotSummaryReducer::new(4, 64, 3, 64);
    for _ in 0..5 {
        r.on_sell(sell(1, 100));
    }
    let empty = BoundedIdSet::with_capacity(1);
    assert_eq!(r.summary(&empty).synchronized_sell_peak, 5);
}

#[test]
fn unique_buyer_capacity_overflow_is_bounded() {
    // Capacity 4, but 6 distinct buyers -> stored 4, overflow flagged, memory
    // never grows past cap.
    let mut r = HotSummaryReducer::new(3, 4, 5, 64);
    for w in 1..=6u64 {
        r.on_buy(buy(w, w));
    }
    let empty = BoundedIdSet::with_capacity(1);
    let sum = r.summary(&empty);
    assert_eq!(sum.unique_buyers, 4);
    assert!(sum.unique_buyers_overflowed);
    // first-N still captured the earliest 3.
    assert_eq!(
        sum.first_n_buyers,
        vec![WalletId(1), WalletId(2), WalletId(3)]
    );
}

#[test]
fn sell_buffer_truncation_is_flagged_and_bounded() {
    // sell_cap = 3 but 5 sells inside one window -> buffer capped, truncated
    // flag set, peak bounded by cap (3).
    let mut r = HotSummaryReducer::new(4, 64, 10, 3);
    for _ in 0..5 {
        r.on_sell(sell(1, 50));
    }
    let empty = BoundedIdSet::with_capacity(1);
    let sum = r.summary(&empty);
    assert_eq!(sum.synchronized_sell_peak, 3);
    assert!(sum.sync_sell_truncated);
}

#[test]
fn empty_stream_is_all_zero() {
    let r = HotSummaryReducer::new(3, 8, 5, 8);
    let empty = BoundedIdSet::with_capacity(1);
    let sum = r.summary(&empty);
    assert_eq!(sum.unique_buyers, 0);
    assert_eq!(sum.max_same_slot_cobuy, 0);
    assert_eq!(sum.synchronized_sell_peak, 0);
    assert_eq!(sum.cluster_adjusted_breadth, 0);
    assert!(sum.first_n_buyers.is_empty());
}

#[test]
fn determinism_same_input_same_output() {
    let build = || {
        let mut r = HotSummaryReducer::new(3, 64, 4, 64);
        for (w, s) in [(7, 1), (8, 1), (7, 2), (9, 4), (8, 4)] {
            r.on_buy(buy(w, s));
        }
        for s in [1, 2, 3] {
            r.on_sell(sell(1, s));
        }
        let empty = BoundedIdSet::with_capacity(1);
        r.summary(&empty)
    };
    assert_eq!(build(), build());
}
