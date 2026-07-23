//! REGRESSION CLASS 2/3 — execution-plane law presence + fail-closed.
//!
//! Three durable invariants over the `pump-quant-execution` construction plane:
//!   * §77/§113 construction-gate PARITY — the minimal builder's bytes match the
//!     golden fixture AND round-trip back to the intended logical op, so the gate
//!     clears a faithfully-built instruction and REJECTS a tampered one.
//!   * the Phase-B live-state simulation rung is FAIL-CLOSED — the deferred stub
//!     never reports a pass, so the full gate can never green-light a "live"
//!     validation on the laptop (the sell-sim gate).
//!   * §78 builder-quarantine TRIPS on construction-class strikes and is STICKY
//!     (a success never clears it), and a would-be submitter's `check` gate flips
//!     to Quarantined at the strike threshold.
//!
//! All integer, deterministic, no wall-clock, no RNG (§22).

use pump_quant_execution::ex_construction_gate::{
    build_ix, decode_ix, golden_fixture, ConstructionValidationGate, GateRejection, GateSide,
    GateVenue, LiveValidatedStatus, LogicalOp, PhaseBDeferredSim,
};

fn op(venue: GateVenue, side: GateSide) -> LogicalOp {
    LogicalOp {
        venue,
        side,
        arg0: 123_456,
        arg1: 7_890,
        primary: [0x11; 32],
    }
}

#[test]
fn construction_gate_clears_faithful_build_across_every_venue_side() {
    for venue in [GateVenue::PumpFun, GateVenue::PumpSwap] {
        for side in [GateSide::Buy, GateSide::Sell] {
            let intended = op(venue, side);
            let built = build_ix(intended);
            let golden = golden_fixture(intended);

            // Parity rung + round-trip rung both pass ⇒ ValidatedDeterministic.
            let status = ConstructionValidationGate::validate(&built, intended, &golden);
            assert_eq!(
                status,
                LiveValidatedStatus::ValidatedDeterministic,
                "faithful build must clear the deterministic gate ({venue:?}/{side:?})"
            );
            assert!(status.is_validated());
            // Round-trip recovers the exact logical op (micro-verification rung).
            assert_eq!(
                decode_ix(&built),
                Some(intended),
                "the built ix must decode back to its intended op"
            );
        }
    }
}

#[test]
fn construction_gate_rejects_a_tampered_instruction() {
    let intended = op(GateVenue::PumpSwap, GateSide::Buy);
    let mut built = build_ix(intended);
    let golden = golden_fixture(intended);

    // Flip one data byte: parity must fail (built bytes != golden fixture).
    built.data[10] ^= 0x01;
    assert_eq!(
        ConstructionValidationGate::validate(&built, intended, &golden),
        LiveValidatedStatus::Rejected(GateRejection::FixtureParityMismatch),
        "a byte-tampered instruction must fail fixture parity — never silently pass"
    );

    // A build for a DIFFERENT op checked against the wrong golden also fails closed.
    let other = op(GateVenue::PumpSwap, GateSide::Sell);
    let other_built = build_ix(other);
    assert!(
        !ConstructionValidationGate::validate(&other_built, intended, &golden).is_validated(),
        "a mismatched (op, golden) pair must not validate"
    );
}

#[test]
fn phase_b_live_sim_rung_is_fail_closed() {
    // The Phase-A stand-in performs NO simulation and must never be mistaken for a
    // passing live check: the full gate rejects at the live-state rung even when
    // both deterministic rungs pass. This is the sell-sim gate failing closed.
    let intended = op(GateVenue::PumpFun, GateSide::Sell);
    let built = build_ix(intended);
    let golden = golden_fixture(intended);

    let status = ConstructionValidationGate::validate_with_sim(
        &built,
        intended,
        &golden,
        &PhaseBDeferredSim,
    );
    assert_eq!(
        status,
        LiveValidatedStatus::Rejected(GateRejection::LiveStateRejected),
        "the deferred live-state sim must fail closed — no ValidatedLive on the laptop"
    );
    assert!(
        !status.is_validated(),
        "a deferred live check can never report validated-live"
    );
}

// ---------------------------------------------------------------------------
// §78 builder-quarantine circuit breaker.
// ---------------------------------------------------------------------------

use pump_quant_execution::ex_builder_quarantine::{
    class_triggers_quarantine, BuilderAdmission, BuilderQuarantineState, QuarantinePhase,
    QUARANTINE_STRIKE_THRESHOLD,
};
use pump_quant_protocol::errors::FailureClass6;

#[test]
fn construction_strikes_trip_the_builder_quarantine_and_stick() {
    let mut q = BuilderQuarantineState::new();
    let builder = 42u32;
    let ver = 1u32;

    // Below the threshold the builder is still admitted.
    for _ in 0..(QUARANTINE_STRIKE_THRESHOLD - 1) {
        q.record_failure(builder, ver, FailureClass6::RouteError);
        assert_eq!(
            q.check(builder, ver),
            BuilderAdmission::Admitted,
            "must stay admitted below the strike threshold"
        );
    }
    // The threshold strike trips the quarantine — the submitter gate must flip.
    let phase = q.record_failure(builder, ver, FailureClass6::RouteError);
    assert_eq!(
        phase,
        QuarantinePhase::Quarantined,
        "threshold strike quarantines"
    );
    assert!(q.is_quarantined(builder, ver));
    assert_eq!(
        q.check(builder, ver),
        BuilderAdmission::Quarantined,
        "a would-be submitter MUST be refused once quarantined"
    );

    // STICKY: a success never clears a construction quarantine (§78).
    q.record_success(builder, ver);
    assert!(
        q.is_quarantined(builder, ver),
        "a success must NOT clear a construction quarantine"
    );

    // Only a registry-version bump clears it.
    q.on_registry_bump(builder, ver + 1);
    assert_eq!(
        q.check(builder, ver + 1),
        BuilderAdmission::Admitted,
        "a registry-version bump is the only path back to admitted"
    );
}

#[test]
fn only_construction_class_failures_trigger_quarantine() {
    // Market / recoverable classes must NEVER accumulate quarantine strikes.
    for benign in [
        FailureClass6::GuardOrSlippage,
        FailureClass6::Transient,
        FailureClass6::StateDrift,
    ] {
        assert!(
            !class_triggers_quarantine(benign),
            "{benign:?} must not be a construction-class strike"
        );
    }
    for constructiony in [
        FailureClass6::RouteError,
        FailureClass6::VersionDrift,
        FailureClass6::Fatal,
    ] {
        assert!(
            class_triggers_quarantine(constructiony),
            "{constructiony:?} must be a construction-class strike"
        );
    }

    // A flood of benign failures never quarantines (fail-OPEN on market noise).
    let mut q = BuilderQuarantineState::new();
    for _ in 0..100 {
        q.record_failure(7, 1, FailureClass6::GuardOrSlippage);
    }
    assert_eq!(
        q.check(7, 1),
        BuilderAdmission::Admitted,
        "market-noise failures must never quarantine a builder"
    );
}
