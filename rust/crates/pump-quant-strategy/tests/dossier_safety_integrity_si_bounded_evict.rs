// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'safety_integrity' component (leaf 'si_bounded_evict').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
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
