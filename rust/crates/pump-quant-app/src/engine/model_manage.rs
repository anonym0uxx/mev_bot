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
//! * ADD is NOT executed. The corpus has two incompatible definitions (see `ManagementAuthority::ScaleIn`
//!   = capital-based, versus the c11 state transitions = +50% of inventory). Until one is pinned the
//!   result is the named `mgmt:add_unsupported` and management is reported incomplete. No amount is
//!   ever substituted.
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
}

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
    /// Net realised lamports of this fill.
    pub net_lamports: i128,
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
        let cash = balance.saturating_sub(committed).saturating_sub(pending);
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
            ManagementAuthority::ScaleIn { .. } => {
                // Unsupported by design, never substituted. Management stays reported incomplete.
                self.mrep("mgmt:add_unsupported");
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
            },
        );
        self.mrep(match kind {
            MgmtKind::Reduce => "mgmt:order:reduce",
            MgmtKind::Exit => "mgmt:order:exit",
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
        if tokens == 0 || tokens > order.intended - order.filled {
            self.mrep("mgmt:recon:rejected:quantity");
            return Err("quantity");
        }
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
    }

    /// Whether management is complete. It is NOT while ADD is unsupported.
    #[must_use]
    pub fn model_management_complete(&self) -> bool {
        false
    }
}
