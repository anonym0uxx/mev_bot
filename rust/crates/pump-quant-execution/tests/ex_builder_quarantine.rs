#![allow(unused_imports)]
use pump_quant_execution::ex_builder_quarantine::*;
use pump_quant_protocol::errors::FailureClass6;

const B: u32 = 7; // a builder id
const V: u32 = 1; // registry version

#[test]
fn fresh_state_admits_and_is_not_quarantined() {
    let q = BuilderQuarantineState::new();
    assert!(!q.is_quarantined(B, V));
    assert_eq!(q.check(B, V), BuilderAdmission::Admitted);
    assert_eq!(q.strikes(B, V), 0);
    assert_eq!(q.tracked_len(), 0);
}

#[test]
fn n_construction_strikes_trip_quarantine() {
    let mut q = BuilderQuarantineState::new();
    // Below threshold: still clear.
    for i in 1..QUARANTINE_STRIKE_THRESHOLD {
        let phase = q.record_failure(B, V, FailureClass6::RouteError);
        assert_eq!(phase, QuarantinePhase::Clear, "strike {i}");
        assert!(!q.is_quarantined(B, V));
        assert_eq!(q.strikes(B, V), i);
    }
    // Nth strike trips.
    let phase = q.record_failure(B, V, FailureClass6::RouteError);
    assert_eq!(phase, QuarantinePhase::Quarantined);
    assert!(q.is_quarantined(B, V));
    assert_eq!(q.check(B, V), BuilderAdmission::Quarantined);
    assert_eq!(q.strikes(B, V), QUARANTINE_STRIKE_THRESHOLD);
}

#[test]
fn version_drift_and_fatal_also_trip() {
    // VersionDrift strikes trip.
    let mut q = BuilderQuarantineState::new();
    for _ in 0..QUARANTINE_STRIKE_THRESHOLD {
        q.record_failure(B, V, FailureClass6::VersionDrift);
    }
    assert!(q.is_quarantined(B, V));

    // Fatal (unknown-code, fail-closed) strikes trip a different builder.
    let mut q2 = BuilderQuarantineState::new();
    for _ in 0..QUARANTINE_STRIKE_THRESHOLD {
        q2.record_failure(99, V, FailureClass6::Fatal);
    }
    assert!(q2.is_quarantined(99, V));
}

#[test]
fn slippage_and_other_market_classes_do_not_quarantine() {
    let mut q = BuilderQuarantineState::new();
    // Far more than the threshold of non-construction failures.
    for _ in 0..(QUARANTINE_STRIKE_THRESHOLD * 5) {
        assert_eq!(
            q.record_failure(B, V, FailureClass6::GuardOrSlippage),
            QuarantinePhase::Clear
        );
        assert_eq!(
            q.record_failure(B, V, FailureClass6::Transient),
            QuarantinePhase::Clear
        );
        assert_eq!(
            q.record_failure(B, V, FailureClass6::StateDrift),
            QuarantinePhase::Clear
        );
    }
    assert!(!q.is_quarantined(B, V));
    assert_eq!(q.strikes(B, V), 0);
    assert_eq!(q.check(B, V), BuilderAdmission::Admitted);
    // Sanity on the classifier helper itself.
    assert!(!class_triggers_quarantine(FailureClass6::GuardOrSlippage));
    assert!(!class_triggers_quarantine(FailureClass6::Transient));
    assert!(!class_triggers_quarantine(FailureClass6::StateDrift));
    assert!(class_triggers_quarantine(FailureClass6::RouteError));
    assert!(class_triggers_quarantine(FailureClass6::VersionDrift));
    assert!(class_triggers_quarantine(FailureClass6::Fatal));
}

#[test]
fn market_failures_do_not_reset_accumulated_strikes() {
    let mut q = BuilderQuarantineState::new();
    q.record_failure(B, V, FailureClass6::RouteError);
    q.record_failure(B, V, FailureClass6::RouteError);
    assert_eq!(q.strikes(B, V), 2);
    // A slippage failure in between must NOT reset the construction strikes.
    q.record_failure(B, V, FailureClass6::GuardOrSlippage);
    assert_eq!(q.strikes(B, V), 2);
    // One more construction strike now trips.
    let phase = q.record_failure(B, V, FailureClass6::RouteError);
    assert_eq!(phase, QuarantinePhase::Quarantined);
}

#[test]
fn success_does_not_clear_quarantine_sticky() {
    let mut q = BuilderQuarantineState::new();
    for _ in 0..QUARANTINE_STRIKE_THRESHOLD {
        q.record_failure(B, V, FailureClass6::RouteError);
    }
    assert!(q.is_quarantined(B, V));
    // A successful trade must NOT clear it (construction failures are sticky).
    q.record_success(B, V);
    assert!(q.is_quarantined(B, V));
    // Even many successes.
    for _ in 0..100 {
        q.record_success(B, V);
    }
    assert!(q.is_quarantined(B, V));
}

#[test]
fn registry_version_bump_resets_via_record() {
    let mut q = BuilderQuarantineState::new();
    for _ in 0..QUARANTINE_STRIKE_THRESHOLD {
        q.record_failure(B, V, FailureClass6::RouteError);
    }
    assert!(q.is_quarantined(B, V));
    // At the NEW version the builder is admitted again (quarantine was per-version).
    assert!(!q.is_quarantined(B, V + 1));
    assert_eq!(q.check(B, V + 1), BuilderAdmission::Admitted);
    // Recording a failure at the new version resets the slot: one strike, clear.
    let phase = q.record_failure(B, V + 1, FailureClass6::RouteError);
    assert_eq!(phase, QuarantinePhase::Clear);
    assert_eq!(q.strikes(B, V + 1), 1);
    // The old version key no longer reports quarantined (slot moved to new ver).
    assert!(!q.is_quarantined(B, V));
}

#[test]
fn registry_version_bump_resets_via_explicit_call() {
    let mut q = BuilderQuarantineState::new();
    for _ in 0..QUARANTINE_STRIKE_THRESHOLD {
        q.record_failure(B, V, FailureClass6::VersionDrift);
    }
    assert!(q.is_quarantined(B, V));
    q.on_registry_bump(B, V + 5);
    assert!(!q.is_quarantined(B, V + 5));
    assert!(!q.is_quarantined(B, V));
    assert_eq!(q.strikes(B, V + 5), 0);
    assert_eq!(q.check(B, V + 5), BuilderAdmission::Admitted);
}

#[test]
fn independent_builders_are_tracked_separately() {
    let mut q = BuilderQuarantineState::new();
    // Builder B trips; builder C stays clear.
    for _ in 0..QUARANTINE_STRIKE_THRESHOLD {
        q.record_failure(B, V, FailureClass6::RouteError);
    }
    q.record_failure(42, V, FailureClass6::RouteError);
    assert!(q.is_quarantined(B, V));
    assert!(!q.is_quarantined(42, V));
    assert_eq!(q.strikes(42, V), 1);
}

#[test]
fn state_is_bounded_and_quarantined_slots_survive_pressure() {
    let mut q = BuilderQuarantineState::new();
    // Quarantine builder 0 first.
    for _ in 0..QUARANTINE_STRIKE_THRESHOLD {
        q.record_failure(0, V, FailureClass6::RouteError);
    }
    assert!(q.is_quarantined(0, V));
    // Now churn far more distinct builders than capacity with single strikes.
    for id in 1..(MAX_TRACKED_BUILDERS as u32 * 3) {
        q.record_failure(id, V, FailureClass6::RouteError);
        assert!(q.tracked_len() <= MAX_TRACKED_BUILDERS);
    }
    // The quarantined builder must never have been evicted (stickiness).
    assert!(q.is_quarantined(0, V));
    assert!(q.tracked_len() <= MAX_TRACKED_BUILDERS);
}

#[test]
fn deterministic_same_sequence_same_state() {
    let seq = [
        (1u32, 1u32, FailureClass6::RouteError),
        (1, 1, FailureClass6::GuardOrSlippage),
        (2, 1, FailureClass6::Fatal),
        (1, 1, FailureClass6::VersionDrift),
        (1, 1, FailureClass6::RouteError),
    ];
    let mut a = BuilderQuarantineState::new();
    let mut b = BuilderQuarantineState::new();
    for &(id, v, c) in &seq {
        a.record_failure(id, v, c);
        b.record_failure(id, v, c);
    }
    assert_eq!(a.is_quarantined(1, 1), b.is_quarantined(1, 1));
    assert_eq!(a.strikes(1, 1), b.strikes(1, 1));
    assert_eq!(a.is_quarantined(2, 1), b.is_quarantined(2, 1));
    assert!(a.is_quarantined(1, 1)); // 3 construction strikes at (1,1)
}
