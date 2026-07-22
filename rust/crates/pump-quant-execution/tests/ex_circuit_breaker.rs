#![allow(unused_imports)]
use pump_quant_execution::ex_circuit_breaker::*;

#[test]
fn new_is_closed() {
    let s = BreakerState::new(3, 15_000);
    assert_eq!(s.phase, BreakerPhase::Closed);
    assert_eq!(s.consecutive_failures, 0);
    assert!(s.allows_send(0));
}

#[test]
fn failures_below_threshold_stay_closed() {
    let mut s = BreakerState::new(3, 15_000);
    s = breaker_next(s, SendOutcome::Failure { now_ms: 100 });
    assert_eq!(s.consecutive_failures, 1);
    assert_eq!(s.phase, BreakerPhase::Closed);
    s = breaker_next(s, SendOutcome::Failure { now_ms: 200 });
    assert_eq!(s.consecutive_failures, 2);
    assert_eq!(s.phase, BreakerPhase::Closed);
}

#[test]
fn threshold_failures_trip_open() {
    let mut s = BreakerState::new(3, 15_000);
    s = breaker_next(s, SendOutcome::Failure { now_ms: 100 });
    s = breaker_next(s, SendOutcome::Failure { now_ms: 200 });
    s = breaker_next(s, SendOutcome::Failure { now_ms: 300 });
    assert_eq!(s.phase, BreakerPhase::Open);
    assert_eq!(s.opened_at_ms, 300);
    assert_eq!(s.consecutive_failures, 3);
    assert!(!s.allows_send(300));
    assert!(!s.allows_send(300 + 14_999));
    assert!(s.allows_send(300 + 15_000)); // cooldown elapsed
    assert_eq!(s.remaining_ms(300 + 5_000), 10_000);
}

#[test]
fn success_resets_failures() {
    let mut s = BreakerState::new(3, 15_000);
    s = breaker_next(s, SendOutcome::Failure { now_ms: 100 });
    s = breaker_next(s, SendOutcome::Failure { now_ms: 200 });
    assert_eq!(s.consecutive_failures, 2);
    s = breaker_next(s, SendOutcome::Success { now_ms: 250 });
    assert_eq!(s.consecutive_failures, 0);
    assert_eq!(s.phase, BreakerPhase::Closed);
}

#[test]
fn open_during_cooldown_ignores_outcome() {
    let mut s = BreakerState::new(2, 10_000);
    s = breaker_next(s, SendOutcome::Failure { now_ms: 0 });
    s = breaker_next(s, SendOutcome::Failure { now_ms: 100 });
    assert_eq!(s.phase, BreakerPhase::Open);
    assert_eq!(s.opened_at_ms, 100);
    // A failure during cooldown must NOT change state (send was skipped).
    let before = s;
    s = breaker_next(s, SendOutcome::Failure { now_ms: 5_000 });
    assert_eq!(s, before);
}

#[test]
fn open_resets_after_cooldown_then_applies_outcome() {
    let mut s = BreakerState::new(2, 10_000);
    s = breaker_next(s, SendOutcome::Failure { now_ms: 0 });
    s = breaker_next(s, SendOutcome::Failure { now_ms: 100 });
    assert_eq!(s.phase, BreakerPhase::Open);
    // After cooldown, a failure resets counter to 0 then increments to 1.
    s = breaker_next(
        s,
        SendOutcome::Failure {
            now_ms: 100 + 10_000,
        },
    );
    assert_eq!(s.phase, BreakerPhase::Closed);
    assert_eq!(s.consecutive_failures, 1);
}

#[test]
fn open_resets_after_cooldown_on_success() {
    let mut s = BreakerState::new(2, 10_000);
    s = breaker_next(s, SendOutcome::Failure { now_ms: 0 });
    s = breaker_next(s, SendOutcome::Failure { now_ms: 100 });
    s = breaker_next(
        s,
        SendOutcome::Success {
            now_ms: 100 + 10_000,
        },
    );
    assert_eq!(s.phase, BreakerPhase::Closed);
    assert_eq!(s.consecutive_failures, 0);
}

#[test]
fn tick_does_not_change_closed_state() {
    let mut s = BreakerState::new(3, 15_000);
    s = breaker_next(s, SendOutcome::Failure { now_ms: 100 });
    let before = s;
    s = breaker_next(s, SendOutcome::Tick { now_ms: 200 });
    assert_eq!(s, before);
}

#[test]
fn tick_after_cooldown_resets_open_breaker() {
    let mut s = BreakerState::new(1, 5_000);
    s = breaker_next(s, SendOutcome::Failure { now_ms: 1_000 });
    assert_eq!(s.phase, BreakerPhase::Open);
    s = breaker_next(
        s,
        SendOutcome::Tick {
            now_ms: 1_000 + 5_000,
        },
    );
    assert_eq!(s.phase, BreakerPhase::Closed);
    assert_eq!(s.consecutive_failures, 0);
}
