// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'lockfree' component (leaf 'lf_mutex_baseline').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.

#[test]
fn prop_mutex_queue_fifo_bounded() {
    let q: MutexQueue<u64, 4> = MutexQueue::new();
    for i in 0..4 { assert!(q.push(i).is_ok()); }
    assert_eq!(q.push(99), Err(99));
    for i in 0..4 { assert_eq!(q.pop(), Some(i)); }
    assert_eq!(q.pop(), None);
}
