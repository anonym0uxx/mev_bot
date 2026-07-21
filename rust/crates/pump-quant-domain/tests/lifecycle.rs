//! Tests for the candidate lifecycle state machine: terminal/failure/position
//! predicates and legal-transition rules, checked against an independently
//! enumerated set of expected legal edges.

use pump_quant_domain::lifecycle::CandidateLifecycleState as S;

#[test]
fn predicates_partition_correctly() {
    for s in S::ALL {
        // Only Archived is fully terminal.
        assert_eq!(s.is_terminal(), s == S::Archived, "terminal {s}");
        // Failure states.
        assert_eq!(
            s.is_failed(),
            matches!(s, S::Rejected | S::PermanentlyInvalidated),
            "failed {s}"
        );
        // Holds a position.
        assert_eq!(
            s.holds_position(),
            matches!(s, S::Entered | S::Managing),
            "holds {s}"
        );
        // Terminal and failed are disjoint (Archived is terminal, not failed).
        assert!(!(s.is_terminal() && s.is_failed()));
    }
    assert_eq!(S::ALL.len(), 10);
}

/// Independently-enumerated set of every legal directed edge, transcribed from
/// the documented state machine (not from the implementation).
fn expected_legal_edges() -> Vec<(S, S)> {
    let live = [
        S::Discovered,
        S::Observing,
        S::Evaluating,
        S::EntryEligible,
        S::Entered,
        S::Managing,
    ];
    let mut edges = vec![
        // Forward happy path.
        (S::Discovered, S::Observing),
        (S::Observing, S::Evaluating),
        (S::Evaluating, S::EntryEligible),
        (S::EntryEligible, S::Entered),
        (S::Entered, S::Managing),
        (S::Managing, S::Exited),
        (S::Exited, S::Archived),
        // Re-evaluation fallbacks.
        (S::EntryEligible, S::Evaluating),
        (S::EntryEligible, S::Observing),
        (S::Evaluating, S::Observing),
        // Failure states archive.
        (S::Rejected, S::Archived),
        (S::PermanentlyInvalidated, S::Archived),
    ];
    // Failure escape hatch from every live pre-exit state.
    for s in live {
        edges.push((s, S::Rejected));
        edges.push((s, S::PermanentlyInvalidated));
    }
    edges
}

#[test]
fn transitions_match_independent_edge_set() {
    let legal: std::collections::BTreeSet<(u8, u8)> = expected_legal_edges()
        .into_iter()
        .map(|(a, b)| (a as u8, b as u8))
        .collect();

    // Check every ordered pair in the full 10x10 grid.
    for from in S::ALL {
        for to in S::ALL {
            let want = legal.contains(&(from as u8, to as u8));
            assert_eq!(
                from.can_transition_to(to),
                want,
                "edge {from} -> {to} expected legal={want}"
            );
        }
    }
}

#[test]
fn terminal_and_failure_transition_constraints() {
    // Archived has no successors at all.
    for to in S::ALL {
        assert!(!S::Archived.can_transition_to(to), "archived -> {to}");
    }
    // A failure state may ONLY go to Archived.
    for from in [S::Rejected, S::PermanentlyInvalidated] {
        for to in S::ALL {
            assert_eq!(
                from.can_transition_to(to),
                to == S::Archived,
                "{from} -> {to}"
            );
        }
    }
    // Exited cannot be "failed" retroactively — only archived.
    assert!(!S::Exited.can_transition_to(S::Rejected));
    assert!(!S::Exited.can_transition_to(S::PermanentlyInvalidated));
    assert!(S::Exited.can_transition_to(S::Archived));
    // No self-loops.
    for s in S::ALL {
        assert!(!s.can_transition_to(s), "self loop {s}");
    }
}

#[test]
fn a_full_happy_path_is_walkable() {
    let path = [
        S::Discovered,
        S::Observing,
        S::Evaluating,
        S::EntryEligible,
        S::Entered,
        S::Managing,
        S::Exited,
        S::Archived,
    ];
    for w in path.windows(2) {
        assert!(w[0].can_transition_to(w[1]), "{} -> {}", w[0], w[1]);
    }
    assert!(path.last().unwrap().is_terminal());
}
