//! Leaf: `lifecycle`. Source-registry FSM (§18.8): legal/illegal transitions,
//! sunset one-way rule, terminal handling, replacement field, immutable
//! authority, and the bounded transition audit ring.

use pump_quant_governance::lifecycle::{
    SourceAuthorityClass, SourceEntry, SourceId, SourceLifecycleStatus, TransitionError,
};

use SourceLifecycleStatus::*;

/// Hand-enumerated legal transitions must be accepted and illegal ones refused,
/// including the one-way sunset rule and terminal `Retired`.
#[test]
fn transition_matrix() {
    // Legal moves.
    let legal = [
        (ActivePrimary, ActiveRedundant),
        (ActivePrimary, Degraded),
        (ActivePrimary, SunsetPending),
        (ActivePrimary, Disabled),
        (ActiveRedundant, ActivePrimary),
        (Transitional, ActiveRedundant),
        (Transitional, SunsetPending),
        (Transitional, Disabled),
        (Degraded, ActivePrimary),
        (Degraded, ActiveRedundant),
        (SunsetPending, Disabled),
        (SunsetPending, Retired),
        (Disabled, ActivePrimary),
        (Disabled, ActiveRedundant),
        (Disabled, Retired),
    ];
    for (from, to) in legal {
        assert!(
            from.can_transition_to(to),
            "{from:?} -> {to:?} should be legal"
        );
    }

    // Illegal moves.
    let illegal = [
        (ActivePrimary, ActivePrimary), // no-op is never a transition
        (ActivePrimary, Retired),       // must pass through sunset/disabled
        (Transitional, ActivePrimary),  // never promoted to permanent primary
        (SunsetPending, ActivePrimary), // sunset is one-way (no revival)
        (SunsetPending, ActiveRedundant),
        (SunsetPending, Degraded),
        (Retired, ActivePrimary), // terminal
        (Retired, Disabled),
    ];
    for (from, to) in illegal {
        assert!(
            !from.can_transition_to(to),
            "{from:?} -> {to:?} should be illegal"
        );
    }
}

#[test]
fn transition_applies_and_records() {
    let mut e = SourceEntry::new(
        SourceId(1),
        SourceAuthorityClass::StructuredObservation,
        ActivePrimary,
        8,
    );
    assert_eq!(e.status(), ActivePrimary);

    e.transition(Degraded, 1).unwrap();
    assert_eq!(e.status(), Degraded);

    // Illegal transition leaves state unchanged and returns the error.
    let err = e.transition(Retired, 2).unwrap_err();
    assert_eq!(
        err,
        TransitionError::IllegalTransition {
            from: Degraded,
            to: Retired
        }
    );
    assert_eq!(e.status(), Degraded);

    e.transition(ActivePrimary, 3).unwrap();
    assert_eq!(e.status(), ActivePrimary);

    let hist = e.history();
    assert_eq!(hist.len(), 2);
    assert_eq!(
        (hist[0].from, hist[0].to, hist[0].sequence),
        (ActivePrimary, Degraded, 1)
    );
    assert_eq!(
        (hist[1].from, hist[1].to, hist[1].sequence),
        (Degraded, ActivePrimary, 3)
    );
}

/// The transitional Jito ShredStream path: TRANSITIONAL -> SUNSET_PENDING ->
/// RETIRED with a recorded replacement (§18.3.1, §18.8 replacement field).
#[test]
fn jito_sunset_to_replaced() {
    let mut jito = SourceEntry::new(
        SourceId(10),
        SourceAuthorityClass::EarliestSignal,
        Transitional,
        4,
    );
    let successor = SourceId(11);

    jito.transition(SunsetPending, 100).unwrap();
    // Cannot revive a sunset feed.
    assert_eq!(
        jito.transition(ActiveRedundant, 101).unwrap_err(),
        TransitionError::IllegalTransition {
            from: SunsetPending,
            to: ActiveRedundant
        }
    );
    jito.retire_replaced_by(successor, 102).unwrap();

    assert_eq!(jito.status(), Retired);
    assert_eq!(jito.replaced_by(), Some(successor));

    // Terminal: any further transition is AlreadyTerminal.
    assert_eq!(
        jito.transition(Disabled, 103).unwrap_err(),
        TransitionError::AlreadyTerminal
    );
}

/// retire_replaced_by is illegal directly from a live state (must reach a
/// retire-eligible state first), and the replacement is not set on failure.
#[test]
fn retire_replaced_by_respects_fsm() {
    let mut e = SourceEntry::new(
        SourceId(2),
        SourceAuthorityClass::CanonicalRepair,
        ActivePrimary,
        4,
    );
    let err = e.retire_replaced_by(SourceId(3), 1).unwrap_err();
    assert_eq!(
        err,
        TransitionError::IllegalTransition {
            from: ActivePrimary,
            to: Retired
        }
    );
    assert_eq!(e.status(), ActivePrimary);
    assert_eq!(e.replaced_by(), None);
}

/// Canonical authority class is immutable across every transition (§18.8).
#[test]
fn authority_is_immutable() {
    let mut e = SourceEntry::new(
        SourceId(5),
        SourceAuthorityClass::ReconciledExecution,
        ActivePrimary,
        4,
    );
    e.transition(Degraded, 1).unwrap();
    e.transition(Disabled, 2).unwrap();
    e.transition(ActiveRedundant, 3).unwrap();
    assert_eq!(e.authority(), SourceAuthorityClass::ReconciledExecution);
}

/// The audit trail is a bounded ring (§57): capacity is never exceeded, oldest
/// entries are evicted, and retained order is chronological.
#[test]
fn transition_log_is_memory_bounded() {
    let mut e = SourceEntry::new(
        SourceId(9),
        SourceAuthorityClass::StructuredObservation,
        ActivePrimary,
        3,
    );
    // Legal oscillation AP<->Degraded generates as many transitions as we like.
    for seq in 1..=5u64 {
        let target = if seq % 2 == 1 {
            Degraded
        } else {
            ActivePrimary
        };
        e.transition(target, seq).unwrap();
    }
    // Only the last 3 transitions (seq 3,4,5) are retained.
    assert_eq!(e.log_len(), 3);
    let hist = e.history();
    let seqs: Vec<u64> = hist.iter().map(|r| r.sequence).collect();
    assert_eq!(seqs, vec![3, 4, 5]);
    // Chronological content check.
    assert_eq!((hist[0].from, hist[0].to), (ActivePrimary, Degraded)); // seq 3
    assert_eq!((hist[1].from, hist[1].to), (Degraded, ActivePrimary)); // seq 4
    assert_eq!((hist[2].from, hist[2].to), (ActivePrimary, Degraded)); // seq 5
}

/// `is_live` / `is_terminal` classifications.
#[test]
fn state_classifications() {
    assert!(ActivePrimary.is_live());
    assert!(Transitional.is_live());
    assert!(Degraded.is_live());
    assert!(!SunsetPending.is_live());
    assert!(!Disabled.is_live());
    assert!(!Retired.is_live());
    assert!(Retired.is_terminal());
    assert!(!SunsetPending.is_terminal());
}
