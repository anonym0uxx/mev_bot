//! `pump_quant_memory` — the research-plane **QuantMemoryStore** for the
//! pump-quant memecoin scalping bot.
//!
//! Responsibility: hold the versioned, typed, deterministic research memory that
//! the governance and reflection systems read and write — hypotheses,
//! experiments and their sealed results, the social-call / markout / source-quality
//! ledger, the amplification graph, and the meta-rotation record — and rank the
//! open-hypothesis research queue by value-of-information. Constitution mapping:
//!
//! * **§29.9** — QuantMemoryStore tables (`meta_categories`, `category_assignments`,
//!   `meta_rotation_snapshots`, `social_calls`, `call_markouts`,
//!   `source_quality_ledger`, `amplification_edges`) plus the shared
//!   experiment/hypothesis/result governance memory.
//! * **§56.1 / §56.4 / §56.9 / §56.10** — sealed, immutable experiments
//!   (seal → hash → reject mutation), the inference lifecycle states, and the
//!   value-of-information-ranked open-hypothesis queue.
//! * **§22** — the whole crate is deterministic and float-free in every
//!   outcome-controlling path: integer / fixed-point (lamports, basis points,
//!   seconds) only, explicit overflow discipline, stable iteration order, and no
//!   I/O, wall-clock, RNG, or network (live persistence and streaming are
//!   out-of-scope `[S]` server responsibilities — modelled behind traits and
//!   never called here).
//! * **§57** — every table is memory-bounded with a durability-first overflow
//!   contract (reject at capacity, never silently drop reconciled evidence).
//!
//! No `f32`/`f64` appears anywhere in this crate.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod experiment;
pub mod hashing;
pub mod rows;
pub mod schema;
pub mod store;
pub mod voi;
