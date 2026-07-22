// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'convexity_ledger' component (leaf 'right_tail_and_participation_by_rank').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_evaluator::convexity_ledger::*;

#[test]
fn right_tail_and_participation_by_rank() {
    fn r(id: u64) -> RuleId {
        RuleId::new(RuleKind::ExitPolicy, id)
    }
    // Biggest counterfactual was suppressed -> right tail destroyed.
    let evs = vec![
        ConvexityEvent::test(r(1), true, 20_000, 0, 25_000),
        ConvexityEvent::test(r(1), false, 1_000, 900, 1_500),
    ];
    let l = build_ledger(&evs, 5_000)[0];
    assert_eq!(l.top10_total, 1);
    assert_eq!(l.top10_participated, 0);
    assert_eq!(l.right_tail_destroyed_bps, 20_000);
    assert_eq!(l.right_tail_preserved_bps, 0);

    // Biggest counterfactual was allowed -> right tail preserved, participated.
    let evs2 = vec![
        ConvexityEvent::test(r(2), false, 12_000, 11_000, 15_000),
        ConvexityEvent::test(r(2), true, -3_000, 0, 200),
    ];
    let l2 = build_ledger(&evs2, 5_000)[0];
    assert_eq!(l2.top10_total, 1);
    assert_eq!(l2.top10_participated, 1);
    assert_eq!(l2.right_tail_preserved_bps, 12_000);
    assert_eq!(l2.right_tail_destroyed_bps, 0);
}
