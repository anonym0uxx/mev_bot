// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'lockfree' component (leaf 'lf_spsc_ring').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_core::lockfree::*;

#[test]
fn prop_spsc_exactly_once_in_order() {
    let (mut p, mut c) = Spsc::<u64, 1024>::new().split();
    let h = std::thread::spawn(move || {
        let mut got = Vec::new();
        while got.len() < 100_000 {
            if let Some(v) = c.pop() {
                got.push(v);
            } else {
                std::hint::spin_loop();
            }
        }
        got
    });
    let mut i = 0u64;
    while i < 100_000 {
        if p.push(i).is_ok() {
            i += 1;
        } else {
            std::hint::spin_loop();
        }
    }
    let got = h.join().unwrap();
    assert_eq!(got.len(), 100_000);
    assert!(got.windows(2).all(|w| w[1] == w[0] + 1)); // in order, exactly once
}
