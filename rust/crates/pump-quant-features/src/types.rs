//! Shared domain types for the feature spine.
//!
//! Responsibility: define the leakage-relevant primitives — event identity,
//! aggressor side, the canonical [`TradeEvent`], completeness tri-state, and the
//! fixed-point price scale — used by every module. Constitution 17 (event
//! schemas), 20 (time-safe features), 22 (integer/fixed-point only).

/// Stable identifier of a source event (constitution 17). Ties every derived
/// feature/bar back to the raw on-chain observation it was computed from, so
/// provenance and point-in-time correctness remain auditable.
pub type EventId = u64;

/// Version of a feature schema (constitution 20). Bumped whenever the meaning of
/// a served value changes so that live and replay never conflate schema versions.
pub type FeatureVersion = u32;

/// Fixed-point scale applied to every price in this crate (constitution 22).
///
/// A `price_fp` value equals `real_price * PRICE_SCALE`, held as [`i128`]. Using a
/// fixed integer scale keeps all price math exact and float-free. `1e9` gives nine
/// fractional decimal places, ample for lamport-denominated AMM prices while
/// leaving enormous [`i128`] head-room for weighted sums.
pub const PRICE_SCALE: i128 = 1_000_000_000;

/// Aggressor side of a swap (constitution 21.7). In a constant-product AMM the
/// "side" is the direction of the swap: `Buy` removes base token from the pool
/// (quote in), `Sell` adds base token (quote out). Order-flow-intent features are
/// built from this direction, not from any resting-order book.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    /// Aggressor bought the base token (net quote inflow to the pool).
    Buy,
    /// Aggressor sold the base token (net quote outflow from the pool).
    Sell,
}

/// A single canonical swap/trade, the atomic input to bars and microstructure
/// features (constitution 21.6/21.7).
///
/// Responsibility: carry exactly the leakage-relevant facts of one decoded swap.
/// `ts_ns` is the *information time* of the event (when the fact became known),
/// used for point-in-time ordering. `price_fp` is the reserve-derived execution
/// price in [`PRICE_SCALE`] units. `base_qty`/`quote_qty` are integer base-unit /
/// lamport amounts. All fields are integers — no floating point (constitution 22).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TradeEvent {
    /// Provenance link to the raw observation (constitution 17).
    pub event_id: EventId,
    /// Information time in nanoseconds — the time this fact became observable.
    pub ts_ns: u64,
    /// Reserve-derived execution price, scaled by [`PRICE_SCALE`].
    pub price_fp: i128,
    /// Token base units transacted.
    pub base_qty: u64,
    /// Quote units (e.g. lamports) transacted.
    pub quote_qty: u64,
    /// Aggressor side of the swap.
    pub side: Side,
}

impl TradeEvent {
    /// Signed quote flow of this trade: `+quote_qty` for a buy, `-quote_qty` for a
    /// sell (constitution 21.7 CVD). Widened to [`i128`] so no single trade can
    /// overflow the accumulator.
    #[must_use]
    pub fn signed_quote(&self) -> i128 {
        let q = i128::from(self.quote_qty);
        match self.side {
            Side::Buy => q,
            Side::Sell => -q,
        }
    }

    /// Signed base flow of this trade: `+base_qty` for a buy, `-base_qty` for a
    /// sell (constitution 21.7 order-flow imbalance). Widened to [`i128`].
    #[must_use]
    pub fn signed_base(&self) -> i128 {
        let b = i128::from(self.base_qty);
        match self.side {
            Side::Buy => b,
            Side::Sell => -b,
        }
    }
}

/// Completeness tri-state of a served value (constitution 20).
///
/// Missing or partial inputs must become an explicit status rather than a silent
/// zero or a fabricated value. This propagates through the feature so a consumer
/// can refuse to trade on incomplete information.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Completeness {
    /// All required inputs were present at computation time.
    Complete,
    /// Some inputs were known-missing; the value is a partial estimate.
    Incomplete,
    /// Presence of inputs could not be established.
    Unknown,
}

/// Errors surfaced by the streaming feature reducers.
///
/// Responsibility: make ordering and domain violations explicit rather than
/// panicking or silently corrupting state (constitution 22 determinism).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureError {
    /// A trade arrived with `ts_ns` strictly earlier than the previously ingested
    /// trade. Bar/window reducers require non-decreasing information time to
    /// preserve point-in-time correctness (constitution 20).
    NonMonotonicTimestamp {
        /// Timestamp of the previously ingested trade.
        previous_ns: u64,
        /// Timestamp of the offending trade.
        offending_ns: u64,
    },
    /// A bar/window was configured with a zero interval, threshold, or capacity,
    /// which has no well-defined semantics.
    InvalidConfiguration,
}

impl core::fmt::Display for FeatureError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FeatureError::NonMonotonicTimestamp {
                previous_ns,
                offending_ns,
            } => write!(
                f,
                "non-monotonic timestamp: previous={previous_ns} offending={offending_ns}"
            ),
            FeatureError::InvalidConfiguration => write!(f, "invalid feature configuration"),
        }
    }
}

impl std::error::Error for FeatureError {}
