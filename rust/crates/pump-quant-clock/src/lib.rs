//! `pump_quant_clock` — the determinism seam for the pump-quant memecoin
//! scalping bot.
//!
//! Responsibility: provide the single injection point through which *all*
//! decision-path time reads flow, so that strategy logic never touches a
//! wall clock and every replay is reproducible (constitution §19
//! "Deterministic Replay Engine", §22 "Deterministic Strategy Core").
//!
//! This crate contains two independent, self-contained pieces:
//!
//! * [`clock`] — the [`Clock`](clock::Clock) trait and its three
//!   implementations ([`ReplayClock`](clock::ReplayClock),
//!   [`DeterministicTestClock`](clock::DeterministicTestClock),
//!   [`WindowsSystemClock`](clock::WindowsSystemClock)). Per §22 the decision
//!   core takes a `&dyn Clock` and *never* calls `SystemTime::now` /
//!   `Instant::now` directly.
//! * [`tie_break`] — the deterministic ordering of equal-timestamp events
//!   ([`EventKey`](tie_break::EventKey) and
//!   [`tie_break_cmp`](tie_break::tie_break_cmp)), so that two runs over the
//!   same journal see events in byte-identical order (§19 "Tie-breaking").
//!
//! Constitution hard rules honored here (§22): no floating point anywhere in
//! this crate (all time is integer nanoseconds / integer slots); overflow is
//! explicit (advancing clocks saturate by contract); everything the tests
//! touch is `pub`; there is no I/O, RNG, network, or real wall-clock call —
//! the "system" clock is an injected placeholder ([S] server surface is out
//! of scope for this build).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod clock;
pub mod tie_break;

pub use clock::{Clock, ClockReading, DeterministicTestClock, ReplayClock, WindowsSystemClock};
pub use tie_break::{stable_tie_break_sort, tie_break_cmp, EventKey};
