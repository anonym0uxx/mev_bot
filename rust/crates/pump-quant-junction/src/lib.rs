//! Junction crate: translate live ingest feeds into AppEvent streams with
//! structural provenance (criterion 65). Bridges pump-quant-ingest (canonical
//! provider-neutral types) and pump-quant-app (engine AppEvent).
//!
//! The engine stays testable against fixtures alone — this crate owns the
//! live-source wiring. parse_events (text-file replay) is untouched and remains
//! the golden-digest certification path.
//!
//! §24/criterion 109: no async on the decode path, no floats in money paths,
//! no panics, no per-event allocation. Bounded queue with explicit backpressure
//! (§6/§99). hotpath_lint covers this crate (see lint_rules.yaml).

#![warn(
    clippy::all,
    clippy::integer_arithmetic,
    clippy::cast_possible_truncation,
)]

// ─── Provenance ───────────────────────────────────────────────────────────

/// Which live subscription produced this event. Structural, not a flag —
/// every AppEvent leaving the junction carries its origin so criterion 65
/// (live observation provenance) is satisfied by construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProvenanceSource {
    /// PumpPortal `subscribeTokenTrade` WebSocket feed.
    PumpPortalTrade,
    /// Helius `accountSubscribe` on the bonding-curve PDA (free tier, proven).
    HeliusAccountSubscribe,
    /// Helius `transactionSubscribe` (Developer tier — upgrade, not a gate).
    HeliusTransactionSubscribe,
}

/// A live-observed AppEvent with structural provenance. The engine consumes
/// the inner `AppEvent`; the provenance travels with it so downstream code
/// can distinguish live from replay without a side-channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProvenancedEvent {
    /// The engine-level event.
    pub event: pump_quant_app::event::AppEvent,
    /// Which subscription produced it.
    pub source: ProvenanceSource,
    /// Slot at which the observation was made (0 when the feed carries none).
    pub slot: u64,
    /// Whether this event came from a live feed or a replay.
    pub is_live: bool,
}

// ─── Backpressure ─────────────────────────────────────────────────────────

/// Bounded queue capacity for the ingest→junction channel.
pub const JUNCTION_QUEUE_CAP: usize = 4096;

/// Overflow outcome when the bounded queue is full. §6 forbids silent drops;
/// the counter is journalled and surfaced by the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverflowStats {
    /// Total events dropped since start.
    pub dropped: u64,
    /// Slot at which the last drop occurred.
    pub last_drop_slot: u64,
}

// ─── Translation ──────────────────────────────────────────────────────────

pub mod translate;
pub mod decode;
pub mod queue;

pub use queue::BoundedJunctionQueue;
pub use translate::canonical_tx_to_market_trade;
