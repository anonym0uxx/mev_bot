//! Canonical output types produced by the provider parsers.
//!
//! Responsibility: the provider-neutral shape that Helius / PumpPortal payloads
//! are decoded into, before they cross the ingestion boundary (ARCHITECTURE
//! rule 3: no provider-shaped type is visible downstream). All monetary and
//! quantity fields are integer / fixed-point (lamports, token base units) —
//! never floats (§22).

/// Direction of a decoded swap, from the *trader's* perspective.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradeDirection {
    /// Trader spends SOL to receive tokens.
    Buy,
    /// Trader spends tokens to receive SOL.
    Sell,
    /// Direction not applicable (e.g. a graduation/migration event) or unknown.
    Unknown,
}

/// The kind of on-chain event a `CanonicalTx` represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxKind {
    /// A bonding-curve / AMM buy or sell.
    Trade,
    /// A token graduation / migration (Raydium / PumpSwap pool creation).
    Graduation,
}

/// Which provider feed decoded this transaction. Retained on the canonical
/// record so downstream source-mix labeling (§16) is possible without leaking
/// provider wire formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// Legacy Helius WebSocket `logsSubscribe` feed.
    HeliusWsLogs,
    /// PumpPortal `subscribeNewToken` / `subscribeTokenTrade` feed.
    PumpPortal,
}

/// Canonical decoded transaction.
///
/// `sol_delta` and `token_delta` are signed and expressed from the trader's
/// perspective: a **buy** has `sol_delta < 0` (SOL spent) and
/// `token_delta > 0` (tokens received); a **sell** has `sol_delta > 0` and
/// `token_delta < 0`. Amounts are integer lamports / token base units — there
/// is no floating point anywhere in the decode path (§22). `i128` is used so
/// the full `u64` magnitude of a Solana amount fits with a sign bit to spare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalTx {
    /// Slot the observation was reported at (0 when the feed does not provide
    /// one, e.g. PumpPortal or Helius logs).
    pub slot: u64,
    /// 64-byte transaction signature.
    pub signature: [u8; 64],
    /// 32-byte token mint (`[0u8; 32]` when the feed cannot supply it, e.g.
    /// Helius `logsSubscribe`, which carries no account keys).
    pub mint: [u8; 32],
    /// 32-byte trader pubkey (`[0u8; 32]` when unavailable).
    pub trader: [u8; 32],
    /// Signed lamport flow for the trader (see type docs).
    pub sol_delta: i128,
    /// Signed token base-unit flow for the trader (see type docs).
    pub token_delta: i128,
    /// Virtual SOL reserves of the bonding curve, in lamports (0 if absent).
    pub vsol_reserves: u128,
    /// Virtual token reserves of the bonding curve, in base units (0 if absent).
    pub vtoken_reserves: u128,
    /// Market cap in lamports (0 if absent).
    pub market_cap_lamports: u128,
    /// Provider-supplied event timestamp in milliseconds, or 0 when the payload
    /// carries none. Never filled from the wall clock — timing injection is an
    /// edge-adapter concern, kept out of this deterministic path (§22).
    pub timestamp_ms: u64,
    /// Buy / sell / unknown direction.
    pub direction: TradeDirection,
    /// Trade vs graduation.
    pub kind: TxKind,
    /// Which feed decoded this record.
    pub source: SourceKind,
}
