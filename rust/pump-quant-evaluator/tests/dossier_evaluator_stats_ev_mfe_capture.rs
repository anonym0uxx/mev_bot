// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'evaluator_stats' component (leaf 'ev_mfe_capture').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.

#[test]
fn prop_capture_ratio_golden_and_screening() {
    let r = |mfe, mae, real, scr| ExcursionRow::test(ArchetypeKey::test(), mfe, mae, real, scr);
    let rows = vec![r(400, 100, 200, true), r(600, 300, 100, true), r(10_000, 0, 10_000, false)];
    let rep = mfe_capture(&rows, ArchetypeKey::test());
    assert_eq!(rep.n, 2);
    assert_eq!(rep.excluded_unscreened, 1);         // phantom excursion excluded
    assert_eq!(rep.capture_bps_of_mfe, (300u32 * 10_000) / 1_000); // 3000 bps = 30%
}
