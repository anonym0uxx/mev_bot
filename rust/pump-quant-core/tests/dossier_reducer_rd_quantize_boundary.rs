// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'reducer' component (leaf 'rd_quantize_boundary').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.

#[test]
fn prop_quantize_contract() {
    assert_eq!(quantize_feature(1.25, 100), Some(125));
    assert_eq!(quantize_feature(-1.255, 1000), Some(-1255));
    assert_eq!(quantize_feature(f64::NAN, 100), None);
    assert_eq!(quantize_feature(f64::INFINITY, 100), None);
    assert_eq!(quantize_feature(0.5, 1), Some(1)); // half away from zero
}
