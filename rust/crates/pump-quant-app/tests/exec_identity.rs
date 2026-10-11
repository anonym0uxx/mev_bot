//! Graduation execution-identity laws.
use pump_quant_app::exec_identity::{AttemptBook, AttemptId, AttemptStatus, IdentityError};
use pump_quant_app::shadow_pool::ShadowVenue::{Curve, Pool};

const M: [u8; 32] = [2u8; 32];

fn a(intent: u64, seq: u32) -> AttemptId {
    AttemptId { intent, seq }
}

#[test]
fn unsubmitted_intent_may_reroute_only_after_pool_verification() {
    let mut b = AttemptBook::default();
    b.register(1, M, 1_000, Curve).unwrap();
    assert_eq!(b.on_graduation(&M), vec![1]);
    assert_eq!(
        b.reroute_to_pool(1, false),
        Err(IdentityError::PoolUnverified)
    );
    assert_eq!(b.intents[&1].route, Curve);
    b.reroute_to_pool(1, true).unwrap();
    assert_eq!(b.submit(a(1, 0), Pool, 1_000, 50), Ok(a(1, 0)));
    assert_eq!(b.intents[&1].attempts[0].venue, Pool);
}

#[test]
fn submitted_or_uncertain_curve_attempt_never_becomes_a_pool_attempt() {
    let mut b = AttemptBook::default();
    b.register(1, M, 1_000, Curve).unwrap();
    b.submit(a(1, 0), Curve, 1_000, 10).unwrap();
    // Graduation happens: the submitted intent is NOT a re-route candidate.
    assert!(b.on_graduation(&M).is_empty());
    assert_eq!(
        b.reroute_to_pool(1, true),
        Err(IdentityError::RerouteAfterSubmission)
    );
    assert_eq!(
        b.route_replacement_to_pool(1, true),
        Err(IdentityError::EarlierAttemptUnresolved)
    );
    b.mark_uncertain(a(1, 0)).unwrap();
    assert_eq!(
        b.reroute_to_pool(1, true),
        Err(IdentityError::RerouteAfterSubmission)
    );
    // Replacement while uncertain: refused (no overlapping disposal).
    assert_eq!(
        b.submit(a(1, 1), Curve, 1_000, 11),
        Err(IdentityError::EarlierAttemptUnresolved)
    );
    // Reservation and evidence stay attributable to the curve attempt.
    assert_eq!(b.reserved(&M), 1_000);
    let at = b.intents[&1].attempts[0];
    assert_eq!(
        (at.venue, at.status, at.reserved),
        (Curve, AttemptStatus::Uncertain, 1_000)
    );
}

#[test]
fn partial_execution_across_migration_needs_explicit_replacement_identity() {
    let mut b = AttemptBook::default();
    b.register(1, M, 1_000, Curve).unwrap();
    b.submit(a(1, 0), Curve, 1_000, 10).unwrap();
    b.observe_fill(a(1, 0), 300).unwrap();
    assert_eq!(
        b.observe_fill(a(1, 0), 200),
        Err(IdentityError::FillInconsistent)
    );
    assert_eq!(
        b.observe_fill(a(1, 0), 1_001),
        Err(IdentityError::FillInconsistent)
    );
    // Definitive reconciliation: the curve attempt landed 400 tokens.
    b.reconcile(a(1, 0), Some(400)).unwrap();
    assert_eq!(
        b.reconcile(a(1, 0), Some(400)),
        Err(IdentityError::AlreadyReconciled)
    );
    assert_eq!(b.reserved(&M), 0);
    // Replacement on the pool: explicit route + explicit next seq.
    assert_eq!(
        b.submit(a(1, 1), Pool, 600, 20),
        Err(IdentityError::VenueMismatch)
    );
    assert_eq!(
        b.route_replacement_to_pool(1, false),
        Err(IdentityError::PoolUnverified)
    );
    b.route_replacement_to_pool(1, true).unwrap();
    assert_eq!(
        b.submit(a(1, 5), Pool, 600, 20),
        Err(IdentityError::AttemptIdentity)
    );
    assert_eq!(
        b.submit(a(1, 0), Pool, 600, 20),
        Err(IdentityError::AttemptIdentity)
    );
    b.submit(a(1, 1), Pool, 600, 20).unwrap();
    let ats = &b.intents[&1].attempts;
    assert_eq!(
        (ats[0].venue, ats[0].filled),
        (Curve, 400),
        "curve evidence kept"
    );
    assert_eq!(
        (ats[1].venue, ats[1].reserved),
        (Pool, 600),
        "replacement = remaining only"
    );
}

#[test]
fn no_overlapping_disposal_across_intents_of_a_mint() {
    let mut b = AttemptBook::default();
    b.register(1, M, 700, Curve).unwrap();
    b.register(2, M, 700, Curve).unwrap();
    b.submit(a(1, 0), Curve, 1_000, 10).unwrap();
    assert_eq!(
        b.submit(a(2, 0), Curve, 1_000, 10),
        Err(IdentityError::OverlappingDisposal)
    );
    b.reconcile(a(1, 0), None).unwrap();
    assert_eq!(
        b.reconcile(a(2, 0), None),
        Err(IdentityError::AttemptIdentity)
    );
    b.submit(a(2, 0), Curve, 1_000, 11).unwrap();
    assert_eq!(b.reserved(&M), 700);
}

#[test]
fn failed_reconciliation_with_observed_fill_is_refused() {
    let mut b = AttemptBook::default();
    b.register(1, M, 100, Curve).unwrap();
    b.submit(a(1, 0), Curve, 100, 1).unwrap();
    b.observe_fill(a(1, 0), 10).unwrap();
    assert_eq!(
        b.reconcile(a(1, 0), None),
        Err(IdentityError::FillInconsistent)
    );
}

#[test]
fn restart_across_migration_keeps_identity_reservation_and_evidence() {
    let mut b = AttemptBook::default();
    b.register(1, M, 1_000, Curve).unwrap();
    b.register(2, M, 200, Curve).unwrap();
    b.submit(a(1, 0), Curve, 2_000, 10).unwrap();
    b.observe_fill(a(1, 0), 250).unwrap();
    let text = serde_json::to_string(&b.to_json()).unwrap();
    // --- restart (graduation happened while down)
    let mut r = AttemptBook::from_json(&serde_json::from_str(&text).unwrap()).unwrap();
    r.on_restart();
    let at = r.intents[&1].attempts[0];
    assert_eq!(
        (at.venue, at.status, at.filled, at.reserved),
        (Curve, AttemptStatus::Uncertain, 250, 1_000)
    );
    assert_eq!(
        r.on_graduation(&M),
        vec![2],
        "only the unsubmitted intent may be re-routed"
    );
    assert_eq!(
        r.reroute_to_pool(1, true),
        Err(IdentityError::RerouteAfterSubmission)
    );
    r.reroute_to_pool(2, true).unwrap();
    // Reservation of the uncertain curve attempt still blocks overlapping disposal.
    assert_eq!(
        r.submit(a(2, 0), Pool, 1_100, 30),
        Err(IdentityError::OverlappingDisposal)
    );
    r.submit(a(2, 0), Pool, 1_200, 30).unwrap();
    // Round trip again is exact.
    let again = AttemptBook::from_json(&r.to_json()).unwrap();
    assert_eq!(again, r);
    // Malformed durable form refused.
    let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
    v["intents"][0]["attempts"][0][4] = serde_json::json!("pending");
    assert!(AttemptBook::from_json(&v).is_err());
}
