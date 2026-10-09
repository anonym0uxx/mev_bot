//! Shared BUY / ADD / SELL settlement (one path for every paper fill).
//!
//! Every reconciled fill increment of ours — entry BUY, ADD, REDUCE/EXIT/protective SELL — goes through
//! [`SettlementLedger::settle`]. There is no second code path that moves cash or committed capital.
//! After EVERY step the ledger asserts `cash + committed == seed + realized` and refuses (named) any step
//! that would break it, leaving the ledger untouched.
//!
//! Fill reports are CUMULATIVE per `(leg, order id)`: a duplicate report is a no-op, a backwards report is a
//! named fault. Pending intent is not inventory: only `settle` changes tokens/cash.
//!
//! The network fee per landed leg is [`crate::shadow_pool::NETWORK_FEE_PER_LANDED_LEG_ESTIMATE`] — a LABELLED
//! ESTIMATE (measured p50), not observed truth. The caller passes the network fee it books per increment; the
//! ledger records it under `network_estimate` so reports never present it as an observed fee.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use crate::shadow_pool::LegKind;
pub use crate::shadow_pool::NETWORK_FEE_PER_LANDED_LEG_ESTIMATE;

/// Label attached to every network fee booked by this ledger.
pub const NETWORK_FEE_LABEL: &str = "network_fee:estimate_p50_10000_per_landed_leg";

/// One cumulative fill report for settlement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FillReport {
    pub mint: [u8; 32],
    pub leg: LegKind,
    pub order_id: u64,
    /// Cumulative tokens delivered (buy) / sold (sell) on this order.
    pub cum_tokens: u64,
    /// Cumulative venue amount: BUY = all-in venue spend (net_in + venue fees); SELL = venue-net proceeds.
    pub cum_venue_lamports: u64,
    /// Cumulative network fee booked for this order (estimate; see module docs).
    pub cum_network_lamports: u64,
}

/// Named settlement refusals. The ledger is unchanged when one is returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettleError {
    /// Cumulative report went backwards.
    Backwards,
    /// A buy needs more free cash than available.
    InsufficientCash,
    /// A sell of more tokens than held.
    InsufficientInventory,
    /// Arithmetic out of range.
    Overflow,
    /// `cash + committed != seed + realized` after the step (should be unreachable; kept as a hard guard).
    InvariantBroken,
}

impl SettleError {
    /// Stable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Backwards => "settle_refused:cumulative_backwards",
            Self::InsufficientCash => "settle_refused:insufficient_cash",
            Self::InsufficientInventory => "settle_refused:insufficient_inventory",
            Self::Overflow => "settle_refused:overflow",
            Self::InvariantBroken => "settle_refused:invariant_cash_committed_seed_realized",
        }
    }
}

/// Outcome of a settle call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    /// The increment was settled; `realized_delta` is the realized PnL change (sells only).
    Applied { tokens: u64, realized_delta: i128 },
    /// Identical to the recorded cumulative: nothing changed.
    Duplicate,
}

/// Per-mint holding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Holding {
    pub tokens: u64,
    /// Committed capital still attributed to the remaining tokens (spend + network of buys, less released).
    pub cost: i128,
}

/// The single settlement ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementLedger {
    pub seed: i128,
    pub cash: i128,
    pub committed: i128,
    pub realized: i128,
    /// Network fees booked so far (ESTIMATE, labelled [`NETWORK_FEE_LABEL`]).
    pub network_estimate: i128,
    pub holdings: BTreeMap<[u8; 32], Holding>,
    applied: BTreeMap<([u8; 32], LegKind, u64), (u64, u64, u64)>,
}

impl SettlementLedger {
    /// A fresh ledger with `seed` lamports of cash.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            seed: i128::from(seed),
            cash: i128::from(seed),
            committed: 0,
            realized: 0,
            network_estimate: 0,
            holdings: BTreeMap::new(),
            applied: BTreeMap::new(),
        }
    }

    /// `cash + committed == seed + realized`.
    #[must_use]
    pub fn invariant_holds(&self) -> bool {
        self.cash + self.committed == self.seed + self.realized
    }

    /// THE settlement path for BUY, ADD and SELL increments.
    ///
    /// # Errors
    /// [`SettleError`]; the ledger is unchanged on error.
    pub fn settle(&mut self, r: FillReport) -> Result<Settled, SettleError> {
        let key = (r.mint, r.leg, r.order_id);
        let prev = self.applied.get(&key).copied().unwrap_or((0, 0, 0));
        let cur = (r.cum_tokens, r.cum_venue_lamports, r.cum_network_lamports);
        if cur == prev {
            return Ok(Settled::Duplicate);
        }
        if cur.0 < prev.0 || cur.1 < prev.1 || cur.2 < prev.2 {
            return Err(SettleError::Backwards);
        }
        let dt = cur.0 - prev.0;
        let dv = i128::from(cur.1 - prev.1);
        let dn = i128::from(cur.2 - prev.2);
        let mut next = self.clone();
        let h = next.holdings.entry(r.mint).or_default();
        let realized_delta;
        if matches!(r.leg, LegKind::Sell) {
            if dt > h.tokens {
                return Err(SettleError::InsufficientInventory);
            }
            // Release cost pro rata; the last token releases the whole remainder (no stranded basis).
            let released = if dt == h.tokens {
                h.cost
            } else {
                h.cost
                    .checked_mul(i128::from(dt))
                    .ok_or(SettleError::Overflow)?
                    / i128::from(h.tokens)
            };
            h.tokens -= dt;
            h.cost -= released;
            next.committed -= released;
            next.cash += dv - dn;
            realized_delta = dv - dn - released;
            next.realized += realized_delta;
        } else {
            let out = dv + dn;
            if out > next.cash {
                return Err(SettleError::InsufficientCash);
            }
            h.tokens = h.tokens.checked_add(dt).ok_or(SettleError::Overflow)?;
            h.cost += out;
            next.cash -= out;
            next.committed += out;
            realized_delta = 0;
        }
        next.network_estimate += dn;
        if next
            .holdings
            .get(&r.mint)
            .is_some_and(|h| h.tokens == 0 && h.cost == 0)
        {
            next.holdings.remove(&r.mint);
        }
        next.applied.insert(key, cur);
        if !next.invariant_holds() {
            return Err(SettleError::InvariantBroken);
        }
        *self = next;
        Ok(Settled::Applied {
            tokens: dt,
            realized_delta,
        })
    }
}
