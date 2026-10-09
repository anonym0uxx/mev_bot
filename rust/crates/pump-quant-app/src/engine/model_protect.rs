//! Protective execution for model-managed positions.
//!
//! A protective trigger (the agreed hard stop, rug precursor, creator dump ...) used to book a close inside the
//! position store. For a model-managed position it now only produces a [`crate::position::ProtectIntent`]; this
//! module turns that into an IDENTIFIABLE protective order (`MgmtKind::Protect`) that settles through exactly the
//! machinery management sells use: the same id sequence, cumulative-settlement evidence, books and durable ledger.
//! Only a reconciled fill changes inventory, cash, cost basis or realized PnL.
//!
//! AUTHORITY AND PRECONDITIONS: the order is created by a safeguard, never by the model, so it needs neither the
//! model lane nor a cleared SAFETY_OFF. Thresholds are unchanged: this module decides nothing about WHEN to sell,
//! only HOW the sell is executed and settled.
//!
//! QUANTITY: `inventory - tokens reserved by an UNRESOLVED management sell` (an order whose acknowledgement is
//! unknown may already have executed, so it is neither cancelled nor sold over). Nothing free: no order, a named
//! deferral, and the trigger stays pending. One protective order per mint; repeated triggers while it works are
//! counted and suppressed.
//!
//! PAPER EXECUTOR ASSUMPTIONS (explicit): the simulated fill uses the same landing-state pricing as management
//! sells (curve reserves / AMM reserves observed at least `MODEL_FILL_LANDING_MS` after creation). The trigger mark
//! is a SPOT trigger, not an executable quote. Fills here prove lifecycle behaviour, not executable AMM proceeds.
use super::*;
use crate::position::{ExitReason as PosExit, ProtectIntent};
use model_manage::{MgmtKind, MgmtOrder};

/// A fired trigger that has not yet been served by a completed protective order.
#[derive(Debug, Clone, Copy)]
pub(super) struct PendingTrigger {
    pub reason: PosExit,
}

impl Engine {
    /// Run `f` with the working protective order of `mint` temporarily in the management slot, so every existing
    /// settlement path (fill, evidence, end) applies to it unchanged. An order that ended while the position is
    /// still open re-arms its trigger. Returns `None` when there is no protective order.
    pub(super) fn model_with_protect_mint<R>(
        &mut self,
        mint: &[u8; 32],
        f: impl FnOnce(&mut Self) -> R,
    ) -> Option<R> {
        let p = self.model_mgmt.protect.remove(mint)?;
        let code = p.protect;
        let stashed = self.model_mgmt.orders.remove(mint);
        self.model_mgmt.orders.insert(*mint, p);
        let r = f(self);
        let back = self.model_mgmt.orders.remove(mint);
        if let Some(s) = stashed {
            self.model_mgmt.orders.insert(*mint, s);
        }
        match back {
            Some(o) => {
                self.model_mgmt.protect.insert(*mint, o);
            }
            None => {
                if self.positions.has(mint) {
                    if let Some(reason) = PosExit::from_code(code) {
                        self.model_protect_pending
                            .insert(*mint, PendingTrigger { reason });
                    }
                }
            }
        }
        Some(r)
    }

    /// Same, but only when the working protective order has this id (a report names an order, not a mint).
    pub(super) fn model_with_protect_id<R>(
        &mut self,
        mint: &[u8; 32],
        order_id: u64,
        f: impl FnOnce(&mut Self) -> R,
    ) -> Option<R> {
        if self.model_mgmt.protect.get(mint).map(|o| o.id) != Some(order_id) {
            return None;
        }
        self.model_with_protect_mint(mint, f)
    }

    /// The position closed: a protective order still working is ended (preempted) with its terminal record.
    pub(super) fn model_protect_forget(&mut self, mint: &[u8; 32]) {
        self.model_protect_pending.remove(mint);
        let _ = self.model_with_protect_mint(mint, |e| e.model_mgmt_end(mint, true));
        self.model_protect_pending.remove(mint);
    }

    /// Collect queued triggers, register them, then serve every pending trigger. Runs after every engine event, so it
    /// is independent of the model lane and of SAFETY_OFF.
    pub(super) fn model_protect_drain(&mut self) {
        let intents: Vec<ProtectIntent> = self.positions.take_intents();
        let fresh: std::collections::BTreeSet<[u8; 32]> = intents.iter().map(|i| i.mint).collect();
        for it in intents {
            if self.model_mgmt.protect.contains_key(&it.mint) {
                self.mrep("protect:duplicate_trigger_suppressed");
                continue;
            }
            self.mrep(format!("protect:trigger:{:?}", it.reason));
            self.model_protect_pending
                .entry(it.mint)
                .or_insert(PendingTrigger { reason: it.reason });
        }
        if self.model_protect_pending.is_empty() {
            return;
        }
        let mints: Vec<[u8; 32]> = self.model_protect_pending.keys().copied().collect();
        for mint in mints {
            let Some(trig) = self.model_protect_pending.get(&mint).copied() else {
                continue;
            };
            if !self.positions.has(&mint) {
                self.model_protect_pending.remove(&mint);
                continue;
            }
            if self.model_mgmt.protect.contains_key(&mint) {
                continue; // an order is working: monitored, never overlapped
            }
            // A level trigger that no longer holds is not re-fired from memory (the stop is a level, not an event).
            if trig.reason == PosExit::HardStop && !fresh.contains(&mint) {
                let mark = self
                    .numeric
                    .latest_price_fp(DomainMint::from_bytes(mint))
                    .filter(|p| *p > 0);
                if let Some(px) = mark {
                    if !self.positions.hard_stop_breached(&mint, px) {
                        self.mrep("protect:pending_hard_stop_recovered");
                        self.model_protect_pending.remove(&mint);
                        continue;
                    }
                }
            }
            self.model_protect_serve(mint, trig.reason);
        }
    }

    /// Create the protective order for `mint` if there is free inventory; otherwise defer by name.
    fn model_protect_serve(&mut self, mint: [u8; 32], reason: PosExit) {
        let Some(inv) = self.positions.inventory_tokens(&mint) else {
            self.positions.note_protect_deferred(&mint);
            self.mrep("protect:deferred_inventory_unknown");
            return;
        };
        // An UNRESOLVED management sell reserves its remainder: it may already have executed. Never cancelled.
        let reserved = self
            .model_mgmt
            .orders
            .get(&mint)
            .filter(|o| o.uncertain && o.kind != MgmtKind::Add)
            .map_or(0, |o| o.intended.saturating_sub(o.filled));
        let free = inv.saturating_sub(reserved);
        if free == 0 {
            self.positions.note_protect_deferred(&mint);
            self.mrep("protect:deferred_all_reserved");
            return;
        }
        // A management order that is NOT uncertain was never submitted anywhere (the paper executor holds it
        // unfilled): it cannot execute behind the protective sell, so it is ended rather than overlapped.
        if self
            .model_mgmt
            .orders
            .get(&mint)
            .is_some_and(|o| !o.uncertain)
        {
            self.model_mgmt_end(&mint, false);
            self.mrep("protect:ended_unsubmitted_mgmt_order");
        }
        self.model_mgmt.seq += 1;
        let id = self.model_mgmt.seq;
        let amm = self.model_cache.snapshot_venue_is_amm(&mint);
        self.model_mgmt.protect.insert(
            mint,
            MgmtOrder {
                id,
                kind: MgmtKind::Protect,
                intended: free,
                filled: 0,
                created_ms: self.model_clock_ms,
                created_slot: self.model_slot,
                version: 0,
                amm,
                max_spend: 0,
                spent: 0,
                fee_bps: 0,
                uncertain: false,
                gross: 0,
                fees: 0,
                protect: reason.code(),
                simulated: false,
            },
        );
        if self.model_external_exec {
            if let Some(o) = self.model_mgmt.protect.get_mut(&mint) {
                o.uncertain = true;
            }
            self.mrep("protect:submitted_external");
        }
        self.model_protect_pending.remove(&mint);
        self.mrep(format!("protect:order:{reason:?}"));
    }

    /// The working protective order of `mint`: (id, intended, filled, trigger code).
    #[must_use]
    pub fn model_protect_pending_order(&self, mint: &[u8; 32]) -> Option<(u64, u64, u64, u8)> {
        self.model_mgmt
            .protect
            .get(mint)
            .map(|o| (o.id, o.intended, o.filled, o.protect))
    }

    /// Whether a fired trigger for `mint` is waiting to be served (deferred or its order ended with exposure left).
    #[must_use]
    pub fn model_protect_trigger_pending(&self, mint: &[u8; 32]) -> bool {
        self.model_protect_pending.contains_key(mint)
    }

    /// Mark the working protective order's acknowledgement UNCERTAIN (never cancelled, never simulated-filled).
    pub fn model_protect_mark_ack_uncertain(&mut self, mint: &[u8; 32], order_id: u64) -> bool {
        match self.model_mgmt.protect.get_mut(mint) {
            Some(o) if o.id == order_id => {
                o.uncertain = true;
                self.mrep("protect:ack_uncertain");
                true
            }
            _ => false,
        }
    }

    /// HARNESS CHECKPOINT (read-only): the durable-relevant management state the replay harness synchronises on.
    /// Per held mint: inventory, the management order (id/kind/intended/filled/remaining/uncertain), the protective
    /// order, the sell reservation the store holds, a pending trigger, deferrals, the latest curve observation
    /// (ts/slot/vsol) and the in-process held-ledger generation. It changes nothing.
    #[must_use]
    pub fn model_harness_checkpoint(&self) -> serde_json::Value {
        let hx = |m: &[u8; 32]| m.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let mut held = Vec::new();
        for h in self.positions.held_records() {
            let m = h.mint;
            let mo = self.model_mgmt.orders.get(&m).map(|o| serde_json::json!({
                "id": o.id, "kind": format!("{:?}", o.kind), "intended": o.intended, "filled": o.filled,
                "remaining": o.intended.saturating_sub(o.filled), "uncertain": o.uncertain,
                "gross": o.gross, "fees": o.fees, "simulated": o.simulated}));
            let po = self.model_mgmt.protect.get(&m).map(|o| serde_json::json!({
                "id": o.id, "intended": o.intended, "filled": o.filled, "uncertain": o.uncertain, "trigger": o.protect}));
            let co = self.model_cache.curve_obs(&m).map(|c| serde_json::json!({
                "ts_ms": c.ts_ms, "slot": c.slot, "v_sol": c.v_sol_lamports, "v_tokens": c.v_tokens}));
            held.push(serde_json::json!({
                "mint": hx(&m), "inventory": self.positions.inventory_tokens(&m),
                "sell_reserved": self.positions.sell_reserved(&m), "mgmt_order": mo, "protect_order": po,
                "trigger_pending": self.model_protect_pending.contains_key(&m),
                "curve_obs": co, "amm": self.model_cache.snapshot_venue_is_amm(&m)}));
        }
        serde_json::json!({
            "clock_ms": self.model_clock_ms, "held_generation": self.model_held.generation,
            "safety_off": self.model_safety.blocked, "protect_deferred_total": self.positions.protect_deferred,
            "paper_fill": self.model_paper_fill_status(),
            "held": held})
    }
}
