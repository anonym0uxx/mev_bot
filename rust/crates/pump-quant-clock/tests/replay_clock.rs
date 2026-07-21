//! Leaf: `ReplayClock` — determinism of sealed-sequence replay (§19 REPLAY).

use pump_quant_clock::{Clock, ClockReading, ReplayClock};

/// Deterministic LCG for generating many test sequences without an RNG crate.
/// Fixed constants (Numerical Recipes) + fixed seed → fully reproducible.
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

fn make_sequence(seed: u64, len: usize) -> Vec<ClockReading> {
    let mut rng = Lcg::new(seed);
    (0..len)
        .map(|_| ClockReading::new(rng.next(), rng.next(), rng.next()))
        .collect()
}

#[test]
fn serves_recorded_readings_in_order() {
    let seq = vec![
        ClockReading::new(10, 1_000, 5),
        ClockReading::new(20, 2_000, 6),
        ClockReading::new(35, 3_500, 8),
    ];
    let clock = ReplayClock::new(seq.clone());

    // Position 0 is served before any advance. Expectation computed directly
    // from the sealed sequence.
    assert_eq!(clock.monotonic_ns(), seq[0].monotonic_ns);
    assert_eq!(clock.wallclock_ns(), seq[0].wallclock_ns);
    assert_eq!(clock.current_slot(), seq[0].current_slot);

    for expected in &seq[1..] {
        let served = clock.advance();
        assert_eq!(served, *expected);
        assert_eq!(clock.monotonic_ns(), expected.monotonic_ns);
        assert_eq!(clock.wallclock_ns(), expected.wallclock_ns);
        assert_eq!(clock.current_slot(), expected.current_slot);
    }
}

#[test]
fn exhaustion_saturates_at_last_reading() {
    let seq = vec![ClockReading::new(1, 2, 3), ClockReading::new(4, 5, 6)];
    let last = *seq.last().unwrap();
    let clock = ReplayClock::new(seq);

    assert!(!clock.is_exhausted());
    clock.advance(); // -> index 1 (last)
    assert!(!clock.is_exhausted());
    // Advancing past the end saturates and latches is_exhausted.
    for _ in 0..10 {
        let served = clock.advance();
        assert_eq!(served, last);
    }
    assert!(clock.is_exhausted());
    assert_eq!(clock.current(), last);
    assert_eq!(clock.position(), clock.len() - 1);
}

#[test]
fn reset_returns_to_first_reading() {
    let seq = make_sequence(0xDEAD, 6);
    let first = seq[0];
    let clock = ReplayClock::new(seq);
    for _ in 0..3 {
        clock.advance();
    }
    clock.reset();
    assert_eq!(clock.position(), 0);
    assert!(!clock.is_exhausted());
    assert_eq!(clock.current(), first);
}

/// Property: two independent replay clocks over the *same* sealed sequence,
/// advanced in lockstep, produce byte-identical reading streams — for many
/// seeds and lengths including the length-1 edge case (§19 reproducibility).
#[test]
fn replay_is_reproducible_across_clocks() {
    for seed in [1u64, 2, 7, 42, 1000, u64::MAX / 3] {
        for len in [1usize, 2, 5, 17, 64] {
            let seq = make_sequence(seed, len);
            let a = ReplayClock::new(seq.clone());
            let b = ReplayClock::new(seq.clone());

            // Drive both past the end to also exercise saturation parity.
            let mut expected_idx = 0usize;
            for step in 0..(len + 3) {
                if step > 0 {
                    a.advance();
                    b.advance();
                    expected_idx = (expected_idx + 1).min(len - 1);
                }
                // Independently computed expectation: the sealed reading at the
                // clamped index.
                let expected = seq[expected_idx];
                assert_eq!(a.current(), expected, "seed={seed} len={len} step={step}");
                assert_eq!(a.current(), b.current());
                assert_eq!(a.monotonic_ns(), b.monotonic_ns());
                assert_eq!(a.wallclock_ns(), b.wallclock_ns());
                assert_eq!(a.current_slot(), b.current_slot());
            }
        }
    }
}

#[test]
#[should_panic(expected = "non-empty")]
fn empty_sequence_is_rejected() {
    let _ = ReplayClock::new(Vec::new());
}
