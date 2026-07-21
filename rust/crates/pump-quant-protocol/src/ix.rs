//! pump.fun instruction **data** serialization (discriminator + args only).
//!
//! # Responsibility
//! Produce the raw instruction `data` byte vector for pump.fun `buy` and `sell`
//! instructions, ported byte-for-byte from `PumpTxBuilder.encodeBuyData` /
//! `encodeSellData`. This crate deliberately builds *only the data blob*: key
//! ordering, signing, compute-budget prefixes and submission are live-I/O
//! concerns and are out of scope (§ deterministic / live-I/O-out-of-scope).
//!
//! Wire layout for both instructions (24 bytes total):
//! ```text
//! offset  size  field
//! 0       8     discriminator (sha256("global:<name>")[..8])
//! 8       8     arg0 (u64 LE)
//! 16      8     arg1 (u64 LE)
//! ```
//!
//! # Constitution
//! * §22 — integer-only; amounts are `u64` lamports / token base units.
//! * Deterministic: identical params always serialize to identical bytes.

/// Anchor-style discriminator for `global:buy` (first 8 bytes of its sha256).
pub const BUY_DISCRIMINATOR: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];

/// Anchor-style discriminator for `global:sell` (first 8 bytes of its sha256).
pub const SELL_DISCRIMINATOR: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];

/// Total serialized length of a buy/sell instruction data blob.
pub const IX_DATA_LEN: usize = 8 + 8 + 8;

/// Parameters for a pump.fun `buy` instruction.
///
/// Mirrors the legacy `encodeBuyData(solAmount, minTokens)` argument order:
/// the on-chain `amount` field is the *minimum tokens* to receive (slippage
/// guard) and `max_sol_cost` is the maximum lamports to spend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuyParams {
    /// Minimum tokens out (on-chain `amount`), in token base units.
    pub min_tokens_out: u64,
    /// Maximum SOL cost (on-chain `max_sol_cost`), in lamports.
    pub max_sol_cost: u64,
}

/// Parameters for a pump.fun `sell` instruction.
///
/// Mirrors the legacy `encodeSellData(tokenAmount, minSolOut)` argument order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SellParams {
    /// Token amount to sell (on-chain `amount`), in token base units.
    pub token_amount: u64,
    /// Minimum SOL output (on-chain `min_sol_output`), in lamports.
    pub min_sol_out: u64,
}

/// Serialize the `data` bytes of a pump.fun `buy` instruction.
///
/// Layout: `BUY_DISCRIMINATOR` ++ `min_tokens_out` (u64 LE) ++ `max_sol_cost`
/// (u64 LE) — exactly `encodeBuyData`, which writes `minTokens` at offset 8 and
/// `solAmount` at offset 16.
///
/// # Constitution
/// §22 — integer-only, deterministic.
pub fn build_buy_ix(params: BuyParams) -> Vec<u8> {
    let mut data = Vec::with_capacity(IX_DATA_LEN);
    data.extend_from_slice(&BUY_DISCRIMINATOR);
    data.extend_from_slice(&params.min_tokens_out.to_le_bytes());
    data.extend_from_slice(&params.max_sol_cost.to_le_bytes());
    data
}

/// Serialize the `data` bytes of a pump.fun `sell` instruction.
///
/// Layout: `SELL_DISCRIMINATOR` ++ `token_amount` (u64 LE) ++ `min_sol_out`
/// (u64 LE) — exactly `encodeSellData`.
///
/// # Constitution
/// §22 — integer-only, deterministic.
pub fn build_sell_ix(params: SellParams) -> Vec<u8> {
    let mut data = Vec::with_capacity(IX_DATA_LEN);
    data.extend_from_slice(&SELL_DISCRIMINATOR);
    data.extend_from_slice(&params.token_amount.to_le_bytes());
    data.extend_from_slice(&params.min_sol_out.to_le_bytes());
    data
}
