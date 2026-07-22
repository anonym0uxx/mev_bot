//! The [`Clock`] trait and its three implementations.
//!
//! Responsibility: the determinism seam of constitution §19 / §22. Every read
//! of "now" in the decision path is routed through a `&dyn Clock`, so the
//! strategy core is a pure reducer whose only source of time is injected. This
//! module provides:
//!
//! * [`ReplayClock`] — replays a fixed, pre-recorded sequence of clock
//!   readings sealed from a journal (§19 REPLAY mode).
//! * [`DeterministicTestClock`] — a manually driven clock for unit/property
//!   tests and counterfactual runs.
//! * [`WindowsSystemClock`] — a *placeholder* for the LIVE-mode clock. On the
//!   real deployment server it would be `QueryPerformanceCounter`-backed
//!   (§9.4 "Clock handling"); live streaming/submission is out of scope for
//!   this build ([S]), so here it merely holds injected values and performs no
//!   syscall. This keeps the type present and `Clock`-shaped without pulling a
//!   nondeterministic wall clock into the codebase.
//!
//! All state uses `AtomicU64` / `AtomicUsize` so the implementations are
//! `Send + Sync` (required by the trait bound) while still exposing `&self`
//! reads — no floating point, no locks (§22).

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// A single sample of the three clock quantities the system tracks.
///
/// Responsibility: bundle the exact readings §19 requires a `Clock` to expose,
/// so a `ReplayClock` can replay them as an atomic unit. All fields are
/// integer (§22: no floating point in outcome logic).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClockReading {
    /// Monotonic nanoseconds — a non-decreasing counter used for *latency
    /// math* only (§9.4: never wall-clock for latency). Has no relationship to
    /// calendar time.
    pub monotonic_ns: u64,
    /// Wall-clock nanoseconds since the Unix epoch — used for *external
    /// correlation* only, never for latency (§9.4).
    pub wallclock_ns: u64,
    /// The Solana slot the system believes is current at this reading.
    pub current_slot: u64,
}

impl ClockReading {
    /// Construct a reading from its three integer components.
    ///
    /// Responsibility: trivial constructor kept `pub` because tests build
    /// injected sequences from it (§22: everything tests touch is `pub`).
    #[must_use]
    pub const fn new(monotonic_ns: u64, wallclock_ns: u64, current_slot: u64) -> Self {
        Self {
            monotonic_ns,
            wallclock_ns,
            current_slot,
        }
    }
}

/// The determinism seam: the only interface through which decision-path code
/// reads time (constitution §19).
///
/// Responsibility: abstract the three time quantities the system needs so that
/// the same `StrategyRuntime` runs unchanged in LIVE, SHADOW, and REPLAY
/// modes, swapping only the `Clock` implementation (§22). Implementations must
/// be `Send + Sync` so a clock can be shared across the ingestion / execution
/// boundary, but the strategy core itself is single-threaded.
///
/// No implementation in this crate calls a real OS clock; the "system" clock
/// is an injected placeholder (see [`WindowsSystemClock`]).
pub trait Clock: Send + Sync {
    /// Monotonic nanoseconds — non-decreasing; for latency math only (§9.4).
    fn monotonic_ns(&self) -> u64;
    /// Wall-clock nanoseconds since the Unix epoch — for correlation only (§9.4).
    fn wallclock_ns(&self) -> u64;
    /// The current Solana slot as this clock understands it (§19).
    fn current_slot(&self) -> u64;
}

/// Replays a fixed, pre-recorded sequence of [`ClockReading`]s (§19 REPLAY
/// mode: "sealed journals → ReplayClock → same StrategyRuntime").
///
/// Responsibility: hand out exactly the readings that were recorded, in order,
/// under explicit caller-driven advancement — never inventing time. This makes
/// replay reproducible from the sealed journal alone.
///
/// Memory-bounded: it owns the injected `Vec<ClockReading>` and a cursor; it
/// never grows. Advancing past the end saturates at the final reading (an
/// exhausted replay repeats its last sealed sample rather than reading a live
/// clock — an explicit overflow contract per §22), and the saturation is
/// observable via [`ReplayClock::is_exhausted`].
#[derive(Debug)]
pub struct ReplayClock {
    sequence: Vec<ClockReading>,
    cursor: AtomicUsize,
}

impl ReplayClock {
    /// Build a replay clock from a sealed, non-empty sequence of readings.
    ///
    /// Responsibility: own the recorded sequence and start the cursor at the
    /// first sample.
    ///
    /// # Panics
    /// Panics if `sequence` is empty — a `Clock` must always be able to answer
    /// a time query, and an empty journal is a programming error at
    /// construction, not a runtime condition to paper over (§22: fail loudly,
    /// no placeholder-panic in the *decision* path — this is a constructor
    /// precondition).
    #[must_use]
    pub fn new(sequence: Vec<ClockReading>) -> Self {
        assert!(
            !sequence.is_empty(),
            "ReplayClock requires a non-empty sealed sequence"
        );
        Self {
            sequence,
            cursor: AtomicUsize::new(0),
        }
    }

    /// Index of the reading currently being served (clamped to the last).
    ///
    /// Responsibility: expose the cursor for step-mode replay and tests.
    #[must_use]
    pub fn position(&self) -> usize {
        self.clamped_index()
    }

    /// Number of readings in the sealed sequence.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sequence.len()
    }

    /// Whether the sealed sequence is empty. Always `false` after
    /// construction (the constructor rejects empty input) but provided so
    /// clippy's `len`-without-`is_empty` lint is satisfied and callers can be
    /// explicit.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sequence.is_empty()
    }

    /// Whether the cursor has advanced past the last sealed reading and is now
    /// saturating on the final sample.
    ///
    /// Responsibility: make the end-of-journal overflow contract observable so
    /// a replay harness can stop rather than silently loop on stale time.
    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.cursor.load(Ordering::SeqCst) >= self.sequence.len()
    }

    /// Advance to the next recorded reading (§19 step-by-observation mode).
    ///
    /// Responsibility: move the cursor forward by exactly one. Saturates at
    /// `len()` — advancing an exhausted clock is a no-op that keeps serving the
    /// final reading (explicit overflow-by-contract, §22). Returns the reading
    /// that will now be served.
    pub fn advance(&self) -> ClockReading {
        let raw = self.cursor.load(Ordering::SeqCst);
        if raw < self.sequence.len() {
            // Saturating step: never exceed len() so `is_exhausted` latches.
            self.cursor.store(raw.saturating_add(1), Ordering::SeqCst);
        }
        self.current()
    }

    /// The reading currently being served, without advancing.
    ///
    /// Responsibility: read the sample at the clamped cursor. The clamp means
    /// an exhausted clock returns the last sealed reading.
    #[must_use]
    pub fn current(&self) -> ClockReading {
        self.sequence[self.clamped_index()]
    }

    /// Reset the cursor to the first reading, for re-running a replay from the
    /// top over the identical sealed sequence (§19 resume-from-checkpoint's
    /// simplest case).
    pub fn reset(&self) {
        self.cursor.store(0, Ordering::SeqCst);
    }

    /// Cursor clamped into `[0, len()-1]` so indexing is always in bounds.
    fn clamped_index(&self) -> usize {
        let raw = self.cursor.load(Ordering::SeqCst);
        // len() >= 1 (constructor precondition), so this subtraction is safe.
        raw.min(self.sequence.len() - 1)
    }
}

impl Clock for ReplayClock {
    fn monotonic_ns(&self) -> u64 {
        self.current().monotonic_ns
    }
    fn wallclock_ns(&self) -> u64 {
        self.current().wallclock_ns
    }
    fn current_slot(&self) -> u64 {
        self.current().current_slot
    }
}

/// A manually driven clock for unit tests, property tests, and counterfactual
/// runs (§19: `DeterministicTestClock`).
///
/// Responsibility: give tests full control over time with no hidden state —
/// every value is set or advanced explicitly by integer steps, so an expected
/// reading can be computed independently by the test.
///
/// Overflow contract (§22): [`advance_monotonic`](Self::advance_monotonic),
/// [`advance_wallclock`](Self::advance_wallclock), and
/// [`advance_slot`](Self::advance_slot) *saturate* at `u64::MAX` rather than
/// wrapping — time never runs backwards in this model.
#[derive(Debug)]
pub struct DeterministicTestClock {
    monotonic_ns: AtomicU64,
    wallclock_ns: AtomicU64,
    current_slot: AtomicU64,
}

impl DeterministicTestClock {
    /// Construct a test clock pinned to explicit starting values.
    #[must_use]
    pub const fn new(monotonic_ns: u64, wallclock_ns: u64, current_slot: u64) -> Self {
        Self {
            monotonic_ns: AtomicU64::new(monotonic_ns),
            wallclock_ns: AtomicU64::new(wallclock_ns),
            current_slot: AtomicU64::new(current_slot),
        }
    }

    /// A test clock starting at all-zero.
    #[must_use]
    pub const fn zeroed() -> Self {
        Self::new(0, 0, 0)
    }

    /// Overwrite the monotonic reading (used to model an arbitrary time).
    pub fn set_monotonic_ns(&self, value: u64) {
        self.monotonic_ns.store(value, Ordering::SeqCst);
    }

    /// Overwrite the wall-clock reading.
    pub fn set_wallclock_ns(&self, value: u64) {
        self.wallclock_ns.store(value, Ordering::SeqCst);
    }

    /// Overwrite the current slot.
    pub fn set_current_slot(&self, value: u64) {
        self.current_slot.store(value, Ordering::SeqCst);
    }

    /// Advance monotonic time by `delta_ns`, saturating at `u64::MAX`.
    /// Returns the new value. Explicit saturating overflow (§22).
    pub fn advance_monotonic(&self, delta_ns: u64) -> u64 {
        Self::saturating_bump(&self.monotonic_ns, delta_ns)
    }

    /// Advance wall-clock time by `delta_ns`, saturating at `u64::MAX`.
    /// Returns the new value.
    pub fn advance_wallclock(&self, delta_ns: u64) -> u64 {
        Self::saturating_bump(&self.wallclock_ns, delta_ns)
    }

    /// Advance the slot by `delta_slots`, saturating at `u64::MAX`.
    /// Returns the new value.
    pub fn advance_slot(&self, delta_slots: u64) -> u64 {
        Self::saturating_bump(&self.current_slot, delta_slots)
    }

    /// Saturating read-modify-write on one atomic field.
    ///
    /// Uses a compare-exchange loop so the saturating contract holds even
    /// under concurrent advances (the clock is `Sync`); single-threaded
    /// strategy use never contends, but correctness must not depend on that.
    fn saturating_bump(field: &AtomicU64, delta: u64) -> u64 {
        let mut current = field.load(Ordering::SeqCst);
        loop {
            let next = current.saturating_add(delta);
            match field.compare_exchange(current, next, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => return next,
                Err(observed) => current = observed,
            }
        }
    }
}

impl Default for DeterministicTestClock {
    fn default() -> Self {
        Self::zeroed()
    }
}

impl Clock for DeterministicTestClock {
    fn monotonic_ns(&self) -> u64 {
        self.monotonic_ns.load(Ordering::SeqCst)
    }
    fn wallclock_ns(&self) -> u64 {
        self.wallclock_ns.load(Ordering::SeqCst)
    }
    fn current_slot(&self) -> u64 {
        self.current_slot.load(Ordering::SeqCst)
    }
}

/// Placeholder for the LIVE-mode clock (§19 `WindowsSystemClock`).
///
/// Responsibility: hold the shape of the production system clock without
/// pulling a nondeterministic wall clock into the build. On the real
/// deployment server this would be backed by `QueryPerformanceCounter` for
/// monotonic timing and precise Windows wall-clock APIs for correlation
/// (§9.4). Live streaming/submission is **out of scope** for this build ([S]),
/// so this implementation performs *no syscall*: it returns time values that
/// were injected at construction (or updated via
/// [`inject`](Self::inject)). This keeps §22's "model any I/O behind a trait,
/// never call it" discipline: the real syscall lives behind this seam and is
/// simply not invoked here.
#[derive(Debug)]
pub struct WindowsSystemClock {
    injected: DeterministicTestClock,
}

impl WindowsSystemClock {
    /// Construct the placeholder from an injected reading.
    ///
    /// Responsibility: stand in for the OS clock using caller-supplied values
    /// so tests and offline runs are fully deterministic ([S]).
    #[must_use]
    pub fn with_injected(reading: ClockReading) -> Self {
        Self {
            injected: DeterministicTestClock::new(
                reading.monotonic_ns,
                reading.wallclock_ns,
                reading.current_slot,
            ),
        }
    }

    /// Replace the injected reading (models the OS clock ticking, driven by a
    /// harness rather than a syscall).
    pub fn inject(&self, reading: ClockReading) {
        self.injected.set_monotonic_ns(reading.monotonic_ns);
        self.injected.set_wallclock_ns(reading.wallclock_ns);
        self.injected.set_current_slot(reading.current_slot);
    }
}

impl Clock for WindowsSystemClock {
    fn monotonic_ns(&self) -> u64 {
        self.injected.monotonic_ns()
    }
    fn wallclock_ns(&self) -> u64 {
        self.injected.wallclock_ns()
    }
    fn current_slot(&self) -> u64 {
        self.injected.current_slot()
    }
}
