// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'evaluator_stats' component (leaf 'ev_inactivity_label').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_evaluator::evaluator_stats::*;

#[test]
fn prop_inactivity_label_golden() {
    // swaps at 0, 10, 300; window ends 1000; delta_t = 200
    // gaps: 0->10 (10), 10->300 (290 >= 200: FIRST qualifying gap), 300->1000 (700)
    let l = label_terminal(&[0, 10, 300], 1_000, 200);
    assert!(l.dead);
    assert_eq!(l.died_at_ns, Some(10)); // first qualifying gap starts after the swap at 10
    assert_eq!(l.params_version, (200, 1_000));
    let alive = label_terminal(&[0, 100, 200, 300, 900], 1_000, 700);
    assert!(!alive.dead);
    let tail_dead = label_terminal(&[0, 50], 1_000, 500);
    assert_eq!(tail_dead.died_at_ns, Some(50)); // trailing (last, window_end) gap qualifies
}
