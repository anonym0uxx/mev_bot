//! Model-managed position lane (paper only): Qwen decides HOLD / REDUCE / EXIT for a held position;
//! Rust keeps truthful state, executes the instruction through fill accounting and enforces the
//! agreed hard safeguards (rug precursor, hard stop — those stay in the position store and are
//! never gated on the model).
//!
//! PINNED SEMANTICS (traced to the corpus builders, not assumed):
//! * cadence: first decision once `held >= 60 s` (the corpus skips ticks with `held < min_hold`), then
//!   every 30 s. The 60 s hold gates ONLY the model's questions. Protection runs in the position store
//!   on every print and never reads it.
//! * REDUCE = `floor(inventory * 5000 / 10000)` RAW TOKENS of the inventory held NOW; EXIT = all of it.
//!   Inventory is the fill-established quantity; pending sells are not inventory and only one order per
//!   mint may be pending, so a pending quantity can never be double-spent.
//! * ADD (operator contract choice for paper, NOT a claim about the corpus's cash transitions):
//!   TARGET = `floor(inventory * 5000 / 10000)` RAW TOKENS of the reconciled inventory held at decision
//!   time, bound to the decision's position version. The seam's capital-based `ScaleIn.account_fraction_bps`
//!   is deliberately IGNORED here and never used as a size. The NOTIONAL is not copied from the corpus:
//!   at the landing state it is the minimal gross that the venue's CURRENT executable economics turn into
//!   at least the target (curve: constant product; AMM: verified `buy_exact_quote_in` with the landing
//!   event's fee parts and virtual quote). Entry price never enters the amount. Bounds, all refusals
//!   named, nothing resized: own-impact veto, free cash above the survival floor, venue economics present.
//!   One order per mint (a pending ADD/REDUCE/EXIT blocks any other); partial fills change only filled
//!   quantities and spend; SAFETY_OFF cancels any unfilled ADD remainder.
//! * the corpus caps an episode at 16 steps. Beyond it the lane keeps asking with the REAL step index,
//!   real hold time and the real fill-anchored MFE/MAE: nothing is reset and no fresh entry is invented.
//!   That is an UNVALIDATED deployment extension and is counted (`mgmt:beyond_corpus_step_cap`).
//! * only a reconciled fill changes inventory or cash. A model instruction creates an order intent.

use std::collections::BTreeMap;

use super::model_admit::{MODEL_FILL_LANDING_MS, MODEL_ORDER_TTL_MS};
use super::*;
use crate::decision_join::MgmtPositionInputs;
use crate::freshness::{DecisionClock, CHAMPION_MAX_DECISION_AGE_MS};
use crate::model_authority::{
    resolve_management, ManagementAuthority, ManagementNoAction, ManagementRequest,
};
use crate::model_lane::{RequestId, RequestTable};
use crate::model_worker::{DispatchRefusal, Job};
use crate::position::{ExitReason as PosExit, SellRefusal};

/// Management request ids live above this base so they can share the worker pool with entry ids.
pub(super) const MGMT_ID_BASE: u64 = 1 << 40;
/// The corpus's `min_hold_ms`.
pub const MGMT_MIN_HOLD_MS: i64 = 60_000;
/// The corpus's decision interval.
pub const MGMT_CADENCE_MS: i64 = 30_000;
/// The corpus's per-episode step cap (a training-data boundary, not a deployment limit).
pub const MGMT_CORPUS_STEP_CAP: i64 = 16;
/// Retry spacing after a refused snapshot (does not consume the 30 s grid).
const MGMT_RETRY_MS: i64 = 2_000;
const LAMPORTS: f64 = 1e9;

/// Kind of a management sell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MgmtKind {
    /// Half of the inventory held at order time.
    Reduce,
    /// All of it.
    Exit,
    /// Buy `floor(inventory/2)` more tokens at current executable economics (risk-increasing).
    Add,
    /// A PROTECTIVE order created by an agreed safeguard (hard stop / rug precursor / ...), never by the model.
    /// It lives in its own per-mint slot, shares the management id sequence and settles through the same books.
    Protect,
}

/// A pending management sell intent. Not inventory; becomes one only through a fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MgmtOrder {
    /// Order id (own namespace, never an entry order id).
    pub id: u64,
    /// Reduce or exit.
    pub kind: MgmtKind,
    /// Tokens the instruction asked to sell.
    pub intended: u64,
    /// Tokens already filled (partial fills keep the remainder pending and monitored).
    pub filled: u64,
    pub(super) created_ms: i64,
    pub(super) created_slot: u64,
    pub(super) version: u64,
    pub(super) amm: bool,
    /// ADD only: the most notional lamports this order may spend in total (set at landing).
    pub max_spend: u64,
    /// ADD only: notional lamports already spent by reconciled fills.
    pub spent: u64,
    /// ADD only: the venue fee rate (bp of notional) the reservation was sized with.
    pub fee_bps: u32,
    /// Acknowledgement unknown: stays pending and unresolved; never expired, filled by the simulator,
    /// or cancelled by a trip. Only a reconciled report or operator evidence resolves it.
    pub uncertain: bool,
    /// REDUCE/EXIT: cumulative gross proceeds (lamports) the executor reported and the books applied.
    pub gross: u64,
    /// REDUCE/EXIT: cumulative all-in fees (lamports) likewise.
    pub fees: u64,
    /// Protective orders only: `ExitReason::code()` of the safeguard that created it (0 = not protective).
    pub protect: u8,
    /// Some fill of this order came from the PAPER EXECUTOR (modelled price and fees, not executed evidence).
    pub simulated: bool,
}

/// How a management order ended. A live order is not in the log (it is in `MgmtLane::orders`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SellState {
    /// Filled to its intended quantity.
    Completed,
    /// Ended (TTL / refusal / trip) with a partial fill; the remainder never executed.
    EndedPartial,
    /// Ended with nothing filled.
    EndedUnfilled,
    /// The position was closed by another path (a protective exit) while the order was unfinished.
    Preempted,
}

/// A settled management order: identity, cumulative fills and terminal state. Never carries financial effects
/// (cash, inventory and realized results live in the books); it exists so a late or duplicate report is
/// recognised and cannot act twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SellRec {
    pub id: u64,
    pub mint: [u8; 32],
    pub kind: MgmtKind,
    pub intended: u64,
    pub filled: u64,
    /// ADD: cumulative notional spent.
    pub spent: u64,
    pub state: SellState,
    pub last_price_fp: u64,
    /// Cumulative gross proceeds / all-in fees (lamports) applied (REDUCE/EXIT).
    pub gross: u64,
    pub fees: u64,
    /// Some fill came from the PAPER EXECUTOR (modelled price/fees): economics unvalidated.
    pub simulated: bool,
}

/// An unresolved management-order conflict. Blocks new exposure on its mint until released.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SellFault {
    pub order_id: u64,
    pub mint: [u8; 32],
    /// `report_exceeds_order`, `report_contradicts_settled` or `uncertain_sell_preempted`.
    pub source: &'static str,
    /// Cumulative filled according to the books when the conflict arose.
    pub books_filled: u64,
    /// The contradicting cumulative reports, verbatim.
    pub reported: Vec<u64>,
}

/// What ingesting one management report did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SellReportResult {
    /// New fill applied (the delta over the books' cumulative).
    Applied { delta: u64 },
    /// Already reflected (equal or older cumulative): nothing changed.
    Duplicate,
    /// Contradicts the books: evidence preserved, fault raised, nothing applied, mint blocked.
    Fault,
    /// Refused without touching state, with the named reason (`unknown_order`, `compacted_order`, ...).
    Rejected(&'static str),
}

/// Settled management-order records kept before the oldest are compacted (a floor then names them).
pub const SELL_LOG_CAP: usize = 4_096;
/// Cumulative settlement checkpoints kept per management order (proof that an older report is stale).
pub const SELL_PREFIX_CAP: usize = 64;

/// Management ADD target: half of the reconciled inventory.
pub const MGMT_ADD_INVENTORY_BPS: u32 = 5_000;

/// One reconciled management fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MgmtFill {
    /// The order it belongs to.
    pub order_id: u64,
    /// Market.
    pub mint: [u8; 32],
    /// Tokens sold by this fill.
    pub tokens: u64,
    /// Fill price, fixed point (lamports per raw token * 1e9).
    pub price_fp: u64,
    /// Whether it closed the position.
    pub closed: bool,
    /// Net realised lamports of this fill (0 for an ADD: a buy realizes nothing).
    pub net_lamports: i128,
    /// ADD fills only: notional lamports spent by this fill.
    pub spent_lamports: u64,
    /// ADD fills only: all-in cost (notional + fee + fixed leg cost) charged to cash.
    pub cost_lamports: u64,
    /// Whether this fill was an ADD.
    pub is_add: bool,
    /// Sells: gross proceeds and all-in fees of THIS fill (0 on the simulated-price path, which derives them).
    pub gross_lamports: u64,
    pub fee_lamports: u64,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct MgmtPos {
    pub fill_ms: i64,
    pub entry_px_fp: u64,
    pub peak_fp: u64,
    pub trough_fp: u64,
    pub step: i64,
    pub last_ask_ms: Option<i64>,
    pub last_try_ms: i64,
    /// Bumped by every fill: a verdict bound to an older version is discarded.
    pub version: u64,
    pub position_order: u64,
}

#[derive(Debug, Clone)]
pub(super) struct MgmtMeta {
    pub snap: crate::decision_join::MgmtSnapshot,
    pub version: u64,
    pub position_order: u64,
    #[allow(dead_code)] // decision step index; carried for provenance, not yet read
    pub step: i64,
}

/// All lane state in one field of the engine.
#[derive(Debug)]
pub(super) struct MgmtLane {
    pub table: RequestTable,
    pub meta: BTreeMap<RequestId, MgmtMeta>,
    pub pos: BTreeMap<[u8; 32], MgmtPos>,
    pub orders: BTreeMap<[u8; 32], MgmtOrder>,
    /// Protective orders, one per mint, beside (never replacing) an unresolved management sell.
    pub protect: BTreeMap<[u8; 32], MgmtOrder>,
    pub seq: u64,
    pub fills: Vec<MgmtFill>,
}

impl MgmtLane {
    pub fn new() -> Self {
        Self {
            table: RequestTable::with_id_base(4, MGMT_ID_BASE),
            meta: BTreeMap::new(),
            pos: BTreeMap::new(),
            orders: BTreeMap::new(),
            protect: BTreeMap::new(),
            seq: 0,
            fills: Vec::new(),
        }
    }
}

impl Engine {
    /// Hand a freshly FILLED model position to the management lane. Called only from the fill path.
    pub(super) fn model_mgmt_on_fill(
        &mut self,
        mint: [u8; 32],
        tokens: Option<u64>,
        entry_price_fp: u64,
        order_id: u64,
    ) {
        self.positions.set_model_managed(&mint);
        if let Some(t) = tokens {
            self.positions.set_inventory_tokens(&mint, t);
        } else {
            self.mrep("mgmt:inventory_unknown_at_fill");
        }
        self.model_mgmt.pos.insert(
            mint,
            MgmtPos {
                fill_ms: self.model_clock_ms,
                entry_px_fp: entry_price_fp,
                peak_fp: entry_price_fp,
                trough_fp: entry_price_fp,
                step: 0,
                last_ask_ms: None,
                last_try_ms: i64::MIN / 2,
                version: 1,
                position_order: order_id,
            },
        );
    }

    /// The position closed: drop every management trace of it (a late verdict is then discarded).
    pub(super) fn model_mgmt_forget(&mut self, mint: &[u8; 32]) {
        self.model_mgmt.pos.remove(mint);
        self.model_protect_forget(mint);
        // The position is gone: an unfinished order on it is PREEMPTED (a protective or other close ended it).
        self.model_mgmt_end(mint, true);
    }

    /// Causal MFE/MAE tracker: every priced print on a held mint since the fill.
    pub(super) fn model_mgmt_note_price(
        &mut self,
        mint: &[u8; 32],
        price_fp: i128,
        recv_unix_ms: Option<i64>,
    ) {
        let Some(mp) = self.model_mgmt.pos.get_mut(mint) else {
            return;
        };
        // The excursion is "since the fill". An overlap replay after a restart re-delivers prints that PREDATE the
        // fill; they rebuild market state but are not part of this position's life. A print with no wire time cannot
        // be ordered against the fill, so it does not count either.
        match recv_unix_ms {
            Some(t) if t >= mp.fill_ms => {}
            _ => return,
        }
        let Ok(p) = u64::try_from(price_fp) else {
            return;
        };
        if p == 0 {
            return;
        }
        mp.peak_fp = mp.peak_fp.max(p);
        mp.trough_fp = mp.trough_fp.min(p);
    }

    fn model_mgmt_inputs(
        &self,
        mint: &[u8; 32],
        clock: i64,
    ) -> Result<MgmtPositionInputs, &'static str> {
        let mp = self.model_mgmt.pos.get(mint).ok_or("no_mgmt_state")?;
        let inv = self
            .positions
            .inventory_tokens(mint)
            .ok_or("inventory_unknown")?;
        if mp.entry_px_fp == 0 {
            return Err("entry_price_unknown");
        }
        let entry = mp.entry_px_fp as f64;
        let bp = |p: u64| (p as f64 / entry - 1.0) * 1e4;
        // Account cash = balance - committed entry cost - capital reserved by pending entry orders.
        let balance = self.bankroll_balance();
        let committed = u64::try_from(self.bankroll_committed).unwrap_or(u64::MAX);
        let pending: u64 = self
            .model_orders
            .values()
            .fold(0u64, |a, o| a.saturating_add(o.clip_lamports));
        let cash = balance
            .saturating_sub(committed)
            .saturating_sub(pending)
            .saturating_sub(self.model_mgmt_reserved(None));
        Ok(MgmtPositionInputs {
            step: mp.step,
            entry_px: entry / LAMPORTS,
            qty_scaled: inv as f64 / LAMPORTS,
            cash_sol: cash as f64 / LAMPORTS,
            held_s: ((clock - mp.fill_ms).max(0)) as f64 / 1000.0,
            mfe_bp: bp(mp.peak_fp),
            mae_bp: bp(mp.trough_fp),
        })
    }

    /// Ask the model about every held position that is due. Never blocks, never gates protection.
    pub(super) fn model_mgmt_schedule(&mut self) {
        let clock = self.model_clock_ms;
        if clock == 0 || self.model_pool.is_none() {
            return;
        }
        let mints: Vec<[u8; 32]> = self.model_mgmt.pos.keys().copied().collect();
        for mint in mints {
            if !self.positions.has(&mint) {
                self.model_mgmt_forget(&mint);
                continue;
            }
            if self.model_mgmt.orders.contains_key(&mint)
                || self.model_mgmt.protect.contains_key(&mint)
                || self.model_mgmt.table.has_live_for(&mint)
            {
                continue;
            }
            let Some(mp) = self.model_mgmt.pos.get(&mint).copied() else {
                continue;
            };
            let due = mp
                .last_ask_ms
                .map_or(mp.fill_ms + MGMT_MIN_HOLD_MS, |t| t + MGMT_CADENCE_MS);
            if clock < due || clock - mp.last_try_ms < MGMT_RETRY_MS {
                continue;
            }
            if let Some(m) = self.model_mgmt.pos.get_mut(&mint) {
                m.last_try_ms = clock;
            }
            let inputs = match self.model_mgmt_inputs(&mint, clock) {
                Ok(i) => i,
                Err(r) => {
                    self.mrep(format!("mgmt:refuse:{r}"));
                    continue;
                }
            };
            let snap = match self.model_cache.management_snapshot(&mint, clock, &inputs) {
                Ok(s) => s,
                Err(r) => {
                    self.mrep(format!("mgmt:refuse:{}", r.as_str()));
                    continue;
                }
            };
            let id = match self.model_mgmt.table.submit(
                mint,
                clock,
                clock + CHAMPION_MAX_DECISION_AGE_MS as i64,
            ) {
                Ok(id) => id,
                Err(e) => {
                    self.mrep(format!("mgmt:refuse:submit:{e:?}"));
                    continue;
                }
            };
            let job = Job {
                id,
                mint,
                system: snap.system_prompt.clone(),
                user: snap.user_prompt.clone(),
                session: self.model_session,
            };
            self.barrier_log_dispatch("mgmt", &mint, &job.user);
            let dispatched = self
                .model_pool
                .as_ref()
                .map(|p| p.try_dispatch(job))
                .unwrap_or(Err(DispatchRefusal::PoolClosed));
            if let Err(e) = dispatched {
                self.model_mgmt.table.release(id);
                self.mrep(format!("mgmt:refuse:dispatch:{e:?}"));
                continue;
            }
            // State age that actually reaches the model: the newest print behind the prompt.
            self.mrep_add(
                "mgmt:sent_state_age_ms_sum",
                (clock - snap.marker.last_recv_ms).max(0) as u64,
            );
            self.mrep("mgmt:sent_state_n");
            if mp.step >= MGMT_CORPUS_STEP_CAP {
                self.mrep("mgmt:beyond_corpus_step_cap");
            }
            let version = mp.version;
            let position_order = mp.position_order;
            self.model_mgmt.meta.insert(
                id,
                MgmtMeta {
                    snap,
                    version,
                    position_order,
                    step: mp.step,
                },
            );
            if let Some(m) = self.model_mgmt.pos.get_mut(&mint) {
                m.last_ask_ms = Some(clock);
                m.step += 1;
            }
            self.mrep("mgmt:dispatched");
        }
    }

    /// A management verdict arrived. Everything is revalidated against CURRENT state.
    pub(super) fn model_mgmt_accept(&mut self, v: crate::model_worker::Verdict, clock: i64) {
        let meta = self.model_mgmt.meta.remove(&v.id);
        let entry = match self.model_mgmt.table.accept(v.id, &v.mint, clock) {
            Ok(e) => e,
            Err(e) => {
                self.mrep(match e {
                    crate::model_lane::AcceptRefusal::Unknown => {
                        "mgmt:discard:unknown_or_duplicate"
                    }
                    crate::model_lane::AcceptRefusal::DeadlineExceeded { .. } => {
                        "mgmt:discard:late"
                    }
                    crate::model_lane::AcceptRefusal::Abandoned => "mgmt:discard:abandoned",
                    crate::model_lane::AcceptRefusal::EntriesBlocked => "mgmt:discard:blocked",
                });
                return;
            }
        };
        let mint = entry.mint;
        let Some(meta) = meta else {
            self.mrep("mgmt:discard:no_binding");
            return;
        };
        let (Some(mp), true) = (
            self.model_mgmt.pos.get(&mint).copied(),
            self.positions.has(&mint),
        ) else {
            self.mrep("mgmt:discard:position_gone");
            return;
        };
        if mp.position_order != meta.position_order || mp.version != meta.version {
            self.mrep("mgmt:discard:state_version_changed");
            return;
        }
        if self.model_mgmt.orders.contains_key(&mint) || self.model_mgmt.protect.contains_key(&mint)
        {
            self.mrep("mgmt:discard:order_already_pending");
            return;
        }
        let req = ManagementRequest {
            system_prompt: &meta.snap.system_prompt,
            user_prompt: &meta.snap.user_prompt,
            clock: DecisionClock {
                decided_at_ms: meta.snap.t_dec_ms,
                resolved_at_ms: clock,
            },
            max_decision_age_ms: CHAMPION_MAX_DECISION_AGE_MS,
        };
        let mut ledger = std::mem::take(&mut self.model_drift);
        let verdict = resolve_management(v.result, &req, &mut ledger);
        self.model_drift = ledger;
        match verdict {
            ManagementAuthority::Hold => self.mrep("mgmt:verdict:hold"),
            // The seam's capital-based field is intentionally unused: ADD is inventory-based here.
            ManagementAuthority::ScaleIn { .. } => {
                self.model_mgmt_place(mint, MgmtKind::Add, MGMT_ADD_INVENTORY_BPS, clock)
            }
            ManagementAuthority::Trim {
                inventory_fraction_bps,
            } => self.model_mgmt_place(mint, MgmtKind::Reduce, inventory_fraction_bps, clock),
            ManagementAuthority::CloseAll => {
                self.model_mgmt_place(mint, MgmtKind::Exit, 10_000, clock)
            }
            ManagementAuthority::NoAction(r) => {
                let k = match r {
                    ManagementNoAction::ModelUnreachable => "model_unreachable".to_string(),
                    ManagementNoAction::OffContract(c) => format!("off_contract:{c:?}"),
                    ManagementNoAction::EntryVerbOnManagementPrompt => "entry_verb".to_string(),
                    ManagementNoAction::StaleDecision(_) => "stale_decision".to_string(),
                };
                self.mrep(format!("mgmt:noaction:{k}"));
            }
        }
    }

    fn model_mgmt_place(&mut self, mint: [u8; 32], kind: MgmtKind, bps: u32, clock: i64) {
        if kind == MgmtKind::Add && self.model_safety_blocked() {
            // Risk-increasing: never while SAFETY_OFF holds. REDUCE/EXIT stay permitted.
            self.mrep("mgmt:refuse:add_blocked_safety_off");
            return;
        }
        let Some(inv) = self.positions.inventory_tokens(&mint) else {
            self.mrep("mgmt:refuse:inventory_unknown");
            return;
        };
        let tokens = u64::try_from(u128::from(inv) * u128::from(bps) / 10_000).unwrap_or(0);
        if tokens == 0 {
            self.mrep("mgmt:refuse:quantity_rounds_to_zero");
            return;
        }
        let Some(mp) = self.model_mgmt.pos.get(&mint).copied() else {
            return;
        };
        let (mut add_bound, mut add_fee_bps) = (0u64, 0u32);
        if kind == MgmtKind::Add {
            // Executable-economics plan against the LATEST observed state (not entry price): a refusal
            // here is named and creates no order.
            match self.model_mgmt_add_plan(&mint, tokens, None, None) {
                Ok(plan) => {
                    add_bound = plan.hi;
                    add_fee_bps = plan.fee_bps;
                }
                Err(r) => {
                    self.mrep(r);
                    return;
                }
            }
        }
        self.model_mgmt.seq += 1;
        let id = self.model_mgmt.seq;
        let amm = self.model_cache.snapshot_venue_is_amm(&mint);
        self.model_mgmt.orders.insert(
            mint,
            MgmtOrder {
                id,
                kind,
                intended: tokens,
                filled: 0,
                created_ms: clock,
                created_slot: self.model_slot,
                version: mp.version,
                amm,
                max_spend: add_bound,
                spent: 0,
                fee_bps: add_fee_bps,
                uncertain: false,
                gross: 0,
                fees: 0,
                protect: 0,
                simulated: false,
            },
        );
        if self.model_external_exec && kind != MgmtKind::Add {
            // Submitted to the external executor: unresolved until it reports. Never paper-filled.
            if let Some(o) = self.model_mgmt.orders.get_mut(&mint) {
                o.uncertain = true;
            }
            self.mrep("mgmt:submitted_external");
        }
        self.mrep(match kind {
            MgmtKind::Reduce => "mgmt:order:reduce",
            MgmtKind::Exit => "mgmt:order:exit",
            MgmtKind::Add => "mgmt:order:add",
            MgmtKind::Protect => "mgmt:order:protect",
        });
    }

    /// Try to fill pending management sells against LANDING state (never the prompt's state).
    pub(super) fn model_mgmt_try_fills(&mut self, clock: i64) {
        let pm: Vec<[u8; 32]> = self.model_mgmt.protect.keys().copied().collect();
        for m in pm {
            let _ = self.model_with_protect_mint(&m, |e| e.model_mgmt_try_fills_orders(clock));
        }
        self.model_mgmt_try_fills_orders(clock);
    }

    fn model_mgmt_try_fills_orders(&mut self, clock: i64) {
        let mints: Vec<[u8; 32]> = self.model_mgmt.orders.keys().copied().collect();
        for mint in mints {
            let Some(order) = self.model_mgmt.orders.get(&mint).copied() else {
                continue;
            };
            if !self.positions.has(&mint) {
                self.model_mgmt_forget(&mint);
                continue;
            }
            if order.uncertain {
                // Acknowledgement unknown: unresolved intent. Neither expired nor simulated-filled.
                self.mrep("mgmt:pending_uncertain_held");
                continue;
            }
            if order.kind == MgmtKind::Add {
                self.model_mgmt_try_add(mint, order, clock);
                continue;
            }
            let landing = order.created_ms + MODEL_FILL_LANDING_MS;
            let tokens = order.intended - order.filled;
            // M3: a SIZE-SPECIFIC quote for exactly the order's remaining tokens against the causal landing state
            // (curve: incl. REAL SOL; pool: the landing swap's own fee parts + virtual quote). Requoted on every
            // landing state at the SAME size: the model's quantity is never resized. A refusal leaves the order
            // pending until TTL (state may change) and is counted by name; it never fabricates a settlement.
            let quoted: Option<
                Result<crate::exec_quote::SellQuote, crate::exec_quote::QuoteRefusal>,
            > = if order.amm {
                self.model_cache
                    .amm_obs(&mint)
                    .filter(|o| {
                        o.ts_ms >= landing
                            && o.ts_ms <= clock
                            && o.slot > order.created_slot
                            && self.model_swap_ctx == Some((o.ts_ms, o.slot))
                    })
                    .map(|o| match self.model_amm_econ.get(&mint).copied() {
                        Some((parts, vq, t)) if t == o.ts_ms => crate::exec_quote::amm_sell(
                            o.base_reserves_raw,
                            o.quote_reserves_lamports,
                            vq,
                            parts,
                            tokens,
                        ),
                        _ => Err(crate::exec_quote::QuoteRefusal::AmmEconomicsMissing),
                    })
            } else {
                self.model_cache
                    .curve_obs(&mint)
                    .filter(|o| {
                        o.ts_ms >= landing && o.ts_ms <= clock && o.slot > order.created_slot
                    })
                    .map(|o| {
                        crate::exec_quote::curve_sell(
                            o.v_sol_lamports,
                            o.v_tokens,
                            o.real_sol_lamports,
                            tokens,
                        )
                    })
            };
            let label = if order.amm {
                "mgmt:fill_amm_sell"
            } else {
                "mgmt:fill_curve"
            };
            let priced = match quoted {
                Some(Ok(q)) => Some(q),
                Some(Err(r)) => {
                    let l = format!("mgmt:{}", r.label());
                    if order.kind == MgmtKind::Protect {
                        self.mrep(l.replacen("mgmt:", "protect:", 1));
                    } else {
                        self.mrep(l);
                    }
                    None
                }
                None => None,
            };
            match priced {
                Some(q) => {
                    // PAPER EXECUTOR. Same cumulative settlement evidence a real executor reports (tokens, gross,
                    // all-in fees), booked through the one settlement path. ASSUMPTIONS (explicit): the landing
                    // state is the first observation >= 400 ms after creation on a newer slot; the quote is the
                    // program's arithmetic at that state for this exact size; fees = venue fees from the quote +
                    // network fee (measured p50) + exit tip, each once. Not executed evidence: `simulated`.
                    let leg = crate::exec_quote::landed_leg_cost(self.cfg.exit_tip_lamports);
                    let (g, f) = (q.gross, q.venue_fees.saturating_add(leg));
                    let Some(px) = crate::exec_quote::price_fp(q.net, tokens) else {
                        self.mrep("mgmt:quote_unavailable:unpriceable");
                        continue;
                    };
                    // Position-side refusals (not held / inventory unknown / exceeds inventory) by name.
                    if self
                        .positions
                        .simulate_sell_settlement(&mint, tokens, px.max(1))
                        .is_err()
                    {
                        self.model_mgmt_book(mint, order, tokens, px, None);
                        continue;
                    }
                    if order.kind == MgmtKind::Protect {
                        self.mrep(label.replacen("mgmt:", "protect:", 1));
                    } else {
                        self.mrep(label);
                    }
                    if q.net <= leg {
                        // Dust: venue-net does not cover the landed leg. Booked as what it is (a net cost),
                        // never presented as recovery.
                        self.mrep("econ:sell_net_below_leg_cost");
                    }
                    self.mrep_add("econ:exit_gross_lamports", q.gross);
                    self.mrep_add("econ:exit_venue_fees_lamports", q.venue_fees);
                    self.mrep_add(
                        "econ:exit_network_lamports",
                        crate::exec_quote::NETWORK_FEE_P50_LAMPORTS,
                    );
                    self.mrep_add("econ:exit_tip_lamports", self.cfg.exit_tip_lamports);
                    if let Some(o) = self.model_mgmt.orders.get_mut(&mint) {
                        o.simulated = true;
                    }
                    let r = self.model_mgmt_ingest_evidence_inner(
                        mint,
                        order.id,
                        order.kind,
                        order.intended,
                        order.filled + tokens,
                        order.gross + g,
                        order.fees + f,
                    );
                    if !matches!(r, SellReportResult::Applied { .. }) {
                        self.mrep("mgmt:paper_fill:not_applied");
                    }
                }
                None => {
                    if clock - order.created_ms > MODEL_ORDER_TTL_MS {
                        self.model_mgmt_end(&mint, false);
                        self.mrep("mgmt:order_expired_unfilled");
                    }
                }
            }
        }
    }

    /// Apply a RECONCILED fill (full or partial) reported for a pending management order. The order is
    /// matched by id and the quantity is checked against what is still pending: a stale or duplicate
    /// report changes nothing.
    pub fn model_mgmt_apply_reconciled_fill(
        &mut self,
        mint: [u8; 32],
        order_id: u64,
        tokens: u64,
        price_fp: u64,
    ) -> Result<(), &'static str> {
        let Some(order) = self.model_mgmt.orders.get(&mint).copied() else {
            self.mrep("mgmt:recon:rejected:no_pending_order");
            return Err("no_pending_order");
        };
        if order.id != order_id {
            self.mrep("mgmt:recon:rejected:order_id_mismatch");
            return Err("order_id_mismatch");
        }
        if order.kind == MgmtKind::Add {
            self.mrep("mgmt:recon:rejected:wrong_kind");
            return Err("wrong_kind");
        }
        if tokens == 0 || tokens > order.intended - order.filled {
            self.mrep("mgmt:recon:rejected:quantity");
            return Err("quantity");
        }
        self.model_mgmt_clear_uncertain(&mint);
        self.model_mgmt_book(mint, order, tokens, price_fp, None);
        Ok(())
    }

    /// Apply the INCREMENT of an authoritative cumulative settlement to a pending REDUCE/EXIT: `tokens` sold for
    /// `gross` lamports with all-in `fee` lamports, both for exactly this increment. The fill price is derived from
    /// the increment (`gross / tokens`), so two partial fills at different prices each book their own proceeds.
    fn model_mgmt_apply_settled_increment(
        &mut self,
        mint: [u8; 32],
        order_id: u64,
        tokens: u64,
        gross: u64,
        fee: u64,
    ) -> Result<(), &'static str> {
        let Some(order) = self.model_mgmt.orders.get(&mint).copied() else {
            return Err("no_pending_order");
        };
        if order.id != order_id || order.kind == MgmtKind::Add {
            return Err("order_id_mismatch");
        }
        if tokens == 0 || tokens > order.intended - order.filled || gross == 0 || fee > gross {
            self.mrep("mgmt:recon:rejected:quantity_or_amount");
            return Err("quantity");
        }
        let px = u64::try_from((u128::from(gross) * 1_000_000_000).div_ceil(u128::from(tokens)))
            .map_err(|_| "price_overflow")?;
        self.model_mgmt_clear_uncertain(&mint);
        self.model_mgmt_book(mint, order, tokens, px, Some((gross, fee)));
        Ok(())
    }

    fn model_mgmt_book(
        &mut self,
        mint: [u8; 32],
        order: MgmtOrder,
        tokens: u64,
        price_fp: u64,
        settled: Option<(u64, u64)>,
    ) {
        let inv_before = self.positions.inventory_tokens(&mint).unwrap_or(0);
        // A protective order books under the safeguard that created it; every other sell is `ModelManaged`.
        let reason = PosExit::from_code(order.protect)
            .filter(|_| order.kind == MgmtKind::Protect)
            .unwrap_or(PosExit::ModelManaged);
        let sold = match settled {
            Some(gf) => self
                .positions
                .sell_tokens_settled(&mint, tokens, price_fp, reason, gf),
            None => self.positions.sell_tokens(&mint, tokens, price_fp, reason),
        };
        let exit = match sold {
            Ok(e) => e,
            Err(r) => {
                // Named, nothing substituted, order dropped so the next cadence decides afresh.
                self.mrep(match r {
                    SellRefusal::ExceedsInventory { .. } => "mgmt:refuse:exceeds_inventory",
                    SellRefusal::InventoryUnknown => "mgmt:refuse:inventory_unknown",
                    SellRefusal::ZeroQuantity => "mgmt:refuse:zero_quantity",
                    SellRefusal::NoPrice => "mgmt:refuse:no_price",
                    SellRefusal::NotHeld => "mgmt:refuse:not_held",
                });
                self.model_mgmt_end(&mint, false);
                return;
            }
        };
        // Partial sell: free the sold share of the committed entry cost so cash reflects proceeds.
        if !exit.closed && inv_before > 0 {
            if let Some(att) = self.open_lane.get_mut(&mint) {
                let rel = u64::try_from(
                    u128::from(att.entry_spend) * u128::from(tokens) / u128::from(inv_before),
                )
                .unwrap_or(0)
                .min(att.entry_spend);
                att.entry_spend -= rel;
                self.bankroll_committed = self.bankroll_committed.saturating_sub(u128::from(rel));
            }
        }
        let net = exit.net_lamports;
        let closed = exit.closed;
        if let Some(o) = self.model_mgmt.orders.get_mut(&mint) {
            o.filled += tokens;
            o.version += 1;
            if let Some((g, f)) = settled {
                o.gross += g;
                o.fees += f;
            }
        }
        self.book_exit(exit);
        self.model_mgmt.fills.push(MgmtFill {
            order_id: order.id,
            mint,
            tokens,
            price_fp,
            closed,
            net_lamports: net,
            spent_lamports: 0,
            cost_lamports: 0,
            is_add: false,
            gross_lamports: settled.map_or(0, |x| x.0),
            fee_lamports: settled.map_or(0, |x| x.1),
        });
        let ns = if order.kind == MgmtKind::Protect {
            "protect"
        } else {
            "mgmt"
        };
        if closed {
            self.model_mgmt_forget(&mint);
            self.mrep(format!("{ns}:fill:closed"));
            return;
        }
        if let Some(mp) = self.model_mgmt.pos.get_mut(&mint) {
            mp.version += 1;
        }
        let done = self
            .model_mgmt
            .orders
            .get(&mint)
            .is_some_and(|o| o.filled >= o.intended);
        if done {
            self.model_mgmt_end(&mint, false);
            self.mrep(format!("{ns}:fill:complete"));
        } else {
            // Partial: the remainder stays pending and monitored (TTL applies to the remainder).
            self.mrep(format!("{ns}:fill:partial_remainder_pending"));
        }
    }

    /// The pending ADD order's reservation on `mint`: `(max_spend, fee_bps, spent, fees, free_cash_held_back)`.
    /// `free_cash_held_back` is what the order still reserves out of free cash (remaining spend + its fee + one fixed leg).
    #[must_use]
    pub fn model_mgmt_add_reservation(&self, mint: &[u8; 32]) -> Option<(u64, u32, u64, u64, u64)> {
        self.model_mgmt
            .orders
            .get(mint)
            .filter(|o| o.kind == MgmtKind::Add)
            .map(|o| {
                (
                    o.max_spend,
                    o.fee_bps,
                    o.spent,
                    o.fees,
                    self.model_mgmt_reserved(None),
                )
            })
    }

    /// The pending management order on `mint`: (id, kind, intended, filled).
    #[must_use]
    pub fn model_mgmt_pending(&self, mint: &[u8; 32]) -> Option<(u64, MgmtKind, u64, u64)> {
        self.model_mgmt
            .orders
            .get(mint)
            .map(|o| (o.id, o.kind, o.intended, o.filled))
    }

    /// OFFLINE REPLAY HARNESS ONLY: route new REDUCE/EXIT/protective orders to the external executor (the report inbox)
    /// instead of the paper executor. Each such order is submitted UNRESOLVED and changes nothing until a validated report.
    pub fn model_set_external_execution(&mut self, on: bool) {
        self.model_external_exec = on;
    }

    /// The pending management order on `mint` with its execution status: (id, kind, intended, filled, uncertain).
    /// `uncertain` = submitted and not definitively resolved (acknowledgement unknown or externally working).
    #[must_use]
    pub fn model_mgmt_order_status(
        &self,
        mint: &[u8; 32],
    ) -> Option<(u64, MgmtKind, u64, u64, bool)> {
        self.model_mgmt
            .orders
            .get(mint)
            .map(|o| (o.id, o.kind, o.intended, o.filled, o.uncertain))
    }

    /// Every reconciled management fill, in order.
    #[must_use]
    pub fn model_mgmt_fills(&self) -> &[MgmtFill] {
        &self.model_mgmt.fills
    }

    /// Authoritative inventory of a held mint (None = unknown, never zero).
    #[must_use]
    pub fn model_inventory_tokens(&self, mint: &[u8; 32]) -> Option<u64> {
        self.positions.inventory_tokens(mint)
    }

    /// Free account cash as the management prompt states it, lamports.
    #[must_use]
    pub fn model_free_cash_lamports(&self) -> u64 {
        let committed = u64::try_from(self.bankroll_committed).unwrap_or(u64::MAX);
        let pending: u64 = self
            .model_orders
            .values()
            .fold(0u64, |a, o| a.saturating_add(o.clip_lamports));
        self.bankroll_balance()
            .saturating_sub(committed)
            .saturating_sub(pending)
            .saturating_sub(self.model_mgmt_reserved(None))
    }

    /// Mints the daemon MUST keep a live reserve (curve/pool) feed for: every held position, independent of
    /// new-opportunity discovery and of eviction pressure from it.
    #[must_use]
    pub fn model_held_mints(&self) -> Vec<[u8; 32]> {
        self.positions
            .held_records()
            .iter()
            .map(|h| h.mint)
            .collect()
    }

    /// Measure, per held position, whether the data a management decision needs is actually fresh. This is
    /// what "management readiness" means; a connected socket proves nothing. Uses the existing 60 s
    /// `PRICING_BUDGET_MS`; no new threshold.
    #[must_use]
    pub fn model_held_data_status(&self) -> Vec<HeldDataStatus> {
        let clock = self.model_clock_ms;
        self.positions
            .held_records()
            .iter()
            .map(|h| {
                let amm = self.model_cache.snapshot_venue_is_amm(&h.mint);
                let reserve_ts = if amm {
                    self.model_cache.amm_obs(&h.mint).map(|o| o.ts_ms)
                } else {
                    self.model_cache.curve_obs(&h.mint).map(|o| o.ts_ms)
                };
                let reserve_age_ms = reserve_ts.map(|t| clock - t);
                let last_print_age_ms = self
                    .model_cache
                    .marker(&h.mint)
                    .map(|m| clock - m.last_recv_ms);
                let reserve_fresh =
                    reserve_age_ms.is_some_and(|a| a <= crate::curve_annotation::PRICING_BUDGET_MS);
                let management_ready = match self.model_mgmt_inputs(&h.mint, clock) {
                    Err(r) => Err(r.to_string()),
                    Ok(inputs) => self
                        .model_cache
                        .management_snapshot(&h.mint, clock, &inputs)
                        .map(|_| ())
                        .map_err(|r| r.as_str().to_string()),
                };
                HeldDataStatus {
                    mint: h.mint,
                    amm,
                    reserve_age_ms,
                    last_print_age_ms,
                    reserve_fresh,
                    management_ready,
                    protect_mark_ms: if amm {
                        self.model_protect_mark_ms.get(&h.mint).copied()
                    } else {
                        None
                    },
                    protect_ignored: if amm {
                        self.model_protect_ignored.get(&h.mint).copied()
                    } else {
                        None
                    },
                    sell_reserved_tokens: self.positions.sell_reserved(&h.mint),
                    inventory_tokens: self.positions.inventory_tokens(&h.mint),
                    protect_deferred: self.positions.protect_deferred_for(&h.mint),
                }
            })
            .collect()
    }

    /// The lane's wire clock (ms): the newest receive time seen. Used to age things on the same clock.
    #[must_use]
    pub fn model_clock_ms_now(&self) -> i64 {
        self.model_clock_ms
    }

    /// DEGRADED: at least one held position cannot be managed because its required state is stale or
    /// missing. Independent protection that remains possible: the hard safeguards (rug precursor, hard
    /// stop) run on PRINTS, so they only work while prints arrive - a position whose print feed is also
    /// silent has NO protection at all, which is why this is reported and not hidden.
    #[must_use]
    pub fn model_held_degraded(&self) -> Vec<HeldDataStatus> {
        self.model_held_data_status()
            .into_iter()
            .filter(|s| s.management_ready.is_err())
            .collect()
    }

    /// A reconciliation view of the money side, all in lamports, so a test (or an operator) can check
    /// `balance == seed + realized` and `free == balance - committed - pending entries - ADD reservations`
    /// together with the position's own remaining cost basis.
    #[must_use]
    pub fn model_accounting_view(&self, mint: &[u8; 32]) -> ModelAccountingView {
        ModelAccountingView {
            seed: self.bankroll_origin.seed_lamports(),
            realized: self.bankroll_realized,
            balance: self.bankroll_balance(),
            committed: u64::try_from(self.bankroll_committed).unwrap_or(u64::MAX),
            free: self.model_free_cash_lamports(),
            attribution_entry_spend: self.open_lane.get(mint).map(|a| a.entry_spend),
            attribution_realized: self.open_lane.get(mint).map(|a| a.realized_acc),
            remaining_cost_basis: self.positions.remaining_cost_basis(mint),
            inventory_tokens: self.positions.inventory_tokens(mint),
        }
    }

    /// Unresolved model-managed exposure at this instant. READ-ONLY and never booked: an end-of-run
    /// view, not a settlement. Valuation is a spot ESTIMATE (zero-size bound, not an executable sell
    /// quote) and is `None` with a named reason whenever the mark is unknown, stale (older than the
    /// existing `PRICING_BUDGET_MS` on the lane clock) or not yet validated for the venue.
    #[must_use]
    pub fn model_open_exposure(&self) -> Vec<ModelOpenExposure> {
        let clock = self.model_clock_ms;
        let budget = crate::curve_annotation::PRICING_BUDGET_MS;
        self.positions
            .held_records()
            .iter()
            .filter(|h| h.model_managed)
            .map(|h| {
                let inv = self.positions.inventory_tokens(&h.mint);
                let amm = self.model_cache.snapshot_venue_is_amm(&h.mint);
                let (mark, mark_ts_ms, why) = if amm {
                    (
                        None,
                        self.model_cache.amm_obs(&h.mint).map(|o| o.ts_ms),
                        Some("amm_offline_value_unvalidated"),
                    )
                } else {
                    match self.model_cache.curve_obs(&h.mint) {
                        None => (None, None, Some("no_reserve_observation")),
                        Some(o) if clock.saturating_sub(o.ts_ms) > budget => {
                            (None, Some(o.ts_ms), Some("mark_stale"))
                        }
                        Some(o) => {
                            match crate::curve_fill::spot_price_fp(o.v_sol_lamports, o.v_tokens) {
                                None => (None, Some(o.ts_ms), Some("degenerate_reserves")),
                                Some(px) => (Some(px), Some(o.ts_ms), None),
                            }
                        }
                    }
                };
                let (estimate, why) = match (inv, mark) {
                    (None, _) => (None, why.or(Some("inventory_unknown"))),
                    (Some(_), None) => (None, why),
                    (Some(t), Some(px)) => (
                        u64::try_from(
                            u128::from(t) * u128::from(px) / crate::curve_fill::PRICE_SCALE,
                        )
                        .ok(),
                        None,
                    ),
                };
                ModelOpenExposure {
                    mint: h.mint,
                    inventory_tokens: inv,
                    remaining_cost_basis: self.positions.remaining_cost_basis(&h.mint),
                    sell_reserved_tokens: self.positions.sell_reserved(&h.mint),
                    amm,
                    mark_price_fp: mark,
                    mark_ts_ms,
                    spot_estimate_lamports: estimate,
                    valuation_unavailable: why,
                }
            })
            .collect()
    }

    /// Whether the management ACTION SET is implemented (HOLD/REDUCE/EXIT/ADD). It says nothing about
    /// profitability, AMM sell-economics validation, or held-state restoration.
    #[must_use]
    pub fn model_management_complete(&self) -> bool {
        true
    }
}

/// Per-held-position data readiness at one instant: a measured condition, not a connection flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldDataStatus {
    /// Market.
    pub mint: [u8; 32],
    /// Whether the position is on the AMM plane (else curve).
    pub amm: bool,
    /// Age of the latest executable reserve observation vs the feed clock, ms; `None` = none observed.
    pub reserve_age_ms: Option<i64>,
    /// Age of the newest trade print the state was built from, ms.
    pub last_print_age_ms: Option<i64>,
    /// Reserve is within the existing `PRICING_BUDGET_MS` (60 s) bound.
    pub reserve_fresh: bool,
    /// A management prompt could be cut now (every join gate passes). `Err` carries the named refusal.
    pub management_ready: Result<(), String>,
    /// AMM positions: wire time of the last VERIFIED protection mark (None = none yet / curve position).
    pub protect_mark_ms: Option<i64>,
    /// AMM positions: the newest swap that could NOT mark the position, as (named reason, wire time).
    pub protect_ignored: Option<(&'static str, i64)>,
    /// Tokens reserved by an UNRESOLVED management sell (acknowledgement unknown).
    pub sell_reserved_tokens: u64,
    /// Reconciled inventory (None = unknown, which also leaves nothing executable).
    pub inventory_tokens: Option<u64>,
    /// Protective closes deferred on this mint because everything remaining was reserved.
    pub protect_deferred: u64,
}

/// Why a held position has NO additional executable protection right now: every remaining token is reserved by an
/// unresolved sell (or the inventory is unknown). Measured from the books, never inferred from a counter.
#[must_use]
pub fn sell_reservation_gap(s: &HeldDataStatus) -> Option<String> {
    if s.sell_reserved_tokens == 0 {
        return None;
    }
    match s.inventory_tokens {
        None => Some("inventory unknown while a sell is unresolved".to_string()),
        Some(inv) if s.sell_reserved_tokens >= inv => Some(format!(
            "all {inv} remaining tokens are reserved by an unresolved sell"
        )),
        Some(_) => None,
    }
}

/// One unresolved model-managed position as the end-of-run report states it. Never booked, never
/// assessable; `spot_estimate_lamports` is an OFFLINE spot estimate, not proceeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelOpenExposure {
    /// Market.
    pub mint: [u8; 32],
    /// Reconciled inventory (`None` = never established by a fill; unknown is not zero).
    pub inventory_tokens: Option<u64>,
    /// Cost basis still attached to the remaining inventory.
    pub remaining_cost_basis: Option<u64>,
    /// Tokens reserved by an unresolved sell.
    pub sell_reserved_tokens: u64,
    /// Whether the position's venue is the AMM.
    pub amm: bool,
    /// The mark used, fixed point (`PRICE_SCALE`), when valid.
    pub mark_price_fp: Option<u64>,
    /// When that mark (or the rejected one) was observed, lane clock ms.
    pub mark_ts_ms: Option<i64>,
    /// `inventory * mark / PRICE_SCALE` when both are valid.
    pub spot_estimate_lamports: Option<u64>,
    /// Why no estimate is given (`None` when one is).
    pub valuation_unavailable: Option<&'static str>,
}

/// Money-side snapshot for reconciliation (lamports).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelAccountingView {
    /// Bankroll seed.
    pub seed: u64,
    /// Realized net since start (partial tranches included).
    pub realized: i128,
    /// `seed + realized`, clamped.
    pub balance: u64,
    /// Capital committed to open positions (all-in entry/ADD cost still attached to inventory).
    pub committed: u64,
    /// Free cash as the management prompt states it.
    pub free: u64,
    /// The open position's attributed committed cost (None when flat).
    pub attribution_entry_spend: Option<u64>,
    /// The open position's realized-so-far (None when flat).
    pub attribution_realized: Option<i128>,
    /// Cost basis still attached to the remaining inventory.
    pub remaining_cost_basis: Option<u64>,
    /// Reconciled inventory.
    pub inventory_tokens: Option<u64>,
}

/// Executable state an ADD is planned against.
#[derive(Debug, Clone, Copy)]
enum AddState {
    Curve {
        vsol: u64,
        vtok: u64,
        real_tok: u64,
    },
    Amm {
        base: u64,
        quote: u64,
        vq: u64,
        lp: u32,
        pr: u32,
        cr: u32,
    },
}

/// A feasible ADD: the minimal notional that delivers the target, and the spend ceiling.
#[derive(Debug, Clone, Copy)]
pub(super) struct AddPlan {
    pub n: u64,
    pub tokens: u64,
    pub hi: u64,
    pub fee_bps: u32,
}

impl AddState {
    fn depth(self) -> u64 {
        match self {
            AddState::Curve { vsol, .. } => vsol,
            AddState::Amm { quote, .. } => quote,
        }
    }
    fn fee_bps(self) -> u32 {
        match self {
            // M3: both venues are priced exact-in (fees INSIDE the notional), so no fee is added on top.
            AddState::Curve { .. } | AddState::Amm { .. } => 0,
        }
    }
    /// Tokens delivered for a notional of `n` lamports, by the venue's own arithmetic.
    fn tokens_for(self, n: u64) -> Option<u64> {
        match self {
            AddState::Curve {
                vsol,
                vtok,
                real_tok,
            } => crate::exec_quote::curve_buy(vsol, vtok, real_tok, n)
                .ok()
                .map(|q| q.tokens),
            AddState::Amm {
                base,
                quote,
                vq,
                lp,
                pr,
                cr,
            } => {
                let f = pump_quant_protocol::pumpswap_event::buy_exact_quote_in(
                    u128::from(base),
                    u128::from(quote),
                    u128::from(vq),
                    u128::from(n),
                    u128::from(lp),
                    u128::from(pr),
                    u128::from(cr),
                )?;
                u64::try_from(f.base_out).ok()
            }
        }
    }
}

/// Largest `n` in `[0, hi]` with `ok(n)` true, for a predicate monotone-decreasing in `n`.
fn largest_ok(hi: u64, ok: impl Fn(u64) -> bool) -> u64 {
    let (mut lo, mut hi) = (0u64, hi);
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if ok(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

impl Engine {
    /// Lamports held back for pending ADD orders (remaining notional + fee + fixed leg cost), so a second
    /// order cannot double-spend the same cash. `except` leaves one mint's own reservation out.
    pub(super) fn model_mgmt_reserved(&self, except: Option<&[u8; 32]>) -> u64 {
        self.model_mgmt
            .orders
            .iter()
            .filter(|(m, o)| o.kind == MgmtKind::Add && Some(*m) != except)
            .fold(0u64, |acc, (_, o)| {
                let rem = o.max_spend.saturating_sub(o.spent);
                let fee = (u128::from(rem) * u128::from(o.fee_bps)).div_ceil(10_000);
                acc.saturating_add(rem)
                    .saturating_add(u64::try_from(fee).unwrap_or(u64::MAX))
                    .saturating_add(crate::exec_quote::landed_leg_cost(
                        self.cfg.entry_tip_lamports,
                    ))
            })
    }

    fn model_mgmt_add_state(
        &self,
        mint: &[u8; 32],
        amm: bool,
        landing: Option<(i64, i64, u64)>,
    ) -> Result<AddState, &'static str> {
        if amm {
            let obs = self.model_cache.amm_obs(mint).filter(|o| match landing {
                Some((lo, hi, slot)) => {
                    o.ts_ms >= lo
                        && o.ts_ms <= hi
                        && o.slot > slot
                        && self.model_swap_ctx == Some((o.ts_ms, o.slot))
                }
                None => true,
            });
            let Some(o) = obs else {
                return Err("mgmt:refuse:add_no_executable_state");
            };
            let Some((Some((lp, pr, cr)), Some(vq), t)) = self.model_amm_econ.get(mint).copied()
            else {
                return Err("mgmt:refuse:add_amm_economics_missing");
            };
            if landing.is_some() && t != o.ts_ms {
                return Err("mgmt:refuse:add_amm_economics_missing");
            }
            if cr == 0 {
                // Possibly a cashback coin whose cashback rate the engine does not receive (exec_quote).
                return Err("mgmt:refuse:add_amm_cashback_unknown");
            }
            Ok(AddState::Amm {
                base: o.base_reserves_raw,
                quote: o.quote_reserves_lamports,
                vq,
                lp,
                pr,
                cr,
            })
        } else {
            let obs = self.model_cache.curve_obs(mint).filter(|o| match landing {
                Some((lo, hi, slot)) => o.ts_ms >= lo && o.ts_ms <= hi && o.slot > slot,
                None => true,
            });
            let Some(o) = obs else {
                return Err("mgmt:refuse:add_no_executable_state");
            };
            Ok(AddState::Curve {
                vsol: o.v_sol_lamports,
                vtok: o.v_tokens,
                real_tok: o.real_tokens,
            })
        }
    }

    /// Plan an ADD of `need` tokens against `state`: spend ceiling = min(free cash above the survival floor
    /// net of this order's own reservation, own-impact limit, the order's own bound), minimal notional that
    /// delivers `need`. Every refusal is a NAMED label; nothing is resized to fit.
    fn model_mgmt_add_plan_at(
        &self,
        mint: &[u8; 32],
        need: u64,
        state: AddState,
        order_bound: Option<u64>,
    ) -> Result<AddPlan, &'static str> {
        let fixed = crate::exec_quote::landed_leg_cost(self.cfg.entry_tip_lamports);
        let fee_bps = state.fee_bps();
        let floor = derive_survival_floor(
            self.bankroll_origin.seed_lamports(),
            self.cfg.floor_fraction_bps,
        );
        let balance = self.bankroll_balance();
        let committed = u64::try_from(self.bankroll_committed).unwrap_or(u64::MAX);
        let pending_entries: u64 = self
            .model_orders
            .values()
            .fold(0u64, |a, o| a.saturating_add(o.clip_lamports));
        let free = balance
            .saturating_sub(committed)
            .saturating_sub(pending_entries)
            .saturating_sub(self.model_mgmt_reserved(Some(mint)));
        // The survival floor applies to what is FREE after committed capital, pending entries and every
        // other pending ADD reservation: total at-risk capital never eats into the floor. (Stricter than
        // the entry gate's balance-based headroom, on purpose: an add increases risk on a held position.)
        let avail = free.saturating_sub(floor);
        let hi_cash = if avail <= fixed {
            0
        } else {
            u64::try_from(u128::from(avail - fixed) * 10_000 / (10_000 + u128::from(fee_bps)))
                .unwrap_or(0)
        };
        let depth = state.depth();
        let hi_impact = largest_ok(depth, |n| {
            crate::impact_cap::own_impact_veto(
                Some(depth),
                n,
                crate::impact_cap::CHAMPION_MAX_OWN_IMPACT_BPS,
            )
            .is_ok()
        });
        let bound = order_bound.unwrap_or(u64::MAX);
        let hi = hi_cash.min(hi_impact).min(bound);
        let feasible = state.tokens_for(hi).is_some_and(|t| t >= need);
        if hi == 0 || !feasible {
            return Err(if hi_cash <= hi_impact && hi_cash <= bound {
                "mgmt:refuse:add_insufficient_funds"
            } else if hi_impact <= bound {
                "mgmt:refuse:add_own_impact_limit"
            } else {
                "mgmt:refuse:add_spend_bound"
            });
        }
        // Minimal notional whose delivered tokens reach the target (tokens_for is non-decreasing).
        let (mut lo, mut up) = (1u64, hi);
        while lo < up {
            let mid = lo + (up - lo) / 2;
            if state.tokens_for(mid).is_some_and(|t| t >= need) {
                up = mid;
            } else {
                lo = mid + 1;
            }
        }
        let tokens = state.tokens_for(lo).ok_or("mgmt:refuse:add_unpriceable")?;
        Ok(AddPlan {
            n: lo,
            tokens,
            hi,
            fee_bps,
        })
    }

    /// Plan against the latest observed state (order placement).
    pub(super) fn model_mgmt_add_plan(
        &self,
        mint: &[u8; 32],
        need: u64,
        order_bound: Option<u64>,
        landing: Option<(i64, i64, u64)>,
    ) -> Result<AddPlan, &'static str> {
        let amm = self.model_cache.snapshot_venue_is_amm(mint);
        let state = self.model_mgmt_add_state(mint, amm, landing)?;
        self.model_mgmt_add_plan_at(mint, need, state, order_bound)
    }

    /// Fill a pending ADD against LANDING state: current executable economics, never entry price.
    fn model_mgmt_try_add(&mut self, mint: [u8; 32], order: MgmtOrder, clock: i64) {
        let landing = order.created_ms + MODEL_FILL_LANDING_MS;
        let need = order.intended - order.filled;
        if self.model_safety_blocked() {
            // Defence in depth: the trip already removes unfilled ADDs.
            self.model_mgmt_end(&mint, false);
            self.mrep("mgmt:add_cancelled_safety_off");
            return;
        }
        let state = match self.model_mgmt_add_state(
            &mint,
            order.amm,
            Some((landing, clock, order.created_slot)),
        ) {
            Ok(s) => s,
            Err(r) => {
                if r == "mgmt:refuse:add_amm_economics_missing" {
                    self.model_mgmt_end(&mint, false);
                    self.mrep(r);
                } else if clock - order.created_ms > MODEL_ORDER_TTL_MS {
                    self.model_mgmt_end(&mint, false);
                    self.mrep("mgmt:order_expired_unfilled");
                }
                return;
            }
        };
        let remaining_bound = order.max_spend.saturating_sub(order.spent);
        match self.model_mgmt_add_plan_at(&mint, need, state, Some(remaining_bound)) {
            Ok(plan) => {
                self.mrep(if order.amm {
                    "mgmt:fill_amm_buy"
                } else {
                    "mgmt:fill_curve_buy"
                });
                let px =
                    (u128::from(plan.n) * 1_000_000_000).div_ceil(u128::from(plan.tokens.max(1)));
                let px = u64::try_from(px).unwrap_or(u64::MAX);
                self.model_mgmt_book_add(mint, order, plan.tokens, plan.n, px, plan.fee_bps, None);
            }
            Err(r) => {
                self.model_mgmt_end(&mint, false);
                self.mrep(r);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn model_mgmt_book_add(
        &mut self,
        mint: [u8; 32],
        order: MgmtOrder,
        tokens: u64,
        spent: u64,
        price_fp: u64,
        fee_bps: u32,
        settled_fee: Option<u64>,
    ) {
        // CONVENTION (explicit): `spent` is the quote notional that bought the tokens and EXCLUDES fees; fees are
        // paid ON TOP. The all-in cash cost is `spent + fees`. A reported fee is authoritative and ALL-IN (it
        // already carries any per-transaction cost); the simulator derives `rate * spent + fixed leg cost`.
        let fee = match settled_fee {
            Some(f) => f,
            None => u64::try_from(u128::from(spent) * u128::from(fee_bps) / 10_000)
                .unwrap_or(0)
                .saturating_add(crate::exec_quote::landed_leg_cost(
                    self.cfg.entry_tip_lamports,
                )),
        };
        let cost = spent.saturating_add(fee);
        if let Err(r) = self
            .positions
            .add_filled(&mint, tokens, spent, cost, price_fp)
        {
            self.mrep(match r {
                crate::position::AddRefusal::NotHeld => "mgmt:refuse:not_held",
                crate::position::AddRefusal::InventoryUnknown => "mgmt:refuse:inventory_unknown",
                crate::position::AddRefusal::ZeroQuantity => "mgmt:refuse:zero_quantity",
                crate::position::AddRefusal::NoPrice => "mgmt:refuse:no_price",
                crate::position::AddRefusal::Overflow => "mgmt:refuse:add_overflow",
            });
            self.model_mgmt_end(&mint, false);
            return;
        }
        // Cash: the all-in cost joins the committed capital and the attribution, so a later close releases
        // exactly what was committed. Nothing realized changes on a buy.
        self.bankroll_committed = self.bankroll_committed.saturating_add(u128::from(cost));
        if let Some(att) = self.open_lane.get_mut(&mint) {
            att.entry_spend = att.entry_spend.saturating_add(cost);
        }
        self.model_mgmt.fills.push(MgmtFill {
            order_id: order.id,
            mint,
            tokens,
            price_fp,
            closed: false,
            net_lamports: 0,
            spent_lamports: spent,
            cost_lamports: cost,
            is_add: true,
            gross_lamports: 0,
            fee_lamports: 0,
        });
        if let Some(mp) = self.model_mgmt.pos.get_mut(&mint) {
            mp.version += 1;
        }
        let mut done = false;
        if let Some(o) = self.model_mgmt.orders.get_mut(&mint) {
            o.filled += tokens;
            o.spent += spent;
            o.version += 1;
            done = o.filled >= o.intended;
        }
        if done {
            self.model_mgmt_end(&mint, false);
            self.mrep("mgmt:fill:add_complete");
        } else {
            self.mrep("mgmt:fill:add_partial_remainder_pending");
        }
    }

    /// Apply a RECONCILED ADD fill: `tokens` delivered for `spent` notional lamports. Matched by order id;
    /// quantity and spend are checked against what the order still allows. The fee is the venue's rate at
    /// the latest observed state (unknown => refused, never zero).
    ///
    /// # Errors
    /// A named refusal; nothing changes.
    pub fn model_mgmt_apply_reconciled_add_fill(
        &mut self,
        mint: [u8; 32],
        order_id: u64,
        tokens: u64,
        spent: u64,
    ) -> Result<(), &'static str> {
        let Some(order) = self.model_mgmt.orders.get(&mint).copied() else {
            self.mrep("mgmt:recon:rejected:no_pending_order");
            return Err("no_pending_order");
        };
        if order.id != order_id {
            self.mrep("mgmt:recon:rejected:order_id_mismatch");
            return Err("order_id_mismatch");
        }
        if order.kind != MgmtKind::Add {
            self.mrep("mgmt:recon:rejected:wrong_kind");
            return Err("wrong_kind");
        }
        if tokens == 0 || spent == 0 || tokens > order.intended - order.filled {
            self.mrep("mgmt:recon:rejected:quantity");
            return Err("quantity");
        }
        if spent > order.max_spend.saturating_sub(order.spent) {
            self.mrep("mgmt:recon:rejected:spend_bound");
            return Err("spend_bound");
        }
        let px = u64::try_from((u128::from(spent) * 1_000_000_000).div_ceil(u128::from(tokens)))
            .map_err(|_| "price_overflow")?;
        self.model_mgmt_clear_uncertain(&mint);
        self.model_mgmt_book_add(mint, order, tokens, spent, px, order.fee_bps, None);
        Ok(())
    }

    /// Apply the INCREMENT of an authoritative cumulative ADD settlement: `tokens` acquired for `spent` quote
    /// lamports (fee-exclusive) with `fee` all-in fee lamports paid on top. Bounded by the order's own reservation:
    /// spend by `max_spend`, cumulative fees by `ceil(max_spend * fee_bps) + one fixed leg`.
    fn model_mgmt_apply_settled_add_increment(
        &mut self,
        mint: [u8; 32],
        order_id: u64,
        tokens: u64,
        spent: u64,
        fee: u64,
    ) -> Result<(), &'static str> {
        let Some(order) = self.model_mgmt.orders.get(&mint).copied() else {
            return Err("no_pending_order");
        };
        if order.id != order_id || order.kind != MgmtKind::Add {
            return Err("order_id_mismatch");
        }
        if tokens == 0 || spent == 0 || tokens > order.intended - order.filled {
            self.mrep("mgmt:recon:rejected:quantity");
            return Err("quantity");
        }
        // POST-EXECUTION evidence: the executor says these tokens WERE acquired for this spend and these fees. A
        // reservation is a PRE-submission limit; evidence that it was exceeded is not discarded (that would leave the
        // books claiming cash and inventory that do not match what executed). The actual effect is booked, and the
        // overrun is an explicit named fault that blocks further exposure on the mint until an operator clears it.
        let spend_over = spent > order.max_spend.saturating_sub(order.spent);
        let fee_cap = u64::try_from(
            (u128::from(order.max_spend) * u128::from(order.fee_bps)).div_ceil(10_000),
        )
        .unwrap_or(u64::MAX)
        .saturating_add(crate::exec_quote::landed_leg_cost(
            self.cfg.entry_tip_lamports,
        ));
        let fee_over = order.fees.saturating_add(fee) > fee_cap;
        let px = u64::try_from((u128::from(spent) * 1_000_000_000).div_ceil(u128::from(tokens)))
            .map_err(|_| "price_overflow")?;
        self.model_mgmt_clear_uncertain(&mint);
        self.model_mgmt_book_add(mint, order, tokens, spent, px, order.fee_bps, Some(fee));
        if let Some(o) = self.model_mgmt.orders.get_mut(&mint) {
            o.fees += fee;
        } else if let Some(r) = self.model_sell_log.get_mut(&order_id) {
            r.fees += fee;
        }
        if spend_over || fee_over {
            let filled_now = order.filled + tokens;
            self.model_sell_fault(
                order_id,
                mint,
                "add_exceeds_reservation",
                filled_now,
                vec![filled_now],
            );
            if spend_over {
                self.mrep("mgmt:recon:booked_over_reservation:spend");
            }
            if fee_over {
                self.mrep("mgmt:recon:booked_over_reservation:fees");
            }
        }
        Ok(())
    }

    /// Protective closes deferred (wholly or in part) because an unresolved sell reserves the tokens.
    #[must_use]
    pub fn model_protection_deferred(&self) -> u64 {
        self.positions.protect_deferred
    }

    /// Reservation sync: tokens an UNRESOLVED (uncertain) REDUCE/EXIT may already have executed are held back from
    /// every protective close (`ScalpLifecycle::close_guarded`). Derived from the order book each tick, so it
    /// cannot drift: a resolved, filled, ended or absent order reserves nothing.
    pub(super) fn model_sync_sell_reservations(&mut self) {
        // SUM per mint: an unresolved management sell and an unresolved protective sell on the same mint are two
        // outstanding commitments against the same inventory (a map collect would let one overwrite the other).
        let mut want: std::collections::BTreeMap<[u8; 32], u64> = std::collections::BTreeMap::new();
        for (m, o) in self
            .model_mgmt
            .orders
            .iter()
            .chain(self.model_mgmt.protect.iter())
            .filter(|(_, o)| o.uncertain && o.kind != MgmtKind::Add)
        {
            let e = want.entry(*m).or_insert(0);
            *e = e.saturating_add(o.intended.saturating_sub(o.filled));
        }
        for m in self.positions.held_records().iter().map(|h| h.mint) {
            let w = want.get(&m).copied().unwrap_or(0);
            if self.positions.sell_reserved(&m) != w {
                self.positions.set_sell_reserved(&m, w);
            }
        }
    }

    /// Mark a pending management order's acknowledgement UNCERTAIN: it stays pending and unresolved.
    pub fn model_mgmt_mark_ack_uncertain(&mut self, mint: &[u8; 32], order_id: u64) -> bool {
        match self.model_mgmt.orders.get_mut(mint) {
            Some(o) if o.id == order_id => {
                o.uncertain = true;
                self.mrep("mgmt:ack_uncertain");
                true
            }
            _ => false,
        }
    }

    /// Operator/chain evidence that an uncertain order did NOT execute: it is removed, nothing booked.
    pub fn model_mgmt_resolve_uncertain_not_executed(
        &mut self,
        mint: &[u8; 32],
        order_id: u64,
    ) -> bool {
        if self
            .model_mgmt
            .orders
            .get(mint)
            .is_some_and(|o| o.id == order_id && o.uncertain)
        {
            self.model_mgmt_end(mint, false);
            self.mrep("mgmt:uncertain_resolved_not_executed");
            true
        } else {
            false
        }
    }

    /// THE single place a live management order leaves `MgmtLane::orders`. Records its terminal state so a late or
    /// duplicate report is recognised. `preempted` = the position closed under it. An order whose acknowledgement
    /// was UNKNOWN and that is ended by anything other than evidence raises a fault: it may have executed.
    pub(super) fn model_mgmt_end(&mut self, mint: &[u8; 32], preempted: bool) {
        let Some(o) = self.model_mgmt.orders.remove(mint) else {
            return;
        };
        let state = if o.filled >= o.intended {
            SellState::Completed
        } else if preempted {
            SellState::Preempted
        } else if o.filled > 0 {
            SellState::EndedPartial
        } else {
            SellState::EndedUnfilled
        };
        let last_price_fp = self
            .model_mgmt
            .fills
            .iter()
            .rev()
            .find(|f| f.order_id == o.id)
            .map_or(0, |f| f.price_fp);
        self.model_sell_log.insert(
            o.id,
            SellRec {
                id: o.id,
                mint: *mint,
                kind: o.kind,
                intended: o.intended,
                filled: o.filled,
                spent: o.spent,
                state,
                last_price_fp,
                gross: o.gross,
                fees: o.fees,
                simulated: o.simulated,
            },
        );
        if o.uncertain && preempted && state != SellState::Completed {
            self.model_sell_fault(
                o.id,
                *mint,
                "uncertain_sell_preempted",
                o.filled,
                Vec::new(),
            );
        }
        self.model_compact_sell_log();
    }

    fn model_sell_fault(
        &mut self,
        order_id: u64,
        mint: [u8; 32],
        source: &'static str,
        books_filled: u64,
        reported: Vec<u64>,
    ) {
        let f = self
            .model_sell_faults
            .entry(order_id)
            .or_insert_with(|| SellFault {
                order_id,
                mint,
                source,
                books_filled,
                reported: Vec::new(),
            });
        for r in reported {
            f.reported.push(r);
        }
        self.mrep(format!("mgmt:FAULT:{source}"));
    }

    fn model_compact_sell_log(&mut self) {
        let cap = self.model_settled_order_cap.unwrap_or(SELL_LOG_CAP);
        let settled: Vec<u64> = self
            .model_sell_log
            .keys()
            .copied()
            .filter(|id| !self.model_sell_faults.contains_key(id))
            .collect();
        if settled.len() <= cap {
            return;
        }
        for id in settled.into_iter().take(self.model_sell_log.len() - cap) {
            self.model_sell_log.remove(&id);
            self.model_sell_prefix.remove(&id);
            self.model_sell_floor = self.model_sell_floor.max(id);
            self.mrep("held_state:sell_record_compacted");
        }
    }

    /// Whether `mint` has an unresolved management-order conflict (blocks new exposure, like an entry fault).
    pub(super) fn model_sell_blocked(&self, mint: &[u8; 32]) -> bool {
        self.model_sell_faults.values().any(|f| f.mint == *mint)
    }

    /// Unresolved management-order conflicts.
    #[must_use]
    pub fn model_sell_faults(&self) -> &BTreeMap<u64, SellFault> {
        &self.model_sell_faults
    }

    /// One settled management order's record.
    #[must_use]
    pub fn model_sell_rec(&self, id: u64) -> Option<SellRec> {
        self.model_sell_log.get(&id).copied()
    }

    /// AUTHORITATIVE TERMINAL execution evidence for an issued sell (REDUCE / EXIT / protective): the executor states
    /// the order is FINAL at these cumulative totals. Distinguishes three cases the plain report cannot:
    /// * totals 0 + terminal          -> definitively NO execution: the order ends `EndedUnfilled`, its reservation is
    ///   released and protection re-evaluates on the next drain (immediately after this event);
    /// * totals > 0 (< intended) + terminal -> the increment settles through the normal path FIRST (prior partial
    ///   settlement preserved), then the remainder is definitively cancelled: `EndedPartial`, remainder released;
    /// * no terminal flag            -> [`Self::model_mgmt_ingest_evidence`]: the remainder stays working/unknown.
    ///
    /// Identity checks are the existing ones. A repeat of the same terminal evidence is a duplicate no-op; terminal
    /// evidence that disagrees with a settled record, or ANY later evidence that adds execution after a terminal
    /// statement, is a durable `report_contradicts_settled` fault (mint blocked, nothing applied). Timeouts, TTL and
    /// model verdicts never reach here. HARNESS-ONLY channel (see report_inbox).
    #[allow(clippy::too_many_arguments)]
    pub fn model_mgmt_ingest_terminal(
        &mut self,
        mint: [u8; 32],
        order_id: u64,
        action: MgmtKind,
        intended: u64,
        cum_tokens: u64,
        cum_gross: u64,
        cum_fees: u64,
    ) -> SellReportResult {
        if action == MgmtKind::Add {
            self.mrep("mgmt:terminal:rejected:add_unsupported");
            return SellReportResult::Rejected("terminal_add_unsupported");
        }
        let is_live = |e: &Self| {
            e.model_mgmt
                .orders
                .get(&mint)
                .is_some_and(|o| o.id == order_id)
                || e.model_mgmt
                    .protect
                    .get(&mint)
                    .is_some_and(|o| o.id == order_id)
        };
        if !is_live(self) {
            // Already settled (or never issued). Same totals as the record = duplicate; anything else contradicts.
            let rec = self
                .model_sell_log
                .get(&order_id)
                .filter(|r| r.mint == mint)
                .copied();
            let Some(r) = rec else {
                return self.model_mgmt_ingest_evidence(
                    mint, order_id, action, intended, cum_tokens, cum_gross, cum_fees,
                );
            };
            if r.kind != action || r.intended != intended {
                return self.model_mgmt_ingest_evidence(
                    mint, order_id, action, intended, cum_tokens, cum_gross, cum_fees,
                );
            }
            if (r.filled, r.gross, r.fees) == (cum_tokens, cum_gross, cum_fees) {
                self.mrep("mgmt:terminal:duplicate");
                return SellReportResult::Duplicate;
            }
            self.model_sell_fault(
                order_id,
                mint,
                "report_contradicts_settled",
                r.filled,
                vec![cum_tokens],
            );
            return SellReportResult::Fault;
        }
        // Live: settle the stated totals through the one evidence path first (no-op if already reflected).
        let r = if cum_tokens == 0 && cum_gross == 0 && cum_fees == 0 {
            // Nothing executed: the books must agree that nothing has.
            let filled = self
                .model_mgmt
                .orders
                .get(&mint)
                .filter(|o| o.id == order_id)
                .or_else(|| {
                    self.model_mgmt
                        .protect
                        .get(&mint)
                        .filter(|o| o.id == order_id)
                })
                .map_or(0, |o| o.filled);
            if filled > 0 {
                self.model_sell_fault(
                    order_id,
                    mint,
                    "report_contradicts_settled",
                    filled,
                    vec![0],
                );
                return SellReportResult::Fault;
            }
            // Identity (kind / intended) still checked by the evidence path: an all-zero report is a duplicate there.
            self.model_mgmt_ingest_evidence(mint, order_id, action, intended, 0, 0, 0)
        } else {
            self.model_mgmt_ingest_evidence(
                mint, order_id, action, intended, cum_tokens, cum_gross, cum_fees,
            )
        };
        match r {
            SellReportResult::Applied { .. } | SellReportResult::Duplicate => {}
            other => return other,
        }
        if !is_live(self) {
            return r; // the totals completed it: the normal path already ended it Completed.
        }
        // Books now equal the terminal totals: the remainder is definitively not executing. End it (not preempted:
        // no fault), releasing the reservation; the protective drain after this event re-evaluates the position.
        let ended =
            match self.model_with_protect_id(&mint, order_id, |e| e.model_mgmt_end(&mint, false)) {
                Some(()) => true,
                None => {
                    self.model_mgmt_end(&mint, false);
                    true
                }
            };
        if ended {
            self.mrep(if cum_tokens == 0 {
                "mgmt:terminal:no_execution"
            } else {
                "mgmt:terminal:remainder_cancelled"
            });
        }
        SellReportResult::Applied { delta: 0 }
    }

    /// Execution-evidence boundary for a management order (the harness report format). The report states the
    /// order's CUMULATIVE settlement: tokens filled, gross proceeds (REDUCE/EXIT; notional spent for an ADD) and
    /// all-in fees, all in lamports/raw tokens and all totals since the order began, never a per-fill price. The
    /// books apply only the INCREMENT over what they already hold, so two partial fills at different prices each book
    /// their own proceeds. Before anything changes it checks the issued identity (id, mint, action, intended
    /// quantity), monotonic totals, and `fee <= gross`. An equal report is a duplicate only if ALL totals match;
    /// an older one is ignored only if it equals a checkpoint the books recorded; contradictions are named faults.
    /// Model text never reaches this function: it takes executor-supplied numbers only.
    #[allow(clippy::too_many_arguments)]
    pub fn model_mgmt_ingest_evidence(
        &mut self,
        mint: [u8; 32],
        order_id: u64,
        action: MgmtKind,
        intended: u64,
        cum_tokens: u64,
        cum_gross: u64,
        cum_fees: u64,
    ) -> SellReportResult {
        // A report naming the working PROTECTIVE order settles through exactly the same checks and books.
        let r = match self.model_with_protect_id(&mint, order_id, |e| {
            e.model_mgmt_ingest_evidence_inner(
                mint, order_id, action, intended, cum_tokens, cum_gross, cum_fees,
            )
        }) {
            Some(r) => r,
            None => self.model_mgmt_ingest_evidence_inner(
                mint, order_id, action, intended, cum_tokens, cum_gross, cum_fees,
            ),
        };
        // EXTERNAL execution evidence proves the order is working at the executor. If a remainder is still open it
        // stays WORKING there (reserved, never paper-filled, never ended as unsubmitted) until definitive evidence.
        if matches!(r, SellReportResult::Applied { .. }) {
            for o in [
                self.model_mgmt.orders.get_mut(&mint),
                self.model_mgmt.protect.get_mut(&mint),
            ]
            .into_iter()
            .flatten()
            {
                if o.id == order_id && o.filled < o.intended {
                    o.uncertain = true;
                    self.model_report
                        .entry("mgmt:external_partial_remainder_working".to_string())
                        .and_modify(|n| *n += 1)
                        .or_insert(1);
                }
            }
        }
        r
    }

    #[allow(clippy::too_many_arguments)]
    fn model_mgmt_ingest_evidence_inner(
        &mut self,
        mint: [u8; 32],
        order_id: u64,
        action: MgmtKind,
        intended: u64,
        cum_tokens: u64,
        cum_gross: u64,
        cum_fees: u64,
    ) -> SellReportResult {
        let live = self
            .model_mgmt
            .orders
            .get(&mint)
            .copied()
            .filter(|o| o.id == order_id);
        // (kind, intended, filled, gross, fees, spent) as the books hold them.
        let held = live
            .map(|o| (o.kind, o.intended, o.filled, o.gross, o.fees, o.spent))
            .or_else(|| {
                self.model_sell_log
                    .get(&order_id)
                    .filter(|r| r.mint == mint)
                    .map(|r| (r.kind, r.intended, r.filled, r.gross, r.fees, r.spent))
            });
        let Some((kind, iss, filled, gross, fees, spent)) = held else {
            // Never issued / compacted / wrong mint: the existing named rejection, nothing applied.
            return self.model_mgmt_ingest_report(mint, order_id, cum_tokens, cum_gross);
        };
        let reject = |s: &mut Self, why: &'static str| {
            s.mrep(format!("mgmt:evidence:rejected:{why}"));
            SellReportResult::Rejected(why)
        };
        if kind != action {
            return reject(self, "action_mismatch");
        }
        if iss != intended {
            return reject(self, "intended_mismatch");
        }
        if cum_fees > cum_gross || (cum_tokens > 0 && cum_gross == 0) {
            return reject(self, "amounts_invalid");
        }
        if cum_tokens > iss {
            self.model_sell_fault(
                order_id,
                mint,
                "report_exceeds_order",
                filled,
                vec![cum_tokens],
            );
            return SellReportResult::Fault;
        }
        let (cur_g, cur_f) = if kind == MgmtKind::Add {
            (spent, fees)
        } else {
            (gross, fees)
        };
        if cum_tokens == filled {
            if (cum_gross, cum_fees) == (cur_g, cur_f) {
                self.mrep("mgmt:report:duplicate");
                return SellReportResult::Duplicate;
            }
            // Same quantity, different settlement: contradictory evidence (also for an order the simulator filled).
            self.model_sell_fault(
                order_id,
                mint,
                "report_contradicts_settled",
                filled,
                vec![cum_tokens],
            );
            return SellReportResult::Fault;
        }
        if cum_tokens < filled {
            let pre = self.model_sell_prefix.get(&order_id);
            if pre.is_some_and(|v| v.contains(&(cum_tokens, cum_gross, cum_fees))) {
                self.mrep("mgmt:report:duplicate");
                return SellReportResult::Duplicate;
            }
            if pre.is_some_and(|v| v.iter().any(|c| c.0 == cum_tokens)) {
                self.model_sell_fault(
                    order_id,
                    mint,
                    "report_contradicts_settled",
                    filled,
                    vec![cum_tokens],
                );
                return SellReportResult::Fault;
            }
            return reject(self, "stale_unproven");
        }
        // cum_tokens > filled: a new increment. Totals must not go backwards.
        if cum_gross < cur_g || cum_fees < cur_f {
            self.model_sell_fault(
                order_id,
                mint,
                "report_non_monotonic",
                filled,
                vec![cum_tokens],
            );
            return SellReportResult::Fault;
        }
        if live.is_none() {
            // The books say this order ended at `filled`; the report says more executed. Not applied, not dropped.
            self.model_sell_fault(
                order_id,
                mint,
                "report_contradicts_settled",
                filled,
                vec![cum_tokens],
            );
            return SellReportResult::Fault;
        }
        let delta = cum_tokens - filled;
        let r = if kind == MgmtKind::Add {
            let (ds, df) = (cum_gross - cur_g, cum_fees - cur_f);
            self.model_mgmt_apply_settled_add_increment(mint, order_id, delta, ds, df)
        } else {
            let (dg, df) = (cum_gross - cur_g, cum_fees - cur_f);
            if dg == 0 || df > dg {
                return reject(self, "amounts_invalid");
            }
            self.model_mgmt_apply_settled_increment(mint, order_id, delta, dg, df)
        };
        match r {
            Ok(()) => {
                let v = self.model_sell_prefix.entry(order_id).or_default();
                v.push((cum_tokens, cum_gross, cum_fees));
                if v.len() > SELL_PREFIX_CAP {
                    v.remove(0);
                }
                SellReportResult::Applied { delta }
            }
            Err(why) => SellReportResult::Rejected(why),
        }
    }

    /// Ingest one execution report for a management order, by CUMULATIVE quantity: `cumulative_tokens` is the
    /// total this order has filled according to the reporter, and `value` is the fill price (REDUCE/EXIT) or the
    /// cumulative notional spent (ADD). Idempotent: a repeat or an older partial changes nothing. Bound to the
    /// order id and mint; never matched by mint alone.
    pub fn model_mgmt_ingest_report(
        &mut self,
        mint: [u8; 32],
        order_id: u64,
        cumulative_tokens: u64,
        value: u64,
    ) -> SellReportResult {
        // A live order on this mint?
        if let Some(o) = self.model_mgmt.orders.get(&mint).copied() {
            if o.id == order_id {
                if cumulative_tokens > o.intended {
                    self.model_sell_fault(
                        order_id,
                        mint,
                        "report_exceeds_order",
                        o.filled,
                        vec![cumulative_tokens],
                    );
                    return SellReportResult::Fault;
                }
                if cumulative_tokens <= o.filled {
                    self.mrep("mgmt:report:duplicate");
                    return SellReportResult::Duplicate;
                }
                let delta = cumulative_tokens - o.filled;
                let r = if o.kind == MgmtKind::Add {
                    let spent_delta = value.saturating_sub(o.spent);
                    self.model_mgmt_apply_reconciled_add_fill(mint, order_id, delta, spent_delta)
                } else {
                    self.model_mgmt_apply_reconciled_fill(mint, order_id, delta, value)
                };
                return match r {
                    Ok(()) => SellReportResult::Applied { delta },
                    Err(why) => SellReportResult::Rejected(why),
                };
            }
        }
        let Some(rec) = self.model_sell_log.get(&order_id).copied() else {
            self.mrep("mgmt:report:rejected");
            return SellReportResult::Rejected(
                if order_id != 0 && order_id <= self.model_sell_floor {
                    "compacted_order"
                } else {
                    "unknown_order"
                },
            );
        };
        if rec.mint != mint {
            self.mrep("mgmt:report:rejected");
            return SellReportResult::Rejected("mint_mismatch");
        }
        if cumulative_tokens <= rec.filled {
            self.mrep("mgmt:report:duplicate");
            return SellReportResult::Duplicate;
        }
        // The books say this order ended at `rec.filled`; the report says more executed. Not applied, not dropped.
        self.model_sell_fault(
            order_id,
            mint,
            "report_contradicts_settled",
            rec.filled,
            vec![cumulative_tokens],
        );
        SellReportResult::Fault
    }

    /// A reconciled report (fill) resolves uncertainty for the order it names.
    pub(super) fn model_mgmt_clear_uncertain(&mut self, mint: &[u8; 32]) {
        if let Some(o) = self.model_mgmt.orders.get_mut(mint) {
            o.uncertain = false;
        }
    }

    /// Unfilled/partially-filled ADD remainders cancelled by a SAFETY_OFF trip (risk-increasing intents).
    /// Uncertain orders are preserved for reconciliation. Returns how many were cancelled.
    pub(super) fn model_mgmt_cancel_adds(&mut self) -> usize {
        let victims: Vec<[u8; 32]> = self
            .model_mgmt
            .orders
            .iter()
            .filter(|(_, o)| o.kind == MgmtKind::Add && !o.uncertain)
            .map(|(m, _)| *m)
            .collect();
        for m in &victims {
            self.model_mgmt_end(m, false);
            self.mrep("safety:add_order_invalidated");
        }
        victims.len()
    }
}

/// Why a held AMM position's price-based protection is unavailable at wire clock `now_ms`, or `None` when it is
/// protected. The age is taken from the last VERIFIED mark - not from the newest hint, a rejected swap, or the
/// connection being up - and the bound is the existing pricing budget (no new threshold).
///
/// What a mark means: an AMM swap event carries the pool's PRE-trade reserves, so each mark is the state before
/// that swap and does not include its own price impact. Protection can therefore detect only what a LATER swap's
/// pre-trade state reveals; a price-moving swap followed by silence is invisible until the budget elapses and this
/// gap is raised.
#[must_use]
pub fn amm_protection_gap(status: &HeldDataStatus, now_ms: i64) -> Option<String> {
    if !status.amm {
        return None;
    }
    let budget = crate::curve_annotation::PRICING_BUDGET_MS;
    let newest_ignored_after_mark = status
        .protect_ignored
        .filter(|(_, t)| status.protect_mark_ms.is_none_or(|m| *t >= m));
    match (status.protect_mark_ms, newest_ignored_after_mark) {
        (_, Some(("no_spot_basis", _))) => {
            Some("protection_mark_unavailable:missing_spot_basis".to_string())
        }
        (None, _) => Some("protection_mark_unavailable:no_verified_mark".to_string()),
        (Some(m), _) if now_ms.saturating_sub(m) > budget => {
            Some("protection_mark_stale:no_valid_update".to_string())
        }
        _ => None,
    }
}

#[cfg(test)]
mod add_planner_tests {
    use super::*;
    use crate::config::Config;

    fn engine(bankroll: u64) -> Engine {
        let mut c = Config::dev_portable();
        c.bankroll_initial_lamports = bankroll;
        Engine::new(c, RunMode::Paper)
    }
    const M: [u8; 32] = [7; 32];
    // A deep, realistic curve: vsol 37.9 SOL, vtok 849e12.
    fn curve(vsol: u64) -> AddState {
        AddState::Curve {
            vsol,
            vtok: 849_000_000_000_000,
            real_tok: 569_000_000_000_000,
        }
    }

    #[test]
    fn minimal_notional_reaches_the_target_and_is_independent_of_any_entry_price() {
        let e = engine(2_000_000_000);
        let st = curve(37_900_000_000);
        let need = 5_000_000_000_000; // ~0.22 SOL on this book, inside the 90 bp impact limit
        let p = e
            .model_mgmt_add_plan_at(&M, need, st, None)
            .expect("feasible");
        assert!(p.tokens >= need);
        // minimal: one lamport less does NOT reach the target
        assert!(st.tokens_for(p.n - 1).is_none_or(|t| t < need));
        // no entry price is an input at all: same state, same result
        let q = e.model_mgmt_add_plan_at(&M, need, st, None).unwrap();
        assert_eq!((p.n, p.tokens), (q.n, q.tokens));
    }

    #[test]
    fn insufficient_funds_is_a_named_refusal_and_nothing_is_resized() {
        // 1 SOL bankroll, 25% floor => 0.75 SOL spendable. A target that needs more than that on a
        // very deep book (impact not binding) must REFUSE, not shrink to what cash allows.
        let e = engine(1_000_000_000);
        let deep = curve(10_000_000_000_000);
        let need = 849_000_000_000_000 / 2;
        let r = e.model_mgmt_add_plan_at(&M, need, deep, None);
        assert_eq!(r.unwrap_err(), "mgmt:refuse:add_insufficient_funds");
    }

    #[test]
    fn own_impact_limit_is_a_named_refusal() {
        // Thin 10 SOL book: the 90 bp impact limit admits ~0.09 SOL; a half-inventory target needs more.
        let e = engine(2_000_000_000);
        let thin = curve(10_000_000_000);
        let r = e.model_mgmt_add_plan_at(&M, 100_000_000_000_000, thin, None);
        assert_eq!(r.unwrap_err(), "mgmt:refuse:add_own_impact_limit");
    }

    #[test]
    fn a_remaining_spend_bound_is_a_named_refusal_not_a_silent_cap() {
        let e = engine(2_000_000_000);
        let st = curve(37_900_000_000);
        let r = e.model_mgmt_add_plan_at(&M, 5_000_000_000_000, st, Some(1_000));
        assert_eq!(r.unwrap_err(), "mgmt:refuse:add_spend_bound");
    }

    #[test]
    fn a_pending_add_reserves_cash_so_a_second_order_cannot_double_spend() {
        let mut e = engine(1_000_000_000);
        let deep = curve(10_000_000_000_000);
        let need = 40_000_000_000; // ~0.48 SOL all-in (125 bp inside) on the 10,000 SOL book: one fits in 0.5 SOL free, two do not
        let first = e
            .model_mgmt_add_plan_at(&M, need, deep, None)
            .expect("fits alone");
        e.model_mgmt.orders.insert(
            M,
            MgmtOrder {
                id: 1,
                kind: MgmtKind::Add,
                intended: need,
                filled: 0,
                created_ms: 0,
                created_slot: 0,
                version: 1,
                amm: false,
                max_spend: first.hi,
                spent: 0,
                fee_bps: first.fee_bps,
                uncertain: false,
                gross: 0,
                fees: 0,
                protect: 0,
                simulated: false,
            },
        );
        assert!(
            e.model_mgmt_reserved(None) >= first.hi,
            "the reservation is held back"
        );
        let other = [8u8; 32];
        let r = e.model_mgmt_add_plan_at(&other, need, deep, None);
        assert_eq!(
            r.unwrap_err(),
            "mgmt:refuse:add_insufficient_funds",
            "second order sees the reservation"
        );
    }
}
