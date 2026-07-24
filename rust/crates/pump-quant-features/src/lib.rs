//! # pump-quant-features
//!
//! Leakage-safe feature spine for the memecoin scalping bot.
//!
//! This crate implements three constitution-mandated pieces of the feature plane,
//! all of them deterministic and free of floating-point arithmetic in every
//! outcome-controlling path (constitution 22):
//!
//! * [`timed_feature`] — the [`TimedFeature`](timed_feature::TimedFeature) record and a
//!   **point-in-time-correct** serving store (constitution 20). Serving `as_of(T)`
//!   is proven to return only information whose times are `<= T` — no look-ahead.
//! * [`bar`] — a streaming [`BarBuilder`](bar::BarBuilder) that folds canonical trade
//!   flow into time bars or volume bars (constitution 21.6). Bars bind back to the
//!   originating events and are built from our own flow, the only leakage-proof source.
//! * [`market_structure`] — deterministic bar-level price-structure detectors
//!   (constitution 21.6): compression/expansion, breakout-and-retest,
//!   failed-breakdown/reclaim, sweep-and-reclaim, and swing-trend structure, all
//!   pure integer functions over the [`bar::Bar`] sequences the builder emits.
//! * [`structure_ext`] — the additive remainder of the §21.6 bar/market-structure
//!   detector family: drawdown/retrace state, a graded volatility regime (with an
//!   explicit high tail), per-bar wick/rejection microstructure, and info-time
//!   conditioning (token age, time-of-day), plus a [`structure_ext::realized_vol_bps`]
//!   helper the execution engine consumes to scale stops — all pure integer.
//! * [`holder_growth`] — the §70.1 holder-growth **acceleration** estimator: a
//!   bounded per-mint holder-count series reduced to a first difference (growth
//!   rate) and a second difference (acceleration) in basis points, selected
//!   point-in-time (`ts_ns <= as_of_ns`) and returning `None` — never a
//!   fabricated neutral `0` — below its documented sample/spacing/staleness
//!   gates (constitution 6.4, 20, 99).
//! * [`micro`] — an integer AMM order-flow / microstructure feature catalog
//!   (constitution 21.7): CVD, delta velocity, order-flow imbalance, VWAP /
//!   anchored VWAP, trade-size distribution & large-print detection, swap-arrival
//!   intensity, and CVD/price divergence classification. Memecoin venues are
//!   constant-product AMMs with no central limit order book, so every feature here
//!   is computed from decoded swap flow and reserve-derived prices, never from an
//!   imagined order book.
//!
//! ## Determinism & purity contract (constitution 22)
//!
//! Nothing in this crate reads a wall clock, performs I/O, allocates randomness, or
//! touches floating point in logic. All money and price quantities are integers or
//! fixed-point integers; every arithmetic step that could overflow uses an explicit
//! checked/saturating strategy documented at the call site. All state is memory
//! bounded: the rolling window and store carry hard capacities with defined eviction.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod bar;
pub mod holder_growth;
pub mod market_structure;
pub mod micro;
pub mod structure_ext;
pub mod timed_feature;
pub mod types;
