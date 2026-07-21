//! Leaf `ex_bundle_assemble`: Jito bundle ordering / validation.
//!
//! Ported from `mev/jito-bundle-builder.ts` `buildBundle`, which assembled a
//! Jito bundle as `[trade tx(s), tip tx]` — one or more trade transactions
//! followed by a single tip transfer — and rejected the bundle if any step
//! failed (`Bundle.addTransactions` / `addTipTx` returning an error). The Jito
//! block engine caps a bundle at 5 transactions (`new Bundle([], 5)`).
//!
//! ## Responsibility
//! Validate and order a set of signed transaction references into a Jito
//! bundle, or reject the set (returning `None`) if it violates the invariants.
//!
//! ## Invariants enforced (all must hold)
//! - Non-empty and at most [`MAX_BUNDLE_TXS`] (5) transactions.
//! - Every transaction is signed.
//! - Every transaction is within the [`MAX_TX_BYTES`] Solana packet limit.
//! - Exactly one tip transaction, and it is the **last** entry.
//! - At least one trade transaction precedes the tip.
//!
//! ## Constitution refs
//! - §22: integer byte/count bookkeeping only.
//! - Deterministic: pure function of the input slice.

/// Maximum transactions in a Jito bundle (block-engine limit).
pub const MAX_BUNDLE_TXS: usize = 5;

/// Maximum serialized size of a single Solana transaction (packet limit), bytes.
pub const MAX_TX_BYTES: usize = 1232;

/// Role of a transaction inside a bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxKind {
    /// A trade (e.g. the Pump.fun buy) transaction.
    Trade,
    /// The Jito tip transfer transaction.
    Tip,
}

/// A reference to a signed transaction destined for a bundle. This crate does
/// not build or sign real transactions (live I/O is out of scope); callers pass
/// the already-signed transaction's role, signed flag, and serialized length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignedTxRef {
    /// Whether this is a trade or the tip transaction.
    pub kind: TxKind,
    /// Whether the transaction has been signed.
    pub signed: bool,
    /// Serialized length of the transaction in bytes.
    pub bytes_len: usize,
}

/// A validated, ordered Jito bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bundle {
    /// Total number of transactions in the bundle.
    pub tx_count: usize,
    /// Index of the tip transaction (always `tx_count - 1`).
    pub tip_index: usize,
    /// Sum of all transaction sizes in bytes.
    pub total_bytes: usize,
}

/// Validate and assemble `txs` into a [`Bundle`], or return `None` if any
/// invariant is violated (see module docs).
pub fn assemble_bundle(txs: &[SignedTxRef]) -> Option<Bundle> {
    // Size bounds: non-empty, within the block-engine cap.
    if txs.is_empty() || txs.len() > MAX_BUNDLE_TXS {
        return None;
    }

    let last_idx = txs.len() - 1;
    let mut tip_count = 0usize;
    let mut trade_count = 0usize;
    let mut total_bytes = 0usize;

    for (i, tx) in txs.iter().enumerate() {
        // Every transaction must be signed and within the packet limit.
        if !tx.signed || tx.bytes_len == 0 || tx.bytes_len > MAX_TX_BYTES {
            return None;
        }
        total_bytes = total_bytes.checked_add(tx.bytes_len)?;

        match tx.kind {
            TxKind::Tip => {
                tip_count += 1;
                // The tip must be the final transaction.
                if i != last_idx {
                    return None;
                }
            }
            TxKind::Trade => {
                trade_count += 1;
            }
        }
    }

    // Exactly one tip (at the end) and at least one preceding trade.
    if tip_count != 1 || trade_count == 0 {
        return None;
    }

    Some(Bundle {
        tx_count: txs.len(),
        tip_index: last_idx,
        total_bytes,
    })
}
