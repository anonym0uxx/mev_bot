#![allow(unused_imports)]
use pump_quant_execution::ex_blockhash_cache::*;

fn reference(cached: u64, cur: u64, max_age: u64) -> bool {
    cur.saturating_sub(cached) <= max_age
}

#[test]
fn default_window_is_150() {
    assert_eq!(DEFAULT_MAX_AGE_SLOTS, 150);
}

#[test]
fn valid_within_window() {
    // cached 1000, cur 1100, age 100 <= 150 -> valid
    assert!(blockhash_valid(1_000, 1_100, 150));
    assert_eq!(
        blockhash_valid(1_000, 1_100, 150),
        reference(1_000, 1_100, 150)
    );
}

#[test]
fn boundary_exactly_at_max_is_valid() {
    // age == max_age -> still valid (<=)
    assert!(blockhash_valid(1_000, 1_150, 150));
    // one past -> invalid
    assert!(!blockhash_valid(1_000, 1_151, 150));
}

#[test]
fn current_behind_cached_saturates_to_valid() {
    // cur < cached -> age 0 -> valid
    assert!(blockhash_valid(2_000, 1_500, 150));
    assert_eq!(
        blockhash_valid(2_000, 1_500, 150),
        reference(2_000, 1_500, 150)
    );
}

#[test]
fn cache_struct_roundtrip_and_checks() {
    let mut c = BlockhashCache::new([7u8; 32], 1_000);
    assert!(c.is_valid(1_050, 150));
    assert!(c.is_valid_default(1_150));
    assert!(!c.is_valid_default(1_151));
    assert_eq!(c.slots_remaining(1_100, 150), 50);
    assert_eq!(c.slots_remaining(2_000, 150), 0); // fully expired

    c.update([9u8; 32], 5_000);
    assert_eq!(c.blockhash, [9u8; 32]);
    assert_eq!(c.cached_slot, 5_000);
    assert!(c.is_valid_default(5_100));
}

#[test]
fn sweep_matches_reference() {
    for cached in [0u64, 100, 1_000, u64::MAX - 10] {
        for cur in [0u64, 100, 1_000, 1_150, 1_151, u64::MAX] {
            for max_age in [0u64, 1, 150, 1_000] {
                assert_eq!(
                    blockhash_valid(cached, cur, max_age),
                    reference(cached, cur, max_age),
                    "cached={cached} cur={cur} max_age={max_age}"
                );
            }
        }
    }
}
