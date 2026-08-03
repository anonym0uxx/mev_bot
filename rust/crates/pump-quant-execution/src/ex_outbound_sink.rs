//! Leaf `ex_outbound_sink`: the engine → tx_build → signer → sender junction
//! contract.
//!
//! ## Why this exists
//! When the engine admits a trade, the paper path books a paper position. The
//! live path must instead fetch on-chain state, build the transaction, sign it,
//! and submit it. This module defines the *contract* between the engine and that
//! outbound pipeline, so the engine can call it without depending on the
//! transport layer (HTTP, signer, sender) that lives in the junction crate.
//!
//! ## Design
//! A trait `OutboundSink` with a single method `on_admit`. The engine calls it
//! after a position is admitted — **side-effect only**, never feeding a
//! decision. The golden-digest invariant holds because the sink's return value
//! is ignored by the engine's decision path (it is logged for the report only).
//!
//! The `NoopSink` is the inert default for paper/replay mode. It returns
//! `Accepted` with a zero signature — this is NOT a real submission, it is a
//! paper-mode placeholder that keeps the report's `outbound_outcomes` count
//! consistent. The zero signature is the signal that no real transaction was
//! sent.
//!
//! ## Constitution refs
//! - §24(b): paper/replay mode is byte-identical to pre-junction — the sink is
//!   `None` (or `NoopSink`) and `on_admit` is a no-op.
//! - §36: the failure taxonomy (Construction, Guard, StateDrift, Route,
//!   ProgramVersionDrift) is the classification the sink returns.
//! - §41: construction parity — the sink must refuse to build if the
//!   LayoutRegistry has no verified fixture for the requested layout.

/// A trade admitted by the engine's gate. This is the payload the outbound
/// sink receives — the minimum the junction needs to fetch state and build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmitRecord {
    /// The mint being traded.
    pub mint: [u8; 32],
    /// The fee-payer / signer pubkey.
    pub user: [u8; 32],
    /// Side: `true` = buy, `false` = sell.
    pub is_buy: bool,
    /// The entry size in lamports (base tokens for buy, quote for sell).
    pub size_lamports: u64,
    /// The entry price the engine computed (for slippage bounds).
    pub entry_price: u64,
    /// The max slippage in basis points the engine will tolerate.
    pub max_slippage_bps: u16,
}

/// The outcome of an outbound submission attempt. The engine logs this for the
/// report; it does NOT feed it into a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboundOutcome {
    /// The transaction was built, signed, and submitted. The signature is the
    /// on-chain transaction signature (base58 in the junction impl).
    Accepted { signature: [u8; 64] },
    /// The construction gate refused — the LayoutRegistry has no verified
    /// fixture for this layout. This is a §41 parity failure.
    Construction(String),
    /// The state-fetch layer failed — the bonding curve was complete, the
    /// account was missing, or the RPC returned an error.
    StateFetch(String),
    /// The transaction built but the signer refused (key not loaded, etc.).
    Signer(String),
    /// The transaction was signed but the sender rejected or timed out.
    Sender(String),
}

/// The junction contract. The engine holds an optional `&dyn OutboundSink` and
/// calls `on_admit` after a position is admitted. In paper/replay mode the sink
/// is `None`; in live mode it is the `OutboundJunction`.
pub trait OutboundSink {
    /// Execute the outbound pipeline for an admitted trade. The return value is
    /// logged for the report; it never feeds an engine decision (§24(b)).
    fn on_admit(&self, record: &AdmitRecord) -> OutboundOutcome;
}

/// The inert sink. `on_admit` returns `Accepted` with a zero signature — this
/// is NOT a real submission, it is a paper-mode placeholder that keeps the
/// report's `outbound_outcomes` count consistent. The zero signature is the
/// signal that no real transaction was sent.
pub struct NoopSink;

impl OutboundSink for NoopSink {
    fn on_admit(&self, _record: &AdmitRecord) -> OutboundOutcome {
        OutboundOutcome::Accepted { signature: [0u8; 64] }
    }
}
