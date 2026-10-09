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
    /// Cumulative OTHER fixed costs of the landed leg(s) that are not venue fees and not the network estimate:
    /// the configured tip and (first buy only) the ATA rent deposit. Out of cash on a buy, deducted from
    /// proceeds on a sell, exactly like the network fee, but never mixed into the labelled network estimate.
    pub cum_fixed_lamports: u64,
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
    /// A void was asked for an entry that is not the sole, unvoided content of the mint's holding.
    VoidNotSoleEntry,
    /// A report for an order that was voided (authority: never filled).
    Voided,
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
            Self::VoidNotSoleEntry => "settle_refused:void_entry_not_sole_holding",
            Self::Voided => "settle_refused:order_voided_never_filled",
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
    /// Fixed (tip + ATA rent) costs booked so far (configured values, not observed fees).
    pub fixed_costs: i128,
    applied: BTreeMap<([u8; 32], LegKind, u64), (u64, u64, u64, u64)>,
    /// Entry orders voided by [`Self::void_entry`] (their applied record is kept so no report can re-apply them).
    voided: std::collections::BTreeSet<([u8; 32], LegKind, u64)>,
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
            fixed_costs: 0,
            holdings: BTreeMap::new(),
            applied: BTreeMap::new(),
            voided: std::collections::BTreeSet::new(),
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
        if self.voided.contains(&key) {
            return Err(SettleError::Voided);
        }
        let prev = self.applied.get(&key).copied().unwrap_or((0, 0, 0, 0));
        let cur = (
            r.cum_tokens,
            r.cum_venue_lamports,
            r.cum_network_lamports,
            r.cum_fixed_lamports,
        );
        if cur == prev {
            return Ok(Settled::Duplicate);
        }
        if cur.0 < prev.0 || cur.1 < prev.1 || cur.2 < prev.2 || cur.3 < prev.3 {
            return Err(SettleError::Backwards);
        }
        let dt = cur.0 - prev.0;
        let dv = i128::from(cur.1 - prev.1);
        // Network estimate and fixed costs move cash identically; they are booked under separate labels.
        let dnet = i128::from(cur.2 - prev.2);
        let dfix = i128::from(cur.3 - prev.3);
        let dn = dnet + dfix;
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
        next.network_estimate += dnet;
        next.fixed_costs += dfix;
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

impl SettlementLedger {
    /// The cumulative `(tokens, venue, network, fixed)` already settled for `(mint, leg, order_id)`
    /// (zeros when none). The engine adds its reconciled INCREMENT to this to form the next cumulative report.
    #[must_use]
    pub fn cumulative(&self, mint: &[u8; 32], leg: LegKind, order_id: u64) -> (u64, u64, u64, u64) {
        self.applied
            .get(&(*mint, leg, order_id))
            .copied()
            .unwrap_or((0, 0, 0, 0))
    }

    /// Settle one reconciled INCREMENT on top of what this ledger already holds for the order (the one path:
    /// it forms the cumulative report and calls [`Self::settle`]).
    ///
    /// # Errors
    /// [`SettleError`]; the ledger is unchanged on error.
    #[allow(clippy::too_many_arguments)]
    pub fn settle_increment(
        &mut self,
        mint: [u8; 32],
        leg: LegKind,
        order_id: u64,
        tokens: u64,
        venue: u64,
        network: u64,
        fixed: u64,
    ) -> Result<Settled, SettleError> {
        let (t, v, n, f) = self.cumulative(&mint, leg, order_id);
        let add = |a: u64, b: u64| a.checked_add(b).ok_or(SettleError::Overflow);
        self.settle(FillReport {
            mint,
            leg,
            order_id,
            cum_tokens: add(t, tokens)?,
            cum_venue_lamports: add(v, venue)?,
            cum_network_lamports: add(n, network)?,
            cum_fixed_lamports: add(f, fixed)?,
        })
    }

    /// VOID an entry BUY that authority says never filled. Allowed only when the mint's holding is EXACTLY what
    /// this one order settled (no ADD, no SELL, no other entry touched it): the holding is removed, its whole cost
    /// returns to cash, and the network estimate / fixed costs it booked are reversed. The applied record is kept
    /// (and marked voided) so a late duplicate report of the same order can never re-apply it.
    ///
    /// # Errors
    /// [`SettleError::VoidNotSoleEntry`] when anything else touched the holding; the ledger is unchanged.
    pub fn void_entry(&mut self, mint: [u8; 32], order_id: u64) -> Result<(), SettleError> {
        let key = (mint, LegKind::Entry, order_id);
        let Some(&(t, v, n, f)) = self.applied.get(&key) else {
            return Err(SettleError::VoidNotSoleEntry);
        };
        if self.voided.contains(&key) {
            return Err(SettleError::VoidNotSoleEntry);
        }
        let others = self
            .applied
            .keys()
            .any(|k| k.0 == mint && *k != key && !self.voided.contains(k));
        let cost = i128::from(v) + i128::from(n) + i128::from(f);
        let h = self.holdings.get(&mint).copied().unwrap_or_default();
        if others || h.tokens != t || h.cost != cost {
            return Err(SettleError::VoidNotSoleEntry);
        }
        let mut next = self.clone();
        next.holdings.remove(&mint);
        next.cash += cost;
        next.committed -= cost;
        next.network_estimate -= i128::from(n);
        next.fixed_costs -= i128::from(f);
        next.voided.insert(key);
        if !next.invariant_holds() {
            return Err(SettleError::InvariantBroken);
        }
        *self = next;
        Ok(())
    }

    /// Durable form (persisted inside the held ledger under `settlement`).
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::json;
        let hx = |b: &[u8; 32]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        json!({
            "seed": self.seed.to_string(),
            "cash": self.cash.to_string(),
            "committed": self.committed.to_string(),
            "realized": self.realized.to_string(),
            "network_estimate": self.network_estimate.to_string(),
            "fixed_costs": self.fixed_costs.to_string(),
            "holdings": self.holdings.iter().map(|(m, h)| json!([hx(m), h.tokens, h.cost.to_string()])).collect::<Vec<_>>(),
            "applied": self.applied.iter().map(|((m, l, id), (t, v, n, f))| json!([hx(m), l.code(), id, t, v, n, f])).collect::<Vec<_>>(),
            "voided": self.voided.iter().map(|(m, l, id)| json!([hx(m), l.code(), id])).collect::<Vec<_>>(),
        })
    }

    /// Parse the durable form. Malformed input is an error, never an empty ledger. The invariant must hold.
    ///
    /// # Errors
    /// A static reason.
    pub fn from_json(v: &serde_json::Value) -> Result<Self, &'static str> {
        let big = |k: &'static str| v[k].as_str().and_then(|s| s.parse::<i128>().ok()).ok_or(k);
        let unhex = |s: &str| -> Option<[u8; 32]> {
            if s.len() != 64 {
                return None;
            }
            let mut o = [0u8; 32];
            for (i, b) in o.iter_mut().enumerate() {
                *b = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
            }
            Some(o)
        };
        let mut out = Self {
            seed: big("seed")?,
            cash: big("cash")?,
            committed: big("committed")?,
            realized: big("realized")?,
            network_estimate: big("network_estimate")?,
            fixed_costs: big("fixed_costs")?,
            holdings: BTreeMap::new(),
            applied: BTreeMap::new(),
            voided: std::collections::BTreeSet::new(),
        };
        for h in v["holdings"].as_array().ok_or("holdings")? {
            let m = h[0].as_str().and_then(unhex).ok_or("holdings.mint")?;
            let tokens = h[1].as_u64().ok_or("holdings.tokens")?;
            let cost = h[2]
                .as_str()
                .and_then(|s| s.parse::<i128>().ok())
                .ok_or("holdings.cost")?;
            out.holdings.insert(m, Holding { tokens, cost });
        }
        for a in v["applied"].as_array().ok_or("applied")? {
            let m = a[0].as_str().and_then(unhex).ok_or("applied.mint")?;
            let l = a[1]
                .as_str()
                .and_then(LegKind::from_code)
                .ok_or("applied.leg")?;
            let n = |i: usize| a[i].as_u64().ok_or("applied.value");
            out.applied
                .insert((m, l, n(2)?), (n(3)?, n(4)?, n(5)?, n(6)?));
        }
        // `voided` is absent from ledgers written before voids existed: absent = none voided. Present but malformed
        // is refused.
        if let Some(vs) = v.get("voided") {
            for a in vs.as_array().ok_or("voided")? {
                let m = a[0].as_str().and_then(unhex).ok_or("voided.mint")?;
                let l = a[1]
                    .as_str()
                    .and_then(LegKind::from_code)
                    .ok_or("voided.leg")?;
                let id = a[2].as_u64().ok_or("voided.id")?;
                if !out.applied.contains_key(&(m, l, id)) {
                    return Err("voided.unknown_order");
                }
                out.voided.insert((m, l, id));
            }
        }
        if !out.invariant_holds() {
            return Err("invariant");
        }
        Ok(out)
    }
}
