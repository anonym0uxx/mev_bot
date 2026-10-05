//! Engine side of the durable SAFETY_OFF (see `crate::safety_off` for the contract).
//!
//! What a block does and does not do:
//! * stops NEW asks and discards in-flight entry answers (the request table's block);
//! * invalidates queued risk-increasing intents: an entry order that is pending and NOT of uncertain
//!   acknowledgement is retired unfilled. An UNCERTAIN order is preserved: it may already have landed, so
//!   it stays pending for reconciliation;
//! * leaves held-position protection, pending-order reconciliation and risk-REDUCING management
//!   (REDUCE/EXIT) running. Nothing here is on the protective path;
//! * persists, and a restart restores the block. Only an explicit `model_safety_rearm` lifts it.

use super::*;
use crate::safety_off::{
    HeldRecord, PendingRecord, RearmRefusal, SafetyLoad, SafetyOff, CONSECUTIVE_ABANDONED_TRIP,
    REASON_OPERATOR, REASON_SHUTDOWN,
};

/// What a controlled shutdown left, for the operator and the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownReport {
    /// Held positions left HELD (never flattened by shutdown).
    pub held: usize,
    /// Entry orders that were invalidated (queued risk-increasing intents).
    pub entry_orders_invalidated: usize,
    /// Orders left pending because their acknowledgement is uncertain.
    pub uncertain_preserved: usize,
    /// Management sell orders left pending.
    pub mgmt_orders_pending: usize,
    /// Whether the state is durable on disk.
    pub persisted: bool,
}

impl Engine {
    /// Attach the durable file and adopt what it says. A blocked file BLOCKS this engine: a restart
    /// does not re-arm.
    pub fn model_safety_attach(&mut self, path: &std::path::Path) -> SafetyLoad {
        let (load, st) = SafetyOff::load(path);
        let blocked = st.blocked;
        self.model_safety = st;
        if blocked {
            self.model_table.set_entries_blocked(true);
            self.mrep("safety:restored_blocked");
        }
        load
    }

    /// Whether entries are blocked.
    #[must_use]
    pub fn model_safety_blocked(&self) -> bool {
        self.model_safety.blocked
    }

    /// The recorded reason.
    #[must_use]
    pub fn model_safety_reason(&self) -> &str {
        &self.model_safety.reason
    }

    /// Failed persist attempts so far.
    #[must_use]
    pub fn model_safety_persist_failures(&self) -> u64 {
        self.model_safety.persist_failures
    }

    fn model_safety_records(&self) -> (Vec<HeldRecord>, Vec<PendingRecord>) {
        let held = self.positions.held_records();
        let mut pending: Vec<PendingRecord> = self
            .model_orders
            .iter()
            .map(|(m, o)| PendingRecord {
                kind: "entry",
                id: o.id,
                mint: *m,
                quantity: o.clip_lamports,
                uncertain: o.uncertain,
            })
            .collect();
        pending.extend(self.model_mgmt.orders.iter().map(|(m, o)| PendingRecord {
            kind: match o.kind {
                model_manage::MgmtKind::Reduce => "reduce",
                model_manage::MgmtKind::Exit => "exit",
            },
            id: o.id,
            mint: *m,
            quantity: o.intended - o.filled,
            uncertain: false,
        }));
        (held, pending)
    }

    /// Persist the current snapshot (callable periodically by the daemon).
    pub fn model_safety_persist(&mut self) -> bool {
        let (h, p) = self.model_safety_records();
        self.model_safety.persist(&h, &p)
    }

    /// Trip SAFETY_OFF. Idempotent: a second trip keeps the first reason and does not bump the epoch.
    pub fn model_safety_trip(&mut self, reason: &str) -> usize {
        let first = !self.model_safety.blocked;
        if first {
            self.model_safety.blocked = true;
            self.model_safety.reason = reason.to_string();
            self.model_safety.epoch += 1;
            self.mrep(format!("safety:tripped:{reason}"));
        }
        self.model_table.set_entries_blocked(true);
        // Invalidate queued risk-increasing intents; keep uncertain ones for reconciliation.
        let victims: Vec<[u8; 32]> = self
            .model_orders
            .iter()
            .filter(|(_, o)| !o.uncertain && o.confirmed.is_none())
            .map(|(m, _)| *m)
            .collect();
        for m in &victims {
            self.model_retire_order(m);
            self.mrep("safety:entry_order_invalidated");
        }
        if !self.model_safety_persist() {
            self.mrep("safety:persist_failed");
        }
        victims.len()
    }

    /// Explicit re-arm. Refused while evidence is unresolved, and refused if it cannot be made durable.
    ///
    /// # Errors
    /// [`RearmRefusal`] — the block stands.
    pub fn model_safety_rearm(&mut self, operator: &str) -> Result<(), RearmRefusal> {
        if !self.model_safety.blocked {
            return Err(RearmRefusal::NotBlocked);
        }
        if operator.trim().is_empty() {
            return Err(RearmRefusal::NoOperator);
        }
        if !self.model_recon_faults.is_empty() {
            return Err(RearmRefusal::UnresolvedReconFault);
        }
        if self
            .model_orders
            .values()
            .any(|o| o.uncertain && o.confirmed.is_none())
        {
            return Err(RearmRefusal::UncertainOrderPending);
        }
        let prev = self.model_safety.clone();
        self.model_safety.blocked = false;
        self.model_safety.reason.clear();
        self.model_safety.epoch += 1;
        self.model_safety.rearmed_by = Some(operator.to_string());
        self.model_safety.consecutive_abandoned = 0;
        if !self.model_safety_persist() {
            // Not durable => did not happen: a restart would otherwise come back blocked while this
            // process runs armed.
            self.model_safety = prev;
            return Err(RearmRefusal::PersistFailed);
        }
        self.model_table.set_entries_blocked(false);
        self.mrep("safety:rearmed");
        Ok(())
    }

    /// Controlled shutdown: block, invalidate queued entries, keep held positions HELD and uncertain
    /// orders PENDING, and write everything down. Merely killing the process does none of this.
    pub fn model_controlled_shutdown(&mut self) -> ShutdownReport {
        let invalidated = self.model_safety_trip(REASON_SHUTDOWN);
        let (held, pending) = self.model_safety_records();
        let persisted = self.model_safety.persist(&held, &pending);
        ShutdownReport {
            held: held.len(),
            entry_orders_invalidated: invalidated,
            uncertain_preserved: pending.iter().filter(|p| p.uncertain).count(),
            mgmt_orders_pending: pending.iter().filter(|p| p.kind != "entry").count(),
            persisted,
        }
    }

    /// Operator trip.
    pub fn model_safety_trip_operator(&mut self) -> usize {
        self.model_safety_trip(REASON_OPERATOR)
    }

    /// Fold one poll's endpoint health into the trip rule: consecutive failures/abandonments.
    pub(super) fn model_safety_note_endpoint(&mut self, ok: u32, failed: u32) {
        if ok > 0 {
            self.model_safety.consecutive_abandoned = 0;
        }
        if failed > 0 {
            self.model_safety.consecutive_abandoned =
                self.model_safety.consecutive_abandoned.saturating_add(failed);
            if self.model_safety.consecutive_abandoned >= CONSECUTIVE_ABANDONED_TRIP
                && !self.model_safety.blocked
            {
                self.model_safety_trip(crate::safety_off::REASON_ENDPOINT_HUNG);
            }
        }
    }
}
