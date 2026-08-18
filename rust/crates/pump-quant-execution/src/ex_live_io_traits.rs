//! Live I/O trait boundaries: the contract between the deterministic execution
//! crate and the concrete I/O implementations in the junction crate.
//!
//! ## Why traits, not concretes
//! The execution crate is std-only (§36, §24): it must compile and test against
//! fixtures alone, with zero network, zero file I/O, zero async runtime. The
//! live pipeline needs all three. Traits here define the seam: the execution
//! crate's `LiveOutboundSink` calls through `dyn LiveSigner` / `dyn LiveSubmitter`
//! / `dyn LiveStateFetcher`, and the junction crate supplies the concrete
//! implementations backed by `pq-stream-capture` (ring ed25519, ureq JSON-RPC,
//! Helius Sender).
//!
//! ## Latency design
//! The hot path (`on_admit`) is the latency-critical surface. Every trait
//! method is synchronous (no `async`): the Solana SVM runtime is synchronous,
//! and an `async` indirection would add a runtime poll and waker allocation
//! per call — pure overhead on a path measured in microseconds.
//!
//! The state fetcher has two modes:
//! - **Prefetch**: `prefetch_state` is called on a background updater thread
//!   (the blockhash + bonding-curve cache). It warms the cache so the hot
//!   path never blocks on an RPC round-trip.
//! - **Hot path**: `fetch_state_hot` returns cached state or fetches synchronously
//!   as a fallback. The cached path is ~0ms; the fallback is ~50-100ms.
//!
//! ## Constitution refs
//! - §24(b): paper/replay mode is byte-identical — these traits are only
//!   implemented in live mode; paper mode uses `NoopSink`.
//! - §36: the failure taxonomy (Construction, StateFetch, Signer, Sender) maps
//!   directly to `LiveSigner::sign` and `LiveSubmitter::submit` error variants.
//! - §41: construction parity — the state fetcher must return decoded on-chain
//!   facts, not placeholders.

use pump_quant_protocol::venue_accounts::PumpCurveCtx;

// ---------------------------------------------------------------------------
// State fetch — decoded on-chain facts for tx_build
// ---------------------------------------------------------------------------

/// Decoded on-chain state needed to build a pump.fun transaction.
///
/// Every field is a *decoded on-chain fact*, fetched via RPC `getAccountInfo`
/// and decoded by the protocol crate's decoders. No placeholders: a field that
/// cannot be decoded is a `StateFetchError`, not a zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveCurveState {
    /// The decoded bonding-curve context (mint, user, fee_recipient, creator,
    /// token_program, is_cashback_coin, quote_mint).
    pub curve_ctx: PumpCurveCtx,
    /// The curve's virtual reserves for slippage computation.
    pub virtual_sol_reserves: u64,
    /// The curve's virtual token reserves.
    pub virtual_token_reserves: u64,
    /// Whether the curve is complete (graduated to pumpswap). A complete curve
    /// cannot be traded via the bonding-curve program — this is a hard refusal.
    pub is_complete: bool,
    /// The real-time slot at which this state was observed.
    pub observed_slot: u64,
    /// The 8 buyback_fee_recipients from the Global account (offset 741).
    /// Used by the live sink to select the BuybackVault PDA for the fee-tail.
    pub buyback_fee_recipients: [[u8; 32]; 8],
}

/// The recent blockhash + slot, fetched via RPC `getLatestBlockhash`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveBlockhash {
    /// The 32-byte recent blockhash.
    pub blockhash: [u8; 32],
    /// The slot at which this blockhash was observed.
    pub slot: u64,
}

/// Why a state fetch failed. Maps to `OutboundOutcome::StateFetch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateFetchError {
    /// The RPC returned an error or timed out.
    RpcError(String),
    /// The bonding curve account was not found (deleted / not yet created).
    AccountNotFound(String),
    /// The account was found but could not be decoded.
    DecodeError(String),
    /// The bonding curve is complete — graduated to pumpswap. Buying via the
    /// bonding-curve program is impossible; selling must use pumpswap.
    CurveComplete,
    /// The blockhash was all-zero (RPC returned a degenerate response).
    ZeroBlockhash,
}

/// The state fetcher: prefetches and returns decoded on-chain state.
///
/// Implementations back this with a real RPC client. The hot path calls
/// `fetch_state_hot`; a background thread calls `prefetch_state` to warm the
/// cache.
pub trait LiveStateFetcher: Send + Sync {
    /// Fetch the bonding-curve state for `mint`, signed by `user`.
    ///
    /// This is the hot-path entry. When the cache is warm (prefetched), this
    /// returns in ~0ms. When cold, it falls back to a synchronous RPC
    /// round-trip (~50-100ms).
    fn fetch_state_hot(&self, mint: &[u8; 32], user: &[u8; 32])
        -> Result<LiveCurveState, StateFetchError>;

    /// Warm the cache for `mint` / `user`. Called by a background updater
    /// thread, not the hot path. Returns the fetched state so the caller can
    /// also update the blockhash cache.
    fn prefetch_state(
        &self,
        mint: &[u8; 32],
        user: &[u8; 32],
    ) -> Result<LiveCurveState, StateFetchError>;

    /// Fetch the latest blockhash. The implementation should cache this and
    /// refresh it on a timer (every ~5s / 20 slots), so the hot path returns
    /// a cached value in ~0ms.
    fn latest_blockhash(&self) -> Result<LiveBlockhash, StateFetchError>;
}

// ---------------------------------------------------------------------------
// Signer — ed25519 signing of the compiled message
// ---------------------------------------------------------------------------

/// Why a sign operation failed. Maps to `OutboundOutcome::Signer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignError {
    /// The signing key is not loaded. This is the fail-closed default.
    KeyNotLoaded,
    /// The message was rejected (empty or too large).
    MessageRejected { bytes: usize, reason: String },
    /// The signature failed verification (self-test or post-sign check).
    VerificationFailed,
}

/// The signer: signs a compiled message with ed25519.
///
/// Implementations back this with `pq_stream_capture::signer::Signer` (ring
/// ed25519). The sign operation is pure compute (~100μs) — no I/O.
pub trait LiveSigner: Send + Sync {
    /// Sign `message_bytes` (the compiled message wire bytes), returning the
    /// 64-byte ed25519 signature.
    ///
    /// This is the only I/O-adjacent operation on the hot path that is NOT
    /// network I/O — it is pure CPU. The trait is synchronous because ed25519
    /// signing is a single `ring::sign` call with no await point.
    fn sign(&self, message_bytes: &[u8]) -> Result<[u8; 64], SignError>;

    /// The 32-byte public key this signer signs for. Not a secret.
    fn public_key(&self) -> [u8; 32];
}

// ---------------------------------------------------------------------------
// Submitter — Helius Sender / RPC submission of the wire transaction
// ---------------------------------------------------------------------------

/// Why a submission failed. Maps to `OutboundOutcome::Sender`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitError {
    /// The HTTP request failed (connection, TLS, timeout).
    HttpError(String),
    /// The endpoint returned a non-200 status or an error body.
    EndpointRejected(String),
    /// The transaction was submitted but not confirmed within the timeout.
    NotConfirmed(String),
    /// The signature was malformed (not a valid base58/64-byte signature).
    InvalidSignature,
}

/// The submitter: submits a wire transaction and returns the on-chain
/// transaction signature.
///
/// Implementations back this with `pq_stream_capture::sender::Sender` (Helius
/// Sender endpoint via ureq). This is the only network I/O on the hot path.
pub trait LiveSubmitter: Send + Sync {
    /// Submit `wire_tx` (the assembled wire transaction bytes) and return the
    /// 64-byte transaction signature.
    ///
    /// `is_buy` controls the `skipPreflight` RPC parameter:
    /// - **true** (buy): skipPreflight=true for lowest latency entry.
    /// - **false** (sell): skipPreflight=false so preflight catches sell failures
    ///   before they consume a blockhash slot and burn fees. The confirmation
    ///   feedback loop (Rev-19) catches confirmed failures, but preflight
    ///   prevents the wasted submission entirely.
    ///
    /// The implementation should:
    /// 1. Base64-encode the wire bytes.
    /// 2. POST to the Helius Sender endpoint with the tier + mev-protect suffix.
    /// 3. Parse the response for the transaction signature.
    /// 4. Optionally poll `getSignatureStatuses` for confirmation.
    ///
    /// For lowest latency, the implementation should NOT wait for full
    /// confirmation — it should return the signature on `sendTransaction`
    /// success and let the reconciliation layer confirm asynchronously.
    fn submit(&self, wire_tx: &[u8], is_buy: bool) -> Result<[u8; 64], SubmitError>;
}
