// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'clock' component (leaf 'clk_replay_serves_in_order').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_clock::clock::*;

#[test]
fn clk_replay_serves_in_order_and_saturates() {
    let seq = vec![
        ClockReading::new(10, 1_000, 5),
        ClockReading::new(20, 2_000, 6),
        ClockReading::new(35, 3_500, 8),
    ];
    let clock = ReplayClock::new(seq.clone());

    // Before any advance: cursor at 0, serving seq[0].
    assert_eq!(clock.position(), 0);
    assert_eq!(clock.len(), 3);
    assert!(!clock.is_empty());
    assert!(!clock.is_exhausted());
    assert_eq!(clock.current(), ClockReading::new(10, 1_000, 5));
    assert_eq!(clock.monotonic_ns(), 10);
    assert_eq!(clock.wallclock_ns(), 1_000);
    assert_eq!(clock.current_slot(), 5);

    // advance() returns the newly-served reading and steps the cursor by one.
    assert_eq!(clock.advance(), ClockReading::new(20, 2_000, 6));
    assert_eq!(clock.position(), 1);
    assert_eq!(clock.advance(), ClockReading::new(35, 3_500, 8));
    assert_eq!(clock.position(), 2);
    assert!(!clock.is_exhausted());

    // Advancing past the end saturates on the final reading and latches exhaustion.
    let last = ClockReading::new(35, 3_500, 8);
    for _ in 0..5 {
        assert_eq!(clock.advance(), last);
    }
    assert!(clock.is_exhausted());
    assert_eq!(clock.position(), 2);
    assert_eq!(clock.current(), last);
    assert_eq!(clock.monotonic_ns(), 35);
}
