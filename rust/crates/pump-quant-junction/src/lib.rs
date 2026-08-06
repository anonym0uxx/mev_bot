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
    /// Derived from the delta between consecutive Helius `accountSubscribe`
    /// snapshots on a bonding-curve PDA — the reserve change IS the trade.
    /// Used when PumpPortal `subscribeTokenTrade` is unavailable (free tier).
    HeliusReserveDelta,
    /// Helius LaserStream gRPC `transactionSubscribe` — Geyser-fed, lowest
    /// latency, self-healing (SDK-internal `from_slot` resume). Primary
    /// canonical ingest lane per criterion 61.
    LaserStream,
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

// ─── Decode provenance (blocker 2 structural fix) ─────────────────────────

/// A `real_sol` value that can ONLY come from a decoded bonding-curve account
/// snapshot, never from arithmetic on a venue-reported figure.
///
/// The inner `u64` is private. The sole constructor is [`from_curve`], which
/// takes a `&PumpCurve` — a type that itself can only be produced by
/// `pump_quant_protocol::decode::decode_pump_curve`. A derived `u64`
/// (`vsol - 30 SOL`) cannot construct this type; the mistake is
/// unrepresentable, not merely tested for.
///
/// The junction's public API accepts `&PumpCurve` for OnchainConfirm
/// construction, never `real_sol: u64`. The text-file replay path
/// (`parse_events`) constructs `AppEvent::OnchainConfirm` with bare `u64`
/// directly — that path is certified by the golden digest and is untouched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodedRealSol(u64);

impl DecodedRealSol {
    /// Construct from a decoded pump.fun bonding-curve account snapshot.
    /// This is the ONLY public constructor — `PumpCurve` can only come from
    /// `decode_pump_curve`, so the value's provenance is structural.
    pub fn from_curve(curve: &pump_quant_protocol::decode::PumpCurve) -> Self {
        Self(curve.real_sol)
    }

    /// Extract the inner `u64` for engine consumption. This is a one-way
    /// extraction — the consumer gets the value, but cannot re-construct a
    /// `DecodedRealSol` from it.
    pub fn into_lamports(self) -> u64 {
        self.0
    }
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
pub mod pumpportal;
pub mod reserve_delta;
pub mod state_fetch;
pub mod outbound;
pub mod laserstream;
pub mod trade_journal;
pub mod tape_export;
pub mod event_stream;
pub mod memory_bank;
pub mod autonomous_bridge;
#[cfg(test)] mod chaos_tests;

pub use queue::BoundedJunctionQueue;
pub use translate::canonical_tx_to_market_trade;
pub use translate::raw_token_metadata_to_event;
pub use pumpportal::{handle_trade_payload, handle_create_payload, handle_migration_payload};
pub use reserve_delta::{derive_market_trade_from_delta, ReserveSnapshot};
pub use laserstream::{
    classify_pump_instructions, instructions_to_events,
    parse_ndjson_line, LaserStreamTx, LaserStreamInstruction,
    LaserStreamUpdate, PumpInstruction, LaserStreamState,
};
