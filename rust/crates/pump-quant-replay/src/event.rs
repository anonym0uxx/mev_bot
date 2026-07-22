//! The unit of replay: a single sealed [`ReplayEvent`] and its [`EventKind`].
//!
//! Responsibility: model exactly one entry of a sealed journal as the driver
//! sees it — an ordering key ([`EventKey`] from `pump_quant_clock`, the §19
//! tie-break leading key), the two wall/monotonic time quantities and the slot
//! needed to build a [`ClockReading`], and the *category* of thing that
//! happened. The category is what lets the driver evaluate step granularities
//! (observation / canonical-event / slot) and break conditions
//! (mint / decision / entry / exit) without ever inspecting a floating-point
//! outcome (§22: integer-only, no f32/f64 in the outcome path).
//!
//! A `ReplayEvent` is `Copy` and carries only integers, so a sealed sequence is
//! a flat, bounded `Vec<ReplayEvent>` (§99: bounded state) that the driver folds
//! deterministically.

use pump_quant_clock::{ClockReading, EventKey};

/// The category of a replayed event.
///
/// Responsibility: name the finite set of event kinds the §19 driver must
/// distinguish. The distinction drives both step granularity (which kinds count
/// as a "canonical event") and break conditions (mint / decision / entry /
/// exit). This is a closed enum — the driver's matching is exhaustive, so a new
/// kind is a compile error until every mode handles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventKind {
    /// A raw market observation with no higher-level meaning on its own — the
    /// finest granularity the driver steps by (§19 step-by-observation).
    Observation,
    /// A canonicalized on-chain event (post-ingest normalized event) that is
    /// not itself a mint/decision/entry/exit. Counts as a canonical event.
    Canonical,
    /// A new token mint was observed — a canonical event *and* a break target
    /// (§19 break-on-mint).
    Mint,
    /// The strategy emitted a `DecisionRecord` — a canonical event *and* a break
    /// target (§19 break-on-decision).
    Decision,
    /// A position was entered — a canonical event *and* a break target (§19
    /// break-on-entry).
    Entry,
    /// A position was exited — a canonical event *and* a break target (§19
    /// break-on-exit).
    Exit,
}

impl EventKind {
    /// Whether this kind is a *canonical event* for the purpose of
    /// step-by-canonical-event granularity (§19).
    ///
    /// Responsibility: define the single source of truth for "is this a
    /// canonical boundary". Every kind except a bare [`EventKind::Observation`]
    /// is canonical — a mint, decision, entry and exit are all canonicalized
    /// events, as is an explicit [`EventKind::Canonical`].
    #[must_use]
    pub const fn is_canonical(self) -> bool {
        !matches!(self, EventKind::Observation)
    }

    /// A small stable integer tag for this kind, folded into the replay state
    /// hash so that two runs over the same journal hash identically only if the
    /// *kinds* also match (§13 byte-equivalence).
    #[must_use]
    pub const fn tag(self) -> u64 {
        match self {
            EventKind::Observation => 1,
            EventKind::Canonical => 2,
            EventKind::Mint => 3,
            EventKind::Decision => 4,
            EventKind::Entry => 5,
            EventKind::Exit => 6,
        }
    }
}

/// One sealed journal entry as the replay driver folds it.
///
/// Responsibility: bundle the deterministic ordering key with the time/slot
/// quantities and the event kind, so the driver can (a) sort a sequence into
/// canonical replay order via the §19 tie-break comparator, (b) drive a
/// `ReplayClock` in lockstep by turning each event into a [`ClockReading`], and
/// (c) evaluate step/break logic from `kind`. All fields are integer (§22).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReplayEvent {
    /// The §19 tie-break ordering key `(ts_ns, source, seq)`. `ts_ns` doubles
    /// as the monotonic reading fed to the clock.
    pub key: EventKey,
    /// Wall-clock nanoseconds since the Unix epoch, for external correlation
    /// only (§9.4) — carried through to the [`ClockReading`].
    pub wallclock_ns: u64,
    /// The Solana slot this event belongs to — the unit of step-by-slot.
    pub slot: u64,
    /// What kind of thing this event is.
    pub kind: EventKind,
}

impl ReplayEvent {
    /// Construct a replay event from its parts.
    ///
    /// Responsibility: ergonomic constructor kept `pub` because tests build
    /// sealed sequences from it (§22: everything tests touch is `pub`).
    #[must_use]
    pub const fn new(key: EventKey, wallclock_ns: u64, slot: u64, kind: EventKind) -> Self {
        Self {
            key,
            wallclock_ns,
            slot,
            kind,
        }
    }

    /// The monotonic timestamp of this event (its ordering `ts_ns`).
    ///
    /// Responsibility: expose the primary time quantity the driver paces on,
    /// without callers reaching into `key`.
    #[must_use]
    pub const fn monotonic_ns(&self) -> u64 {
        self.key.ts_ns
    }

    /// Project this event onto the three-quantity [`ClockReading`] the
    /// `ReplayClock` replays (§19: monotonic, wall-clock, slot).
    ///
    /// Responsibility: keep the mapping from a journal entry to a clock sample
    /// in one place so the driver's parallel clock never drifts from the events.
    #[must_use]
    pub const fn reading(&self) -> ClockReading {
        ClockReading::new(self.key.ts_ns, self.wallclock_ns, self.slot)
    }
}
