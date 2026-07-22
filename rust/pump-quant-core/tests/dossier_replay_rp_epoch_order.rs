// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'replay' component (leaf 'rp_epoch_order').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.

#[test]
fn prop_epoch_merge_total_order() {
    let f = |e, s| FrameMeta::test(e, s);
    let frames = vec![f(2, 0), f(1, 5), f(1, 4), f(2, 1)];
    let order = epoch_merge(&frames).unwrap();
    let seq: Vec<_> = order.iter().map(|&i| (frames[i].epoch, frames[i].seq)).collect();
    assert_eq!(seq, vec![(1, 4), (1, 5), (2, 0), (2, 1)]);
    assert!(epoch_merge(&[f(1, 1), f(1, 1)]).is_err());
}
