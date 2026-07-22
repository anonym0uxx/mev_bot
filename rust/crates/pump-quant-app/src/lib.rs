//! # pump-quant-app — the Hermes nervous system
//!
//! The continuous discovery→gate→scalp→reflect loop that composes the pump-quant
//! leaf crates into a running paper/replay trading engine. This crate is the spine:
//! it does not add new market math, it *orchestrates* the crates that do, under one
//! deterministic logical clock.
//!
//! ## The loop (constitution §71)
//!
//! 1. **Discovery — union, not intersection** ([`lane`]). Four independent lanes —
//!    on-chain numeric flow, narrative/attention, social calls, smart-money wallets
//!    — each surface candidates on their own. No lane waits for another to agree.
//! 2. **Fusion & ranking** ([`engine`], via `pump_quant_watchlist`). Candidates are
//!    unioned per mint (strongest lane evidence wins the tie), recency-decayed, and
//!    ranked into a bounded watchlist.
//! 3. **Gate — corroboration + viability** ([`gate`]). The top-ranked candidates
//!    face two hard hurdles: on-chain confirmation with real numeric microstructure
//!    (social/narrative/wallet evidence can never authorise entry alone — fade-first,
//!    §29), and an economic size-band that must survive its own costs (§18).
//! 4. **Scalp — paper fill** ([`scalp`], via `pump_quant_simulator`). Admits are
//!    filled through the calibrated fill model; no capital moves on the laptop.
//! 5. **Reflect — discovery learns** ([`reflect`]). Realized net-SOL per lane nudges
//!    that lane's discovery weight inside a governance envelope, so reflection
//!    *enhances discovery* rather than merely grading it. Net SOL is the objective.
//!
//! Every decision is journaled ([`journal_log`]) into a canonical rolling hash, so
//! the same event stream reproduces the same decisions and the same net-SOL exactly
//! — replay is a correctness authority, not a demo (§54).
//!
//! ## What is deliberately absent
//!
//! Live capital, key signing and fund movement are Tier-0 human-gated and are not
//! reachable from this crate. [`engine::RunMode`] can express only `Paper` and
//! `Replay`; there is no `Live` variant to construct.
//!
//! ## Determinism (§22)
//!
//! No floating point reaches any outcome decision, and no wall-clock is read: time
//! is the explicit tick advanced by [`event::AppEvent::Tick`]. Every stage is a pure
//! function of prior state and the event.

#![forbid(unsafe_code)]

pub mod attention;
pub mod config;
pub mod engine;
pub mod event;
pub mod gate;
pub mod journal_log;
pub mod lane;
pub mod reflect;
pub mod scalp;
pub mod social_earn;
pub mod social_ingest;
pub mod token_ingest;

pub use config::{Config, ConfigError, FillModeCfg};
pub use engine::{Engine, Report, RunMode};
pub use event::{AppEvent, LaneKind};
pub use gate::{Confirmation, GateDecision, GateReject};
