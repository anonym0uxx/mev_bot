// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'fixedpoint' component (leaf 'cp_amount_out').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.

#[test]
fn prop_amount_out_golden() {
    // golden vector from a real pump.fun swap (illustrative values)
    let out = amount_out(1_000_000_000, 2_000_000_000, 100_000_000, 1, 100).unwrap();
    assert!(out > 0 && out < 2_000_000_000);
    assert_eq!(amount_out(0, 10, 5, 1, 100), None);
    assert_eq!(amount_out(10, 10, 0, 1, 100), None);
}
