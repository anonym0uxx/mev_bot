//! The §19 replay execution-mode driver.
//!
//! Responsibility: compose the existing determinism primitives —
//! `pump_quant_clock`'s [`ReplayClock`] (advance / reset / position) and the
//! `(ts_ns, source, seq)` tie-break comparator — into the mode-selectable,
//! break-driven stepping engine constitution §19 requires and §13 names
//! `pq-replay` ("deterministic runner, step modes, checkpoints,
//! byte-equivalence"). It folds a *sealed* `Vec<ReplayEvent>` in canonical
//! order and exposes:
//!
//! * **Execution / pacing modes** ([`ReplayMode`]): `MaximumSpeed`, `RealTime`,
//!   `ScaledTime { speed_num, speed_den }`. Pacing is an integer nanosecond
//!   *delay* the driver computes and returns; it never sleeps and never reads a
//!   wall clock, so pacing cannot perturb an outcome (§22). A live harness may
//!   choose to honor the delay; the folded events and the replay state hash are
//!   byte-identical regardless.
//! * **Step granularities** ([`ReplayMode`] step variants): `StepByObservation`,
//!   `StepByCanonicalEvent`, `StepBySlot` — one [`step`](ReplayDriver::step)
//!   advances exactly one unit of the selected granularity.
//! * **Break conditions** ([`BreakCondition`] / [`BreakSet`]): break-on-mint /
//!   decision / entry / exit — [`run_to_break`](ReplayDriver::run_to_break)
//!   drives until the first matching event or exhaustion.
//! * **Resume-from-checkpoint** ([`Checkpoint`]): capture a tiny `Copy` token
//!   and later re-seat the driver on it; re-running from a checkpoint yields the
//!   identical subsequent event stream and identical state hash (§13
//!   byte-equivalence), which is more than `ReplayClock::reset`'s
//!   resume-from-top.
//!
//! Constitution hard rules honored (§22): no floating point anywhere — pacing
//! math is integer / `u128`-widened division, the state hash is integer FNV-1a;
//! overflow is explicit (pacing and counters saturate by contract); state is
//! bounded (§99: the sealed `Vec`, a `ReplayClock`, and a fixed set of `usize` /
//! `u64` cursors — nothing grows without bound); there is no I/O, RNG, network,
//! key or wall-clock read.

use pump_quant_clock::{tie_break_cmp, ReplayClock};

use crate::event::{EventKind, ReplayEvent};

/// FNV-1a 64-bit offset basis — the seed of the rolling replay state hash.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// A §19 replay execution mode: how the driver paces and/or steps.
///
/// Responsibility: be the single greppable enum naming every §19 execution
/// mode, so callers select one value rather than re-deriving pacing/stepping
/// behavior. The three *pacing* modes emit continuously (one observation per
/// [`step`](ReplayDriver::step)) but differ in the integer delay returned; the
/// three *step* modes advance exactly one unit of their granularity per `step`
/// and never pace (delay is always `0`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReplayMode {
    /// Emit as fast as possible — pacing delay is always `0` (§19
    /// maximum-speed).
    MaximumSpeed,
    /// Pace at the recorded rate — the delay before an event equals the
    /// monotonic-ns gap to the previous emitted event (§19 real-time).
    RealTime,
    /// Pace at a rational playback speed `speed_num / speed_den` of real time
    /// (§19 scaled-time). Delay = `raw_gap * speed_den / speed_num` (integer,
    /// `u128`-widened). `speed_num = 2, speed_den = 1` is 2× speed (half the
    /// delay); `1 / 2` is half speed (double the delay). A `speed_num` of `0`
    /// is degenerate and falls back to real-time pacing rather than dividing by
    /// zero.
    ScaledTime {
        /// Numerator of the playback-speed multiplier.
        speed_num: u32,
        /// Denominator of the playback-speed multiplier.
        speed_den: u32,
    },
    /// One `step` advances exactly one observation (§19 step-by-observation).
    StepByObservation,
    /// One `step` advances up to and including the next canonical event (§19
    /// step-by-canonical-event) — see [`EventKind::is_canonical`].
    StepByCanonicalEvent,
    /// One `step` advances through the run of events sharing the current slot,
    /// stopping at the slot boundary (§19 step-by-slot).
    StepBySlot,
}

/// The granularity one [`step`](ReplayDriver::step) advances by, derived from
/// [`ReplayMode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Granularity {
    /// Exactly one event.
    Observation,
    /// Up to and including the next canonical event.
    Canonical,
    /// The run of events sharing the current slot.
    Slot,
}

impl ReplayMode {
    /// The step granularity this mode advances by.
    const fn granularity(self) -> Granularity {
        match self {
            ReplayMode::MaximumSpeed
            | ReplayMode::RealTime
            | ReplayMode::ScaledTime { .. }
            | ReplayMode::StepByObservation => Granularity::Observation,
            ReplayMode::StepByCanonicalEvent => Granularity::Canonical,
            ReplayMode::StepBySlot => Granularity::Slot,
        }
    }

    /// Compute the integer pacing delay (ns) for a single emission whose
    /// monotonic gap to the previous emitted event is `raw_gap_ns`.
    ///
    /// Step modes and maximum-speed never pace (return `0`); real-time returns
    /// the gap unchanged; scaled-time scales it by `speed_den / speed_num`
    /// using `u128` so the multiply cannot overflow, then clamps to `u64`.
    fn pacing_delay(self, raw_gap_ns: u64) -> u64 {
        match self {
            ReplayMode::RealTime => raw_gap_ns,
            ReplayMode::ScaledTime {
                speed_num,
                speed_den,
            } => {
                if speed_num == 0 {
                    // Degenerate speed: fall back to real-time rather than /0.
                    return raw_gap_ns;
                }
                let scaled =
                    (u128::from(raw_gap_ns) * u128::from(speed_den)) / u128::from(speed_num);
                if scaled > u128::from(u64::MAX) {
                    u64::MAX
                } else {
                    scaled as u64
                }
            }
            // MaximumSpeed and all step modes: no pacing.
            _ => 0,
        }
    }
}

/// A §19 break condition — the event kind that halts a
/// [`run_to_break`](ReplayDriver::run_to_break).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BreakCondition {
    /// Stop after emitting a mint event (§19 break-on-mint).
    OnMint,
    /// Stop after emitting a decision event (§19 break-on-decision).
    OnDecision,
    /// Stop after emitting a position-entry event (§19 break-on-entry).
    OnEntry,
    /// Stop after emitting a position-exit event (§19 break-on-exit).
    OnExit,
}

impl BreakCondition {
    /// The single-bit mask this condition occupies inside a [`BreakSet`].
    const fn bit(self) -> u8 {
        match self {
            BreakCondition::OnMint => 1 << 0,
            BreakCondition::OnDecision => 1 << 1,
            BreakCondition::OnEntry => 1 << 2,
            BreakCondition::OnExit => 1 << 3,
        }
    }

    /// The break condition an event of `kind` would trigger, if any.
    const fn for_kind(kind: EventKind) -> Option<Self> {
        match kind {
            EventKind::Mint => Some(BreakCondition::OnMint),
            EventKind::Decision => Some(BreakCondition::OnDecision),
            EventKind::Entry => Some(BreakCondition::OnEntry),
            EventKind::Exit => Some(BreakCondition::OnExit),
            EventKind::Observation | EventKind::Canonical => None,
        }
    }
}

/// A bounded set of active break conditions, packed into a single byte.
///
/// Responsibility: hold which of the four §19 break conditions are armed, in
/// `O(1)` bounded state (one `u8`) — no allocation, no growth (§99).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct BreakSet(u8);

impl BreakSet {
    /// An empty set — no break conditions armed.
    #[must_use]
    pub const fn none() -> Self {
        Self(0)
    }

    /// Return a copy of this set with `c` armed (const builder).
    #[must_use]
    pub const fn with(self, c: BreakCondition) -> Self {
        Self(self.0 | c.bit())
    }

    /// Arm `c` in place.
    pub fn insert(&mut self, c: BreakCondition) {
        self.0 |= c.bit();
    }

    /// Whether `c` is armed.
    #[must_use]
    pub const fn contains(self, c: BreakCondition) -> bool {
        self.0 & c.bit() != 0
    }

    /// Whether no condition is armed.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// Why a [`step`](ReplayDriver::step) or [`run_to_break`](ReplayDriver::run_to_break)
/// stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// An armed break condition fired on the last emitted event.
    Break(BreakCondition),
    /// The sealed sequence was fully consumed with no armed break firing.
    Exhausted,
}

/// The result of a single [`step`](ReplayDriver::step).
///
/// Responsibility: report exactly what one unit of stepping emitted. `emitted`
/// is transient (it is a return value, not retained driver state) and bounded
/// by the number of events in the stepped unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepResult {
    /// The events emitted in this step, in canonical order.
    pub emitted: Vec<ReplayEvent>,
    /// The summed integer pacing delay (ns) for the emissions in this step.
    pub pacing_delay_ns: u64,
    /// The first armed break condition that fired within this step, if any.
    pub break_hit: Option<BreakCondition>,
    /// Whether the sequence is now exhausted (cursor at end).
    pub exhausted: bool,
}

/// The result of a [`run_to_break`](ReplayDriver::run_to_break).
///
/// Responsibility: summarize a run without retaining the emitted events —
/// only integer counts, the total pacing delay, the stop reason, and the
/// rolling replay state hash — so the driver's state stays bounded (§99).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunResult {
    /// Total number of events emitted across the run.
    pub emitted: u64,
    /// Total integer pacing delay (ns) accumulated across the run.
    pub pacing_delay_ns: u64,
    /// Why the run stopped.
    pub stop: StopReason,
    /// The rolling replay state hash after the run (§13 byte-equivalence).
    pub state_hash: u64,
}

/// A tiny, `Copy` resume token capturing a driver position (§19
/// resume-from-checkpoint).
///
/// Responsibility: snapshot exactly the folding state needed to re-seat a
/// driver mid-stream — the cursor, the emitted count, the pacing predecessor,
/// and the rolling state hash — so [`resume`](ReplayDriver::resume) reproduces
/// the identical subsequent stream. It carries no reference to the sequence and
/// never grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Checkpoint {
    cursor: usize,
    emitted: u64,
    prev_monotonic: u64,
    has_prev: bool,
    state_hash: u64,
}

impl Checkpoint {
    /// Index of the next event that would be emitted from this checkpoint.
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// Number of events emitted up to this checkpoint.
    #[must_use]
    pub const fn emitted(&self) -> u64 {
        self.emitted
    }

    /// The rolling replay state hash at this checkpoint.
    #[must_use]
    pub const fn state_hash(&self) -> u64 {
        self.state_hash
    }
}

/// The mode-selectable, break-driven §19 replay driver.
///
/// Responsibility: fold a sealed, canonically ordered `Vec<ReplayEvent>` under
/// a chosen [`ReplayMode`] and [`BreakSet`], keeping a `ReplayClock` advanced in
/// lockstep so every time read during replay comes from the injected clock seam
/// (§19), and maintaining a rolling integer state hash that proves
/// byte-equivalence across runs and checkpoints (§13).
#[derive(Debug)]
pub struct ReplayDriver {
    /// The sealed sequence, sorted once into canonical `(ts_ns, source, seq)`
    /// order at construction. Bounded input state (§99).
    events: Vec<ReplayEvent>,
    /// The injected clock seam, advanced to reflect the last emitted event.
    clock: ReplayClock,
    /// Number of `advance()` calls made on `clock` (mirrors its cursor).
    clock_advances: usize,
    /// Selected execution mode.
    mode: ReplayMode,
    /// Armed break conditions.
    breaks: BreakSet,
    /// Index of the next event to emit (`0..=len`).
    cursor: usize,
    /// Monotonic ns of the last emitted event (for pacing).
    prev_monotonic: u64,
    /// Whether any event has been emitted (guards the first-event pacing = 0).
    has_prev: bool,
    /// Count of events emitted so far.
    emitted_count: u64,
    /// Rolling FNV-1a hash over every emitted event (§13 byte-equivalence).
    state_hash: u64,
}

impl ReplayDriver {
    /// Build a driver over a sealed sequence, a mode, and a set of break
    /// conditions.
    ///
    /// The sequence is sorted into canonical replay order using the §19
    /// tie-break comparator (`(ts_ns, source, seq)`), so callers may pass events
    /// in any order and still get deterministic replay. The internal
    /// `ReplayClock` is seeded from the sorted readings.
    ///
    /// # Panics
    /// Panics if `events` is empty — a replay driver must always be able to
    /// serve time via its `ReplayClock`, and `ReplayClock::new` rejects an empty
    /// sequence. An empty sealed journal is a construction-time programming
    /// error, not a runtime condition to paper over (§22: fail loudly at the
    /// precondition).
    #[must_use]
    pub fn new(mut events: Vec<ReplayEvent>, mode: ReplayMode, breaks: BreakSet) -> Self {
        assert!(
            !events.is_empty(),
            "ReplayDriver requires a non-empty sealed sequence"
        );
        // Canonical order via the §19 tie-break comparator (stable: genuine
        // duplicates keep input order).
        events.sort_by(|a, b| tie_break_cmp(&a.key, &b.key));
        let readings = events.iter().map(ReplayEvent::reading).collect::<Vec<_>>();
        let clock = ReplayClock::new(readings);
        Self {
            events,
            clock,
            clock_advances: 0,
            mode,
            breaks,
            cursor: 0,
            prev_monotonic: 0,
            has_prev: false,
            emitted_count: 0,
            state_hash: FNV_OFFSET,
        }
    }

    /// The selected execution mode.
    #[must_use]
    pub const fn mode(&self) -> ReplayMode {
        self.mode
    }

    /// Borrow the injected clock seam, advanced to the last emitted event.
    ///
    /// Responsibility: let a harness read replay time through the same
    /// `ReplayClock` the driver drives, rather than a second clock (§19).
    #[must_use]
    pub const fn clock(&self) -> &ReplayClock {
        &self.clock
    }

    /// Number of events in the sealed sequence.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether the sealed sequence is empty (always `false` after
    /// construction).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Index of the next event that would be emitted.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.cursor
    }

    /// Number of events emitted so far.
    #[must_use]
    pub const fn emitted(&self) -> u64 {
        self.emitted_count
    }

    /// The rolling replay state hash over every emitted event (§13
    /// byte-equivalence). Two runs over the same journal that emit the same
    /// events in the same order produce the same hash.
    #[must_use]
    pub const fn state_hash(&self) -> u64 {
        self.state_hash
    }

    /// Whether the cursor has reached the end of the sealed sequence.
    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.cursor >= self.events.len()
    }

    /// Advance exactly one unit of the current mode's granularity, checking
    /// armed break conditions on each emitted event.
    ///
    /// Returns the emitted events, the summed pacing delay, the first break hit
    /// (if any — a break stops the unit early), and whether the sequence is now
    /// exhausted. Emitting nothing (empty `emitted`) with `exhausted = true`
    /// means the driver was already at the end.
    pub fn step(&mut self) -> StepResult {
        if self.cursor >= self.events.len() {
            return StepResult {
                emitted: Vec::new(),
                pacing_delay_ns: 0,
                break_hit: None,
                exhausted: true,
            };
        }
        let gran = self.mode.granularity();
        let start_slot = self.events[self.cursor].slot;
        let mut emitted = Vec::new();
        let mut delay_total = 0u64;
        let mut break_hit = None;

        loop {
            if self.cursor >= self.events.len() {
                break;
            }
            // Slot granularity halts *before* emitting an event of a new slot.
            if gran == Granularity::Slot
                && !emitted.is_empty()
                && self.events[self.cursor].slot != start_slot
            {
                break;
            }

            let (ev, delay) = self.emit_current();
            delay_total = delay_total.saturating_add(delay);
            emitted.push(ev);

            // Break check on the just-emitted event takes priority over
            // granularity continuation.
            if let Some(bc) = BreakCondition::for_kind(ev.kind) {
                if self.breaks.contains(bc) {
                    break_hit = Some(bc);
                    break;
                }
            }

            match gran {
                Granularity::Observation => break,
                Granularity::Canonical => {
                    if ev.kind.is_canonical() {
                        break;
                    }
                }
                Granularity::Slot => {
                    // Continue; the loop-top check ends the slot run.
                }
            }
        }

        StepResult {
            emitted,
            pacing_delay_ns: delay_total,
            break_hit,
            exhausted: self.cursor >= self.events.len(),
        }
    }

    /// Drive repeatedly until the first armed break condition fires or the
    /// sequence is exhausted, whichever comes first.
    ///
    /// Returns integer counts, the total pacing delay, the stop reason, and the
    /// final state hash — deliberately *not* the emitted events, so this method
    /// runs in bounded state regardless of sequence length (§99). With no armed
    /// break conditions this runs to exhaustion.
    pub fn run_to_break(&mut self) -> RunResult {
        let mut total_emitted = 0u64;
        let mut total_delay = 0u64;
        loop {
            let s = self.step();
            total_emitted = total_emitted.saturating_add(s.emitted.len() as u64);
            total_delay = total_delay.saturating_add(s.pacing_delay_ns);
            if let Some(bc) = s.break_hit {
                return RunResult {
                    emitted: total_emitted,
                    pacing_delay_ns: total_delay,
                    stop: StopReason::Break(bc),
                    state_hash: self.state_hash,
                };
            }
            if s.exhausted {
                return RunResult {
                    emitted: total_emitted,
                    pacing_delay_ns: total_delay,
                    stop: StopReason::Exhausted,
                    state_hash: self.state_hash,
                };
            }
        }
    }

    /// Capture a resume token at the current position (§19
    /// resume-from-checkpoint).
    #[must_use]
    pub const fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            cursor: self.cursor,
            emitted: self.emitted_count,
            prev_monotonic: self.prev_monotonic,
            has_prev: self.has_prev,
            state_hash: self.state_hash,
        }
    }

    /// Re-seat the driver on a previously captured [`Checkpoint`], re-syncing
    /// the clock, so a subsequent run reproduces the identical stream and hash.
    ///
    /// # Panics
    /// Panics if the checkpoint's cursor exceeds this driver's sequence length
    /// — a token from a different (longer) sequence is a programming error
    /// (§22: fail loudly rather than seek out of bounds).
    pub fn resume(&mut self, cp: Checkpoint) {
        assert!(
            cp.cursor <= self.events.len(),
            "checkpoint cursor {} exceeds sequence length {}",
            cp.cursor,
            self.events.len()
        );
        self.cursor = cp.cursor;
        self.emitted_count = cp.emitted;
        self.prev_monotonic = cp.prev_monotonic;
        self.has_prev = cp.has_prev;
        self.state_hash = cp.state_hash;
        self.restore_clock(cp.cursor);
    }

    /// Reset the driver to the top of the sequence (§19 resume-from-checkpoint's
    /// simplest case — replay from the beginning over the identical sealed
    /// sequence).
    pub fn reset(&mut self) {
        self.cursor = 0;
        self.emitted_count = 0;
        self.prev_monotonic = 0;
        self.has_prev = false;
        self.state_hash = FNV_OFFSET;
        self.clock.reset();
        self.clock_advances = 0;
    }

    // --- internals -----------------------------------------------------------

    /// Emit the event at `cursor`, advancing the clock in lockstep, folding the
    /// state hash, computing the pacing delay, and stepping the cursor. Returns
    /// the emitted event and its pacing delay.
    fn emit_current(&mut self) -> (ReplayEvent, u64) {
        let idx = self.cursor;
        let ev = self.events[idx];
        self.sync_clock_to(idx);
        let delay = if self.has_prev {
            let raw = ev.monotonic_ns().saturating_sub(self.prev_monotonic);
            self.mode.pacing_delay(raw)
        } else {
            0
        };
        self.prev_monotonic = ev.monotonic_ns();
        self.has_prev = true;
        self.state_hash = fold_event(self.state_hash, &ev);
        self.emitted_count = self.emitted_count.saturating_add(1);
        self.cursor += 1;
        (ev, delay)
    }

    /// Advance (or rewind-then-advance) the `ReplayClock` so its served reading
    /// is the event at index `idx`.
    fn sync_clock_to(&mut self, idx: usize) {
        if idx < self.clock_advances {
            self.clock.reset();
            self.clock_advances = 0;
        }
        while self.clock_advances < idx {
            self.clock.advance();
            self.clock_advances += 1;
        }
    }

    /// Re-seat the clock so it reflects the last emitted event for a cursor of
    /// `cursor` (i.e. index `cursor - 1`), or the top when nothing was emitted.
    fn restore_clock(&mut self, cursor: usize) {
        if cursor == 0 {
            self.clock.reset();
            self.clock_advances = 0;
        } else {
            self.sync_clock_to(cursor - 1);
        }
    }
}

/// Fold one event into the rolling FNV-1a state hash (integer-only, §22).
///
/// Every integer quantity of the event is mixed little-endian, so two runs hash
/// identically only if the ordering keys, times, slots, and kinds all match —
/// the byte-equivalence check of §13.
fn fold_event(hash: u64, ev: &ReplayEvent) -> u64 {
    let mut h = hash;
    let words = [
        ev.key.ts_ns,
        u64::from(ev.key.source.0),
        ev.key.seq,
        ev.wallclock_ns,
        ev.slot,
        ev.kind.tag(),
    ];
    for word in words {
        for b in word.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(FNV_PRIME);
        }
    }
    h
}
