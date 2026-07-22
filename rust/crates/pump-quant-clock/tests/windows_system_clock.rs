//! Leaf: `WindowsSystemClock` placeholder — injected time, no syscall ([S]).

use pump_quant_clock::{Clock, ClockReading, WindowsSystemClock};

#[test]
fn returns_injected_reading() {
    let r = ClockReading::new(123, 456, 789);
    let c = WindowsSystemClock::with_injected(r);
    assert_eq!(c.monotonic_ns(), 123);
    assert_eq!(c.wallclock_ns(), 456);
    assert_eq!(c.current_slot(), 789);
}

#[test]
fn inject_updates_all_fields() {
    let c = WindowsSystemClock::with_injected(ClockReading::new(0, 0, 0));
    // Model the OS clock ticking forward, driven by a harness (no syscall).
    for i in 1..=5u64 {
        let r = ClockReading::new(i * 10, i * 100, i);
        c.inject(r);
        assert_eq!(c.monotonic_ns(), i * 10);
        assert_eq!(c.wallclock_ns(), i * 100);
        assert_eq!(c.current_slot(), i);
    }
}

/// Two placeholders injected with the same reading are indistinguishable —
/// there is no hidden nondeterministic state (proves the [S] placeholder makes
/// no real syscall).
#[test]
fn is_deterministic_given_injection() {
    let r = ClockReading::new(u64::MAX, 42, 9_000);
    let a = WindowsSystemClock::with_injected(r);
    let b = WindowsSystemClock::with_injected(r);
    assert_eq!(a.monotonic_ns(), b.monotonic_ns());
    assert_eq!(a.wallclock_ns(), b.wallclock_ns());
    assert_eq!(a.current_slot(), b.current_slot());
}

#[test]
fn usable_as_dyn_clock() {
    let c = WindowsSystemClock::with_injected(ClockReading::new(7, 8, 9));
    let dynref: &dyn Clock = &c;
    assert_eq!(dynref.wallclock_ns(), 8);
}
