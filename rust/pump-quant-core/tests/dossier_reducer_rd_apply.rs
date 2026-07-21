// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'reducer' component (leaf 'rd_apply').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.

#[test]
fn prop_apply_pure_and_deterministic() {
    let s0 = MarketState::test();
    let ev = CanonEvent::test(10, 0, 0, 1);
    let a = apply(&s0, &ev);
    let b = apply(&s0, &ev);
    assert_eq!(a, b);
    assert_eq!(s0, MarketState::test()); // input untouched
}
