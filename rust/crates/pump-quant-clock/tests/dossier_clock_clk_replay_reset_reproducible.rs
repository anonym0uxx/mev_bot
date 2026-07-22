// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'clock' component (leaf 'clk_replay_reset_reproducible').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_clock::clock::*;

#[test]
fn clk_replay_reset_and_reproducible() {
    let seq = vec![
        ClockReading::new(1, 2, 3),
        ClockReading::new(4, 5, 6),
        ClockReading::new(7, 8, 9),
    ];

    // Two independent clocks over the same sealed sequence, advanced in
    // lockstep past the end, must produce byte-identical streams (§19).
    let a = ReplayClock::new(seq.clone());
    let b = ReplayClock::new(seq.clone());
    let mut expected_idx = 0usize;
    for step in 0..(seq.len() + 3) {
        if step > 0 {
            a.advance();
            b.advance();
            expected_idx = (expected_idx + 1).min(seq.len() - 1);
        }
        assert_eq!(a.current(), seq[expected_idx]);
        assert_eq!(a.current(), b.current());
        assert_eq!(a.position(), b.position());
    }

    // After exhaustion, reset returns the cursor to the first reading and
    // clears exhaustion.
    assert!(a.is_exhausted());
    a.reset();
    assert_eq!(a.position(), 0);
    assert!(!a.is_exhausted());
    assert_eq!(a.current(), ClockReading::new(1, 2, 3));
}

#[test]
#[should_panic(expected = "non-empty")]
fn clk_replay_rejects_empty_sequence() {
    let _ = ReplayClock::new(Vec::new());
}
