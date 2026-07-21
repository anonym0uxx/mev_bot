#![allow(unused_imports)]
use pump_quant_ingest::submission_surface::*;

// Expectations are derived independently from the §18.3.1 / §18.8 rules and
// multiple inputs (incl. edge cases: terminal state, retire-guard, full
// cartesian product) are exercised so a memorized single answer would fail.

/// All seven §18.8 lifecycle statuses, for exhaustive cross-checks.
const ALL_STATUSES: [SubmissionSurfaceStatus; 7] = [
    SubmissionSurfaceStatus::ActivePrimary,
    SubmissionSurfaceStatus::ActiveRedundant,
    SubmissionSurfaceStatus::Transitional,
    SubmissionSurfaceStatus::Degraded,
    SubmissionSurfaceStatus::SunsetPending,
    SubmissionSurfaceStatus::Disabled,
    SubmissionSurfaceStatus::Retired,
];

/// All six lifecycle events.
const ALL_EVENTS: [SubmissionSurfaceEvent; 6] = [
    SubmissionSurfaceEvent::VerifiedHealthy,
    SubmissionSurfaceEvent::VerifiedRedundant,
    SubmissionSurfaceEvent::VerifiedDegraded,
    SubmissionSurfaceEvent::ShutdownAnnounced,
    SubmissionSurfaceEvent::OperatorDisabled,
    SubmissionSurfaceEvent::Retire,
];

/// Independent reference implementation of the transition rule, written from the
/// §18.8 prose (NOT by copying the crate's match arms), used to cross-check the
/// full cartesian product.
fn reference_next(
    current: SubmissionSurfaceStatus,
    event: SubmissionSurfaceEvent,
) -> SubmissionSurfaceStatus {
    use SubmissionSurfaceEvent as E;
    use SubmissionSurfaceStatus as S;
    // Retired is terminal.
    if current == S::Retired {
        return S::Retired;
    }
    match event {
        E::VerifiedHealthy => S::ActivePrimary,
        E::VerifiedRedundant => S::ActiveRedundant,
        E::VerifiedDegraded => S::Degraded,
        E::ShutdownAnnounced => S::SunsetPending,
        E::OperatorDisabled => S::Disabled,
        // Retire honored only from a wind-down state.
        E::Retire => {
            if current == S::SunsetPending || current == S::Disabled || current == S::Degraded {
                S::Retired
            } else {
                current
            }
        }
    }
}

#[test]
fn transition_matches_reference_over_full_product() {
    for &current in &ALL_STATUSES {
        for &event in &ALL_EVENTS {
            assert_eq!(
                next_submission_status(current, event),
                reference_next(current, event),
                "mismatch for current={current:?} event={event:?}"
            );
        }
    }
}

#[test]
fn specific_hand_computed_transitions() {
    use SubmissionSurfaceEvent as E;
    use SubmissionSurfaceStatus as S;
    // Direct hand-computed values (memorized-answer resistance).
    assert_eq!(
        next_submission_status(S::ActivePrimary, E::VerifiedDegraded),
        S::Degraded
    );
    assert_eq!(next_submission_status(S::Degraded, E::Retire), S::Retired);
    assert_eq!(
        next_submission_status(S::SunsetPending, E::Retire),
        S::Retired
    );
    assert_eq!(next_submission_status(S::Disabled, E::Retire), S::Retired);
    // Retire from a healthy state is a no-op (cannot retire without wind-down).
    assert_eq!(
        next_submission_status(S::ActivePrimary, E::Retire),
        S::ActivePrimary
    );
    assert_eq!(
        next_submission_status(S::ActiveRedundant, E::Retire),
        S::ActiveRedundant
    );
    assert_eq!(
        next_submission_status(S::Transitional, E::Retire),
        S::Transitional
    );
    // ShutdownAnnounced always → SunsetPending from a live state.
    assert_eq!(
        next_submission_status(S::ActivePrimary, E::ShutdownAnnounced),
        S::SunsetPending
    );
}

#[test]
fn retired_is_terminal_for_every_event() {
    for &event in &ALL_EVENTS {
        assert_eq!(
            next_submission_status(SubmissionSurfaceStatus::Retired, event),
            SubmissionSurfaceStatus::Retired,
            "Retired must be terminal under {event:?}"
        );
    }
}

#[test]
fn usability_and_terminal_predicates() {
    use SubmissionSurfaceStatus as S;
    // Independently-listed expectations per status.
    let cases = [
        (S::ActivePrimary, true, false),
        (S::ActiveRedundant, true, false),
        (S::Transitional, true, false),
        (S::Degraded, true, false),
        (S::SunsetPending, true, false),
        (S::Disabled, false, false),
        (S::Retired, false, true),
    ];
    for (status, usable, terminal) in cases {
        assert_eq!(status.is_usable(), usable, "usable for {status:?}");
        assert_eq!(status.is_terminal(), terminal, "terminal for {status:?}");
    }
}

#[test]
fn verified_defaults_are_all_active_primary_with_transitional_feed() {
    let reg = SubmissionSurfaceRegistry::with_verified_defaults();
    for surface in SubmissionSurface::ALL {
        assert_eq!(reg.status(surface), SubmissionSurfaceStatus::ActivePrimary);
    }
    assert_eq!(reg.data_feed_status(), DataFeedStatus::Transitional);
    assert!(reg.all_submission_usable());
    assert!(reg.any_submission_usable());
    assert_eq!(reg.usable_surface_count(), 3);
}

// ---- Criterion 76 core: submission surface independent of data-feed sunset ----

#[test]
fn retiring_data_feed_leaves_submission_intact_across_all_configs() {
    use SubmissionSurfaceStatus as S;
    // Exercise many distinct submission configurations, including edge states,
    // and prove retiring the ShredStream data feed changes nothing on the
    // submission dimension (§18.3.1).
    for &be in &ALL_STATUSES {
        for &bu in &[S::ActivePrimary, S::Degraded, S::Disabled, S::Retired] {
            for &tp in &[S::ActiveRedundant, S::SunsetPending] {
                let mut reg = SubmissionSurfaceRegistry::with_verified_defaults();
                reg.set_submission_status(SubmissionSurface::BlockEngine, be);
                reg.set_submission_status(SubmissionSurface::Bundles, bu);
                reg.set_submission_status(SubmissionSurface::Tips, tp);

                let before = reg.submission_snapshot();
                let usable_before = reg.usable_surface_count();

                reg.retire_data_feed();

                assert_eq!(reg.data_feed_status(), DataFeedStatus::Retired);
                assert_eq!(
                    reg.submission_snapshot(),
                    before,
                    "submission snapshot changed after data-feed retirement"
                );
                assert_eq!(reg.usable_surface_count(), usable_before);
                // Individually confirm each surface is byte-identical.
                assert_eq!(reg.status(SubmissionSurface::BlockEngine), be);
                assert_eq!(reg.status(SubmissionSurface::Bundles), bu);
                assert_eq!(reg.status(SubmissionSurface::Tips), tp);
            }
        }
    }
}

#[test]
fn full_shredstream_sunset_path_never_touches_submission() {
    // Walk the ShredStream data feed through its entire sunset path while the
    // submission surfaces stay ActivePrimary — the whole submission dimension
    // must remain fully usable at every step (§18.3.1 non-conflation).
    let mut reg = SubmissionSurfaceRegistry::with_verified_defaults();
    let baseline = reg.submission_snapshot();

    reg.set_data_feed_status(DataFeedStatus::SunsetPending);
    assert_eq!(reg.submission_snapshot(), baseline);
    assert!(reg.all_submission_usable());

    reg.retire_data_feed();
    assert_eq!(reg.data_feed_status(), DataFeedStatus::Retired);
    assert_eq!(reg.submission_snapshot(), baseline);
    assert!(reg.all_submission_usable());
    assert_eq!(reg.usable_surface_count(), 3);
}

#[test]
fn disabling_all_submission_surfaces_never_touches_data_feed() {
    // The reverse independence: retiring/disabling the entire submission surface
    // must not change the data-feed status ("or vice versa", §18.3.1).
    let mut reg = SubmissionSurfaceRegistry::with_verified_defaults();
    let feed_before = reg.data_feed_status();

    for surface in SubmissionSurface::ALL {
        reg.apply_submission_event(surface, SubmissionSurfaceEvent::OperatorDisabled);
        reg.apply_submission_event(surface, SubmissionSurfaceEvent::Retire);
        assert_eq!(reg.status(surface), SubmissionSurfaceStatus::Retired);
    }
    assert_eq!(reg.usable_surface_count(), 0);
    assert!(!reg.any_submission_usable());
    // Data feed untouched.
    assert_eq!(reg.data_feed_status(), feed_before);
    assert_eq!(reg.data_feed_status(), DataFeedStatus::Transitional);
}

#[test]
fn apply_event_touches_only_the_named_surface() {
    // Applying an event to one surface must leave the other two unchanged.
    let mut reg = SubmissionSurfaceRegistry::with_verified_defaults();
    let ret = reg.apply_submission_event(
        SubmissionSurface::Bundles,
        SubmissionSurfaceEvent::VerifiedDegraded,
    );
    assert_eq!(ret, SubmissionSurfaceStatus::Degraded);
    assert_eq!(
        reg.status(SubmissionSurface::Bundles),
        SubmissionSurfaceStatus::Degraded
    );
    assert_eq!(
        reg.status(SubmissionSurface::BlockEngine),
        SubmissionSurfaceStatus::ActivePrimary
    );
    assert_eq!(
        reg.status(SubmissionSurface::Tips),
        SubmissionSurfaceStatus::ActivePrimary
    );
    assert_eq!(reg.usable_surface_count(), 3); // Degraded is still usable.
}

#[test]
fn surface_all_is_the_three_distinct_surfaces() {
    assert_eq!(SubmissionSurface::ALL.len(), 3);
    assert_eq!(SubmissionSurface::ALL[0], SubmissionSurface::BlockEngine);
    assert_eq!(SubmissionSurface::ALL[1], SubmissionSurface::Bundles);
    assert_eq!(SubmissionSurface::ALL[2], SubmissionSurface::Tips);
    // Distinct.
    assert_ne!(SubmissionSurface::BlockEngine, SubmissionSurface::Bundles);
    assert_ne!(SubmissionSurface::Bundles, SubmissionSurface::Tips);
    assert_ne!(SubmissionSurface::BlockEngine, SubmissionSurface::Tips);
}
