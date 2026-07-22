//! `pump_quant_replay` — the constitution §19 deterministic replay driver
//! (§13 `pq-replay`: "deterministic runner, step modes, checkpoints,
//! byte-equivalence").
//!
//! Responsibility: compose the determinism primitives that already exist —
//! `pump_quant_clock`'s [`ReplayClock`](pump_quant_clock::ReplayClock) (advance
//! / reset / position) and its `(ts_ns, source, seq)` tie-break comparator —
//! into the mode-selectable, break-driven stepping engine §19 requires but that
//! no crate previously provided. The substrate (clock seam, tie-break, journal
//! codec, checkpoint parity in `pump_quant_core`) was present; this crate is the
//! runner that folds a sealed sequence through it.
//!
//! Two modules:
//!
//! * [`event`] — the unit of replay ([`ReplayEvent`], [`EventKind`]): an
//!   integer ordering key plus the time/slot quantities and the event category.
//! * [`driver`] — the [`ReplayDriver`] plus its [`ReplayMode`] (maximum-speed /
//!   real-time / scaled-time / step-by-observation / step-by-canonical-event /
//!   step-by-slot), [`BreakCondition`]/[`BreakSet`] (break-on mint / decision /
//!   entry / exit) and [`Checkpoint`] (resume-from-checkpoint).
//!
//! Constitution hard rules honored (§22): no floating point anywhere — pacing is
//! integer / `u128`-widened division, the replay state hash is integer FNV-1a;
//! overflow is explicit (pacing and counters saturate by contract); state is
//! bounded (§99); there is no I/O, RNG, network, key, or wall-clock read — the
//! only source of time is the injected [`ReplayClock`]. Real-time / scaled-time
//! pacing is returned as an integer delay for a harness to honor and never
//! perturbs the folded events or the state hash, so every outcome-bearing mode
//! is byte-equivalent across runs and across resume-from-checkpoint.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod driver;
pub mod event;

pub use driver::{
    BreakCondition, BreakSet, Checkpoint, ReplayDriver, ReplayMode, RunResult, StepResult,
    StopReason,
};
pub use event::{EventKind, ReplayEvent};
