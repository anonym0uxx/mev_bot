//! # pump-quant-market-state
//!
//! Deterministic, integer/fixed-point market-state reducers for the pump-quant
//! memecoin scalping bot.
//!
//! ## Responsibility
//! This crate implements the *market-state reducer* family called out in the
//! constitution architecture (`pq-market-state`: "reducers, breadth
//! decomposition, creator state, MarketRegimeState"). It provides pure integer
//! reducers over event streams that produce inspectable, multi-dimensional
//! state:
//!
//! * [`breadth`] — manipulation- / cluster-adjusted **breadth decomposition
//!   reducer** (§21.2 market-state reconstruction; §21.7/§28 flow-authenticity
//!   list "store separately raw unique buyers ... never collapse into one
//!   opaque score").
//! * [`creator`] — **creator-state reducer** (§21.2 "creator position and
//!   sells"; §22 behavioral-risk creator inputs).
//! * [`regime`] — [`regime::MarketRegimeState`] and its reducer/classifier
//!   (§21.3 "a deterministic, time-safe, independently observable regime state
//!   ... never collapse it invisibly into a composite score").
//! * [`meta`] — [`meta::MetaRotationState`] on-chain measures plus the
//!   deterministic, time-safe **category-assignment classifier v0** (§21.4;
//!   criterion 81 "category assignments are timestamped and never
//!   retroactive").
//!
//! ## Constitution invariants honored here (§22)
//! * **No `f32`/`f64` anywhere in outcome logic** — every derived quantity is
//!   an integer or a fixed-point ratio in basis points (bps, parts per 10 000).
//! * **Explicit overflow discipline** — counts saturate by contract, value
//!   sums use `i128`/`u128` with saturating-by-contract accumulation, and
//!   ratios go through checked helpers.
//! * **Determinism** — no wall-clock, RNG, network, or float. All time is
//!   carried *in* the events (slots / nanosecond stamps supplied by the
//!   caller); nothing here reads a clock. Any I/O would be modeled behind a
//!   trait; this crate performs none.
//! * **Memory-bounded** — every stateful reducer has an explicit capacity and
//!   reports [`Completeness`] when that capacity is exceeded rather than
//!   growing without bound.
//!
//! Live streaming / submission is out of scope for this crate (server-side).

mod macros;

pub mod breadth;
pub mod common;
pub mod creator;
pub mod meta;
pub mod regime;

pub use common::{ratio_bps, signed_ratio_bps, BoundedMap, BoundedSet, Completeness, EntityId};
