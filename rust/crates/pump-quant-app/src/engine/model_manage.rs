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
}

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
    pub step: i64,
}

/// All lane state in one field of the engine.
#[derive(Debug)]
pub(super) struct MgmtLane {
    pub table: RequestTable,
    pub meta: BTreeMap<RequestId, MgmtMeta>,
    pub pos: BTreeMap<[u8; 32], MgmtPos>,
    pub orders: BTreeMap<[u8; 32], MgmtOrder>,
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
        self.model_mgmt.orders.remove(mint);
    }

    /// Causal MFE/MAE tracker: every priced print on a held mint since the fill.
    pub(super) fn model_mgmt_note_price(&mut self, mint: &[u8; 32], price_fp: i128) {
        let Some(mp) = self.model_mgmt.pos.get_mut(mint) else {
            return;
        };
        let Ok(p) = u64::try_from(price_fp) else {
            return;
        };
        if p == 0 {
            return;
        }
        mp.peak_fp = mp.peak_fp.max(p);
        mp.trough_fp = mp.trough_fp.min(p);
    }

    fn model_mgmt_inputs(&self, mint: &[u8; 32], clock: i64) -> Result<MgmtPositionInputs, &'static str> {
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
            };
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
                    crate::model_lane::AcceptRefusal::Unknown => "mgmt:discard:unknown_or_duplicate",
                    crate::model_lane::AcceptRefusal::DeadlineExceeded { .. } => "mgmt:discard:late",
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
        let (Some(mp), true) = (self.model_mgmt.pos.get(&mint).copied(), self.positions.has(&mint))
        else {
            self.mrep("mgmt:discard:position_gone");
            return;
        };
        if mp.position_order != meta.position_order || mp.version != meta.version {
            self.mrep("mgmt:discard:state_version_changed");
            return;
        }
        if self.model_mgmt.orders.contains_key(&mint) {
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
            },
        );
        self.mrep(match kind {
            MgmtKind::Reduce => "mgmt:order:reduce",
            MgmtKind::Exit => "mgmt:order:exit",
            MgmtKind::Add => "mgmt:order:add",
        });
    }

    /// Try to fill pending management sells against LANDING state (never the prompt's state).
    pub(super) fn model_mgmt_try_fills(&mut self, clock: i64) {
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
            let priced: Option<(u64, &'static str)> = if order.amm {
                let obs = self.model_cache.amm_obs(&mint).filter(|o| {
                    o.ts_ms >= landing
                        && o.ts_ms <= clock
                        && o.slot > order.created_slot
                        && self.model_swap_ctx == Some((o.ts_ms, o.slot))
                });
                obs.and_then(|o| {
                    // The sell-side FEE rounding is not program-verified (protocol::pumpswap_event):
                    // priced at the GROSS quote; the store applies the configured venue fee. Labelled.
                    let (_, vq, t) = self.model_amm_econ.get(&mint).copied()?;
                    if t != o.ts_ms {
                        return None;
                    }
                    let gross = pump_quant_protocol::pumpswap_event::sell_gross_quote_out(
                        u128::from(o.base_reserves_raw),
                        u128::from(o.quote_reserves_lamports),
                        u128::from(vq?),
                        u128::from(tokens),
                    )?;
                    let px = gross.checked_mul(1_000_000_000)?.checked_div(u128::from(tokens))?;
                    Some((u64::try_from(px).ok()?, "mgmt:fill_amm_sell_fee_unverified"))
                })
            } else {
                let obs = self.model_cache.curve_obs(&mint).filter(|o| {
                    o.ts_ms >= landing && o.ts_ms <= clock && o.slot > order.created_slot
                });
                obs.and_then(|o| {
                    let px = if self.cfg.curve_exact_fill_enable {
                        // The store applies the exact own-impact itself: hand it the spot, not an
                        // already-impacted average, so impact is charged once.
                        crate::curve_fill::spot_price_fp(o.v_sol_lamports, o.v_tokens)?
                    } else {
                        crate::curve_fill::sell_avg_price_fp(o.v_sol_lamports, o.v_tokens, tokens)?
                    };
                    Some((px, "mgmt:fill_curve"))
                })
            };
            match priced {
                Some((px, label)) => {
                    self.mrep(label);
                    self.model_mgmt_book(mint, order, tokens, px);
                }
                None => {
                    if clock - order.created_ms > MODEL_ORDER_TTL_MS {
                        self.model_mgmt.orders.remove(&mint);
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
        self.model_mgmt_book(mint, order, tokens, price_fp);
        Ok(())
    }

    fn model_mgmt_book(&mut self, mint: [u8; 32], order: MgmtOrder, tokens: u64, price_fp: u64) {
        let inv_before = self.positions.inventory_tokens(&mint).unwrap_or(0);
        let exit = match self
            .positions
            .sell_tokens(&mint, tokens, price_fp, PosExit::ModelManaged)
        {
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
                self.model_mgmt.orders.remove(&mint);
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
        });
        if closed {
            self.model_mgmt_forget(&mint);
            self.mrep("mgmt:fill:closed");
            return;
        }
        if let Some(mp) = self.model_mgmt.pos.get_mut(&mint) {
            mp.version += 1;
        }
        let mut done = false;
        if let Some(o) = self.model_mgmt.orders.get_mut(&mint) {
            o.filled += tokens;
            o.version += 1;
            done = o.filled >= o.intended;
        }
        if done {
            self.model_mgmt.orders.remove(&mint);
            self.mrep("mgmt:fill:complete");
        } else {
            // Partial: the remainder stays pending and monitored (TTL applies to the remainder).
            self.mrep("mgmt:fill:partial_remainder_pending");
        }
    }

    /// The pending management order on `mint`: (id, kind, intended, filled).
    #[must_use]
    pub fn model_mgmt_pending(&self, mint: &[u8; 32]) -> Option<(u64, MgmtKind, u64, u64)> {
        self.model_mgmt
            .orders
            .get(mint)
            .map(|o| (o.id, o.kind, o.intended, o.filled))
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
        self.positions.held_records().iter().map(|h| h.mint).collect()
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
                let last_print_age_ms = self.model_cache.marker(&h.mint).map(|m| clock - m.last_recv_ms);
                let reserve_fresh = reserve_age_ms
                    .is_some_and(|a| a <= crate::curve_annotation::PRICING_BUDGET_MS);
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
    Curve { vsol: u64, vtok: u64 },
    Amm { base: u64, quote: u64, vq: u64, lp: u32, pr: u32, cr: u32 },
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
            AddState::Curve { vsol, .. } => crate::cost_model::venue_fee_bps_per_leg(vsol),
            AddState::Amm { .. } => 0, // the pool takes its fee from the input: tokens out are net
        }
    }
    /// Tokens delivered for a notional of `n` lamports, by the venue's own arithmetic.
    fn tokens_for(self, n: u64) -> Option<u64> {
        match self {
            AddState::Curve { vsol, vtok } => crate::curve_fill::buy_tokens_out(vsol, vtok, n),
            AddState::Amm { base, quote, vq, lp, pr, cr } => {
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
                    .saturating_add(crate::cost_model::FIXED_LAMPORTS_PER_LEG)
            })
    }

    fn model_mgmt_add_state(&self, mint: &[u8; 32], amm: bool, landing: Option<(i64, i64, u64)>) -> Result<AddState, &'static str> {
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
            let Some((Some((lp, pr, cr)), Some(vq), t)) = self.model_amm_econ.get(mint).copied() else {
                return Err("mgmt:refuse:add_amm_economics_missing");
            };
            if landing.is_some() && t != o.ts_ms {
                return Err("mgmt:refuse:add_amm_economics_missing");
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
            Ok(AddState::Curve { vsol: o.v_sol_lamports, vtok: o.v_tokens })
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
        let fixed = crate::cost_model::FIXED_LAMPORTS_PER_LEG;
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
            u64::try_from(
                u128::from(avail - fixed) * 10_000 / (10_000 + u128::from(fee_bps)),
            )
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
        Ok(AddPlan { n: lo, tokens, hi, fee_bps })
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
            self.model_mgmt.orders.remove(&mint);
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
                    self.model_mgmt.orders.remove(&mint);
                    self.mrep(r);
                } else if clock - order.created_ms > MODEL_ORDER_TTL_MS {
                    self.model_mgmt.orders.remove(&mint);
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
                let px = (u128::from(plan.n) * 1_000_000_000).div_ceil(u128::from(plan.tokens.max(1)));
                let px = u64::try_from(px).unwrap_or(u64::MAX);
                self.model_mgmt_book_add(mint, order, plan.tokens, plan.n, px, plan.fee_bps);
            }
            Err(r) => {
                self.model_mgmt.orders.remove(&mint);
                self.mrep(r);
            }
        }
    }

    fn model_mgmt_book_add(
        &mut self,
        mint: [u8; 32],
        order: MgmtOrder,
        tokens: u64,
        spent: u64,
        price_fp: u64,
        fee_bps: u32,
    ) {
        let fee = u64::try_from(u128::from(spent) * u128::from(fee_bps) / 10_000).unwrap_or(0);
        let cost = spent
            .saturating_add(fee)
            .saturating_add(crate::cost_model::FIXED_LAMPORTS_PER_LEG);
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
            self.model_mgmt.orders.remove(&mint);
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
            self.model_mgmt.orders.remove(&mint);
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
        self.model_mgmt_book_add(mint, order, tokens, spent, px, order.fee_bps);
        Ok(())
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
    pub fn model_mgmt_resolve_uncertain_not_executed(&mut self, mint: &[u8; 32], order_id: u64) -> bool {
        if self
            .model_mgmt
            .orders
            .get(mint)
            .is_some_and(|o| o.id == order_id && o.uncertain)
        {
            self.model_mgmt.orders.remove(mint);
            self.mrep("mgmt:uncertain_resolved_not_executed");
            true
        } else {
            false
        }
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
            self.model_mgmt.orders.remove(m);
            self.mrep("safety:add_order_invalidated");
        }
        victims.len()
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
        AddState::Curve { vsol, vtok: 849_000_000_000_000 }
    }

    #[test]
    fn minimal_notional_reaches_the_target_and_is_independent_of_any_entry_price() {
        let e = engine(2_000_000_000);
        let st = curve(37_900_000_000);
        let need = 5_000_000_000_000; // ~0.22 SOL on this book, inside the 90 bp impact limit
        let p = e.model_mgmt_add_plan_at(&M, need, st, None).expect("feasible");
        assert!(p.tokens >= need);
        // minimal: one lamport less does NOT reach the target
        assert!(st.tokens_for(p.n - 1).map_or(true, |t| t < need));
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
        let need = 800_000_000_000_000_u64.min(849_000_000_000_000 / 2);
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
        let need = 42_000_000_000; // ~0.5 SOL on the 10,000 SOL book: one fits in 0.75 SOL, two do not
        let first = e.model_mgmt_add_plan_at(&M, need, deep, None).expect("fits alone");
        e.model_mgmt.orders.insert(
            M,
            MgmtOrder {
                id: 1, kind: MgmtKind::Add, intended: need, filled: 0, created_ms: 0, created_slot: 0,
                version: 1, amm: false, max_spend: first.hi, spent: 0, fee_bps: first.fee_bps, uncertain: false,
            },
        );
        assert!(e.model_mgmt_reserved(None) >= first.hi, "the reservation is held back");
        let other = [8u8; 32];
        let r = e.model_mgmt_add_plan_at(&other, need, deep, None);
        assert_eq!(r.unwrap_err(), "mgmt:refuse:add_insufficient_funds", "second order sees the reservation");
    }
}
