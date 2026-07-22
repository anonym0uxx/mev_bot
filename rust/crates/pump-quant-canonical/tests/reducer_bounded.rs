//! Leaf: the stateful, memory-bounded grouping reducer (§15).
//!
//! Verifies grouping by signature, order-independent determinism, bounded
//! signature eviction by recency, and per-signature observation caps.

mod common;

use common::{base, sig};
use pump_quant_canonical::{
    canonicalize_group, Canonicalizer, DeliveryMode, Provider, SourceClass, TransactionObservation,
    TxClaim,
};

fn slot_obs(
    id: u64,
    s: pump_quant_canonical::Signature,
    class: SourceClass,
    slot: u64,
) -> TransactionObservation {
    let mut o = base(id, s, class, Provider::Helius, DeliveryMode::Live, id);
    o.claim = TxClaim {
        slot: Some(slot),
        ..TxClaim::default()
    };
    o
}

#[test]
fn groups_observations_by_signature() {
    let a = sig(40);
    let b = sig(41);
    let mut c = Canonicalizer::new(8, 8);
    assert_eq!(
        c.ingest(slot_obs(1, a, SourceClass::EarliestSignal, 100)),
        None
    );
    assert_eq!(
        c.ingest(slot_obs(2, b, SourceClass::EarliestSignal, 200)),
        None
    );
    assert_eq!(
        c.ingest(slot_obs(3, a, SourceClass::CanonicalRepair, 101)),
        None
    );
    assert_eq!(c.len(), 2);

    let ca = c.peek(&a).expect("a tracked");
    assert_eq!(ca.observation_count, 2);
    assert_eq!(ca.fields.slot.value, Some(101)); // repair authority over earliest
    let cb = c.peek(&b).expect("b tracked");
    assert_eq!(cb.observation_count, 1);
    assert_eq!(cb.fields.slot.value, Some(200));
}

#[test]
fn canonicalization_is_order_independent() {
    let s = sig(42);
    let o1 = slot_obs(1, s, SourceClass::EarliestSignal, 100);
    let o2 = slot_obs(2, s, SourceClass::StructuredObservation, 101);
    let o3 = slot_obs(3, s, SourceClass::CanonicalRepair, 102);

    let forward = canonicalize_group(s, &[o1.clone(), o2.clone(), o3.clone()]);
    let reversed = canonicalize_group(s, &[o3, o2, o1]);
    assert_eq!(forward, reversed);
}

#[test]
fn peek_matches_pure_function() {
    let s = sig(43);
    let o1 = slot_obs(1, s, SourceClass::StructuredObservation, 10);
    let o2 = slot_obs(2, s, SourceClass::CanonicalRepair, 11);
    let mut c = Canonicalizer::new(4, 4);
    c.ingest(o1.clone());
    c.ingest(o2.clone());
    assert_eq!(c.peek(&s).unwrap(), canonicalize_group(s, &[o1, o2]));
}

#[test]
fn evicts_least_recently_updated_signature() {
    let a = sig(44);
    let b = sig(45);
    let d = sig(46);
    let mut c = Canonicalizer::new(2, 8);
    c.ingest(slot_obs(1, a, SourceClass::EarliestSignal, 1)); // a latest=1
    c.ingest(slot_obs(2, b, SourceClass::EarliestSignal, 2)); // b latest=2
    c.ingest(slot_obs(5, a, SourceClass::CanonicalRepair, 3)); // a latest=5 (bumped)
    assert_eq!(c.len(), 2);

    // New signature d at capacity => evict lowest latest id => b (latest 2).
    let ev = c
        .ingest(slot_obs(6, d, SourceClass::EarliestSignal, 4))
        .expect("eviction");
    assert_eq!(ev.canonical.signature, b);
    assert_eq!(c.len(), 2);
    assert!(c.peek(&a).is_some());
    assert!(c.peek(&d).is_some());
    assert!(c.peek(&b).is_none());
}

#[test]
fn per_signature_observation_cap_drops_extras() {
    let s = sig(47);
    let mut c = Canonicalizer::new(4, 2); // keep at most 2 observations per sig
    c.ingest(slot_obs(1, s, SourceClass::EarliestSignal, 100));
    c.ingest(slot_obs(2, s, SourceClass::StructuredObservation, 101));
    c.ingest(slot_obs(3, s, SourceClass::CanonicalRepair, 102)); // dropped (cap hit)

    assert_eq!(c.total_dropped_observations(), 1);
    let ct = c.peek(&s).unwrap();
    assert_eq!(ct.observation_count, 2); // only first two retained
                                         // The dropped CanonicalRepair claim is absent, so canonical slot is the
                                         // highest-authority *retained* claim: StructuredObservation => 101.
    assert_eq!(ct.fields.slot.value, Some(101));
}

#[test]
fn drain_all_returns_signature_sorted() {
    let mut c = Canonicalizer::new(8, 8);
    c.ingest(slot_obs(1, sig(51), SourceClass::EarliestSignal, 1));
    c.ingest(slot_obs(2, sig(50), SourceClass::EarliestSignal, 2));
    c.ingest(slot_obs(3, sig(52), SourceClass::EarliestSignal, 3));
    let drained = c.drain_all();
    let sigs: Vec<_> = drained.iter().map(|ct| ct.signature).collect();
    assert_eq!(sigs, vec![sig(50), sig(51), sig(52)]);
    assert!(c.is_empty());
}
