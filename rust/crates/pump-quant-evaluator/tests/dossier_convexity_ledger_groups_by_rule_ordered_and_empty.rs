// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'convexity_ledger' component (leaf 'groups_by_rule_ordered_and_empty').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_evaluator::convexity_ledger::*;

#[test]
fn groups_by_rule_ordered_and_empty() {
    let evs = vec![
        ConvexityEvent::test(RuleId::new(RuleKind::ExitPolicy, 9), true, -1_000, 0, 0),
        ConvexityEvent::test(RuleId::new(RuleKind::Veto, 1), true, -2_000, 0, 0),
        ConvexityEvent::test(RuleId::new(RuleKind::Veto, 1), true, -3_000, 0, 0),
    ];
    let led = build_ledger(&evs, 5_000);
    assert_eq!(led.len(), 2);
    // Veto sorts before ExitPolicy by RuleKind discriminant order.
    assert_eq!(led[0].rule.kind, RuleKind::Veto);
    assert_eq!(led[0].n, 2);
    assert_eq!(led[0].losses_avoided_bps, 5_000);
    assert_eq!(led[1].rule.kind, RuleKind::ExitPolicy);
    assert_eq!(led[1].n, 1);

    // Empty input -> empty ledger.
    assert!(build_ledger(&[], 1_000).is_empty());
}
