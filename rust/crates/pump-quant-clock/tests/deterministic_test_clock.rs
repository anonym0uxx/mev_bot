//! Leaf: `DeterministicTestClock` — manual drive + saturating overflow (§22).

use pump_quant_clock::{Clock, DeterministicTestClock};

struct Lcg(u64);
impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed)
    }
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
}

#[test]
fn starts_at_injected_values() {
    let c = DeterministicTestClock::new(11, 22, 33);
    assert_eq!(c.monotonic_ns(), 11);
    assert_eq!(c.wallclock_ns(), 22);
    assert_eq!(c.current_slot(), 33);
}

#[test]
fn zeroed_and_default_are_all_zero() {
    for c in [
        DeterministicTestClock::zeroed(),
        DeterministicTestClock::default(),
    ] {
        assert_eq!(c.monotonic_ns(), 0);
        assert_eq!(c.wallclock_ns(), 0);
        assert_eq!(c.current_slot(), 0);
    }
}

#[test]
fn set_overwrites_each_field() {
    let c = DeterministicTestClock::zeroed();
    c.set_monotonic_ns(500);
    c.set_wallclock_ns(600);
    c.set_current_slot(7);
    assert_eq!(c.monotonic_ns(), 500);
    assert_eq!(c.wallclock_ns(), 600);
    assert_eq!(c.current_slot(), 7);
}

/// Property: advancing accumulates exactly (independently computed running
/// sums), across many random deltas and multiple fields.
#[test]
fn advance_accumulates_exactly() {
    for seed in [3u64, 99, 12345, u64::MAX / 7] {
        let mut rng = Lcg::new(seed);
        let c = DeterministicTestClock::zeroed();
        let (mut mono, mut wall, mut slot) = (0u64, 0u64, 0u64);
        for _ in 0..200 {
            // Keep deltas small so we test accumulation, not saturation, here.
            let dm = rng.next() % 1_000_000;
            let dw = rng.next() % 1_000_000;
            let ds = rng.next() % 1_000;
            let rm = c.advance_monotonic(dm);
            let rw = c.advance_wallclock(dw);
            let rs = c.advance_slot(ds);
            mono += dm;
            wall += dw;
            slot += ds;
            assert_eq!(rm, mono);
            assert_eq!(rw, wall);
            assert_eq!(rs, slot);
            assert_eq!(c.monotonic_ns(), mono);
            assert_eq!(c.wallclock_ns(), wall);
            assert_eq!(c.current_slot(), slot);
        }
    }
}

/// Edge case: advancing saturates at u64::MAX and never wraps (§22 explicit
/// overflow contract). Independently computed expectation = u64::MAX.
#[test]
fn advance_saturates_at_max() {
    let c = DeterministicTestClock::new(u64::MAX - 5, u64::MAX - 5, u64::MAX - 5);
    assert_eq!(c.advance_monotonic(10), u64::MAX);
    assert_eq!(c.advance_wallclock(10), u64::MAX);
    assert_eq!(c.advance_slot(10), u64::MAX);
    // Further advances stay pinned.
    assert_eq!(c.advance_monotonic(1), u64::MAX);
    assert_eq!(c.monotonic_ns(), u64::MAX);

    // Boundary: exact-hit and zero delta.
    let d = DeterministicTestClock::new(u64::MAX, 0, u64::MAX - 1);
    assert_eq!(d.advance_monotonic(1), u64::MAX);
    assert_eq!(d.advance_wallclock(0), 0);
    assert_eq!(d.advance_slot(1), u64::MAX);
}

/// The trait object seam works: strategy code takes `&dyn Clock`.
#[test]
fn usable_as_dyn_clock() {
    let c = DeterministicTestClock::new(1, 2, 3);
    let dynref: &dyn Clock = &c;
    assert_eq!(dynref.monotonic_ns(), 1);
    c.advance_slot(4);
    assert_eq!(dynref.current_slot(), 7);
}
