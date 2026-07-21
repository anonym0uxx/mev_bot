//! `pump_quant_watchlist` — the always-scanning "eye" of the memecoin scalping bot.
//!
//! Responsibility: continuous candidate discovery. Multiple discovery lanes
//! (per the StrategyRuntime setup families of the constitution — CreationSniper,
//! EarlyConfirmation, GraduationTransition, ActiveMarketScalp) each surface
//! mints they consider interesting. This crate unions those observations,
//! deduplicates them by mint keeping the strongest lane evidence, holds them in
//! a bounded, ranked, decaying working set, and promotes the strongest to the
//! downstream scalp pipeline. It also accumulates realized net-SOL per lane so
//! lane quality can feed back into ranking.
//!
//! Constitution alignment:
//! - §22 — all outcome-path arithmetic is integer / fixed-point. There is **no**
//!   `f32`/`f64` anywhere in this crate. Every operation is deterministic: no
//!   wall-clock, no RNG, no network, no float. Logical time is an explicit `u64`
//!   tick supplied by the caller (modelled I/O, never called here).
//! - §99 — every stateful structure is capacity-bounded with an explicit
//!   eviction policy; nothing grows without bound.
//! - §102 — no silent magic numbers: every tuning constant is a named `pub const`
//!   with a documented rationale and is overridable by the caller.
//! - §74 — lane performance is measured in realized net-SOL (signed lamports),
//!   never win-rate.
//!
//! The six leaves (per the task's §71 watchlist decomposition):
//! - [`candidate`]        — `wl_candidate`: the typed [`candidate::Candidate`].
//! - [`lane_ingest`]      — `wl_lane_ingest`: union multi-lane intake + dedup.
//! - [`state`]            — `wl_state`: bounded, ranked, decaying working set.
//! - [`rank`]             — `wl_rank`: discovery_score × recency × lane weight.
//! - [`promote`]          — `wl_promote`: top candidates to the scalp pipeline.
//! - [`lane_performance`] — `wl_lane_performance`: realized net-SOL per lane.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod candidate;
pub mod lane_ingest;
pub mod lane_performance;
pub mod promote;
pub mod rank;
pub mod state;

pub use candidate::{Candidate, Features, Lane, Mint};
pub use lane_ingest::ingest_union;
pub use lane_performance::LanePerformance;
pub use promote::promote_top;
pub use rank::{recency_factor, score_rank, LaneWeights, RankParams, RECENCY_ONE, WEIGHT_ONE};
pub use state::WatchlistState;
