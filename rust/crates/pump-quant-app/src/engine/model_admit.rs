//! The paper-model admission path: candidate -> snapshot -> async verdict -> pending order ->
//! simulated fill -> position.
//!
//! WHAT THIS REPLACES, AND WHAT IT DOES NOT. When the lane is armed, `gate_evaluate` is NEVER
//! called for a promoted candidate (no legacy verdict, no legacy sizing law, no fallback). Everything
//! that is *accounting* rather than *deciding* -- fees, ATA rent, the wallet floor, the round-trip
//! cost the exit ladder is derived from -- is reused unchanged, so a model fill is booked exactly
//! like any other. Discretionary entry, SIZE and PRICE LIMIT are the model's; Rust validates,
//! bounds by solvency / per-position notional / own-impact, and refuses by name.
//!
//! WHY A CHILD OF `engine`. The state it touches (positions, bankroll, `open_lane`) is private to
//! the engine; a child module reaches it without widening any visibility.
//!
//! NOTHING HERE BLOCKS. The model call runs on `InferencePool` workers. This module only
//! dispatches (non-blocking), polls (non-blocking), and expires deadlines, so the feed, held-position
//! protection and any emergency path run on the same thread exactly as before.
//!
//! PAPER ONLY. A `RunMode::Live` engine, or one with an outbound sink installed, refuses every
//! model admission by name (`refuse:live_forbidden`) and dispatches nothing.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use pump_quant_inference::seam::DriftLedger;
use pump_quant_inference::EntryVenue;
use pump_quant_watchlist::candidate::{Candidate, DiscoveryLane, Lane as WlLane};

use super::{
    deployable_capital, derive_survival_floor, wallet_floor_guard, Decision, Engine, FloorVerdict,
    PendingEntry, RunMode,
};
use crate::decision_join::PromptSnapshot;
use crate::freshness::{DecisionClock, CHAMPION_MAX_DECISION_AGE_MS};
use crate::impact_cap::CHAMPION_MAX_OWN_IMPACT_BPS;
use crate::model_authority::{resolve_entry, EntryAuthority, EntryRequest, NoTradeReason};
use crate::model_lane::{AcceptRefusal, SubmitRefusal};
use crate::model_worker::{DispatchRefusal, Job};
use crate::portfolio::PortfolioCap;

/// Worker threads and queue depth: a hard ceiling on concurrent model calls.
pub(super) const MODEL_WORKERS: usize = 2;
pub(super) const MODEL_QUEUE: usize = 4;
/// A verdict older than this (feed clock) is late. Equal to the freshness limit the authority applies.
const MODEL_DEADLINE_MS: i64 = CHAMPION_MAX_DECISION_AGE_MS as i64;
/// Minimum feed-clock gap between asking about the same mint again.
const MODEL_REASK_MS: i64 = 15_000;
/// One slot (~400 ms): a fill is evaluated at LANDING state, never at the observation state
/// (criterion 103 -- see `curve_fill`).
pub(super) const MODEL_FILL_LANDING_MS: i64 = 400;
/// A pending order with no landing state by then expires unfilled.
pub(super) const MODEL_ORDER_TTL_MS: i64 = 5_000;
const FIRST_CAND_CAP: usize = 100_000;
/// Bound on stream-registered markets.
const REGISTRY_CAP: usize = 100_000;
/// Maximum asks started per tick: a coalescing bound, not a strategy filter.
const SCHEDULE_PER_TICK: usize = 8;
/// Work bound: markets EXAMINED per tick (dispatched or refused). Unexamined markets stay queued, oldest first.
const EVAL_PER_TICK: usize = 64;
/// Bound on the in-memory order log (oldest terminal records are the only candidates to evict).
const ORDER_LOG_CAP: usize = 100_000;

/// Outcome of one admit attempt. The scheduler uses it to keep an INELIGIBLE market from consuming the
/// per-tick budget that an eligible one could use; it never changes what a market is eligible for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Admit {
    /// A request was handed to the pool.
    Dispatched,
    /// Not eligible now (identity/data/readiness/held/pending/re-ask): the market stays queued, no budget used.
    Ineligible,
    /// Eligible but the lane is full (request table or pool queue): stop starting entries this tick.
    Backpressure,
    /// The lane cannot admit anything (live forbidden, no source, no pool, no feed clock).
    Blocked,
}

/// A canonical-pool swap handed to the engine (see `AppEvent::AmmSwap`).
#[derive(Debug, Clone, Copy)]
pub struct AmmSwapIn {
    pub mint: pump_quant_domain::ids::Mint,
    pub pool: [u8; 32],
    pub canonical: bool,
    pub quote_is_wsol: bool,
    pub token_reserve_pre: u64,
    pub quote_reserve_pre: u64,
    pub fee_bps: Option<u32>,
    pub fee_parts: Option<(u32, u32, u32)>,
    pub virtual_quote: Option<u64>,
    pub is_buy: bool,
    pub token_amount: u64,
    pub quote_lamports: u64,
    pub trader: [u8; 32],
    pub fee_lamports: Option<u64>,
    pub cu_consumed: Option<u64>,
    pub recv_unix_ms: Option<i64>,
    pub slot: u64,
}

/// A fill reported by an authority outside the paper simulator (execution truth).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FillReport {
    pub entry_price_fp: u64,
    pub reserve_sol_lamports: u64,
}

/// A routing-fill exit kept OUT of every economic assessment (visible, with its reason).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExcludedExit {
    pub mint: [u8; 32],
    /// Recorded for audit only; never summed into PnL and never treated as a zero return.
    pub net_lamports: i128,
    pub reason: &'static str,
}

/// One paper-model fill and what is (not) established about it. Assessment, evaluation and
/// promotion consumers MUST read [`Engine::model_assessable_fills`], never positions directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelFillRecord {
    /// The order that opened this inventory.
    pub order_id: OrderId,
    pub mint: [u8; 32],
    pub amm: bool,
    /// Quote arithmetic validated for this instruction/fee case (protocol vectors).
    pub quote_validated: bool,
    /// Landing/ordering realism validated (Track A). Always false until demonstrated.
    pub landing_validated: bool,
    /// Came from `model_reconcile` (execution truth) rather than the paper simulator.
    pub from_reconcile: bool,
}

/// Identity of one paper-model order. Evidence is bound to this, never to the mint alone.
pub type OrderId = u64;

/// Where one order is in its life. Terminal states are final; evidence that contradicts one is a fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderState {
    /// Accepted verdict, awaiting a simulated landing. Not inventory.
    Pending,
    /// Acknowledgement unknown; still pending, never TTL-cleared.
    PendingUncertain,
    /// Filled: this order opened (or topped up) inventory.
    Filled,
    /// Resolved as never filled (by evidence or by TTL with no landing state).
    NotFilled,
    /// Closed out after fill (position exited).
    Closed,
}

/// One order's durable record: identity, attempt, intended and filled quantity, and terminal evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderRec {
    pub id: OrderId,
    pub mint: [u8; 32],
    /// Execution attempt for this order (1 for the first submission; a retry keeps the order id).
    pub attempt: u32,
    /// The quote lamports the model asked to spend (the clip).
    pub clip_lamports: u64,
    /// Inventory this order actually opened, set only by a fill (0 until then).
    pub filled_clip_lamports: u64,
    pub state: OrderState,
    /// First terminal evidence applied to this order, if any (kept for audit).
    pub terminal: Option<ReconcileOutcome>,
}

/// Evidence about ONE order: which order, which attempt, and what quantity it speaks about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Evidence {
    pub order_id: OrderId,
    pub attempt: u32,
    /// Quantity (clip lamports) the evidence source says it is reporting on.
    pub clip_lamports: u64,
    pub outcome: ReconcileOutcome,
}

/// A durable reconciliation fault: contradictory evidence for one order, preserved verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconFault {
    pub order_id: OrderId,
    pub mint: [u8; 32],
    /// The first terminal evidence, or `None` when the first "fact" was the engine's own book
    /// (a paper fill, or an order expired without evidence); `first_source` names which.
    pub first: Option<ReconcileOutcome>,
    pub first_source: &'static str,
    pub contradicting: Vec<ReconcileOutcome>,
}

/// What `model_ingest_evidence` did with a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceResult {
    Applied,
    Duplicate,
    /// Contradicts the order's first terminal evidence: fault raised, nothing applied.
    Fault,
    /// Refused without touching any state, with the named reason.
    Rejected(&'static str),
}

/// The result of resolving a reconciliation fault against authoritative evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultResolution {
    /// There was no fault on this mint.
    NoFault,
    /// Books now agree with the evidence and the exposure block is released.
    Released { unwound: bool },
    /// The books cannot be reconciled without inventing data; the fault and block REMAIN.
    Refused(&'static str),
}

/// How an uncertain acknowledgement was resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileOutcome {
    NotFilled,
    Filled(FillReport),
}

/// What the engine remembers about one outstanding request: the immutable snapshot it was bound to.
#[derive(Debug, Clone)]
pub(super) struct ModelReqMeta {
    pub snap: PromptSnapshot,
    pub lane: WlLane,
    pub discovery_lane: DiscoveryLane,
    pub dims: String,
}

/// An accepted BUY awaiting its simulated fill. Creating this moves NO capital and opens NO position.
#[derive(Debug, Clone, Copy)]
pub(super) struct ModelOrder {
    pub id: OrderId,
    pub attempt: u32,
    pub clip_lamports: u64,
    pub price_limit: Option<f64>,
    pub created_ms: i64,
    /// Highest ON-CHAIN slot the feed had shown when the order was created. A fill state must come
    /// from a STRICTLY later slot: a swap observed late but executed earlier is not a landing state.
    pub created_slot: u64,
    /// The prompt's decision clock; distinct from `created_ms` (verdict accepted) and from the
    /// reserve receipt time that prices the fill.
    pub snap_t_dec_ms: i64,
    /// Acknowledgement unknown: stays PENDING (not inventory, not TTL-cleared) until reconciled.
    pub uncertain: bool,
    /// Execution truth reported by `model_reconcile`, applied exactly once.
    pub confirmed: Option<FillReport>,
    pub lane: WlLane,
    pub discovery_lane: DiscoveryLane,
    pub snap_price: f64,
    /// The AMM plane governed this decision: the fill is priced from the POOL, not the curve.
    pub amm: bool,
}

fn age_bucket(age_s: Option<f64>) -> &'static str {
    match age_s {
        None => "launch_unknown",
        Some(a) if a < 300.0 => "lt5m",
        Some(a) if a < 1_800.0 => "5to30m",
        Some(a) if a < 3_600.0 => "30to60m",
        Some(_) => "ge1h",
    }
}

fn warm_bucket(n: u64) -> &'static str {
    match n {
        0..=19 => "lt20",
        20..=99 => "20to99",
        _ => "ge100",
    }
}

/// Retry spacing (wire-clock ms) for a failed missing-history write.
const MISSING_RETRY_MS: i64 = 5_000;

/// Missing-history persistence bookkeeping (one field on the engine).
#[derive(Debug, Default)]
pub(super) struct MissingStore {
    pub path: Option<std::path::PathBuf>,
    pub writer: Option<crate::missing_history_store::Writer>,
    pub persisted_rev: u64,
    pub submitted_seq: u64,
    pub last_submit_ms: i64,
    pub failure_reported: bool,
}

/// Flow-history durability bookkeeping (one field on the engine).
#[derive(Default)]
pub(super) struct FlowStore {
    pub writer: Option<crate::flow_checkpoint::CkptWriter>,
    pub persisted_rev: u64,
    pub submitted_seq: u64,
    pub last_submit_ms: i64,
    pub failure_reported: bool,
    /// Wall time the engine thread spent cloning the last snapshot (the only engine-thread cost), microseconds.
    pub last_clone_us: u64,
    pub max_clone_us: u64,
}

impl std::fmt::Debug for FlowStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FlowStore")
    }
}

const FLOW_CKPT_MIN_INTERVAL_MS: i64 = 30_000;

/// Startup result of attaching durable flow history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlowAttach {
    /// No checkpoint on disk: a fresh history (prompts need a seed or refuse as before).
    Fresh,
    /// Restored; `unavailable_ms` is the interval between the newest cursor and the resume time that no source covers.
    Restored {
        unavailable_ms: i64,
        complete: bool,
        late: usize,
    },
    /// A checkpoint existed but cannot be trusted: every prompt refuses by name; the file is left untouched.
    Untrusted(&'static str),
}

impl Engine {
    /// Attach durable flow history BEFORE any inference. Restores and validates the checkpoint, then declares the
    /// resume time so any interval between the persisted cursors and the live feed becomes a NAMED gap. A valid but
    /// old checkpoint is therefore never taken as proof of uninterrupted history.
    pub fn model_flow_attach(
        &mut self,
        path: &std::path::Path,
        params: pump_quant_market_state::flow_reducer::FlowParams,
        provenance: crate::flow_checkpoint::Provenance,
        resume_ms: i64,
    ) -> FlowAttach {
        use crate::flow_checkpoint::{load, CkptWriter, FlowHistory, Load};
        let out = match load(params, path) {
            Load::NeverWritten => {
                self.model_cache
                    .attach_flow_history(FlowHistory::new(params, provenance));
                FlowAttach::Fresh
            }
            Load::Loaded(h)
                if h.meta.held_gen_seen.is_none() && self.model_held.restored_from_file =>
            {
                // No generation field: this history cannot be tied to the ledger that was just restored.
                // Absence of metadata is not compatibility. Readiness is refused by name; the file is untouched.
                self.model_cache
                    .flow_state_untrusted("flow_generation_unbound");
                self.mrep("flow_history:untrusted:flow_generation_unbound");
                FlowAttach::Untrusted("flow_generation_unbound")
            }
            Load::Loaded(h) if h.meta.held_gen_seen.unwrap_or(0) > self.model_held.generation => {
                // The history has seen books NEWER than the durable ledger now attached: the ledger was deleted,
                // replaced by an older copy or rolled back. Financial effects between the two are unaccounted for.
                // Nothing is applied and the file is never overwritten.
                self.model_cache.flow_state_untrusted("flow_ahead_of_books");
                self.mrep("flow_history:untrusted:flow_ahead_of_books");
                FlowAttach::Untrusted("flow_ahead_of_books")
            }
            Load::Loaded(h) => {
                self.model_cache.attach_flow_history(*h);
                let late = self.model_cache.flow_meta().map_or(0, |m| m.late.len());
                match self.model_cache.flow_restore_resume(resume_ms) {
                    Some(r) => FlowAttach::Restored {
                        unavailable_ms: r.unavailable_ms,
                        complete: r.complete,
                        late,
                    },
                    None => FlowAttach::Untrusted("flow_attach_failed"),
                }
            }
            Load::Untrusted(why) => {
                self.model_cache.flow_state_untrusted(why);
                self.mrep(format!("flow_history:untrusted:{why}"));
                FlowAttach::Untrusted(why)
            }
        };
        if !matches!(out, FlowAttach::Untrusted(_)) {
            // An untrusted file is never overwritten (the evidence stays on disk): no writer is started for it.
            self.flow_store.writer = Some(CkptWriter::start(path.to_path_buf(), None));
            self.flow_store.persisted_rev = 0;
        }
        out
    }

    /// Test seam: attach a writer with a forced-failure hook.
    #[doc(hidden)]
    pub fn model_flow_attach_writer(&mut self, w: crate::flow_checkpoint::CkptWriter) {
        self.flow_store.writer = Some(w);
        self.flow_store.persisted_rev = 0;
    }

    /// Tick path: one integer compare when nothing changed (or too soon). A change hands ONE consistent clone to the
    /// background writer; the tick never waits on disk. The durable cursor advances only when the file is on disk.
    pub(super) fn model_flow_persist_if_changed(&mut self) {
        let Some((durable, fails)) = self.flow_store.writer.as_ref().map(|w| {
            (
                w.durable(),
                w.consecutive_failures
                    .load(std::sync::atomic::Ordering::SeqCst),
            )
        }) else {
            return;
        };
        if fails > 0 && !self.flow_store.failure_reported {
            self.flow_store.failure_reported = true;
            self.mrep("flow_history:persist_failed");
        }
        if fails == 0 {
            self.flow_store.failure_reported = false;
        }
        let _ = durable;
        let rev = self.model_cache.flow_rev();
        let due = self.model_clock_ms - self.flow_store.last_submit_ms >= FLOW_CKPT_MIN_INTERVAL_MS;
        let retry = fails > 0 && due;
        if (rev == self.flow_store.persisted_rev || !due) && !retry {
            return;
        }
        let t0 = std::time::Instant::now();
        let Some((r, mut m)) = self.model_cache.flow_snapshot() else {
            return;
        };
        // Bind this snapshot to the durable financial generation current right now.
        m.held_gen_seen = Some(self.model_held.generation);
        let us = t0.elapsed().as_micros() as u64;
        self.flow_store.last_clone_us = us;
        self.flow_store.max_clone_us = self.flow_store.max_clone_us.max(us);
        let Some(w) = self.flow_store.writer.as_ref() else {
            return;
        };
        self.flow_store.submitted_seq = w.submit(r, m);
        self.flow_store.persisted_rev = rev;
        self.flow_store.last_submit_ms = self.model_clock_ms;
    }

    /// Shutdown / tests only: force a snapshot now and block (bounded) until durable.
    pub fn model_flow_flush(&mut self, timeout: std::time::Duration) -> bool {
        self.flow_store.last_submit_ms = i64::MIN / 2;
        self.flow_store.persisted_rev = u64::MAX;
        self.model_flow_persist_if_changed();
        match self.flow_store.writer.as_ref() {
            Some(w) => w.wait_durable(self.flow_store.submitted_seq, timeout),
            None => false,
        }
    }

    /// (durable seq, submitted seq, consecutive failures, total failures, last clone us, max clone us, last bytes,
    /// last encode us, last write us).
    #[must_use]
    pub fn model_flow_health(&self) -> (u64, u64, u64, u64, u64, u64, u64, u64, u64) {
        use std::sync::atomic::Ordering::SeqCst;
        match self.flow_store.writer.as_ref() {
            Some(w) => (
                w.durable(),
                self.flow_store.submitted_seq,
                w.consecutive_failures.load(SeqCst),
                w.total_failures.load(SeqCst),
                self.flow_store.last_clone_us,
                self.flow_store.max_clone_us,
                w.last_bytes.load(SeqCst),
                w.last_encode_us.load(SeqCst),
                w.last_write_us.load(SeqCst),
            ),
            None => (0, 0, 0, 0, 0, 0, 0, 0, 0),
        }
    }

    pub(super) fn mrep(&mut self, key: impl Into<String>) {
        *self.model_report.entry(key.into()).or_insert(0) += 1;
    }

    pub(super) fn mrep_add(&mut self, key: impl Into<String>, n: u64) {
        *self.model_report.entry(key.into()).or_insert(0) += n;
    }

    /// Block or release NEW model entries (an emergency / SAFETY_OFF supervisor hook). Held-position
    /// handling, the feed and every tick are untouched; only new asks stop, and in-flight answers
    /// are discarded rather than acted on.
    pub fn set_model_entries_blocked(&mut self, blocked: bool) {
        self.model_table.set_entries_blocked(blocked);
    }

    /// The model lane's coverage / refusal / lifecycle counters (candidate-ticks, not unique mints).
    /// Ingest accounting for the decision cache: every market event the engine received that the
    /// cache saw, split by what became of it. Scheduling coalesces ASK requests only; this proves
    /// whether any trade was dropped from the flow/feature history before that.
    #[must_use]
    pub fn model_ingest_counters(&self) -> crate::decision_join::IngestCounters {
        self.model_cache.counters()
    }

    /// Record a print the feed derivation dropped before the flow reducer could see it (a
    /// reserve delta it refused), so any 300 s flow window that contains it is refused by
    /// name rather than served as complete or quietly idle. No-op unless the paper-model
    /// lane is armed — nothing else serves flow.
    pub fn note_flow_upstream_drop(&mut self, mint: [u8; 32], drop_unix_ms: i64) {
        if self.paper_model_mode {
            self.model_cache.note_flow_upstream_drop(mint, drop_unix_ms);
        }
    }

    /// Attach the durable missing-history ledger and RESTORE it before any inference.
    ///
    /// * trusted records are restored (the gaps keep refusing);
    /// * a missing file is a clean start;
    /// * an unreadable / incompatible file raises the named conservative refusal for every prompt
    ///   and is NEVER overwritten while untrusted (the evidence stays on disk).
    pub fn model_missing_attach(
        &mut self,
        path: &std::path::Path,
    ) -> crate::missing_history_store::StoreLoad {
        use crate::missing_history_store::{load, StoreLoad, Writer};
        let l = load(path);
        match &l {
            StoreLoad::NeverWritten => {}
            StoreLoad::Records(r) => {
                let _ = self.model_cache.restore_missing_history(r, true);
            }
            StoreLoad::Untrusted(why) => {
                let _ = self.model_cache.restore_missing_history(&[], false);
                self.mrep(format!("missing_history:untrusted:{why}"));
            }
        }
        self.missing_store.path = Some(path.to_path_buf());
        self.missing_store.writer = Some(Writer::start(path.to_path_buf()));
        // What is on disk (or absent) is the baseline: the first change writes.
        self.missing_store.persisted_rev = self.model_cache.missing_rev();
        l
    }

    /// Publish the trusted EMPTY ledger for a clean start (nothing was ever written, no exposure was restored).
    ///
    /// Without this a run that never sees a gap never writes the file, so a later restart that restores exposure
    /// cannot tell "no gap" from "ledger deleted" and must refuse (`absent_with_restored_exposure`). This makes
    /// "trusted, zero unresolved gaps as of this run" a durable fact. It never runs when continuity is untrusted
    /// and never overwrites an existing file.
    pub fn model_missing_publish_baseline(&mut self) -> bool {
        if self.model_cache.history_continuity_unknown() {
            return false;
        }
        let Some(w) = self.missing_store.writer.as_ref() else {
            return false;
        };
        let body =
            crate::missing_history_store::encode(&self.model_cache.missing_history_records());
        self.missing_store.submitted_seq = w.submit(body);
        self.missing_store.persisted_rev = self.model_cache.missing_rev();
        self.missing_store.last_submit_ms = self.model_clock_ms;
        true
    }

    /// Test seam: attach with a caller-supplied writer (forced-failure injection).
    #[doc(hidden)]
    pub fn model_missing_attach_with_writer(&mut self, w: crate::missing_history_store::Writer) {
        self.missing_store.writer = Some(w);
        self.missing_store.persisted_rev = self.model_cache.missing_rev();
    }

    /// Called every tick: one integer compare when nothing changed. A change (or an unconfirmed /
    /// failed previous write) hands the newest snapshot to the background writer; the tick never
    /// waits on disk. Never writes while continuity is untrusted.
    pub(super) fn model_missing_persist_if_changed(&mut self) {
        let Some((w_written, fails)) = self
            .missing_store
            .writer
            .as_ref()
            .map(|w| (w.written_seq(), w.consecutive_failures()))
        else {
            return;
        };
        if self.model_cache.history_continuity_unknown() {
            return;
        }
        let rev = self.model_cache.missing_rev();
        if fails > 0 && !self.missing_store.failure_reported {
            self.missing_store.failure_reported = true;
            self.mrep("missing_history:persist_failed");
        }
        if fails == 0 {
            self.missing_store.failure_reported = false;
        }
        let _ = w_written;
        let retry = fails > 0
            && self.model_clock_ms - self.missing_store.last_submit_ms >= MISSING_RETRY_MS;
        if rev == self.missing_store.persisted_rev && !retry {
            return;
        }
        let body =
            crate::missing_history_store::encode(&self.model_cache.missing_history_records());
        let Some(w) = self.missing_store.writer.as_ref() else {
            return;
        };
        self.missing_store.submitted_seq = w.submit(body);
        self.missing_store.persisted_rev = rev;
        self.missing_store.last_submit_ms = self.model_clock_ms;
    }

    /// Persistence health for the status writer: (unflushed, consecutive_failures, total_failures).
    #[must_use]
    pub fn model_missing_persist_health(&self) -> (bool, u64, u64) {
        self.missing_store
            .writer
            .as_ref()
            .map_or((false, 0, 0), |w| {
                (
                    self.missing_store.submitted_seq > w.written_seq(),
                    w.consecutive_failures(),
                    w.total_failures(),
                )
            })
    }

    /// Block (bounded) until the newest snapshot is durable. Shutdown and tests ONLY.
    pub fn model_missing_flush(&mut self, timeout: std::time::Duration) -> bool {
        self.model_missing_persist_if_changed();
        match self.missing_store.writer.as_ref() {
            Some(w) => w.wait_durable(self.missing_store.submitted_seq, timeout),
            None => false,
        }
    }

    /// Record a refused reserve observation with the producer's classification and source id.
    pub fn note_missing_observation(
        &mut self,
        mint: [u8; 32],
        drop_unix_ms: i64,
        kind: crate::decision_join::MissingKind,
        source_id: String,
    ) {
        if self.paper_model_mode {
            self.model_cache
                .note_missing_observation(mint, drop_unix_ms, kind, source_id);
        }
    }

    /// Read-only (tests, status): flow aggregates for `mint` at `t_ms`, and the history's scope refusal.
    #[must_use]
    pub fn model_flow_aggregates(
        &self,
        mint: &[u8; 32],
        t_ms: i64,
    ) -> pump_quant_market_state::flow_reducer::FlowOutcome {
        self.model_cache.flow_aggregates(mint, t_ms)
    }

    /// See [`Self::model_flow_aggregates`].
    #[must_use]
    pub fn model_flow_scope_refusal(
        &self,
        mint: &[u8; 32],
        t_ms: i64,
    ) -> Option<(&'static str, i64, i64)> {
        self.model_cache.flow_scope_refusal(mint, t_ms)
    }

    /// Low-frequency health view of upstream-dropped prints for the status writers: the
    /// cumulative drop count and how many mints' 300 s flow windows are currently
    /// incomplete (readiness refused by [`crate::decision_join::JoinRefusal::FlowUpstreamDrop`]).
    /// All-zero unless the paper-model lane is armed, because nothing else serves flow.
    /// Must only be called from a periodic writer — it scans per-mint rings.
    #[must_use]
    pub fn model_flow_drop_summary(&self) -> crate::decision_join::FlowDropSummary {
        if self.paper_model_mode {
            self.model_cache.flow_drop_summary(self.model_clock_ms)
        } else {
            crate::decision_join::FlowDropSummary::default()
        }
    }

    /// Clear the CUMULATIVE incompleteness for `mint` after its missing history has been
    /// reconstructed (a bounded replay/backfill from an authoritative capture, preserving event
    /// identity/order/dedup) or explicitly reconciled by an operator. NEVER by a timer — a fresh
    /// reserve snapshot does not restore missing trade history. Returns true when an
    /// unreconciled observation was cleared. No-op unless the paper-model lane is armed.
    pub fn model_reconcile_flow_history(
        &mut self,
        mint: &[u8; 32],
        receipt: &crate::decision_join::ReconstructionReceipt,
    ) -> Result<(), crate::decision_join::ReconcileRefusal> {
        if self.paper_model_mode {
            self.model_cache.reconcile_flow_history(mint, receipt)
        } else {
            Err(crate::decision_join::ReconcileRefusal::NoGap)
        }
    }

    /// Per-mint missing-history readiness for the status writer (never on the hot path).
    #[must_use]
    pub fn model_missing_history_status(
        &self,
        mint: &[u8; 32],
    ) -> Option<crate::decision_join::MissingHistoryStatus> {
        if self.paper_model_mode {
            self.model_cache.missing_history_status(mint)
        } else {
            None
        }
    }

    /// Every UNRESOLVED missing-history record, for durable persistence.
    #[must_use]
    pub fn model_missing_history_records(
        &self,
    ) -> Vec<([u8; 32], crate::decision_join::MissingObservation)> {
        if self.paper_model_mode {
            self.model_cache.missing_history_records()
        } else {
            Vec::new()
        }
    }

    /// Restore persisted missing-history state BEFORE entry/management inference resumes.
    /// `integrity_ok = false` (unreadable/incompatible record) raises the conservative
    /// [`crate::decision_join::JoinRefusal::HistoryContinuityUnknown`] refusal.
    pub fn model_restore_missing_history(
        &mut self,
        records: &[([u8; 32], crate::decision_join::MissingObservation)],
        integrity_ok: bool,
    ) -> Result<(), crate::decision_join::RestoreRefusal> {
        self.model_cache
            .restore_missing_history(records, integrity_ok)
    }

    /// Whether startup continuity could not be established.
    #[must_use]
    pub fn model_history_continuity_unknown(&self) -> bool {
        self.model_cache.history_continuity_unknown()
    }

    pub fn model_lane_report(&self) -> &std::collections::BTreeMap<String, u64> {
        &self.model_report
    }

    /// Test hook: arm the mode flag WITHOUT a source -- the misconfiguration the lane must refuse
    /// by name. Not reachable from production wiring (`enable_paper_model` always installs one).
    #[doc(hidden)]
    pub fn arm_model_mode_without_source_for_test(&mut self) {
        self.paper_model_mode = true;
        self.model_source = None;
    }

    /// Mints with an accepted BUY awaiting its simulated fill.
    #[must_use]
    pub fn model_pending_orders(&self) -> usize {
        self.model_orders.len()
    }

    /// Whether a position is open on `mint` (read-only, for tests and reporting).
    #[must_use]
    pub fn model_position_open(&self, mint: &[u8; 32]) -> bool {
        self.open_lane.contains_key(mint)
    }

    /// Advance the lane's clock from a wire receive time. Monotone: an older stamp never rewinds it.
    pub(super) fn model_note_slot(&mut self, slot: u64) {
        if slot > self.model_slot {
            self.model_slot = slot;
        }
    }

    pub(super) fn model_note_clock(&mut self, ms: i64) {
        if ms > self.model_clock_ms {
            self.model_clock_ms = ms;
        }
    }

    fn model_live_mints(&self) -> BTreeSet<[u8; 32]> {
        let mut s: BTreeSet<[u8; 32]> = self.open_lane.keys().copied().collect();
        s.extend(self.model_orders.keys().copied());
        s
    }

    /// A legacy watchlist promotion in paper-model mode. It is COUNTED and nothing else: legacy rank, score and
    /// tick state neither dispatch, prioritise nor exclude a Qwen entry request. Every supported, data-ready
    /// market reaches the model through the one stream scheduler (`model_stream_schedule`).
    pub(super) fn model_admit_candidate(&mut self, cand: Candidate) {
        self.mrep("legacy_promotion_observed_not_dispatched");
        let cm = cand.mint.bytes();
        let (venue, _, _) = self.model_cache.describe(&cm, self.model_clock_ms);
        self.model_uniq("legacy_promoted", &cm, venue);
    }

    /// Register a stream-discovered market. Bounded; idempotent; never consults legacy state.
    pub(super) fn model_register(&mut self, mint: [u8; 32]) {
        // "Dirty" means an observation NEWER than the last answered ask. An overlap replay after a restart
        // re-delivers observations that predate the restored ask; they rebuild the cache but are not new.
        if self.model_clock_ms <= self.model_replay_through_ms {
            self.mrep("queue:replayed_observation_not_dirty");
        } else if self.model_dirty.insert(mint) {
            self.model_dirty_since.insert(mint, self.model_clock_ms);
        } else {
            // A further observation folded into an already-queued market: coalesced, not lost.
            self.mrep("queue:coalesced_update");
        }
        if self.model_registry.contains(&mint) {
            return;
        }
        if self.model_registry.len() >= REGISTRY_CAP {
            self.mrep("refuse:registry_full");
            return;
        }
        self.model_registry.insert(mint);
        let clock = self.model_clock_ms;
        if self.model_first_cand.len() < FIRST_CAND_CAP {
            self.model_first_cand.entry(mint).or_insert(clock);
        }
        let (venue, _, _) = self.model_cache.describe(&mint, clock);
        self.model_uniq("discovered", &mint, venue);
    }

    /// First time `mint` reaches `stage`: one unique-market count, split by venue.
    fn model_uniq(&mut self, stage: &str, mint: &[u8; 32], venue: &str) {
        if self.model_uniq_seen.insert((stage.to_string(), *mint)) {
            self.mrep(format!("uniq_{stage}|venue={venue}"));
        }
    }

    /// THE entry scheduler. Every market with an observation newer than its last answered ask is queued
    /// (`model_dirty`, with the time it FIRST became dirty, preserved across coalesced updates). Each tick:
    /// * markets are visited OLDEST-DIRTY-FIRST with the mint as the stable tie-break;
    /// * at most `SCHEDULE_PER_TICK` requests are dispatched (the existing budget);
    /// * a market that is ineligible now (readiness, identity, held/pending, re-ask) does NOT consume that
    ///   budget, so it cannot block an eligible one; at most `EVAL_PER_TICK` markets are examined per tick as a
    ///   work bound, and the unexamined stay queued in the same order (no starvation: oldest first);
    /// * lane backpressure (request table / pool queue full) or a lane-wide block stops entry starts for the
    ///   tick and leaves the rest queued.
    /// Held-position management has its OWN request table and runs BEFORE this in the tick, so an entry
    /// backlog cannot take its slots. Legacy watchlist state is never read here.
    pub(super) fn model_stream_schedule(&mut self) {
        let clock = self.model_clock_ms;
        let mut order: Vec<(i64, [u8; 32])> = self
            .model_dirty
            .iter()
            .map(|m| (self.model_dirty_since.get(m).copied().unwrap_or(0), *m))
            .collect();
        order.sort_unstable();
        let queued = order.len();
        let mut dispatched = 0usize;
        let mut examined = 0usize;
        for (pos, (since, mint)) in order.into_iter().enumerate() {
            if dispatched >= SCHEDULE_PER_TICK {
                self.mrep("sched_deferred_budget");
                break;
            }
            if examined >= EVAL_PER_TICK {
                self.mrep("sched_deferred_eval_bound");
                break;
            }
            if self
                .model_last_ask
                .get(&mint)
                .is_some_and(|t| clock - *t < MODEL_REASK_MS)
            {
                // Not eligible yet and costs nothing: stays queued at its original position, uses no budget.
                self.mrep("sched:reask_window_wait");
                continue;
            }
            examined += 1;
            self.model_dirty.remove(&mint);
            self.model_dirty_since.remove(&mint);
            self.mrep("admit_attempt|src=stream");
            let age = (clock - since).max(0);
            let (v, _, _) = self.model_cache.describe(&mint, clock);
            let b = match age {
                0..=999 => "lt1s",
                1_000..=4_999 => "1to5s",
                5_000..=29_999 => "5to30s",
                30_000..=299_999 => "30sto5m",
                _ => "ge5m",
            };
            self.mrep(format!("queue_age|{b}|venue={v}"));
            self.mrep("queue_age_n");
            self.mrep_add("queue_age_ms_sum", age as u64);
            let watched = self.model_watch.as_ref().is_some_and(|w| {
                mint.iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
                    .starts_with(w.as_str())
            });
            if watched {
                self.model_watch_log.push(format!(
                    "sched clock={clock} queue_pos={pos}/{queued} since={since} dispatched_so_far={dispatched}"
                ));
            }
            let out =
                self.model_admit_mint(mint, WlLane::ActiveMarketScalp, DiscoveryLane::ActiveMarket);
            match out {
                Admit::Dispatched => dispatched += 1,
                Admit::Ineligible => {}
                Admit::Backpressure | Admit::Blocked => {
                    // The lane cannot start more entries now: keep this market (and the rest) queued with its
                    // ORIGINAL dirty time and stop for this tick.
                    self.model_dirty.insert(mint);
                    self.model_dirty_since.insert(mint, since);
                    self.mrep(if out == Admit::Backpressure {
                        "sched_stopped_backpressure"
                    } else {
                        "sched_stopped_lane_blocked"
                    });
                    break;
                }
            }
        }
        self.mrep_add("sched_queue_depth_sum", queued as u64);
        self.mrep("sched_ticks");
    }

    /// A canonical-pool PumpSwap swap, token-oriented and pool-bound by the decoder. Non-canonical
    /// and non-WSOL pools are counted by name and never priced from.
    pub(super) fn model_on_amm_swap(&mut self, a: AmmSwapIn) {
        let Some(ts_ms) = a.recv_unix_ms else {
            self.mrep("refuse:amm_swap_no_clock");
            return;
        };
        self.model_note_clock(ts_ms);
        self.model_note_slot(a.slot);
        if !a.quote_is_wsol {
            self.mrep("amm_excluded:quote_not_wsol");
            return;
        }
        if !a.canonical {
            self.model_other_pools
                .entry(*a.mint.as_bytes())
                .or_default()
                .insert(a.pool);
            self.mrep("amm_excluded:pool_not_canonical");
            return;
        }
        let mint = *a.mint.as_bytes();
        let pool_s = a
            .pool
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        self.model_cache.bind_pool(mint, &pool_s);
        let others = self.model_other_pools.get(&mint).map_or(0, |s| s.len());
        let applied = self.model_cache.observe_amm(
            mint,
            crate::curve_annotation::AmmObservation {
                pool: pool_s,
                base_reserves_raw: a.token_reserve_pre,
                quote_reserves_lamports: a.quote_reserve_pre,
                quote_is_wsol: true,
                ts_ms,
                slot: a.slot,
            },
            crate::curve_annotation::AmmAttribution {
                wsol_pools: vec![a.pool.iter().map(|b| format!("{b:02x}")).collect()],
                pools_total: 1 + others,
                graduated: true,
            },
        );
        // The pool's fee rate is a per-event fact. A swap that does not carry it must not erase the
        // last observed rate (and is never defaulted): the fill uses the LAST OBSERVED rate, labelled.
        self.model_amm_fee.insert(mint, (a.fee_bps, ts_ms));
        self.model_amm_econ
            .insert(mint, (a.fee_parts, a.virtual_quote, ts_ms));
        if !applied {
            self.mrep("amm_swap_dropped_out_of_order");
        }
        // The swap's own execution price (quote per token incl. fees) feeds the flow/price windows
        // as a PRICED print. It is NOT the executable state: that is the pre-trade reserve above.
        if a.token_amount > 0 && a.quote_lamports > 0 {
            let price_fp =
                (i128::from(a.quote_lamports) * 1_000_000_000) / i128::from(a.token_amount);
            self.model_mgmt_note_price(&mint, price_fp, Some(ts_ms));
            let signed = i64::try_from(a.token_amount).unwrap_or(i64::MAX);
            let entity =
                pump_quant_wallet_graph::tracked_wallet_matcher::wallet_entity_id(&a.trader);
            // With the corpus-row producer on, the TRAINED windows are fed by `CorpusFlowRow` (corpus trader-delta
            // basis, once per instruction); feeding this swap's own legacy-basis print too would double-count it.
            if !self.corpus_flow_rows {
                self.model_cache
                    .observe_trade(&crate::decision_join::TradeObs {
                        mint,
                        price_fp,
                        quote_lamports: a.quote_lamports,
                        signed_base: if a.is_buy { signed } else { -signed },
                        buyer_entity: entity,
                        trader: Some(a.trader),
                        recv_unix_ms: Some(ts_ms),
                        slot: Some(a.slot),
                        fee_lamports: a.fee_lamports,
                        cu_consumed: a.cu_consumed,
                        venue: crate::state_ledger::VenueLabel::Pumpswap,
                        event_id: None,
                        feature: None,
                    });
            }
            // A HELD market is marked from the pool so the existing lifecycle can monitor it. This is
            // deliberately limited to held mints: the legacy numeric lane must not DISCOVER from it.
            if self.open_lane.contains_key(&mint) {
                self.numeric.observe(
                    pump_quant_domain::ids::Mint::from_bytes(mint),
                    price_fp,
                    a.quote_lamports,
                    a.quote_reserve_pre,
                    if a.is_buy { signed } else { -signed },
                    entity,
                    0,
                    self.now,
                );
            }
        }
        // PRINT-DRIVEN SAFEGUARD ON A HELD AMM POSITION (hard stop / rug precursor). Fed ONLY by a verified EXECUTED
        // pool swap: successful transaction, canonical WSOL pool, the pool this mint is bound to, a real executed
        // price, a fresh and in-order observation, and an identity not already folded. Never from an instruction
        // hint, a requested quantity, a zero/missing price, or another pool. The mark is the swap's own execution
        // price (SOL per raw token, the same basis as the AMM entry price): a SPOT-TRIGGER mark. It is not an
        // executable sell quote, and a fill it triggers is routing evidence only (the quarantine flag stays set).
        if self.positions.has(&mint) {
            self.model_amm_protect(&mint, &pool_s_for_protect(&a), &a, applied, ts_ms);
        }
        self.model_register(mint);
        // Event-driven: the FIRST eligible landing state fills the order, not whatever is newest at
        // the next tick.
        let c = self.model_clock_ms;
        self.model_swap_ctx = Some((ts_ms, a.slot));
        self.model_try_fills(c);
        self.model_mgmt_try_fills(c);
        self.model_swap_ctx = None;
    }

    fn model_amm_protect(
        &mut self,
        mint: &[u8; 32],
        pool_s: &str,
        a: &AmmSwapIn,
        applied: bool,
        ts_ms: i64,
    ) {
        if !applied {
            self.mrep("protect:amm_mark_ignored_out_of_order");
            self.model_protect_ignored
                .insert(*mint, ("out_of_order", ts_ms));
            return;
        }
        if self.model_cache.pool_conflicting(mint) || !self.model_cache.pool_is(mint, pool_s) {
            self.mrep("protect:amm_mark_ignored_wrong_pool");
            self.model_protect_ignored
                .insert(*mint, ("wrong_pool", ts_ms));
            return;
        }
        if self.model_clock_ms.saturating_sub(ts_ms) > crate::curve_annotation::PRICING_BUDGET_MS {
            self.mrep("protect:amm_mark_ignored_stale");
            self.model_protect_ignored.insert(*mint, ("stale", ts_ms));
            return;
        }
        // SPOT-TRIGGER MARK: the pool's own pre-trade effective price, (vault quote + virtual quote) / base reserve,
        // in PRICE_SCALE lamports per raw token - the SAME basis the AMM entry price is computed on. A single swap's
        // execution price (it embeds fees and size impact) swings far more than the pool moved and would trip the
        // single-swap rug-precursor step on noise, so it is not the trigger basis. Not an executable sell quote.
        let (Some(vq), true) = (
            a.virtual_quote,
            a.token_reserve_pre > 0 && a.quote_reserve_pre > 0,
        ) else {
            self.mrep("protect:amm_mark_ignored_no_spot_basis");
            self.model_protect_ignored
                .insert(*mint, ("no_spot_basis", ts_ms));
            return;
        };
        let id = amm_swap_identity(a, ts_ms);
        if !self.agg_remember(id) {
            self.mrep("protect:amm_mark_replay_not_applied");
            return;
        }
        let px = (u128::from(a.quote_reserve_pre) + u128::from(vq)) * 1_000_000_000
            / u128::from(a.token_reserve_pre);
        let Ok(price_u) = u64::try_from(px) else {
            self.mrep("protect:amm_mark_ignored_no_spot_basis");
            self.model_protect_ignored
                .insert(*mint, ("no_spot_basis", ts_ms));
            return;
        };
        if price_u == 0 {
            self.mrep("protect:amm_mark_ignored_no_spot_basis");
            self.model_protect_ignored
                .insert(*mint, ("no_spot_basis", ts_ms));
            return;
        }
        let signed_quote = if a.is_buy {
            i128::from(a.quote_lamports)
        } else {
            -i128::from(a.quote_lamports)
        };
        self.mrep("protect:amm_mark_applied");
        self.model_protect_mark_ms.insert(*mint, ts_ms);
        if let Some(exit) =
            self.positions
                .on_trade(mint, price_u, signed_quote, self.now, a.quote_reserve_pre)
        {
            self.mrep(format!("protect:amm_exit:{:?}", exit.reason));
            self.book_exit(exit);
        }
    }

    fn model_admit_mint(
        &mut self,
        mint: [u8; 32],
        cand_lane: WlLane,
        cand_dlane: DiscoveryLane,
    ) -> Admit {
        if self.mode == RunMode::Live || self.outbound_sink.is_some() {
            self.mrep("refuse:live_forbidden");
            return Admit::Blocked;
        }
        if let Err(fault) = self.model_source() {
            self.mrep(format!("fault:{fault:?}"));
            return Admit::Blocked;
        }
        if self.model_pool.is_none() {
            self.mrep("fault:missing_pool");
            return Admit::Blocked;
        }
        let clock = self.model_clock_ms;
        if clock == 0 {
            self.mrep("refuse:no_feed_clock");
            return Admit::Blocked;
        }
        if self.model_mint_blocked(&mint) {
            self.mrep("refuse:recon_fault_blocks_exposure");
            return Admit::Ineligible;
        }
        if self.open_lane.contains_key(&mint)
            || self.model_orders.contains_key(&mint)
            || self.model_table.has_live_for(&mint)
        {
            self.mrep("skip:held_or_pending");
            return Admit::Ineligible;
        }
        if self
            .model_last_ask
            .get(&mint)
            .is_some_and(|t| clock - *t < MODEL_REASK_MS)
        {
            self.mrep("skip:reask_window");
            return Admit::Ineligible;
        }
        if self.model_first_cand.len() < FIRST_CAND_CAP {
            self.model_first_cand.entry(mint).or_insert(clock);
        }
        let (venue, age, n) = self.model_cache.describe(&mint, clock);
        let dims = format!(
            "venue={venue}|age={}|warm={}",
            age_bucket(age),
            warm_bucket(n)
        );
        let snap = match self.model_cache.snapshot(&mint, clock) {
            Ok(s) => s,
            Err(r) => {
                if self.model_last_refusal.len() < REGISTRY_CAP {
                    self.model_last_refusal.insert(mint, r.as_str().to_string());
                }
                self.mrep(format!("refuse:{}|{dims}", r.as_str()));
                return Admit::Ineligible;
            }
        };
        self.mrep(format!("snapshot_ok|{dims}"));
        // ONE observation per mint: the delay from discovery to the FIRST usable prompt. (It was
        // previously added on every ready tick, which made n equal total asks.)
        if !self.model_uniq_seen.contains(&("ready".to_string(), mint)) {
            if let Some(first) = self.model_first_cand.get(&mint).copied() {
                *self
                    .model_report
                    .entry("first_ready_delay_ms_sum".into())
                    .or_insert(0) += (clock - first).max(0) as u64;
                *self
                    .model_report
                    .entry("first_ready_delay_n".into())
                    .or_insert(0) += 1;
            }
            self.model_uniq("ready", &mint, venue);
        }
        let sent_last_recv_ms = snap.marker.last_recv_ms;
        let id = match self
            .model_table
            .submit(mint, clock, clock + MODEL_DEADLINE_MS)
        {
            Ok(id) => id,
            Err(e) => {
                self.mrep(match e {
                    SubmitRefusal::DuplicateForMint => "refuse:request_duplicate",
                    SubmitRefusal::AtCapacity => "refuse:request_capacity",
                    SubmitRefusal::EntriesBlocked => "refuse:entries_blocked",
                });
                return match e {
                    SubmitRefusal::AtCapacity => Admit::Backpressure,
                    SubmitRefusal::DuplicateForMint => Admit::Ineligible,
                    SubmitRefusal::EntriesBlocked => Admit::Blocked,
                };
            }
        };
        let job = Job {
            id,
            mint,
            system: snap.system_prompt.clone(),
            user: snap.user_prompt.clone(),
            session: self.model_session,
        };
        self.barrier_log_dispatch("entry", &mint, &job.user);
        let dispatched = self
            .model_pool
            .as_ref()
            .map(|p| p.try_dispatch(job))
            .unwrap_or(Err(DispatchRefusal::PoolClosed));
        if let Err(e) = dispatched {
            self.model_table.release(id);
            self.mrep(match e {
                DispatchRefusal::QueueFull => "refuse:dispatch_queue_full",
                DispatchRefusal::PoolClosed => "refuse:dispatch_pool_closed",
            });
            return match e {
                DispatchRefusal::QueueFull => Admit::Backpressure,
                DispatchRefusal::PoolClosed => Admit::Blocked,
            };
        }
        self.model_last_ask.insert(mint, clock);
        self.model_meta.insert(
            id,
            ModelReqMeta {
                snap,
                lane: cand_lane,
                discovery_lane: cand_dlane,
                dims,
            },
        );
        // STATE VERSION that actually reaches the model: the prompt's decision clock and the number of
        // strictly-prior trades it was built from. Logged per dispatch (sum/count) so the staleness
        // of what Qwen sees versus the feed at dispatch time is measurable.
        // AGE OF THE NEWEST OBSERVATION INSIDE the state that was sent: dispatch clock minus the last
        // receive time the snapshot was built from. (The previous `clock - snap.t_dec_ms` compared the
        // snapshot to the instant it was cut AT, so it was identically 0 and could never show staleness.)
        self.mrep_add(
            "sent_state_age_ms_sum",
            (clock - sent_last_recv_ms).max(0) as u64,
        );
        self.mrep("sent_state_n");
        self.model_uniq("dispatched", &mint, venue);
        self.mrep(format!("dispatched|venue={venue}"));
        self.mrep("dispatched");
        Admit::Dispatched
    }

    /// Non-blocking: collect finished verdicts, expire deadlines, then try pending fills. Runs at the
    /// top of every evaluation tick, so it can never delay the feed or held-position handling.
    pub(super) fn model_poll(&mut self) {
        let clock = self.model_clock_ms;
        let mut done = Vec::new();
        if self.barrier.on {
            done = self.barrier_take_settled();
        } else if let Some(p) = &self.model_pool {
            while let Some(v) = p.try_recv() {
                done.push(v);
            }
        }
        let (mut ok_n, mut bad_n) = (0u32, 0u32);
        // ENDPOINT HEALTH is a property of the transport and the contract, never of the decision:
        //   failure  = transport error / non-200 / unreadable body (Err), a TRUNCATED completion, or a
        //              completion that does not parse as the trained grammar (malformed);
        //   healthy  = any well-formed decision, INCLUDING a valid HOLD/SKIP. A missing-data refusal or an
        //              execution veto happens AFTER this point and never counts against the endpoint.
        // Deadlines (abandoned asks) are counted separately below.
        for v in &done {
            let healthy = match &v.result {
                Ok(c) => {
                    !c.truncated()
                        && pump_quant_inference::seam::parse_decision_payload(&c.text).is_ok()
                }
                Err(_) => false,
            };
            if healthy {
                ok_n += 1;
            } else {
                bad_n += 1;
                self.mrep(match &v.result {
                    Err(_) => "endpoint:transport_error",
                    Ok(c) if c.truncated() => "endpoint:truncated",
                    Ok(_) => "endpoint:malformed",
                });
            }
        }
        for v in done {
            self.model_route_verdict(v, clock);
        }
        for _id in self.model_table.expire(clock) {
            self.mrep("request_abandoned_deadline");
            bad_n += 1;
        }
        for _id in self.model_mgmt.table.expire(clock) {
            self.mrep("mgmt:request_abandoned_deadline");
            bad_n += 1;
        }
        self.model_safety_note_endpoint(ok_n, bad_n);
        self.model_try_fills(clock);
        self.model_mgmt_try_fills(clock);
    }

    /// Offer a verdict that did NOT come out of this process's own pool (a late or replayed response). It goes through
    /// exactly the production accept path: an id this process's request table never issued, or one issued for another
    /// market, is discarded by name and creates no order. Test control for the abandoned-process case.
    pub fn model_offer_external_verdict(
        &mut self,
        session: u64,
        id: u64,
        mint: [u8; 32],
        text: &str,
    ) {
        let clock = self.model_clock_ms;
        let v = crate::model_worker::Verdict {
            id: crate::model_lane::RequestId(id),
            mint,
            session,
            result: Ok(pump_quant_inference::Completion {
                text: text.to_string(),
                finish_reason: Some("stop".to_string()),
            }),
        };
        self.model_route_verdict(v, clock);
    }

    /// The ONE place a finished verdict enters the engine (the tick poll and the external-offer test control both
    /// call it). PROCESS-SESSION BINDING: a verdict answering a request issued by another process (ids restart at 1
    /// per process) is discarded by name BEFORE it can touch either request table, so it can neither create an
    /// order nor consume or answer this process's own request that has the same number.
    fn model_route_verdict(&mut self, v: crate::model_worker::Verdict, clock: i64) {
        if v.session != self.model_session {
            self.mrep("discard:foreign_session");
            return;
        }
        if v.id.0 >= super::model_manage::MGMT_ID_BASE {
            self.model_mgmt_accept(v, clock);
        } else {
            self.model_accept(v, clock);
        }
    }

    /// This process's session id (never persisted: a restarted process always has a different one).
    #[must_use]
    pub fn model_session_id(&self) -> u64 {
        self.model_session
    }

    fn model_accept(&mut self, v: crate::model_worker::Verdict, clock: i64) {
        let meta = self.model_meta.remove(&v.id);
        let accepted = self.model_table.accept(v.id, &v.mint, clock);
        let entry = match accepted {
            Ok(e) => e,
            Err(e) => {
                self.mrep(match e {
                    AcceptRefusal::Unknown => "discard:unknown_or_duplicate",
                    AcceptRefusal::DeadlineExceeded { .. } => "discard:late",
                    AcceptRefusal::Abandoned => "discard:abandoned",
                    AcceptRefusal::EntriesBlocked => "discard:entries_blocked",
                });
                return;
            }
        };
        let Some(meta) = meta else {
            self.mrep("discard:no_binding");
            return;
        };
        // Revalidate against CURRENT state, not the state the prompt saw.
        if self.mode == RunMode::Live || self.outbound_sink.is_some() {
            self.mrep("refuse:live_forbidden");
            return;
        }
        if self.model_cache.marker(&entry.mint).is_none() || meta.snap.mint != entry.mint {
            self.mrep("discard:state_gone");
            return;
        }
        let floor = derive_survival_floor(
            self.bankroll_origin.seed_lamports(),
            self.cfg.floor_fraction_bps,
        );
        let balance = self.bankroll_balance();
        let committed = u64::try_from(self.bankroll_committed).unwrap_or(u64::MAX);
        let live = self.model_live_mints();
        let req = EntryRequest {
            system_prompt: &meta.snap.system_prompt,
            user_prompt: &meta.snap.user_prompt,
            free_cash_lamports: balance.saturating_sub(committed),
            portfolio_deployable_lamports: deployable_capital(balance, floor),
            bankroll_floor_lamports: floor,
            venue: if meta.snap.size_amm {
                EntryVenue::Amm
            } else {
                EntryVenue::BondingCurve
            },
            mint: entry.mint,
            live_mints: &live,
            portfolio: PortfolioCap::enforced(self.cfg.max_concurrent_positions),
            depth_lamports: meta.snap.depth_lamports,
            max_own_impact_bps: CHAMPION_MAX_OWN_IMPACT_BPS,
            clock: DecisionClock {
                decided_at_ms: meta.snap.t_dec_ms,
                resolved_at_ms: clock,
            },
            max_decision_age_ms: CHAMPION_MAX_DECISION_AGE_MS,
        };
        let mut ledger: DriftLedger = std::mem::take(&mut self.model_drift);
        let verdict = resolve_entry(v.result, &req, &mut ledger);
        self.model_drift = ledger;
        match verdict {
            EntryAuthority::Buy {
                clip_lamports,
                price_limit,
                ..
            } => {
                self.mrep(format!("verdict:buy|{}", meta.dims));
                self.model_order_seq += 1;
                let oid = self.model_order_seq;
                if self.model_order_log.len() < ORDER_LOG_CAP {
                    self.model_order_log.insert(
                        oid,
                        OrderRec {
                            id: oid,
                            mint: entry.mint,
                            attempt: 1,
                            clip_lamports,
                            filled_clip_lamports: 0,
                            state: OrderState::Pending,
                            terminal: None,
                        },
                    );
                }
                self.model_orders.insert(
                    entry.mint,
                    ModelOrder {
                        id: oid,
                        attempt: 1,
                        clip_lamports,
                        price_limit,
                        created_ms: clock,
                        snap_t_dec_ms: meta.snap.t_dec_ms,
                        uncertain: false,
                        confirmed: None,
                        created_slot: self.model_slot,
                        lane: meta.lane,
                        discovery_lane: meta.discovery_lane,
                        snap_price: meta.snap.price_lamports_per_raw_token,
                        amm: meta.snap.size_amm,
                    },
                );
            }
            EntryAuthority::Watch => self.mrep("verdict:watch"),
            EntryAuthority::Skip => self.mrep("verdict:skip"),
            EntryAuthority::NoTrade(r) => {
                let k = match r {
                    NoTradeReason::ModelUnreachable => "model_unreachable".to_string(),
                    NoTradeReason::OffContract(c) => format!("off_contract:{c:?}"),
                    NoTradeReason::UnpayableClip(_) => "unpayable_clip".to_string(),
                    NoTradeReason::BreachesBankrollFloor => "breaches_floor".to_string(),
                    NoTradeReason::ManagementVerb => "management_verb".to_string(),
                    NoTradeReason::Portfolio(_) => "portfolio".to_string(),
                    NoTradeReason::OwnImpact(_) => "own_impact".to_string(),
                    NoTradeReason::StaleDecision(_) => "stale_decision".to_string(),
                };
                self.mrep(format!("notrade:{k}"));
            }
        }
    }

    /// Simulated fills. The position is opened HERE and only here, from the reserves observed at or
    /// after the landing time -- never from the state the prompt saw, never at order creation.
    pub(super) fn model_try_fills(&mut self, clock: i64) {
        let mints: Vec<[u8; 32]> = self.model_orders.keys().copied().collect();
        for mint in mints {
            let Some(order) = self.model_orders.get(&mint).copied() else {
                continue;
            };
            if order.uncertain && order.confirmed.is_none() {
                // Acknowledgement unknown: still pending intent. Neither expired nor filled here.
                self.mrep("pending_uncertain_held");
                continue;
            }
            if let Some(fr) = order.confirmed {
                self.model_retire_order(&mint);
                self.mrep("fill:applied_from_reconcile");
                self.model_open_filled(mint, order, fr.reserve_sol_lamports, fr.entry_price_fp, 0);
                continue;
            }
            let landing = order.created_ms + MODEL_FILL_LANDING_MS;
            let size = order.clip_lamports;
            // Landing state: the first reserve observation at/after landing, from the plane the
            // DECISION used. A curve order is never priced from a pool, nor the reverse.
            let (reserve_sol, tokens_out, entry_price, entry_fee_bps) = if order.amm {
                let obs = self.model_cache.amm_obs(&mint).filter(|o| {
                    o.ts_ms >= landing
                        && o.ts_ms <= clock
                        && o.slot > order.created_slot
                        // Pre-trade liquidity is valid ONLY at the instant of the swap it precedes.
                        // Once that swap has been observed the pool has moved, so a later tick must
                        // never fill against it: only the swap being processed right now qualifies.
                        && self.model_swap_ctx == Some((o.ts_ms, o.slot))
                });
                let Some(obs) = obs else {
                    if clock - order.created_ms > MODEL_ORDER_TTL_MS {
                        self.model_retire_order(&mint);
                        self.mrep("fill_none:no_landing_state");
                    }
                    continue;
                };
                self.model_retire_order(&mint);
                self.model_note_latency(&order, obs.ts_ms);
                // The fee is the rate the LANDING-STATE swap's own event reported (lp + protocol +
                // creator, charged on the input). It is NEVER carried forward from an earlier swap:
                // PumpSwap fees are dynamic, so a stale rate would price a different pool state.
                // Executable economics come from the LANDING-STATE swap's own event: the fee parts and
                // the pool's virtual quote reserve at that instant. Either missing => the quote is
                // unsupported and the order is refused (never a carried-forward or defaulted value).
                let Some((Some((lp, pr, cr)), Some(vq), _)) = self
                    .model_amm_econ
                    .get(&mint)
                    .copied()
                    .filter(|(_, _, t)| *t == obs.ts_ms)
                else {
                    self.mrep("fill_none:amm_economics_not_on_landing_event");
                    continue;
                };
                // Verified `buy_exact_quote_in` arithmetic (see protocol::pumpswap_event): effective
                // quote = vault + virtual reserve; fees ceil-rounded per component on the net input.
                let Some(fill) = pump_quant_protocol::pumpswap_event::buy_exact_quote_in(
                    u128::from(obs.base_reserves_raw),
                    u128::from(obs.quote_reserves_lamports),
                    u128::from(vq),
                    u128::from(size),
                    u128::from(lp),
                    u128::from(pr),
                    u128::from(cr),
                ) else {
                    self.mrep("fill_none:unpriceable");
                    continue;
                };
                let Ok(out) = u64::try_from(fill.base_out) else {
                    self.mrep("fill_none:unpriceable");
                    continue;
                };
                // All-in average price, lamports per raw token in PRICE_SCALE units. The pool took
                // its fee from the INPUT, so `out` is already net of it: no separate entry fee.
                let px = (u128::from(size) * 1_000_000_000).div_ceil(u128::from(out));
                let Ok(px) = u64::try_from(px) else {
                    self.mrep("fill_none:unpriceable");
                    continue;
                };
                (obs.quote_reserves_lamports, out, px, 0u32)
            } else {
                let obs = self.model_cache.curve_obs(&mint).filter(|o| {
                    o.ts_ms >= landing && o.ts_ms <= clock && o.slot > order.created_slot
                });
                let Some(obs) = obs else {
                    if clock - order.created_ms > MODEL_ORDER_TTL_MS {
                        self.model_retire_order(&mint);
                        self.mrep("fill_none:no_landing_state");
                    }
                    continue;
                };
                self.model_retire_order(&mint);
                let Some(out) =
                    crate::curve_fill::buy_tokens_out(obs.v_sol_lamports, obs.v_tokens, size)
                else {
                    self.mrep("fill_none:unpriceable");
                    continue;
                };
                let Some(px) =
                    crate::curve_fill::buy_avg_price_fp(obs.v_sol_lamports, obs.v_tokens, size)
                else {
                    self.mrep("fill_none:unpriceable");
                    continue;
                };
                (
                    obs.v_sol_lamports,
                    out,
                    px,
                    crate::cost_model::venue_fee_bps_per_leg(obs.v_sol_lamports),
                )
            };
            // THE MODEL'S OWN BOUND, converted by the same authority the live sink uses.
            if let Some(limit) = order.price_limit {
                match pump_quant_execution::price_anchor::min_tokens_from_price_limit(size, limit) {
                    Ok(min_tokens) if tokens_out < min_tokens => {
                        self.mrep("fill_none:limit_not_met");
                        continue;
                    }
                    Ok(_) => {}
                    Err(_) => {
                        self.mrep("fill_none:limit_unusable");
                        continue;
                    }
                }
            }
            self.model_open_filled(mint, order, reserve_sol, entry_price, entry_fee_bps);
        }
    }

    /// Open the position for an order whose fill price/liquidity are established. The ONLY place a
    /// model order becomes inventory, so a fill is applied at most once (the caller has already
    /// removed the order; a second report finds none).
    pub(super) fn model_open_filled(
        &mut self,
        mint: [u8; 32],
        order: ModelOrder,
        reserve_sol: u64,
        entry_price: u64,
        entry_fee_bps: u32,
    ) {
        let size = order.clip_lamports;
        let Some(rt_bps) = self.unified_rt_bps(&mint, size, reserve_sol) else {
            self.mrep("fill_none:undecoded_quote");
            return;
        };
        let entry_fee = (u128::from(size) * u128::from(entry_fee_bps) / 10_000) as u64;
        let needs_ata = !self.ata_open.contains(&mint);
        let entry_cost = size
            .saturating_add(entry_fee)
            .saturating_add(crate::cost_model::FIXED_LAMPORTS_PER_LEG)
            .saturating_add(if needs_ata {
                crate::cost_model::ATA_RENT_LAMPORTS
            } else {
                0
            });
        let floor = derive_survival_floor(
            self.bankroll_origin.seed_lamports(),
            self.cfg.floor_fraction_bps,
        );
        if wallet_floor_guard(entry_cost, self.bankroll_balance(), floor)
            == FloorVerdict::RefusedBelowFloor
        {
            self.mrep("fill_none:below_wallet_floor");
            return;
        }
        let pe = PendingEntry {
            lane: order.lane,
            discovery_lane: order.discovery_lane,
            archetype: self.classify_archetype(&mint),
            mint,
            entry_price,
            size,
            entry_cost,
            // No economic band and no expected-net exist for a model entry: the brain decided
            // it, and arbitration is bypassed. Zero, not a fabricated figure.
            expected_net: 0,
            round_trip_cost_bps: rt_bps,
            entry_vsol: reserve_sol,
            entry_obs: crate::expected_move::SignalObs::none(),
            x_min: 0,
            x_cost: 0,
            x_max: 0,
            priced_move: self.priced_move(order.lane, None),
            // 2 = decoded from the curve account; 3 = migrated pool, decoded from the pool's
            // own swap-event reserves.
            depth_basis: if order.amm { 3 } else { 2 },
            brain: None,
            t_dec: None,
            price_limit: order.price_limit,
            full_clip: true,
        };
        self.open_pending(&pe);
        if self.open_lane.contains_key(&mint) {
            let (quote_validated, landing_validated) = (order.amm, false);
            if !(quote_validated && landing_validated) {
                self.model_quarantine.insert(mint);
            }
            if let Some(rec) = self.model_order_log.get_mut(&order.id) {
                rec.state = OrderState::Filled;
                rec.filled_clip_lamports = order.clip_lamports;
            }
            self.model_position_order.insert(mint, order.id);
            if order.amm {
                // The AMM fill is priced from the verified landing-state swap observed now: that is the first verified mark.
                self.model_protect_mark_ms.insert(
                    mint,
                    self.model_swap_ctx.map_or(self.model_clock_ms, |(t, _)| t),
                );
                self.model_protect_ignored.remove(&mint);
            }
            // The fill is the ONLY source of inventory: tokens delivered at the fill price.
            let tokens =
                u64::try_from(u128::from(size) * 1_000_000_000 / u128::from(entry_price.max(1)))
                    .ok()
                    .filter(|t| *t > 0);
            self.model_mgmt_on_fill(mint, tokens, entry_price, order.id);
            self.model_fills.push(ModelFillRecord {
                order_id: order.id,
                mint,
                amm: order.amm,
                quote_validated,
                landing_validated,
                from_reconcile: order.confirmed.is_some(),
            });
            self.mrep(if order.amm {
                "fill:position_opened_amm|quote=unvalidated"
            } else {
                "fill:position_opened"
            });
            self.journal.record(Decision::Promoted {
                mint,
                lane: order.lane as u8,
                rank: 0,
            });
        } else {
            self.mrep("fill_none:position_cap");
        }
    }

    /// Record the three distinct times behind a fill: decision snapshot -> verdict accepted ->
    /// reserve receipt. A sum/count pair each, so a report can show the real latencies.
    fn model_note_latency(&mut self, order: &ModelOrder, receipt_ms: i64) {
        let d2v = (order.created_ms - order.snap_t_dec_ms).max(0) as u64;
        let v2r = (receipt_ms - order.created_ms).max(0) as u64;
        for (k, v) in [("lat_dec_to_verdict", d2v), ("lat_verdict_to_receipt", v2r)] {
            *self.model_report.entry(format!("{k}_ms_sum")).or_insert(0) += v;
            *self.model_report.entry(format!("{k}_n")).or_insert(0) += 1;
        }
    }

    /// Remove a pending order from the pending book. If its log record is still pending it becomes
    /// `NotFilled` (TTL expiry / no landing state); a fill sets `Filled` afterwards, so the order of
    /// calls (retire, then open) leaves the right final state.
    pub(super) fn model_retire_order(&mut self, mint: &[u8; 32]) {
        if let Some(o) = self.model_orders.remove(mint) {
            if let Some(rec) = self.model_order_log.get_mut(&o.id) {
                if matches!(
                    rec.state,
                    OrderState::Pending | OrderState::PendingUncertain
                ) {
                    rec.state = OrderState::NotFilled;
                }
            }
        }
    }

    /// The held position on `mint` was closed: its opening order is `Closed` and the mint no longer
    /// maps to an order. Settled history is not touched.
    pub(super) fn model_on_position_closed(&mut self, mint: &[u8; 32]) {
        self.model_mgmt_forget(mint);
        if let Some(id) = self.model_position_order.remove(mint) {
            if let Some(rec) = self.model_order_log.get_mut(&id) {
                if rec.state == OrderState::Filled {
                    rec.state = OrderState::Closed;
                }
            }
        }
    }

    /// Identity of the pending order on `mint`: what a report-ingestion layer must quote back.
    #[must_use]
    pub fn model_pending_order(&self, mint: &[u8; 32]) -> Option<(OrderId, u32, u64)> {
        self.model_orders
            .get(mint)
            .map(|o| (o.id, o.attempt, o.clip_lamports))
    }

    /// Identity of the order that opened the held position on `mint`, if any.
    #[must_use]
    pub fn model_position_order_id(&self, mint: &[u8; 32]) -> Option<OrderId> {
        self.model_position_order.get(mint).copied()
    }

    /// One order's durable record.
    #[must_use]
    pub fn model_order_rec(&self, id: OrderId) -> Option<OrderRec> {
        self.model_order_log.get(&id).copied()
    }

    /// The execution acknowledgement for a pending order is unknown. The order stays pending: it is
    /// not inventory, and it is NOT cleared by the TTL. Bound to the exact order id.
    pub fn model_mark_ack_uncertain(&mut self, order_id: OrderId) -> bool {
        let Some(mint) = self.model_order_log.get(&order_id).map(|r| r.mint) else {
            self.mrep("ack:unknown_order");
            return false;
        };
        match self.model_orders.get_mut(&mint) {
            Some(o) if o.id == order_id => {
                o.uncertain = true;
                if let Some(rec) = self.model_order_log.get_mut(&order_id) {
                    rec.state = OrderState::PendingUncertain;
                }
                self.mrep("ack:uncertain_marked");
                true
            }
            _ => {
                self.mrep("ack:no_pending_order");
                false
            }
        }
    }

    /// Ingest execution evidence for ONE order. The report must quote the order id, the attempt and
    /// the quantity it speaks about; a mismatch is rejected with no state touched. Never keyed by
    /// mint, so evidence for one order cannot clear, unwind or confirm another order or pre-existing
    /// inventory. Conflicting terminal evidence is a durable fault; settled history is never rewritten.
    pub fn model_ingest_evidence(&mut self, ev: Evidence) -> EvidenceResult {
        let Some(rec) = self.model_order_log.get(&ev.order_id).copied() else {
            // Never applied, never guessed. An id at or below the compaction floor is NAMED as compacted (an order
            // we once settled and later dropped from the log): it must not read as a harmless stranger, and it can
            // never be matched to a position. An id this process never issued stays plainly unknown.
            if ev.order_id <= self.model_order_floor {
                self.mrep("evidence:unresolved:compacted_order");
                return EvidenceResult::Rejected("compacted_order");
            }
            self.mrep("evidence:unresolved:unknown_order");
            self.mrep("evidence:rejected:unknown_order");
            return EvidenceResult::Rejected("unknown_order");
        };
        if rec.attempt != ev.attempt {
            self.mrep("evidence:rejected:attempt_mismatch");
            return EvidenceResult::Rejected("attempt_mismatch");
        }
        if rec.clip_lamports != ev.clip_lamports {
            self.mrep("evidence:rejected:quantity_mismatch");
            return EvidenceResult::Rejected("quantity_mismatch");
        }
        let mint = rec.mint;
        if let Some(first) = rec.terminal {
            if first == ev.outcome {
                self.mrep("reconcile:duplicate_same_terminal");
                return EvidenceResult::Duplicate;
            }
            self.model_raise_fault(&rec, Some(first), "first_terminal_evidence", ev.outcome);
            return EvidenceResult::Fault;
        }
        match (rec.state, ev.outcome) {
            (OrderState::Pending | OrderState::PendingUncertain, ReconcileOutcome::NotFilled) => {
                self.model_orders.remove(&mint);
                if let Some(r) = self.model_order_log.get_mut(&ev.order_id) {
                    r.state = OrderState::NotFilled;
                    r.terminal = Some(ev.outcome);
                }
                self.mrep("reconcile:not_filled_cleared");
                EvidenceResult::Applied
            }
            (OrderState::Pending | OrderState::PendingUncertain, ReconcileOutcome::Filled(fr)) => {
                if let Some(o) = self.model_orders.get_mut(&mint) {
                    if o.id == ev.order_id {
                        o.confirmed = Some(fr);
                    }
                }
                if let Some(r) = self.model_order_log.get_mut(&ev.order_id) {
                    r.terminal = Some(ev.outcome);
                }
                let c = self.model_clock_ms;
                self.model_try_fills(c);
                EvidenceResult::Applied
            }
            (OrderState::Filled | OrderState::Closed, ReconcileOutcome::Filled(_)) => {
                // The paper simulator already filled this order; the evidence agrees. Recorded, no
                // second application.
                if let Some(r) = self.model_order_log.get_mut(&ev.order_id) {
                    r.terminal = Some(ev.outcome);
                }
                self.mrep("reconcile:confirms_existing_fill");
                EvidenceResult::Applied
            }
            (OrderState::Filled | OrderState::Closed, ReconcileOutcome::NotFilled) => {
                // Contradicts the book's fill. Not applied, not dropped.
                self.model_raise_fault(&rec, None, "paper_fill", ev.outcome);
                EvidenceResult::Fault
            }
            (OrderState::NotFilled, ReconcileOutcome::Filled(_)) => {
                self.model_raise_fault(
                    &rec,
                    None,
                    "expired_or_cleared_without_evidence",
                    ev.outcome,
                );
                EvidenceResult::Fault
            }
            (OrderState::NotFilled, ReconcileOutcome::NotFilled) => {
                if let Some(r) = self.model_order_log.get_mut(&ev.order_id) {
                    r.terminal = Some(ev.outcome);
                }
                self.mrep("reconcile:confirms_not_filled");
                EvidenceResult::Applied
            }
        }
    }

    /// Report-ingestion entry (the `AppEvent::ModelOrderEvidence` arm). The event's mint must equal
    /// the logged order's mint, so a mislabelled report cannot reach another market's order.
    pub(super) fn model_on_evidence_event(
        &mut self,
        mint: [u8; 32],
        order_id: OrderId,
        attempt: u32,
        clip_lamports: u64,
        filled: Option<(u64, u64)>,
    ) {
        if self
            .model_order_log
            .get(&order_id)
            .is_some_and(|r| r.mint != mint)
        {
            self.mrep("evidence:rejected:mint_mismatch");
            return;
        }
        let outcome = match filled {
            Some((entry_price_fp, reserve_sol_lamports)) => ReconcileOutcome::Filled(FillReport {
                entry_price_fp,
                reserve_sol_lamports,
            }),
            None => ReconcileOutcome::NotFilled,
        };
        let _ = self.model_ingest_evidence(Evidence {
            order_id,
            attempt,
            clip_lamports,
            outcome,
        });
    }

    fn model_raise_fault(
        &mut self,
        rec: &OrderRec,
        first: Option<ReconcileOutcome>,
        source: &'static str,
        contradicting: ReconcileOutcome,
    ) {
        let f = self
            .model_recon_faults
            .entry(rec.id)
            .or_insert_with(|| ReconFault {
                order_id: rec.id,
                mint: rec.mint,
                first,
                first_source: source,
                contradicting: Vec::new(),
            });
        f.contradicting.push(contradicting);
        self.journal.record(Decision::ReconFault {
            mint: rec.mint,
            order_id: rec.id,
            closed: u8::from(rec.state == OrderState::Closed),
        });
        self.mrep("reconcile:FAULT_conflicting_terminal");
    }

    /// Fills usable for assessing trading skill: quote AND landing validated. While the landing
    /// assumption is unvalidated (Track A) this is EMPTY by construction, so routing-test fills
    /// cannot enter PnL, evaluation or promotion reports.
    #[must_use]
    pub fn model_assessable_fills(&self) -> Vec<ModelFillRecord> {
        self.model_fills
            .iter()
            .filter(|f| f.quote_validated && f.landing_validated)
            .copied()
            .collect()
    }

    /// Exits excluded from assessment, with reasons (routing visibility; not a return series).
    #[must_use]
    pub fn model_excluded_exits(&self) -> &[ExcludedExit] {
        &self.model_excluded_exits
    }

    /// Every model fill with its validation flags (routing simulation included, labelled).
    #[must_use]
    pub fn model_all_fills(&self) -> &[ModelFillRecord] {
        &self.model_fills
    }

    /// Every order-level reconciliation fault, evidence preserved verbatim, keyed by order id.
    #[must_use]
    pub fn model_recon_faults(&self) -> &BTreeMap<OrderId, ReconFault> {
        &self.model_recon_faults
    }

    /// Resolve a fault against AUTHORITATIVE evidence for that exact order. The block is released only
    /// after the books agree for THIS order, and only this order's own effects are unwound: another
    /// order's fill, or pre-existing inventory, is never touched. A closed position is never rewritten
    /// (it needs a deliberate ledger adjustment, which does not exist yet), so that fault stays.
    pub fn model_resolve_recon_fault(
        &mut self,
        order_id: OrderId,
        authoritative: ReconcileOutcome,
    ) -> FaultResolution {
        let Some(fault) = self.model_recon_faults.get(&order_id).cloned() else {
            return FaultResolution::NoFault;
        };
        let Some(rec) = self.model_order_log.get(&order_id).copied() else {
            return FaultResolution::Refused("order_not_in_log");
        };
        let mint = fault.mint;
        let refuse = |s: &mut Self, why: &'static str| {
            s.mrep(format!("reconcile:resolution_refused:{why}"));
            FaultResolution::Refused(why)
        };
        let owns_position = self.model_position_order.get(&mint) == Some(&order_id)
            && self.open_lane.contains_key(&mint);
        match authoritative {
            ReconcileOutcome::NotFilled => match rec.state {
                OrderState::Closed => {
                    refuse(self, "position_already_closed_needs_ledger_adjustment")
                }
                OrderState::Filled if owns_position => {
                    let cost = self.open_lane.get(&mint).map_or(0, |a| a.entry_spend);
                    self.positions.reverse_paper_entry(&mint, cost);
                    self.admitted = self.admitted.saturating_sub(1);
                    self.bankroll_committed =
                        self.bankroll_committed.saturating_sub(u128::from(cost));
                    self.ata_open.remove(&mint);
                    self.open_lane.remove(&mint);
                    self.model_quarantine.remove(&mint);
                    self.model_position_order.remove(&mint);
                    self.theses.remove(&mint);
                    self.thesis_adverse.remove(&mint);
                    self.model_finish_fault(order_id, OrderState::NotFilled, authoritative);
                    self.mrep("reconcile:fault_resolved_not_filled");
                    FaultResolution::Released { unwound: true }
                }
                OrderState::Filled => {
                    refuse(self, "order_filled_but_does_not_own_the_held_position")
                }
                OrderState::Pending | OrderState::PendingUncertain => {
                    self.model_orders.remove(&mint);
                    self.model_finish_fault(order_id, OrderState::NotFilled, authoritative);
                    self.mrep("reconcile:fault_resolved_not_filled");
                    FaultResolution::Released { unwound: true }
                }
                OrderState::NotFilled => {
                    self.model_finish_fault(order_id, OrderState::NotFilled, authoritative);
                    self.mrep("reconcile:fault_resolved_not_filled");
                    FaultResolution::Released { unwound: false }
                }
            },
            ReconcileOutcome::Filled(_) => match rec.state {
                OrderState::Filled | OrderState::Closed => {
                    self.model_finish_fault(order_id, rec.state, authoritative);
                    self.mrep("reconcile:fault_resolved_filled_consistent");
                    FaultResolution::Released { unwound: false }
                }
                // No inventory exists for an order the books cleared; inventing an entry from
                // after-the-fact evidence would fabricate economics.
                OrderState::NotFilled => {
                    refuse(self, "filled_evidence_without_matching_book_state")
                }
                OrderState::Pending | OrderState::PendingUncertain => {
                    refuse(self, "filled_evidence_for_pending_order_use_ingest")
                }
            },
        }
    }

    fn model_finish_fault(
        &mut self,
        order_id: OrderId,
        state: OrderState,
        outcome: ReconcileOutcome,
    ) {
        self.model_recon_faults.remove(&order_id);
        if let Some(r) = self.model_order_log.get_mut(&order_id) {
            r.state = state;
            r.terminal = Some(outcome);
        }
    }

    /// New exposure on `mint` is blocked while ANY of its orders has an unresolved fault.
    /// Whether an unresolved reconciliation fault blocks new exposure on `mint` (the entry gate reads this).
    #[must_use]
    pub fn model_mint_is_blocked(&self, mint: &[u8; 32]) -> bool {
        self.model_mint_blocked(mint)
    }

    pub(super) fn model_mint_blocked(&self, mint: &[u8; 32]) -> bool {
        self.model_recon_faults.values().any(|f| f.mint == *mint) || self.model_sell_blocked(mint)
    }

    /// OLD-vs-NEW admission, measured on whatever stream the engine has seen: unique markets reaching
    /// the model through the legacy priced-print promotion (`legacy_promoted`), through stream
    /// discovery (`discovered`), or both, split by venue. Computed from recorded sets, not inferred.
    #[must_use]
    pub fn model_admission_comparison(&self) -> BTreeMap<String, u64> {
        let clock = self.model_clock_ms;
        let mut out: BTreeMap<String, u64> = BTreeMap::new();
        let mut all: BTreeSet<[u8; 32]> = BTreeSet::new();
        for (stage, m) in &self.model_uniq_seen {
            if stage == "legacy_promoted" || stage == "discovered" {
                all.insert(*m);
            }
        }
        for m in all {
            let old = self
                .model_uniq_seen
                .contains(&("legacy_promoted".to_string(), m));
            let new = self
                .model_uniq_seen
                .contains(&("discovered".to_string(), m));
            let (venue, _, _) = self.model_cache.describe(&m, clock);
            let k = match (old, new) {
                (true, true) => "both",
                (true, false) => "legacy_only",
                (false, true) => "stream_only",
                (false, false) => continue,
            };
            *out.entry(format!("{k}|venue={venue}")).or_insert(0) += 1;
            *out.entry(k.to_string()).or_insert(0) += 1;
        }
        out
    }

    /// The opportunity funnel by venue, in UNIQUE markets, including those that never became
    /// ready (with the last named reason each was refused). Computed from the registry, so a
    /// market that was observed but never dispatched is counted, not silently absent.
    #[must_use]
    pub fn model_funnel(&self) -> BTreeMap<String, u64> {
        let mut out: BTreeMap<String, u64> = BTreeMap::new();
        let clock = self.model_clock_ms;
        for mint in &self.model_registry {
            let (venue, _, _) = self.model_cache.describe(mint, clock);
            *out.entry(format!("discovered|venue={venue}")).or_insert(0) += 1;
            let ready = self.model_uniq_seen.contains(&("ready".to_string(), *mint));
            let disp = self
                .model_uniq_seen
                .contains(&("dispatched".to_string(), *mint));
            if ready {
                *out.entry(format!("ready|venue={venue}")).or_insert(0) += 1;
            } else {
                let why = self
                    .model_last_refusal
                    .get(mint)
                    .map_or("never_evaluated", String::as_str);
                *out.entry(format!("never_ready|venue={venue}|last={why}"))
                    .or_insert(0) += 1;
            }
            if disp {
                *out.entry(format!("dispatched|venue={venue}")).or_insert(0) += 1;
            }
        }
        out
    }
}

fn pool_s_for_protect(a: &AmmSwapIn) -> String {
    a.pool.iter().map(|b| format!("{b:02x}")).collect()
}

/// Identity of one executed pool swap for aggregate/protection dedup (no signature is carried on the event, so it is
/// the tuple that fixes one swap: market, pool, slot, trader, side, both legs, receive time).
fn amm_swap_identity(a: &AmmSwapIn, ts_ms: i64) -> u128 {
    use std::hash::{Hash, Hasher};
    let mut h1 = std::collections::hash_map::DefaultHasher::new();
    let mut h2 = std::collections::hash_map::DefaultHasher::new();
    (
        a.mint.as_bytes(),
        a.pool,
        a.slot,
        a.trader,
        a.is_buy,
        a.token_amount,
        a.quote_lamports,
        ts_ms,
    )
        .hash(&mut h1);
    (
        0xA55u16,
        ts_ms,
        a.slot,
        a.token_amount,
        a.quote_lamports,
        a.pool,
        a.trader,
    )
        .hash(&mut h2);
    (u128::from(h1.finish()) << 64) | u128::from(h2.finish())
}

impl Engine {
    /// The id of the entry request most recently issued by THIS process (0 before any). Test/barrier bookkeeping.
    #[must_use]
    pub fn model_table_last_issued_for_test(&self) -> u64 {
        self.model_table.last_issued_id()
    }
}
