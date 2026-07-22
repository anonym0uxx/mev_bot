//! Section 28 **Tier-1 bounded production summaries**.
//!
//! Deterministic, timing-safe, hot-path-eligible reducers that fold a stream of
//! per-candidate buy / sell events into a small fixed-capacity summary. Every
//! stateful structure is memory-bounded (Section 57): the reducer never
//! allocates in proportion to the event stream once its capacities are reached;
//! it saturates counters instead.
//!
//! The summary deliberately keeps its components *separate* — raw unique
//! buyers, same-block co-buy peak, first-N buyers, cluster-adjusted breadth,
//! and synchronized-sell peak are distinct fields. Per Section 28 these are
//! **never collapsed into one opaque score**, and raw wallet count is not
//! treated as organic breadth.
//!
//! # Event ordering contract
//!
//! Buy and sell events are delivered per candidate in non-decreasing slot
//! order (the canonical stream is slot-ordered). The same-block co-buy and
//! synchronized-sell computations rely on this; out-of-order slots are handled
//! defensively (they reset the current-slot accumulator) but the ordering
//! contract is the intended usage.

use crate::{BoundedIdSet, WalletId};

/// A decoded buy event for one candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuyEvent {
    /// Buyer wallet.
    pub wallet: WalletId,
    /// Slot in which the buy landed (proxy for "block").
    pub slot: u64,
}

/// A decoded sell event for one candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SellEvent {
    /// Seller wallet.
    pub wallet: WalletId,
    /// Slot in which the sell landed.
    pub slot: u64,
}

/// Immutable snapshot of the Tier-1 summary for one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotSummary {
    /// Distinct buyer wallets observed, bounded by the reducer's unique-buyer
    /// capacity. This is a lower bound once `unique_buyers_overflowed` is true.
    pub unique_buyers: u32,
    /// Whether the distinct-buyer set hit capacity and dropped ids.
    pub unique_buyers_overflowed: bool,
    /// Largest number of buy events observed within a single slot
    /// (same-block co-buy peak).
    pub max_same_slot_cobuy: u32,
    /// The first-N distinct buyers, in arrival order (first-N buyer
    /// co-occurrence).
    pub first_n_buyers: Vec<WalletId>,
    /// Largest number of distinct sells falling inside one sliding
    /// `sell_window_slots` window (synchronized-sell risk peak).
    pub synchronized_sell_peak: u32,
    /// Whether the synchronized-sell tracker truncated its window buffer.
    pub sync_sell_truncated: bool,
    /// Cluster-adjusted breadth = distinct buyers **not** flagged as cluster
    /// members. Computed against the stored (bounded) buyer set, so it is a
    /// lower bound when the buyer set overflowed.
    pub cluster_adjusted_breadth: u32,
}

/// Bounded, deterministic Tier-1 hot-summary reducer for a single candidate.
#[derive(Debug, Clone)]
pub struct HotSummaryReducer {
    first_n_cap: usize,
    first_n_buyers: Vec<u64>,
    unique_buyers: BoundedIdSet,
    cur_buy_slot: Option<u64>,
    cur_slot_cobuy: u32,
    max_same_slot_cobuy: u32,
    sell_window_slots: u64,
    sell_slots: std::collections::VecDeque<u64>,
    sell_cap: usize,
    sync_sell_peak: u32,
    sync_sell_truncated: bool,
}

impl HotSummaryReducer {
    /// Create a reducer.
    ///
    /// * `first_n_cap` — how many of the earliest distinct buyers to retain.
    /// * `unique_cap` — capacity of the distinct-buyer set.
    /// * `sell_window_slots` — width (in slots) of the synchronized-sell window.
    /// * `sell_cap` — maximum number of in-window sell timestamps buffered; if
    ///   exceeded the oldest is dropped and the summary is marked truncated
    ///   (the synchronized-sell peak is then bounded by `sell_cap`).
    #[must_use]
    pub fn new(
        first_n_cap: usize,
        unique_cap: usize,
        sell_window_slots: u64,
        sell_cap: usize,
    ) -> Self {
        Self {
            first_n_cap,
            first_n_buyers: Vec::with_capacity(first_n_cap),
            unique_buyers: BoundedIdSet::with_capacity(unique_cap),
            cur_buy_slot: None,
            cur_slot_cobuy: 0,
            max_same_slot_cobuy: 0,
            sell_window_slots,
            sell_slots: std::collections::VecDeque::with_capacity(sell_cap),
            sell_cap,
            sync_sell_peak: 0,
            sync_sell_truncated: false,
        }
    }

    /// Fold in one buy event.
    pub fn on_buy(&mut self, ev: BuyEvent) {
        // Same-block co-buy: count buys sharing the current slot.
        match self.cur_buy_slot {
            Some(s) if s == ev.slot => {
                self.cur_slot_cobuy = self.cur_slot_cobuy.saturating_add(1);
            }
            _ => {
                self.cur_buy_slot = Some(ev.slot);
                self.cur_slot_cobuy = 1;
            }
        }
        if self.cur_slot_cobuy > self.max_same_slot_cobuy {
            self.max_same_slot_cobuy = self.cur_slot_cobuy;
        }

        // Distinct buyers + first-N co-occurrence.
        let newly = self.unique_buyers.insert(ev.wallet.0);
        if newly && self.first_n_buyers.len() < self.first_n_cap {
            self.first_n_buyers.push(ev.wallet.0);
        }
    }

    /// Fold in one sell event, updating the synchronized-sell window.
    pub fn on_sell(&mut self, ev: SellEvent) {
        // Evict timestamps that have fallen out of the trailing window.
        // Window covers [slot - (sell_window_slots - 1), slot].
        let lower = ev
            .slot
            .saturating_sub(self.sell_window_slots.saturating_sub(1));
        while let Some(&front) = self.sell_slots.front() {
            if front < lower {
                self.sell_slots.pop_front();
            } else {
                break;
            }
        }
        self.sell_slots.push_back(ev.slot);
        // Enforce the memory bound: never let the buffer exceed sell_cap.
        while self.sell_slots.len() > self.sell_cap {
            self.sell_slots.pop_front();
            self.sync_sell_truncated = true;
        }
        let cur = self.sell_slots.len() as u32;
        if cur > self.sync_sell_peak {
            self.sync_sell_peak = cur;
        }
    }

    /// Produce an immutable snapshot. `cluster_members` supplies the set of
    /// wallets currently flagged as belonging to a known cluster; the
    /// cluster-adjusted breadth subtracts those from the distinct-buyer count.
    #[must_use]
    pub fn summary(&self, cluster_members: &BoundedIdSet) -> HotSummary {
        let unique = self.unique_buyers.len() as u32;
        let mut clustered: u32 = 0;
        for &id in self.unique_buyers.as_slice() {
            if cluster_members.contains(id) {
                clustered = clustered.saturating_add(1);
            }
        }
        HotSummary {
            unique_buyers: unique,
            unique_buyers_overflowed: self.unique_buyers.overflow() > 0,
            max_same_slot_cobuy: self.max_same_slot_cobuy,
            first_n_buyers: self.first_n_buyers.iter().map(|&id| WalletId(id)).collect(),
            synchronized_sell_peak: self.sync_sell_peak,
            sync_sell_truncated: self.sync_sell_truncated,
            cluster_adjusted_breadth: unique.saturating_sub(clustered),
        }
    }

    /// Current distinct-buyer count (bounded).
    #[must_use]
    pub fn unique_buyer_count(&self) -> u32 {
        self.unique_buyers.len() as u32
    }
}
