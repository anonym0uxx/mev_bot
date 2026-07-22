//! §99 bound: WorldState never exceeds its capacity; FIFO eviction on overflow.
#![allow(clippy::bool_comparison)]
use pump_quant_core::reducer::*;

#[test]
fn world_state_is_capacity_bounded_fifo() {
    let cap = 8;
    let mut w = WorldState::with_capacity(cap);
    // Insert more distinct markets than the cap.
    for i in 0..100u64 {
        w.upsert_market(i, MarketState::test_with(i));
    }
    assert_eq!(w.len(), cap, "never grows past the cap");
    assert_eq!(w.capacity(), cap);
    // Oldest-inserted keys were evicted; only the most recent `cap` remain.
    for i in 0..(100 - cap as u64) {
        assert!(w.market(i).is_none(), "old key {i} evicted");
    }
    for i in (100 - cap as u64)..100 {
        assert!(w.market(i).is_some(), "recent key {i} retained");
    }
}

#[test]
fn reupsert_existing_key_never_evicts() {
    let mut w = WorldState::with_capacity(2);
    w.upsert_market(1, MarketState::test_with(1));
    w.upsert_market(2, MarketState::test_with(2));
    // Re-upserting an existing key must not evict the other.
    w.upsert_market(1, MarketState::test_with(1));
    assert!(w.market(1).is_some() && w.market(2).is_some());
    assert_eq!(w.len(), 2);
}

#[test]
fn default_cap_is_large_enough_for_normal_use() {
    let w = WorldState::new();
    assert_eq!(w.capacity(), DEFAULT_MARKET_CAP);
    // New worlds adopt the default cap, comfortably above any realistic universe.
    assert!(w.capacity() > 1_000);
}
