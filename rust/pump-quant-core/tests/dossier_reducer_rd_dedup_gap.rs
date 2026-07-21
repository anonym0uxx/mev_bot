// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'reducer' component (leaf 'rd_dedup_gap').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.

#[test]
fn prop_dedup_and_gaps() {
    let mut s = SeqState::new();
    let k = |slot| EventKey::test(slot, 0, 0);
    assert!(matches!(admit(&mut s, k(1)), Admit::Apply));
    assert!(matches!(admit(&mut s, k(1)), Admit::Duplicate));
    assert!(matches!(admit(&mut s, k(5)), Admit::GapThenApply(3)));
    assert!(matches!(admit(&mut s, k(0)), Admit::Regression));
}
