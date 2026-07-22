// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'evaluator_stats' component (leaf 'ev_topk_excision').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.

#[test]
fn prop_excision_fragility_detected() {
    let t = |id, v: i128| (TradeId::test(id), v);
    // Kamat-shaped book: +117 total, top-3 carry it
    let book = vec![t(1, 60), t(2, 40), t(3, 30), t(4, -5), t(5, -8)];
    let ex = topk_excision(&book, &[1, 3]);
    assert_eq!(ex[0].net_without_topk, 57);
    assert!(!ex[0].flipped_negative);
    assert_eq!(ex[1].net_without_topk, -13);
    assert!(ex[1].flipped_negative); // the lottery ticket exposed
}
