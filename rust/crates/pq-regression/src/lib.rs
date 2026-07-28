//! `pq-regression` — Hermes end-to-end regression harness.
//!
//! A durable, fast (<30s), fully deterministic (integer-only, no wall-clock, no
//! RNG) tripwire suite that would CATCH a regression in any of the recently-wired
//! constitution laws or core invariants. It ADDS coverage; it pins nothing new
//! about behaviour. If a test here fails against the current green HEAD, the
//! HARNESS is wrong — fix the harness, not the engine.
//!
//! # Regression classes and what each protects
//!
//! 1. **Determinism / replay** (`tests/regression_determinism.rs`) — drives the
//!    golden tape N=3 times and asserts a byte-identical decision-journal digest
//!    every run, equal to the pinned [`baselines::GOLDEN_DIGEST`], plus the pinned
//!    net / promoted / admitted / rejected / universe_filtered. Also asserts
//!    permutation-invariance on a small causally-independent scenario ("shuffled-
//!    but-causal, where legal"). Protects against any non-behaviour-preserving
//!    change (a hidden float, an ordering bug, a silent re-pin).
//!
//! 2. **Law-presence invariants** (`tests/regression_laws.rs`) — for every newly
//!    wired law toggle: (a) its `Config` field / `apply()` key EXISTS and defaults
//!    to the pinned value, and (b) flipping it changes ≥1 audited output. Because
//!    the journal digest is seeded with the whole-`Config` hash (§19 strategy
//!    identity), flipping ANY law toggle must move the golden digest — proving the
//!    law is still part of the strategy identity and has not been dead-coded out of
//!    it. A curated subset additionally proves a DECISION-level effect (net,
//!    admitted, reject codes) so a law wired only into the seed is caught too.
//!
//! 3. **Fail-closed invariants** (`tests/regression_failclosed.rs`) — the
//!    properties that must never regress: no `RunMode::Live` variant is
//!    constructible; `promotion_readiness` never returns `live_probe_eligible` on a
//!    pure paper/replay run; absent/thin evidence stays UNKNOWN on the
//!    sentiment/aggregator (source classification) and creator (classifier) paths;
//!    and bounded state stays ≤ cap when fed far past capacity.
//!
//! 4. **Decoder property / fuzz** (`tests/regression_decoder_fuzz.rs`) — the
//!    PumpSwap account / instruction / event decoders never panic on truncated,
//!    oversized, or garbage buffers at EVERY length boundary (exhaustive small
//!    lengths + a deterministic hash-driven byte fill; no RNG), and the
//!    encode→decode round-trip holds where an encoder exists.
//!
//! 5. **Cross-crate integration** (lives with the binaries it exercises:
//!    `crates/pq-evaluator/tests/smoke.rs` and
//!    `crates/pq-research-runner/tests/smoke.rs`) — the two canonical
//!    binaries run on a tiny JSONL fixture and emit the expected JSON keys plus a
//!    stable hash. These use `CARGO_BIN_EXE_*`, which only the defining crate's
//!    tests receive, so they cannot live in this crate.
//!
//! 6. **Regression manifest** (`src/baselines.rs` + `REGRESSION_MANIFEST.md`, checked by
//!    `tests/regression_manifest.rs`) — every pinned invariant in ONE place, with a
//!    test that the code manifest and the markdown manifest never drift apart.
//!
//! # Updating a baseline on an intentional re-pin
//! When a change to the engine is a *deliberate* behaviour change (an operator-
//! approved law reversal, a golden-tape re-pin), update the numbers in exactly one
//! place — [`baselines`] — mirror the same edit into `REGRESSION_MANIFEST.md`, and re-sync
//! the verbatim tape in [`golden_tape`] if `golden_digest.rs::drive` changed. The
//! manifest test then re-locks the new values. Never edit a magic number inside a
//! test body; the tests read [`baselines`].

#![forbid(unsafe_code)]

pub mod baselines;
pub mod golden_tape;

/// **The SOL-side reserve every hand-built fixture in this crate declares: a
/// pump.fun bonding curve at LAUNCH depth, 30 SOL of virtual SOL reserve**
/// (`pump_quant_app::curve_state::LAUNCH_VSOL_LAMPORTS`, and the shallowest depth
/// the venue can present).
///
/// **Re-pin #26 (2026-07-28).** Every fixture here used to declare `400_000_000` —
/// 0.4 SOL — which was harmless while the gate's impact model was a config constant
/// and became fatal the moment `pump_quant_app::cost_model::impact_den_for` began
/// deriving it per candidate as `vsol / 10_000`. A 0.1 SOL floor clip into a 0.4 SOL
/// pool is 2_500 bps of own impact PER LEG; every fixture market refused as
/// `EconomicallyUnviable`, and four regression tests whose whole subject is "a real
/// admission happened" failed on their own non-vacuity guards ("the tape must open at
/// least one position", "both markets must open") rather than on anything they were
/// written to protect.
///
/// A declared depth is now a PRICE. It is stated once, here, so the fixtures in this
/// crate cannot drift apart from each other or from the venue.
pub const FIXTURE_VSOL_LAMPORTS: u64 = 30_000_000_000;

/// The confirmed sellable depth every hand-built fixture here declares, just under
/// [`FIXTURE_VSOL_LAMPORTS`] — the "an on-chain confirm proves slightly less than the
/// pool holds" discipline the golden tape uses. Re-pin #26: was `500_000_000`.
pub const FIXTURE_SELLABLE_LAMPORTS: u64 = 29_000_000_000;
