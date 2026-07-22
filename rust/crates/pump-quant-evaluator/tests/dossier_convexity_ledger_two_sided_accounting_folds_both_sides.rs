// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'convexity_ledger' component (leaf 'two_sided_accounting_folds_both_sides').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_evaluator::convexity_ledger::*;

#[test]
fn two_sided_accounting_folds_both_sides() {
    fn r(id: u64) -> RuleId {
        RuleId::new(RuleKind::Veto, id)
    }
    let evs = vec![
        ConvexityEvent::test(r(1), true, -5_000, 0, 100),
        ConvexityEvent::test(r(1), true, 8_000, 0, 9_000),
        ConvexityEvent::test(r(1), false, 3_000, 2_500, 4_000),
    ];
    let led = build_ledger(&evs, 5_000);
    assert_eq!(led.len(), 1);
    let l = led[0];
    assert_eq!(l.n, 3);
    assert_eq!(l.suppressed_n, 2);
    assert_eq!(l.losses_avoided_bps, 5_000);
    assert_eq!(l.net_forgone_bps, 8_000);
    assert_eq!(l.runners_missed, 1);
    assert_eq!(l.runners_missed_bps, 8_000);
    assert_eq!(l.mfe_killed_bps, 9_100);
    assert_eq!(l.mfe_captured_bps, 2_500);

    // Edge: counterfactual just below threshold is forgone but NOT a runner.
    let evs2 = vec![ConvexityEvent::test(r(2), true, 4_999, 0, 0)];
    let l2 = build_ledger(&evs2, 5_000)[0];
    assert_eq!(l2.runners_missed, 0);
    assert_eq!(l2.net_forgone_bps, 4_999);
    assert_eq!(l2.losses_avoided_bps, 0);
}
