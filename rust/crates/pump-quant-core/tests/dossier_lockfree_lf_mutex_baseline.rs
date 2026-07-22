// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'lockfree' component (leaf 'lf_mutex_baseline').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_core::lockfree::*;

#[test]
fn prop_mutex_queue_fifo_bounded() {
    let q: MutexQueue<u64, 4> = MutexQueue::new();
    for i in 0..4 {
        assert!(q.push(i).is_ok());
    }
    assert_eq!(q.push(99), Err(99));
    for i in 0..4 {
        assert_eq!(q.pop(), Some(i));
    }
    assert_eq!(q.pop(), None);
}
