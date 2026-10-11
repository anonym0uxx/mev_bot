//! Graduation execution identity (curve -> pool migration) for our paper/live SELL attempts.
//!
//! RULES (each tested in `tests/exec_identity.rs`):
//! * An UNSUBMITTED intent may be re-routed from the curve to the pool, and only after the pool route was
//!   verified (`pool_verified`). The intent keeps its id; no attempt existed, so nothing is re-labelled.
//! * Once an attempt is SUBMITTED (or its outcome is UNCERTAIN) its venue is fixed forever. A curve attempt
//!   can never silently become a pool attempt. Its reservation (tokens it may dispose of) and its evidence
//!   (venue, slot, cumulative fill) stay attributable to it until DEFINITIVE reconciliation
//!   (`Landed` final / `Failed` / `Expired`).
//! * A REPLACEMENT needs an explicit new attempt identity (`AttemptId { intent, seq }`, `seq` strictly
//!   increasing) and is refused while any earlier attempt of the same intent is unresolved, so two attempts
//!   can never dispose of the same tokens (`no overlapping disposal`). Its quantity is the intent's remaining
//!   quantity after every earlier attempt's reconciled fill; Rust never resizes Qwen's quantity upward.
//! * Across ALL intents of a mint, open reservations never exceed held inventory.
//! * Everything persists and restores exactly; after a restart an attempt that was submitted stays
//!   submitted/uncertain (never reset to unsubmitted).
#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::shadow_pool::ShadowVenue;

/// Explicit attempt identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AttemptId {
    pub intent: u64,
    pub seq: u32,
}

/// Attempt status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptStatus {
    /// Sent; outcome not yet known.
    Submitted,
    /// Outcome unknown (timeout / restart / blockhash expiry not yet proven).
    Uncertain,
    /// Definitively reconciled with a final cumulative fill (may be partial).
    Final,
    /// Definitively did not land (fill 0) — e.g. proven failure or expiry.
    Failed,
}

impl AttemptStatus {
    const fn open(self) -> bool {
        matches!(self, Self::Submitted | Self::Uncertain)
    }
    const fn code(self) -> &'static str {
        match self {
            Self::Submitted => "submitted",
            Self::Uncertain => "uncertain",
            Self::Final => "final",
            Self::Failed => "failed",
        }
    }
    fn from_code(s: &str) -> Option<Self> {
        Some(match s {
            "submitted" => Self::Submitted,
            "uncertain" => Self::Uncertain,
            "final" => Self::Final,
            "failed" => Self::Failed,
            _ => return None,
        })
    }
}

fn venue_code(v: ShadowVenue) -> &'static str {
    match v {
        ShadowVenue::Curve => "curve",
        ShadowVenue::Pool => "pool",
    }
}

fn venue_from(s: &str) -> Option<ShadowVenue> {
    match s {
        "curve" => Some(ShadowVenue::Curve),
        "pool" => Some(ShadowVenue::Pool),
        _ => None,
    }
}

/// One attempt: its venue is fixed at submission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attempt {
    pub id: AttemptId,
    pub venue: ShadowVenue,
    /// Tokens this attempt may dispose of (reserved while open).
    pub reserved: u64,
    /// Cumulative tokens observed filled for this attempt (evidence).
    pub filled: u64,
    pub status: AttemptStatus,
    pub submit_slot: u64,
}

/// A sell intent (Qwen's decision; quantity never resized upward).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent {
    pub id: u64,
    pub mint: [u8; 32],
    pub quantity: u64,
    /// Route for the NEXT attempt (only changeable while no attempt is open).
    pub route: ShadowVenue,
    pub attempts: Vec<Attempt>,
}

impl Intent {
    fn filled(&self) -> u64 {
        self.attempts.iter().map(|a| a.filled).sum()
    }
    /// Remaining quantity after all fills so far.
    #[must_use]
    pub fn remaining(&self) -> u64 {
        self.quantity.saturating_sub(self.filled())
    }
    fn open_attempt(&self) -> Option<&Attempt> {
        self.attempts.iter().find(|a| a.status.open())
    }
    /// Whether no attempt was ever submitted.
    #[must_use]
    pub fn unsubmitted(&self) -> bool {
        self.attempts.is_empty()
    }
}

/// Named refusals (state unchanged when returned).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityError {
    UnknownIntent,
    DuplicateIntent,
    /// Re-route of an intent that already has an attempt (submitted, uncertain or reconciled).
    RerouteAfterSubmission,
    /// Re-route to the pool before the pool route was verified.
    PoolUnverified,
    /// A new attempt while an earlier attempt of the intent is unresolved.
    EarlierAttemptUnresolved,
    /// Attempt seq not strictly increasing / attempt identity mismatch.
    AttemptIdentity,
    /// The new attempt's venue differs from the intent route (route must be changed explicitly first).
    VenueMismatch,
    /// Reservation would exceed held inventory not already reserved (overlapping disposal).
    OverlappingDisposal,
    /// Nothing left to dispose of.
    NothingRemaining,
    /// Fill evidence beyond the reservation or going backwards.
    FillInconsistent,
    /// Attempt already definitively reconciled.
    AlreadyReconciled,
}

impl IdentityError {
    /// Stable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::UnknownIntent => "exec_identity:unknown_intent",
            Self::DuplicateIntent => "exec_identity:duplicate_intent",
            Self::RerouteAfterSubmission => "exec_identity:reroute_after_submission",
            Self::PoolUnverified => "exec_identity:pool_unverified",
            Self::EarlierAttemptUnresolved => "exec_identity:earlier_attempt_unresolved",
            Self::AttemptIdentity => "exec_identity:attempt_identity",
            Self::VenueMismatch => "exec_identity:venue_mismatch",
            Self::OverlappingDisposal => "exec_identity:overlapping_disposal",
            Self::NothingRemaining => "exec_identity:nothing_remaining",
            Self::FillInconsistent => "exec_identity:fill_inconsistent",
            Self::AlreadyReconciled => "exec_identity:already_reconciled",
        }
    }
}

/// All intents + attempts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AttemptBook {
    pub intents: BTreeMap<u64, Intent>,
}

impl AttemptBook {
    /// Register a new intent on `route`.
    ///
    /// # Errors
    /// `DuplicateIntent`.
    pub fn register(
        &mut self,
        id: u64,
        mint: [u8; 32],
        quantity: u64,
        route: ShadowVenue,
    ) -> Result<(), IdentityError> {
        if self.intents.contains_key(&id) {
            return Err(IdentityError::DuplicateIntent);
        }
        self.intents.insert(
            id,
            Intent {
                id,
                mint,
                quantity,
                route,
                attempts: Vec::new(),
            },
        );
        Ok(())
    }

    /// Tokens of `mint` reserved by OPEN attempts (submitted or uncertain).
    #[must_use]
    pub fn reserved(&self, mint: &[u8; 32]) -> u64 {
        self.intents
            .values()
            .filter(|i| &i.mint == mint)
            .flat_map(|i| i.attempts.iter())
            .filter(|a| a.status.open())
            .map(|a| a.reserved)
            .sum()
    }

    /// Re-route an UNSUBMITTED intent to the pool after verification.
    ///
    /// # Errors
    /// `RerouteAfterSubmission`, `PoolUnverified`, `UnknownIntent`.
    pub fn reroute_to_pool(&mut self, id: u64, pool_verified: bool) -> Result<(), IdentityError> {
        let i = self
            .intents
            .get_mut(&id)
            .ok_or(IdentityError::UnknownIntent)?;
        if !i.unsubmitted() {
            return Err(IdentityError::RerouteAfterSubmission);
        }
        if !pool_verified {
            return Err(IdentityError::PoolUnverified);
        }
        i.route = ShadowVenue::Pool;
        Ok(())
    }

    /// Route a REPLACEMENT attempt to the pool: allowed only when every earlier attempt of the intent is
    /// definitively reconciled and the pool route is verified. Earlier attempts keep their venue.
    ///
    /// # Errors
    /// `EarlierAttemptUnresolved`, `PoolUnverified`, `UnknownIntent`.
    pub fn route_replacement_to_pool(
        &mut self,
        id: u64,
        pool_verified: bool,
    ) -> Result<(), IdentityError> {
        let i = self
            .intents
            .get_mut(&id)
            .ok_or(IdentityError::UnknownIntent)?;
        if i.open_attempt().is_some() {
            return Err(IdentityError::EarlierAttemptUnresolved);
        }
        if !pool_verified {
            return Err(IdentityError::PoolUnverified);
        }
        i.route = ShadowVenue::Pool;
        Ok(())
    }

    /// Submit the next attempt with EXPLICIT identity `attempt` on `venue`, reserving the intent's remaining
    /// quantity. `held` = tokens of the mint held now.
    ///
    /// # Errors
    /// See [`IdentityError`].
    pub fn submit(
        &mut self,
        attempt: AttemptId,
        venue: ShadowVenue,
        held: u64,
        slot: u64,
    ) -> Result<AttemptId, IdentityError> {
        let reserved_now = {
            let i = self
                .intents
                .get(&attempt.intent)
                .ok_or(IdentityError::UnknownIntent)?;
            self.reserved(&i.mint)
        };
        let i = self
            .intents
            .get_mut(&attempt.intent)
            .ok_or(IdentityError::UnknownIntent)?;
        if i.open_attempt().is_some() {
            return Err(IdentityError::EarlierAttemptUnresolved);
        }
        let next_seq = i.attempts.last().map_or(0, |a| a.seq_next());
        if attempt.seq != next_seq {
            return Err(IdentityError::AttemptIdentity);
        }
        if venue != i.route {
            return Err(IdentityError::VenueMismatch);
        }
        let qty = i.remaining();
        if qty == 0 {
            return Err(IdentityError::NothingRemaining);
        }
        if qty > held.saturating_sub(reserved_now) {
            return Err(IdentityError::OverlappingDisposal);
        }
        i.attempts.push(Attempt {
            id: attempt,
            venue,
            reserved: qty,
            filled: 0,
            status: AttemptStatus::Submitted,
            submit_slot: slot,
        });
        Ok(attempt)
    }

    fn attempt_mut(&mut self, id: AttemptId) -> Result<&mut Attempt, IdentityError> {
        self.intents
            .get_mut(&id.intent)
            .ok_or(IdentityError::UnknownIntent)?
            .attempts
            .iter_mut()
            .find(|a| a.id == id)
            .ok_or(IdentityError::AttemptIdentity)
    }

    /// Mark an open attempt's outcome uncertain (timeout / restart). Reservation stays.
    ///
    /// # Errors
    /// `AlreadyReconciled`, identity errors.
    pub fn mark_uncertain(&mut self, id: AttemptId) -> Result<(), IdentityError> {
        let a = self.attempt_mut(id)?;
        if !a.status.open() {
            return Err(IdentityError::AlreadyReconciled);
        }
        a.status = AttemptStatus::Uncertain;
        Ok(())
    }

    /// Record observed partial fill evidence (cumulative) on an open attempt; it stays open.
    ///
    /// # Errors
    /// `FillInconsistent`, `AlreadyReconciled`.
    pub fn observe_fill(&mut self, id: AttemptId, cum_filled: u64) -> Result<(), IdentityError> {
        let a = self.attempt_mut(id)?;
        if !a.status.open() {
            return Err(IdentityError::AlreadyReconciled);
        }
        if cum_filled < a.filled || cum_filled > a.reserved {
            return Err(IdentityError::FillInconsistent);
        }
        a.filled = cum_filled;
        Ok(())
    }

    /// DEFINITIVE reconciliation: `Some(cum)` = landed with final cumulative fill `cum`; `None` = proven not
    /// landed (any previously observed fill must then be 0). Releases the reservation.
    ///
    /// # Errors
    /// `FillInconsistent`, `AlreadyReconciled`.
    pub fn reconcile(
        &mut self,
        id: AttemptId,
        final_fill: Option<u64>,
    ) -> Result<(), IdentityError> {
        let a = self.attempt_mut(id)?;
        if !a.status.open() {
            return Err(IdentityError::AlreadyReconciled);
        }
        match final_fill {
            Some(c) => {
                if c < a.filled || c > a.reserved {
                    return Err(IdentityError::FillInconsistent);
                }
                a.filled = c;
                a.status = AttemptStatus::Final;
            }
            None => {
                if a.filled != 0 {
                    return Err(IdentityError::FillInconsistent);
                }
                a.status = AttemptStatus::Failed;
            }
        }
        Ok(())
    }

    /// Graduation of `mint` observed: UNSUBMITTED curve intents become candidates for re-route (returned);
    /// open curve attempts are untouched and stay curve attempts.
    #[must_use]
    pub fn on_graduation(&self, mint: &[u8; 32]) -> Vec<u64> {
        self.intents
            .values()
            .filter(|i| &i.mint == mint && i.route == ShadowVenue::Curve && i.unsubmitted())
            .map(|i| i.id)
            .collect()
    }

    /// Restart: every open attempt becomes Uncertain (outcome unknown until reconciled); nothing is reset.
    pub fn on_restart(&mut self) {
        for i in self.intents.values_mut() {
            for a in &mut i.attempts {
                if a.status.open() {
                    a.status = AttemptStatus::Uncertain;
                }
            }
        }
    }

    /// Durable form.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let v: Vec<Value> = self
            .intents
            .values()
            .map(|i| {
                let at: Vec<Value> = i
                    .attempts
                    .iter()
                    .map(|a| {
                        json!([
                            a.id.seq,
                            venue_code(a.venue),
                            a.reserved,
                            a.filled,
                            a.status.code(),
                            a.submit_slot
                        ])
                    })
                    .collect();
                json!({
                    "id": i.id,
                    "mint": i.mint.iter().map(|x| format!("{x:02x}")).collect::<String>(),
                    "quantity": i.quantity,
                    "route": venue_code(i.route),
                    "attempts": at,
                })
            })
            .collect();
        json!({"version": 1, "intents": v})
    }

    /// Read the durable form; malformed -> `Err` (never a silent empty book).
    ///
    /// # Errors
    /// Static reason.
    pub fn from_json(v: &Value) -> Result<Self, &'static str> {
        if v["version"].as_u64() != Some(1) {
            return Err("exec_identity.version");
        }
        let mut out = Self::default();
        for i in v["intents"].as_array().ok_or("exec_identity.intents")? {
            let id = i["id"].as_u64().ok_or("exec_identity.id")?;
            let hexs = i["mint"].as_str().ok_or("exec_identity.mint")?;
            if hexs.len() != 64 || !hexs.is_ascii() {
                return Err("exec_identity.mint");
            }
            let mut mint = [0u8; 32];
            for (k, o) in mint.iter_mut().enumerate() {
                *o = u8::from_str_radix(&hexs[k * 2..k * 2 + 2], 16)
                    .map_err(|_| "exec_identity.mint")?;
            }
            let quantity = i["quantity"].as_u64().ok_or("exec_identity.quantity")?;
            let route = i["route"]
                .as_str()
                .and_then(venue_from)
                .ok_or("exec_identity.route")?;
            let mut attempts = Vec::new();
            for a in i["attempts"].as_array().ok_or("exec_identity.attempts")? {
                let e = "exec_identity.attempt";
                attempts.push(Attempt {
                    id: AttemptId {
                        intent: id,
                        seq: u32::try_from(a[0].as_u64().ok_or(e)?).map_err(|_| e)?,
                    },
                    venue: a[1].as_str().and_then(venue_from).ok_or(e)?,
                    reserved: a[2].as_u64().ok_or(e)?,
                    filled: a[3].as_u64().ok_or(e)?,
                    status: a[4].as_str().and_then(AttemptStatus::from_code).ok_or(e)?,
                    submit_slot: a[5].as_u64().ok_or(e)?,
                });
            }
            out.intents.insert(
                id,
                Intent {
                    id,
                    mint,
                    quantity,
                    route,
                    attempts,
                },
            );
        }
        Ok(out)
    }
}

impl Attempt {
    const fn seq_next(&self) -> u32 {
        self.id.seq + 1
    }
}
