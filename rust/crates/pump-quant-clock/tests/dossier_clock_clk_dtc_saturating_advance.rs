// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'clock' component (leaf 'clk_dtc_saturating_advance').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(
    unused_imports,
    dead_code,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_clock::clock::*;

#[test]
fn clk_dtc_accumulates_then_saturates() {
    let c = DeterministicTestClock::new(100, 200, 3);
    assert_eq!(c.monotonic_ns(), 100);
    assert_eq!(c.wallclock_ns(), 200);
    assert_eq!(c.current_slot(), 3);

    // advance returns the new running total and mutates in place.
    assert_eq!(c.advance_monotonic(25), 125);
    assert_eq!(c.advance_wallclock(50), 250);
    assert_eq!(c.advance_slot(4), 7);
    assert_eq!(c.monotonic_ns(), 125);
    assert_eq!(c.wallclock_ns(), 250);
    assert_eq!(c.current_slot(), 7);

    // set overwrites the field outright.
    c.set_monotonic_ns(1_000);
    assert_eq!(c.monotonic_ns(), 1_000);

    // Overflow contract: saturate at u64::MAX, never wrap (§22).
    let d = DeterministicTestClock::new(u64::MAX - 5, u64::MAX, u64::MAX - 1);
    assert_eq!(d.advance_monotonic(10), u64::MAX);
    assert_eq!(d.advance_wallclock(1), u64::MAX);
    assert_eq!(d.advance_slot(1), u64::MAX);
    // Pinned after saturation.
    assert_eq!(d.advance_monotonic(999), u64::MAX);
    assert_eq!(d.monotonic_ns(), u64::MAX);

    // Zero delta is a no-op read.
    let z = DeterministicTestClock::zeroed();
    assert_eq!(z.advance_slot(0), 0);
    assert_eq!(z.current_slot(), 0);

    // Usable behind the trait-object seam.
    let dynref: &dyn Clock = &c;
    assert_eq!(dynref.monotonic_ns(), 1_000);
}
