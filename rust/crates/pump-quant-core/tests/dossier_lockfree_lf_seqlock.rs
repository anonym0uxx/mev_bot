// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'lockfree' component (leaf 'lf_seqlock').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_core::lockfree::*;

#[test]
fn prop_seqlock_no_torn_reads() {
    #[derive(Copy, Clone, PartialEq, Debug)]
    struct Pair(u64, u64); // invariant: .1 == .0 * 2
    let cell = std::sync::Arc::new(SeqCell::new(Pair(0, 0)));
    let w = cell.clone();
    let writer = std::thread::spawn(move || {
        for i in 0..200_000u64 {
            w.write(Pair(i, i * 2));
        }
    });
    for _ in 0..200_000 {
        let p = cell.read();
        assert_eq!(p.1, p.0 * 2, "torn read observed");
    }
    writer.join().unwrap();
}
