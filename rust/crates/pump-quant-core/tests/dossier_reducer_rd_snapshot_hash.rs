#![allow(unused_imports)]
use pump_quant_core::reducer::*;
#[test]
fn prop_hash_order_stable() {
    let mut w1 = WorldState::new();
    let mut w2 = WorldState::new();
    for i in 0..100u64 { w1.upsert_market(i, MarketState::test_with(i)); }
    for i in (0..100u64).rev() { w2.upsert_market(i, MarketState::test_with(i)); }
    assert_eq!(state_hash(&w1), state_hash(&w2)); // insertion order irrelevant
    assert_eq!(state_hash(&w1), state_hash(&w1)); // stable within process
}
