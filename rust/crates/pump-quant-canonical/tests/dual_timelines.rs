//! Leaf: dual timelines — observation truth vs canonical chain truth (§15, §16, §18.6).
//!
//! Verifies first-seen timing is kept per (source class, delivery mode) and never
//! equated across them; replay receipts never override live first-seen; and the
//! canonical commitment timeline is populated from live deliveries only.

mod common;

use common::{base, sig};
use pump_quant_canonical::{
    canonicalize_group, Commitment, DeliveryMode, Provider, SourceClass, TxClaim,
};

#[test]
fn first_seen_kept_separately_per_source_class() {
    let s = sig(20);
    let earliest = base(
        1,
        s,
        SourceClass::EarliestSignal,
        Provider::Jito,
        DeliveryMode::Live,
        10,
    );
    let structured = base(
        2,
        s,
        SourceClass::StructuredObservation,
        Provider::Helius,
        DeliveryMode::Live,
        25,
    );
    let ct = canonicalize_group(s, &[earliest, structured]);

    let e = ct
        .observation_timeline
        .first_seen_earliest_live()
        .expect("earliest live");
    let st = ct
        .observation_timeline
        .first_seen_structured_live()
        .expect("structured live");
    assert_eq!(e.time_ns, 10);
    assert_eq!(st.time_ns, 25);
    // Two distinct (class, mode) entries — timing never pooled across classes.
    assert_eq!(ct.observation_timeline.all().len(), 2);
}

#[test]
fn replay_receipt_never_overrides_live_first_seen() {
    let s = sig(21);
    // A provider-replay receipt arrives with an *earlier* time (5) than the live
    // receipt (25). Live first-seen must remain 25 (§18.6).
    let live = base(
        2,
        s,
        SourceClass::StructuredObservation,
        Provider::Helius,
        DeliveryMode::Live,
        25,
    );
    let replay = base(
        1,
        s,
        SourceClass::StructuredObservation,
        Provider::Helius,
        DeliveryMode::ProviderReplay,
        5,
    );
    let ct = canonicalize_group(s, &[live, replay]);

    let live_seen = ct
        .observation_timeline
        .first_seen_structured_live()
        .expect("live");
    assert_eq!(live_seen.time_ns, 25);
    // The replay receipt is recorded distinctly under its own key.
    let replay_seen = ct
        .observation_timeline
        .first_seen(
            SourceClass::StructuredObservation,
            DeliveryMode::ProviderReplay,
        )
        .expect("replay");
    assert_eq!(replay_seen.time_ns, 5);
    assert_eq!(ct.observation_timeline.all().len(), 2);
}

#[test]
fn earliest_within_same_key_wins() {
    let s = sig(22);
    // Two earliest-signal live receipts; the smaller time (8) wins.
    let a = base(
        1,
        s,
        SourceClass::EarliestSignal,
        Provider::Jito,
        DeliveryMode::Live,
        10,
    );
    let b = base(
        2,
        s,
        SourceClass::EarliestSignal,
        Provider::SuccessorShred(1),
        DeliveryMode::Live,
        8,
    );
    let ct = canonicalize_group(s, &[a, b]);
    let seen = ct
        .observation_timeline
        .first_seen_earliest_live()
        .expect("earliest");
    assert_eq!(seen.time_ns, 8);
    assert_eq!(seen.observation_id, 2);
}

#[test]
fn reconstructed_earliest_is_minimum_reported() {
    let s = sig(23);
    let mut a = base(
        1,
        s,
        SourceClass::EarliestSignal,
        Provider::Jito,
        DeliveryMode::Live,
        10,
    );
    a.reconstructed_time_ns = Some(30);
    let mut b = base(
        2,
        s,
        SourceClass::EarliestSignal,
        Provider::SuccessorShred(1),
        DeliveryMode::Live,
        12,
    );
    b.reconstructed_time_ns = Some(12);
    let ct = canonicalize_group(s, &[a, b]);
    let r = ct
        .observation_timeline
        .reconstructed_earliest()
        .expect("reconstructed");
    assert_eq!(r.time_ns, 12);
}

#[test]
fn canonical_commitment_timeline_from_live_only() {
    let s = sig(24);
    let mk = |id: u64, mode: DeliveryMode, level: Commitment, t: u64| {
        let mut o = base(
            id,
            s,
            SourceClass::StructuredObservation,
            Provider::Helius,
            mode,
            t,
        );
        o.claim = TxClaim {
            commitment: Some(level),
            ..TxClaim::default()
        };
        o
    };
    let obs = vec![
        mk(1, DeliveryMode::Live, Commitment::Processed, 100),
        mk(2, DeliveryMode::Live, Commitment::Confirmed, 150),
        mk(3, DeliveryMode::Live, Commitment::Finalized, 200),
        // A replay claiming Finalized much earlier must NOT populate the live line.
        mk(4, DeliveryMode::ProviderReplay, Commitment::Finalized, 1),
    ];
    let ct = canonicalize_group(s, &obs);
    assert_eq!(
        ct.canonical_timeline.processed_ns.map(|t| t.time_ns),
        Some(100)
    );
    assert_eq!(
        ct.canonical_timeline.confirmed_ns.map(|t| t.time_ns),
        Some(150)
    );
    assert_eq!(
        ct.canonical_timeline.finalized_ns.map(|t| t.time_ns),
        Some(200)
    );
    assert_eq!(ct.canonical_timeline.seen_ns, None);
}

#[test]
fn replay_only_finalized_leaves_live_finalized_empty() {
    let s = sig(25);
    let mut o = base(
        1,
        s,
        SourceClass::CanonicalRepair,
        Provider::CanonicalRpc,
        DeliveryMode::ProviderReplay,
        1,
    );
    o.claim = TxClaim {
        commitment: Some(Commitment::Finalized),
        ..TxClaim::default()
    };
    let ct = canonicalize_group(s, &[o]);
    // No live delivery => canonical live commitment timeline stays empty.
    assert_eq!(ct.canonical_timeline.finalized_ns, None);
    // But the fact (commitment=Finalized) is still resolved in fields.
    assert_eq!(ct.fields.commitment.value, Some(Commitment::Finalized));
}
