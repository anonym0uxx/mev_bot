// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'lifecycle' component (leaf 'cls_transition_authority').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_domain::lifecycle::*;

#[test]
fn cls_transition_authority_matches_edge_set() {
    use CandidateLifecycleState::*;
    let live = [
        Discovered,
        Observing,
        Evaluating,
        EntryEligible,
        Entered,
        Managing,
    ];
    let mut legal: std::collections::BTreeSet<(u8, u8)> = std::collections::BTreeSet::new();
    let base = [
        (Discovered, Observing),
        (Observing, Evaluating),
        (Evaluating, EntryEligible),
        (EntryEligible, Entered),
        (Entered, Managing),
        (Managing, Exited),
        (Exited, Archived),
        (EntryEligible, Evaluating),
        (EntryEligible, Observing),
        (Evaluating, Observing),
        (Rejected, Archived),
        (PermanentlyInvalidated, Archived),
    ];
    for (a, b) in base {
        legal.insert((a as u8, b as u8));
    }
    for s in live {
        legal.insert((s as u8, Rejected as u8));
        legal.insert((s as u8, PermanentlyInvalidated as u8));
    }
    // Exactly 24 legal directed edges.
    assert_eq!(legal.len(), 24);

    let mut count = 0u32;
    for from in CandidateLifecycleState::ALL {
        for to in CandidateLifecycleState::ALL {
            let want = legal.contains(&(from as u8, to as u8));
            assert_eq!(from.can_transition_to(to), want, "{from} -> {to}");
            if from.can_transition_to(to) {
                count += 1;
            }
        }
    }
    assert_eq!(count, 24);
}
