//! Leaf: authority-ordered canonical field resolution (§15, §18.8).
//!
//! Verifies that the canonical value of a field is decided by source authority
//! class (never by provider or feed quality), with deterministic tie-breaks, and
//! with independently hand-computed expectations across multiple inputs.

mod common;

use common::{base, sig};
use pump_quant_canonical::{
    canonicalize_group, DeliveryMode, Provider, SourceClass, TransactionObservation, TxClaim,
};

fn with_slot(mut o: TransactionObservation, slot: u64) -> TransactionObservation {
    o.claim = TxClaim {
        slot: Some(slot),
        ..o.claim
    };
    o
}

#[test]
fn higher_authority_class_wins_slot() {
    let s = sig(1);
    let obs = vec![
        with_slot(
            base(
                10,
                s,
                SourceClass::EarliestSignal,
                Provider::Jito,
                DeliveryMode::Live,
                1,
            ),
            100,
        ),
        with_slot(
            base(
                11,
                s,
                SourceClass::StructuredObservation,
                Provider::Helius,
                DeliveryMode::Live,
                2,
            ),
            101,
        ),
        with_slot(
            base(
                12,
                s,
                SourceClass::CanonicalRepair,
                Provider::CanonicalRpc,
                DeliveryMode::RpcRepair,
                3,
            ),
            102,
        ),
    ];
    let ct = canonicalize_group(s, &obs);

    // CanonicalRepair (rank 2) outranks StructuredObservation (1) and EarliestSignal (0).
    assert_eq!(ct.fields.slot.value, Some(102));
    assert_eq!(ct.fields.slot.authority, Some(SourceClass::CanonicalRepair));
    assert_eq!(ct.fields.slot.contributing, 3);
    assert!(!ct.fields.slot.agreed); // three distinct slots => disagreement preserved
}

#[test]
fn reconciled_execution_outranks_repair() {
    let s = sig(2);
    let obs = vec![
        with_slot(
            base(
                1,
                s,
                SourceClass::CanonicalRepair,
                Provider::CanonicalRpc,
                DeliveryMode::RpcRepair,
                5,
            ),
            102,
        ),
        with_slot(
            base(
                2,
                s,
                SourceClass::ReconciledExecution,
                Provider::Helius,
                DeliveryMode::Live,
                6,
            ),
            555,
        ),
    ];
    let ct = canonicalize_group(s, &obs);
    assert_eq!(ct.fields.slot.value, Some(555));
    assert_eq!(
        ct.fields.slot.authority,
        Some(SourceClass::ReconciledExecution)
    );
}

#[test]
fn same_class_ties_break_by_lowest_observation_id() {
    let s = sig(3);
    // Two equally-authoritative CanonicalRepair claims disagree on slot.
    // Tie-break = lowest observation_id => id 2 (slot 301) wins over id 5 (slot 300).
    let obs = vec![
        with_slot(
            base(
                5,
                s,
                SourceClass::CanonicalRepair,
                Provider::CanonicalRpc,
                DeliveryMode::RpcRepair,
                1,
            ),
            300,
        ),
        with_slot(
            base(
                2,
                s,
                SourceClass::CanonicalRepair,
                Provider::Other(7),
                DeliveryMode::RpcRepair,
                2,
            ),
            301,
        ),
    ];
    let ct = canonicalize_group(s, &obs);
    assert_eq!(ct.fields.slot.value, Some(301));
    assert_eq!(ct.fields.slot.authority, Some(SourceClass::CanonicalRepair));
    assert!(!ct.fields.slot.agreed);
}

#[test]
fn success_resolved_by_authority_not_by_earliest_signal() {
    let s = sig(4);
    // Earliest signal (unconfirmed, droppable) claims success; canonical repair
    // reveals failure. Repair authority must win.
    let mut a = base(
        1,
        s,
        SourceClass::EarliestSignal,
        Provider::Jito,
        DeliveryMode::Live,
        1,
    );
    a.claim = TxClaim {
        success: Some(true),
        ..TxClaim::default()
    };
    let mut b = base(
        2,
        s,
        SourceClass::CanonicalRepair,
        Provider::CanonicalRpc,
        DeliveryMode::RpcRepair,
        2,
    );
    b.claim = TxClaim {
        success: Some(false),
        ..TxClaim::default()
    };

    let ct = canonicalize_group(s, &[a, b]);
    assert_eq!(ct.fields.success.value, Some(false));
    assert_eq!(
        ct.fields.success.authority,
        Some(SourceClass::CanonicalRepair)
    );
    assert!(!ct.fields.success.agreed);
}

#[test]
fn unasserted_field_is_empty_and_agreed() {
    let s = sig(5);
    // Only slot asserted; base fee never asserted.
    let obs = vec![with_slot(
        base(
            1,
            s,
            SourceClass::StructuredObservation,
            Provider::Helius,
            DeliveryMode::Live,
            1,
        ),
        7,
    )];
    let ct = canonicalize_group(s, &obs);
    assert_eq!(ct.fields.base_fee_lamports.value, None);
    assert_eq!(ct.fields.base_fee_lamports.authority, None);
    assert_eq!(ct.fields.base_fee_lamports.contributing, 0);
    assert!(ct.fields.base_fee_lamports.agreed);
    assert!(!ct.has_disagreement()); // single slot claim, nothing disagrees
}

#[test]
fn commitment_takes_highest_level_among_equal_authority() {
    let s = sig(6);
    // Two ReconciledExecution observations: Processed then Finalized. Highest
    // commitment (Finalized) must survive despite Processed having lower id.
    let mut a = base(
        1,
        s,
        SourceClass::ReconciledExecution,
        Provider::Helius,
        DeliveryMode::Live,
        10,
    );
    a.claim = TxClaim {
        commitment: Some(pump_quant_canonical::Commitment::Processed),
        ..TxClaim::default()
    };
    let mut b = base(
        2,
        s,
        SourceClass::ReconciledExecution,
        Provider::Helius,
        DeliveryMode::Live,
        20,
    );
    b.claim = TxClaim {
        commitment: Some(pump_quant_canonical::Commitment::Finalized),
        ..TxClaim::default()
    };
    let ct = canonicalize_group(s, &[a, b]);
    assert_eq!(
        ct.fields.commitment.value,
        Some(pump_quant_canonical::Commitment::Finalized)
    );
}
