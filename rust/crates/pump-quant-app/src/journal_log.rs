//! The decision journal: a canonical, hashable record of what the engine did.
//!
//! Determinism is only useful if it is *checkable*. Every material decision — a
//! promotion, a gate verdict, a fill, a reflection weight move — is appended here in
//! a fixed integer encoding, and folded into a rolling FNV-1a hash using the real
//! `pump_quant_memory::hashing` primitives. Two runs over the same events produce
//! byte-identical journals and therefore identical `digest()`s; a single divergence
//! (a non-determinism bug, an accidental wall-clock read) flips the hash. That is
//! what makes replay a correctness authority rather than a demo (§54).

use pump_quant_memory::hashing::{fnv1a_64, push_bytes, push_u64};

/// A single journaled decision, in the order the engine took it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// A candidate was promoted to the gate at a given rank.
    Promoted { mint: [u8; 32], lane: u8, rank: u64 },
    /// The gate admitted a candidate at a chosen size.
    Admitted { mint: [u8; 32], size_lamports: u64 },
    /// The gate rejected a candidate; `reason` is a stable small code.
    Rejected { mint: [u8; 32], reason: u8 },
    /// A paper scalp realized a signed net PnL.
    Filled {
        mint: [u8; 32],
        net_pnl_lamports: i128,
    },
    /// A reflection pass moved a lane weight.
    Reweighted {
        lane: u8,
        before_bp: u32,
        after_bp: u32,
    },
}

impl Decision {
    /// A stable 1-byte tag so the encoding is unambiguous across variants.
    const fn tag(&self) -> u8 {
        match self {
            Decision::Promoted { .. } => 1,
            Decision::Admitted { .. } => 2,
            Decision::Rejected { .. } => 3,
            Decision::Filled { .. } => 4,
            Decision::Reweighted { .. } => 5,
        }
    }

    fn encode(&self, buf: &mut Vec<u8>) {
        buf.push(self.tag());
        match *self {
            Decision::Promoted { mint, lane, rank } => {
                push_bytes(buf, &mint);
                buf.push(lane);
                push_u64(buf, rank);
            }
            Decision::Admitted {
                mint,
                size_lamports,
            } => {
                push_bytes(buf, &mint);
                push_u64(buf, size_lamports);
            }
            Decision::Rejected { mint, reason } => {
                push_bytes(buf, &mint);
                buf.push(reason);
            }
            Decision::Filled {
                mint,
                net_pnl_lamports,
            } => {
                push_bytes(buf, &mint);
                // Encode the signed 128-bit PnL as its two's-complement bytes so the
                // hash is exact and sign-stable.
                push_bytes(buf, &net_pnl_lamports.to_le_bytes());
            }
            Decision::Reweighted {
                lane,
                before_bp,
                after_bp,
            } => {
                buf.push(lane);
                push_u64(buf, before_bp as u64);
                push_u64(buf, after_bp as u64);
            }
        }
    }
}

/// An append-only journal of decisions with a canonical rolling hash.
#[derive(Clone, Debug, Default)]
pub struct DecisionJournal {
    records: Vec<Decision>,
    buf: Vec<u8>,
}

impl DecisionJournal {
    /// A fresh, empty journal.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a decision.
    pub fn record(&mut self, d: Decision) {
        d.encode(&mut self.buf);
        self.records.push(d);
    }

    /// The number of journaled decisions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether the journal is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// The recorded decisions in order.
    #[must_use]
    pub fn records(&self) -> &[Decision] {
        &self.records
    }

    /// The canonical FNV-1a digest of the whole journal. Identical across any two
    /// runs that took identical decisions.
    #[must_use]
    pub fn digest(&self) -> u64 {
        fnv1a_64(&self.buf)
    }
}
