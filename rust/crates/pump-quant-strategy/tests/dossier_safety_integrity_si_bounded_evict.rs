#![allow(unused_imports)]
use pump_quant_strategy::safety_integrity::*;

#[test]
fn never_exceeds_capacity_and_evicts_oldest() {
    let mut ring: BoundedRing<u32> = BoundedRing::new(3);
    assert_eq!(ring.capacity(), 3);
    assert_eq!(admit_with_eviction(&mut ring, 1), None);
    assert_eq!(admit_with_eviction(&mut ring, 2), None);
    assert_eq!(admit_with_eviction(&mut ring, 3), None);
    assert_eq!(ring.len(), 3);
    // at capacity: evict oldest (1) deterministically
    assert_eq!(admit_with_eviction(&mut ring, 4), Some(1));
    assert_eq!(ring.len(), 3);
    assert!(ring.len() <= ring.capacity());
    assert_eq!(admit_with_eviction(&mut ring, 5), Some(2));
    assert_eq!(ring.front(), Some(&3));
    assert_eq!(ring.back(), Some(&5));
}
