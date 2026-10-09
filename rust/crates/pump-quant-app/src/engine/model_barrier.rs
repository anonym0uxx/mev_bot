//! OFFLINE PAPER REPLAY decision barriers.
//!
//! Accelerated input plus wall-clock ticks makes asynchronous inference timing decide WHICH state a verdict applies to.
//! For a replay fixture this module removes that nondeterminism WITHOUT touching production behaviour:
//!
//! * `barrier_enable` is called only by the daemon's offline-replay harness (`PQ_OFFLINE_PAPER_REPLAY=1`, paper,
//!   armed). Nothing here runs otherwise; production cadence, timestamp checks and the real monotonic deadlines are
//!   untouched.
//! * At a barrier the daemon has applied EXACTLY the input prefix up to a source-time clock, then calls
//!   [`Engine::barrier_settle`]: every request dispatched since the previous barrier is waited for (bounded), and the
//!   verdicts are applied in LOGICAL (request-id) order by the next production `Tick`. A verdict therefore applies to
//!   the state of the barrier at which it was cut, never to whatever the wall clock reached.
//! * Request ids are logical (counter state is persisted in the held ledger); the dispatch log records the id, the
//!   kind, the mint and a hash of the rendered prompt. Retransmitting a request after a restart is a new dispatch of
//!   the same logical state, not an execution effect; an ORDER or FILL duplicate is detected by the ledgers.
//! * A verdict whose request id is not in the outstanding table (it came from an abandoned process) is discarded by
//!   the production `accept` path (`discard:unknown_or_duplicate`); the settle step only ever waits for ids THIS
//!   process dispatched.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use super::Engine;
use crate::model_lane::RequestId;
use crate::model_worker::Verdict;

/// One dispatch, in logical terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchLogEntry {
    /// Logical request id.
    pub id: u64,
    /// "entry" or "mgmt".
    pub kind: &'static str,
    /// Market.
    pub mint: [u8; 32],
    /// SHA-256 (hex) of the exact rendered user prompt.
    pub prompt_sha256: String,
}

/// Barrier bookkeeping (all inert while `on` is false).
#[derive(Debug, Default)]
pub struct BarrierState {
    /// Barrier mode active.
    pub on: bool,
    /// Ids dispatched since the last settle and not yet delivered.
    pub outstanding: BTreeSet<u64>,
    /// Verdicts received, keyed by logical id (applied in id order).
    pub settled: std::collections::BTreeMap<u64, Verdict>,
    /// Dispatch log since enable.
    pub log: Vec<DispatchLogEntry>,
    /// Verdicts received that this process never dispatched (abandoned-session responses).
    pub foreign: u64,
}

impl Engine {
    /// Enable barrier mode. OFFLINE REPLAY HARNESS ONLY.
    pub fn barrier_enable(&mut self) {
        self.barrier.on = true;
    }

    /// Whether barrier mode is on.
    #[must_use]
    pub fn barrier_enabled(&self) -> bool {
        self.barrier.on
    }

    pub(super) fn barrier_log_dispatch(&mut self, kind: &'static str, mint: &[u8; 32], user: &str) {
        if !self.barrier.on {
            return;
        }
        let id = self.barrier_last_id(kind);
        let h = pump_quant_protocol::sha256::to_hex(&pump_quant_protocol::sha256::sha256(
            user.as_bytes(),
        ));
        self.barrier.outstanding.insert(id);
        self.barrier.log.push(DispatchLogEntry {
            id,
            kind,
            mint: *mint,
            prompt_sha256: h,
        });
    }

    fn barrier_last_id(&self, kind: &'static str) -> u64 {
        if kind == "mgmt" {
            self.model_mgmt.table.last_issued_id()
        } else {
            self.model_table.last_issued_id()
        }
    }

    /// Take the verdicts settled for the coming tick, in logical id order.
    pub(super) fn barrier_take_settled(&mut self) -> Vec<Verdict> {
        std::mem::take(&mut self.barrier.settled)
            .into_values()
            .collect()
    }

    /// Wait (bounded) for every outstanding dispatched request, then stage the verdicts for the next tick, ordered by
    /// logical id. Returns the number still missing (0 = settled). A verdict for an id this process did not dispatch is
    /// counted as `foreign`; a verdict of another session is dropped, one for an unknown id of this session is handed to the production path, which refuses it by name.
    pub fn barrier_settle(&mut self, wait: Duration) -> usize {
        if self.model_pool.is_none() {
            return 0;
        }
        let t0 = Instant::now();
        while !self.barrier.outstanding.is_empty() && t0.elapsed() < wait {
            let left = wait.saturating_sub(t0.elapsed());
            let got = self
                .model_pool
                .as_ref()
                .and_then(|p| p.recv_timeout(left.min(Duration::from_millis(50))));
            if let Some(v) = got {
                self.barrier_stage(v);
            }
        }
        // anything else already queued (e.g. a foreign verdict)
        while let Some(v) = self.model_pool.as_ref().and_then(|p| p.try_recv()) {
            self.barrier_stage(v);
        }
        self.barrier.outstanding.len()
    }

    /// Stage one verdict. A verdict from another process-session is counted `foreign` and DROPPED here: staged by id it
    /// could overwrite this process's own verdict that has the same (restarted) request number.
    fn barrier_stage(&mut self, v: Verdict) {
        if v.session != self.model_session {
            self.barrier.foreign += 1;
            self.mrep("discard:foreign_session");
            return;
        }
        if !self.barrier.outstanding.remove(&v.id.0) {
            self.barrier.foreign += 1;
        }
        self.barrier.settled.insert(v.id.0, v);
    }

    /// READ-ONLY queue classification (never in any digest): every market still waiting in the entry queue, with
    /// its dirty age, the last named refusal the scheduler saw for it, venue, accepted-observation count, and
    /// whether the re-ask window is the only thing holding it. `eligible_now` is true only when NOTHING named
    /// currently prevents a dispatch (it has no refusal on record and is outside the re-ask window).
    #[must_use]
    pub fn barrier_waiting_report(&self) -> Vec<String> {
        let clock = self.model_clock_ms;
        let mut rows: Vec<(i64, [u8; 32])> = self
            .model_dirty
            .iter()
            .map(|m| (self.model_dirty_since.get(m).copied().unwrap_or(0), *m))
            .collect();
        rows.sort_unstable();
        rows.iter()
            .enumerate()
            .map(|(pos, (since, m))| {
                let (venue, _, n) = self.model_cache.describe(m, clock);
                let last_ask = self.model_last_ask.get(m).copied();
                let in_reask = last_ask.is_some_and(|t| clock - t < 15_000);
                let refusal = self.model_last_refusal.get(m).map_or("none", String::as_str);
                let held = self.open_lane.contains_key(m)
                    || self.model_orders.contains_key(m)
                    || self.model_table.has_live_for(m);
                let blocked = self.model_mint_blocked(m);
                let why = if blocked {
                    "recon_fault_blocks_exposure"
                } else if held {
                    "held_or_pending"
                } else if in_reask {
                    "reask_window"
                } else if refusal != "none" {
                    refusal
                } else {
                    "none_named"
                };
                format!(
                    "pos={pos} mint={} since={since} age_ms={} venue={venue} n_accepted={n} last_ask={last_ask:?} why={why} eligible_now={}",
                    m.iter().take(6).map(|b| format!("{b:02x}")).collect::<String>(),
                    (clock - since).max(0),
                    why == "none_named",
                )
            })
            .collect()
    }

    /// Verdicts staged for the next tick.
    #[must_use]
    pub fn barrier_staged(&self) -> usize {
        self.barrier.settled.len()
    }

    /// Drain and return the dispatch log (harness reads it at each barrier).
    pub fn barrier_take_log(&mut self) -> Vec<DispatchLogEntry> {
        std::mem::take(&mut self.barrier.log)
    }

    /// Verdicts from an abandoned session seen so far.
    #[must_use]
    pub fn barrier_foreign(&self) -> u64 {
        self.barrier.foreign
    }

    /// Deterministic digest of the LOGICAL decision/accounting state at a barrier: per-market readiness and state
    /// versions, order/fill ledgers, positions (inventory, cost basis, age), cash and committed capital, request-id
    /// counters. Excludes wall-clock fields, process-session ids and anything not a function of the input prefix and
    /// the answered decisions.
    #[must_use]
    pub fn barrier_state_lines(&self) -> Vec<String> {
        let hx = |m: &[u8; 32]| {
            m.iter()
                .take(6)
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        let mut out = Vec::new();
        out.push(format!(
            "clock={} order_seq={} mgmt_seq={} seed={} realized={} balance={} committed={} free={}",
            self.model_clock_ms,
            self.model_order_seq,
            self.model_mgmt.seq,
            self.bankroll_origin.seed_lamports(),
            self.books_realized(),
            self.bankroll_balance(),
            self.books_committed(),
            self.model_free_cash_lamports(),
        ));
        for (m, o) in &self.model_orders {
            out.push(format!(
                "pending entry mint={} id={} attempt={} clip={} amm={} created_ms={} uncertain={}",
                hx(m),
                o.id,
                o.attempt,
                o.clip_lamports,
                o.amm,
                o.created_ms,
                o.uncertain
            ));
        }
        for (m, o) in &self.model_mgmt.orders {
            out.push(format!(
                "pending mgmt mint={} id={} kind={:?} intended={} filled={} spent={}",
                hx(m),
                o.id,
                o.kind,
                o.intended,
                o.filled,
                o.spent
            ));
        }
        for (id, r) in &self.model_order_log {
            out.push(format!(
                "order id={} mint={} attempt={} clip={} state={:?} filled_clip={} terminal={:?}",
                id,
                hx(&r.mint),
                r.attempt,
                r.clip_lamports,
                r.state,
                r.filled_clip_lamports,
                r.terminal
            ));
        }
        for f in &self.model_fills {
            out.push(format!(
                "fill order={} mint={} amm={} from_reconcile={}",
                f.order_id,
                hx(&f.mint),
                f.amm,
                f.from_reconcile
            ));
        }
        for h in self.positions.export_held() {
            let mp = self.model_mgmt.pos.get(&h.mint);
            out.push(format!(
                "held mint={} entry_px={} size={} cost={} remaining_bps={} inventory={:?} managed={} fill_ms={:?} step={:?} version={:?} position_order={:?}",
                hx(&h.mint),
                h.entry_price_fp,
                h.size_lamports,
                h.cost_lamports,
                h.remaining_bps,
                h.inventory_tokens,
                h.model_managed,
                mp.map(|p| p.fill_ms),
                mp.map(|p| p.step),
                mp.map(|p| p.version),
                mp.map(|p| p.position_order)
            ));
        }
        for m in &self.model_registry {
            let mk = self.model_cache.marker(m);
            out.push(format!(
                "market {} last_recv_ms={:?} n_accepted={:?} dirty={}",
                hx(m),
                mk.map(|k| k.last_recv_ms),
                mk.map(|k| k.n_accepted),
                self.model_dirty.contains(m)
            ));
        }
        out
    }

    /// Diagnostics only (never in the digest): trace one market's scheduling inputs and admit outcomes.
    pub fn barrier_watch(&mut self, hex_prefix: &str) {
        self.model_watch = Some(hex_prefix.to_string());
    }

    /// Drain the watched market's trace lines since the last call.
    pub fn barrier_watch_drain(&mut self) -> Vec<String> {
        std::mem::take(&mut self.model_watch_log)
    }

    /// SHA-256 over [`Self::barrier_state_lines`].
    #[must_use]
    pub fn barrier_state_digest(&self) -> String {
        let s = self.barrier_state_lines().join("\n");
        pump_quant_protocol::sha256::to_hex(&pump_quant_protocol::sha256::sha256(s.as_bytes()))
    }
}

#[allow(dead_code)]
fn _id(_: RequestId) {}
