//! Leaf: feed-disagreement preservation (§15).
//!
//! The canonicalizer must never silently pick one provider's interpretation: any
//! field on which sources disagree is retained with full attribution.

mod common;

use common::{base, sig};
use pump_quant_canonical::{
    canonicalize_group, DeliveryMode, FieldName, Provider, SourceClass, TransactionObservation,
    TxClaim,
};

fn with_prio(mut o: TransactionObservation, fee: u64) -> TransactionObservation {
    o.claim = TxClaim {
        priority_fee_lamports: Some(fee),
        ..o.claim
    };
    o
}

#[test]
fn two_sources_disagree_on_priority_fee() {
    let s = sig(10);
    let obs = vec![
        with_prio(
            base(
                1,
                s,
                SourceClass::StructuredObservation,
                Provider::Helius,
                DeliveryMode::Live,
                1,
            ),
            5000,
        ),
        with_prio(
            base(
                2,
                s,
                SourceClass::CanonicalRepair,
                Provider::CanonicalRpc,
                DeliveryMode::RpcRepair,
                2,
            ),
            6000,
        ),
    ];
    let ct = canonicalize_group(s, &obs);

    // Canonical value from higher authority.
    assert_eq!(ct.fields.priority_fee_lamports.value, Some(6000));
    assert!(!ct.fields.priority_fee_lamports.agreed);
    assert!(ct.has_disagreement());

    let d = ct
        .disagreement(FieldName::PriorityFeeLamports)
        .expect("disagreement present");
    // Distinct claims sorted ascending by encoded value: 5000 then 6000.
    assert_eq!(d.claims.len(), 2);
    assert_eq!(d.claims[0].value_repr, 5000);
    assert_eq!(d.claims[0].sources.len(), 1);
    assert_eq!(
        d.claims[0].sources[0].source_class,
        SourceClass::StructuredObservation
    );
    assert_eq!(d.claims[0].sources[0].observation_id, 1);
    assert_eq!(d.claims[1].value_repr, 6000);
    assert_eq!(
        d.claims[1].sources[0].source_class,
        SourceClass::CanonicalRepair
    );
}

#[test]
fn full_agreement_yields_no_disagreement() {
    let s = sig(11);
    let obs = vec![
        with_prio(
            base(
                1,
                s,
                SourceClass::StructuredObservation,
                Provider::Helius,
                DeliveryMode::Live,
                1,
            ),
            4200,
        ),
        with_prio(
            base(
                2,
                s,
                SourceClass::CanonicalRepair,
                Provider::CanonicalRpc,
                DeliveryMode::RpcRepair,
                2,
            ),
            4200,
        ),
    ];
    let ct = canonicalize_group(s, &obs);
    assert_eq!(ct.fields.priority_fee_lamports.value, Some(4200));
    assert!(ct.fields.priority_fee_lamports.agreed);
    assert!(!ct.has_disagreement());
    assert!(ct.disagreement(FieldName::PriorityFeeLamports).is_none());
}

#[test]
fn agreeing_sources_are_grouped_under_one_value() {
    let s = sig(12);
    // Three sources say slot 42 (ids 3,1,2), one says slot 43 (id 4).
    let mk = |id: u64, class: SourceClass, slot: u64| {
        let mut o = base(id, s, class, Provider::Helius, DeliveryMode::Live, id);
        o.claim = TxClaim {
            slot: Some(slot),
            ..TxClaim::default()
        };
        o
    };
    let obs = vec![
        mk(3, SourceClass::EarliestSignal, 42),
        mk(1, SourceClass::StructuredObservation, 42),
        mk(2, SourceClass::EarliestSignal, 42),
        mk(4, SourceClass::CanonicalRepair, 43),
    ];
    let ct = canonicalize_group(s, &obs);

    // Canonical value: CanonicalRepair (highest authority) => 43.
    assert_eq!(ct.fields.slot.value, Some(43));
    let d = ct.disagreement(FieldName::Slot).expect("disagreement");
    assert_eq!(d.claims.len(), 2);
    // Value 42 grouped with its three sources, sorted by observation_id: 1,2,3.
    assert_eq!(d.claims[0].value_repr, 42);
    let ids: Vec<u64> = d.claims[0]
        .sources
        .iter()
        .map(|c| c.observation_id)
        .collect();
    assert_eq!(ids, vec![1, 2, 3]);
    // Value 43 has the single CanonicalRepair source.
    assert_eq!(d.claims[1].value_repr, 43);
    assert_eq!(d.claims[1].sources.len(), 1);
    assert_eq!(d.claims[1].sources[0].observation_id, 4);
}

#[test]
fn single_source_never_disagrees() {
    let s = sig(13);
    let obs = vec![with_prio(
        base(
            1,
            s,
            SourceClass::EarliestSignal,
            Provider::Jito,
            DeliveryMode::Live,
            1,
        ),
        999,
    )];
    let ct = canonicalize_group(s, &obs);
    assert_eq!(ct.fields.priority_fee_lamports.value, Some(999));
    assert!(ct.fields.priority_fee_lamports.agreed);
    assert!(!ct.has_disagreement());
}
