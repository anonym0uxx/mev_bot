//! Leaf: fork status resolution and dropped-fork preservation (§15, §17).
//!
//! An early observation may precede canonical inclusion and later be dropped; the
//! canonicalizer resolves fork status by authority and preserves disagreement,
//! never assuming an early sighting is canonical.

mod common;

use common::{base, sig};
use pump_quant_canonical::{
    canonicalize_group, DeliveryMode, FieldName, ForkStatus, Provider, SourceClass,
    TransactionObservation, TxClaim,
};

fn with_fork(mut o: TransactionObservation, f: ForkStatus) -> TransactionObservation {
    o.claim = TxClaim {
        fork: Some(f),
        ..o.claim
    };
    o
}

#[test]
fn canonical_repair_drops_a_fork_seen_earlier() {
    let s = sig(30);
    // Earliest signal saw it on a fork; structured saw it as canonical; canonical
    // repair reveals it was dropped. Highest authority (repair) => Dropped.
    let obs = vec![
        with_fork(
            base(
                1,
                s,
                SourceClass::EarliestSignal,
                Provider::Jito,
                DeliveryMode::Live,
                1,
            ),
            ForkStatus::OnFork,
        ),
        with_fork(
            base(
                2,
                s,
                SourceClass::StructuredObservation,
                Provider::Helius,
                DeliveryMode::Live,
                2,
            ),
            ForkStatus::Canonical,
        ),
        with_fork(
            base(
                3,
                s,
                SourceClass::CanonicalRepair,
                Provider::CanonicalRpc,
                DeliveryMode::RpcRepair,
                3,
            ),
            ForkStatus::Dropped,
        ),
    ];
    let ct = canonicalize_group(s, &obs);
    assert_eq!(ct.fork_status(), ForkStatus::Dropped);
    assert_eq!(ct.fork.authority, Some(SourceClass::CanonicalRepair));
    assert!(!ct.fork.agreed);

    // Disagreement preserves all three claimed fork states.
    let d = ct.disagreement(FieldName::Fork).expect("fork disagreement");
    assert_eq!(d.claims.len(), 3);
}

#[test]
fn agreeing_canonical_fork_status() {
    let s = sig(31);
    let obs = vec![
        with_fork(
            base(
                1,
                s,
                SourceClass::StructuredObservation,
                Provider::Helius,
                DeliveryMode::Live,
                1,
            ),
            ForkStatus::Canonical,
        ),
        with_fork(
            base(
                2,
                s,
                SourceClass::ReconciledExecution,
                Provider::Helius,
                DeliveryMode::Live,
                2,
            ),
            ForkStatus::Canonical,
        ),
    ];
    let ct = canonicalize_group(s, &obs);
    assert_eq!(ct.fork_status(), ForkStatus::Canonical);
    assert!(ct.fork.agreed);
    assert!(ct.disagreement(FieldName::Fork).is_none());
}

#[test]
fn unasserted_fork_is_unknown() {
    let s = sig(32);
    // No source asserts fork status.
    let obs = vec![base(
        1,
        s,
        SourceClass::EarliestSignal,
        Provider::Jito,
        DeliveryMode::Live,
        1,
    )];
    let ct = canonicalize_group(s, &obs);
    assert_eq!(ct.fork_status(), ForkStatus::Unknown);
    assert_eq!(ct.fork.value, None);
    assert_eq!(ct.fork.contributing, 0);
}

#[test]
fn reconciled_execution_confirms_canonical_over_earlier_onfork() {
    let s = sig(33);
    // Same transaction: earliest OnFork, then reconciled execution confirms Canonical.
    let obs = vec![
        with_fork(
            base(
                1,
                s,
                SourceClass::EarliestSignal,
                Provider::SuccessorShred(2),
                DeliveryMode::Live,
                1,
            ),
            ForkStatus::OnFork,
        ),
        with_fork(
            base(
                2,
                s,
                SourceClass::ReconciledExecution,
                Provider::Helius,
                DeliveryMode::Live,
                2,
            ),
            ForkStatus::Canonical,
        ),
    ];
    let ct = canonicalize_group(s, &obs);
    assert_eq!(ct.fork_status(), ForkStatus::Canonical);
    assert_eq!(ct.fork.authority, Some(SourceClass::ReconciledExecution));
    assert!(!ct.fork.agreed); // OnFork vs Canonical preserved as disagreement
}
