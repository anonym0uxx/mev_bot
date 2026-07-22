// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'safety_integrity' component (leaf 'si_signal_scoring_only').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(
    unused_imports,
    dead_code,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_strategy::safety_integrity::*;

#[test]
fn inverting_lowers_and_single_application() {
    let base = ScoringInputs {
        base_score: 500,
        smart_money_applied: false,
        smart_money_delta: 0,
    };
    let inv = apply_smart_money(base.clone(), SmartMoneyClass::Inverting);
    assert!(inv.base_score < base.base_score);
    assert!(inv.smart_money_applied);
    // applying again is a no-op (exactly once)
    let inv2 = apply_smart_money(inv.clone(), SmartMoneyClass::Inverting);
    assert_eq!(inv2, inv);
}
#[test]
fn qualified_raises_unqualified_neutral() {
    let base = ScoringInputs {
        base_score: 100,
        smart_money_applied: false,
        smart_money_delta: 0,
    };
    let q = apply_smart_money(base.clone(), SmartMoneyClass::Qualified);
    assert!(q.base_score > base.base_score);
    let base2 = ScoringInputs {
        base_score: 100,
        smart_money_applied: false,
        smart_money_delta: 0,
    };
    let u = apply_smart_money(base2, SmartMoneyClass::Unqualified);
    assert_eq!(u.base_score, 100);
}
