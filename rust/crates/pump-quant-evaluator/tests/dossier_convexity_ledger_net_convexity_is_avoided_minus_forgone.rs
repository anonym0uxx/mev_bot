// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'convexity_ledger' component (leaf 'net_convexity_is_avoided_minus_forgone').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_evaluator::convexity_ledger::*;

#[test]
fn net_convexity_is_avoided_minus_forgone() {
    fn r(id: u64) -> RuleId {
        RuleId::new(RuleKind::Veto, id)
    }
    // One loser avoided (+6000 saved), one winner forgone (7000 cost).
    let evs = vec![
        ConvexityEvent::test(r(1), true, -6_000, 0, 0),
        ConvexityEvent::test(r(1), true, 7_000, 0, 0),
    ];
    let l = build_ledger(&evs, 100_000)[0];
    assert_eq!(l.losses_avoided_bps, 6_000);
    assert_eq!(l.net_forgone_bps, 7_000);
    assert_eq!(l.net_convexity_bps(), -1_000);

    // Edge: only losers avoided -> strictly positive net convexity.
    let evs2 = vec![ConvexityEvent::test(r(2), true, -2_000, 0, 0)];
    let l2 = build_ledger(&evs2, 100_000)[0];
    assert_eq!(l2.net_convexity_bps(), 2_000);
}
