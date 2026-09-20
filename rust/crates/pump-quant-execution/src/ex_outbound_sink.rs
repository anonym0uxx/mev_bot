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
//! ## Operator refs
//! - §24(b): paper/replay mode is byte-identical to pre-junction — the sink is
//!   `None` (or `NoopSink`) and `on_admit` is a no-op.
//! - §36: the failure taxonomy (Construction, Guard, StateDrift, Route,
//!   ProgramVersionDrift) is the classification the sink returns.
//! - §41: construction parity — the sink must refuse to build if the
//!   LayoutRegistry has no verified fixture for the requested layout.

/// A trade admitted by the engine's gate. This is the payload the outbound
/// sink receives — the minimum the junction needs to fetch state and build.
// NOT `Eq`: the record now carries the model's price limit as an `f64`, and a float has no
// total equality. `PartialEq` is what the callers use.
#[derive(Debug, Clone, Copy, PartialEq)]
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
    /// The trading brain's own `PRICE LIMIT` for this entry, in LAMPORTS PER RAW TOKEN — the
    /// same unit the corpus renders (established against a real row's `PRICE UNITS` line, not
    /// assumed). `None` when the completion carried none, which is 68% of trained BUYs.
    ///
    /// When present it governs `min_tokens_out`; the slippage budget is the fallback. See
    /// [`crate::price_anchor`].
    pub price_limit_lamports_per_raw_token: Option<f64>,
}

/// The outcome of an outbound submission attempt. The engine logs this for the
/// report; it does NOT feed it into a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboundOutcome {
    /// The transaction was built, signed, and submitted. The signature is the
    /// on-chain transaction signature (base58 in the junction impl).
    ///
    /// `submit_rpc_us` is the measured duration of the submit call alone — the
    /// network leg, excluding the state fetch and the local build/sign work that
    /// `LatencyTrace::submit_call_us` folds in. Real clock, live mode only.
    Accepted {
        signature: [u8; 64],
        submit_rpc_us: u64,
    },
    /// E3: the record was handed to an ASYNC worker and is being built/signed/
    /// submitted off the decision thread. `ticket` identifies it until the worker
    /// reports the real outcome back (the daemon drains those reports on a later
    /// tick and calls back into the engine). Nothing is known yet about whether the
    /// transaction landed — this is explicitly NOT a failure, and the engine must
    /// not roll the position back on it. The decision thread never blocks on the
    /// ~5-50 ms build+submit round trip.
    Queued { ticket: u64 },
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
/// Implementors are shared across threads: E3 runs the pipeline on a dedicated
/// worker, so `Send + Sync` is part of the contract, not an accident.
pub trait OutboundSink: Send + Sync {
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
        OutboundOutcome::Accepted {
            signature: [0u8; 64],
            submit_rpc_us: 0,
        }
    }
}
