//! Held-state persistence and restore for the model lane.
//!
//! PERSIST: the held ledger is rewritten (atomic, fsync) whenever the exposure picture changes (position
//! opened/closed, inventory or cost basis moved, order created/filled/cancelled) and at a bounded cadence.
//! If a write FAILS while exposure exists, the lane trips SAFETY_OFF (entries blocked) - never silently
//! continues with state it cannot recover.
//!
//! RESTORE (fresh engine only, before the first tick): rebuilds positions, committed capital, realized
//! total, attribution, management history and pending orders, all-or-nothing. Pending orders come back as
//! UNCERTAIN (the acknowledgement died with the process) and stay unresolved until a reconciled report or
//! operator evidence settles them. Nothing is guessed: any inconsistency refuses the WHOLE restore by name
//! and the file is left untouched. A restored position keeps its true fill time, step and extremes.

use std::path::{Path, PathBuf};

use super::model_manage::{MgmtKind, MgmtOrder, MgmtPos};
use super::{Engine, OpenAttribution};
use crate::expected_move::SignalObs;
use crate::held_state::{
    HeldEntry, HeldLedger, HeldPending, LedgerReadError, RestoreRefusal, RestoreReport,
};
use pump_quant_watchlist::candidate::{DiscoveryLane, Lane as WlLane};

/// Minimum interval between unchanged-state rewrites (wire-clock ms); a change writes immediately.
pub(super) const HELD_REFRESH_MS: i64 = 30_000;

/// Persistence bookkeeping (one field on the engine).
#[derive(Debug, Default)]
pub(super) struct HeldPersist {
    pub path: Option<PathBuf>,
    pub last_digest: String,
    pub last_write_ms: i64,
    pub failures: u64,
    pub writes: u64,
}

fn wl_lane(i: u8) -> Option<WlLane> {
    WlLane::ALL.get(usize::from(i)).copied()
}

fn disc_lane(i: u8) -> Option<DiscoveryLane> {
    DiscoveryLane::ALL.get(usize::from(i)).copied()
}

impl Engine {
    /// Where the held ledger lives (None = persistence not attached).
    pub fn model_held_attach(&mut self, path: &Path) {
        self.model_held.path = Some(path.to_path_buf());
    }

    /// Snapshot the whole exposure picture as a durable ledger.
    #[must_use]
    pub fn model_held_ledger(&self) -> HeldLedger {
        let mut held = Vec::new();
        for x in self.positions.export_held() {
            let att = self.open_lane.get(&x.mint);
            let mp = self.model_mgmt.pos.get(&x.mint);
            let amm = self.model_cache.snapshot_venue_is_amm(&x.mint);
            held.push(HeldEntry {
                mint: x.mint,
                entry_price_fp: x.entry_price_fp,
                size_lamports: x.size_lamports,
                cost_lamports: x.cost_lamports,
                remaining_bps: x.remaining_bps,
                inventory_tokens: x.inventory_tokens,
                model_managed: x.model_managed,
                entry_spend: att.map_or(0, |a| a.entry_spend),
                realized_acc: att.map_or(0, |a| a.realized_acc),
                lane_index: att.map_or(0, |a| lane_index(a.lane)),
                discovery_lane_index: att.map_or(0, |a| disc_index(a.discovery_lane)),
                amm,
                fill_ms: mp.map_or(0, |m| m.fill_ms),
                peak_fp: mp.map_or(x.entry_price_fp, |m| m.peak_fp),
                trough_fp: mp.map_or(x.entry_price_fp, |m| m.trough_fp),
                step: mp.map_or(0, |m| m.step),
                version: mp.map_or(1, |m| m.version),
                position_order: mp.map_or(0, |m| m.position_order),
            });
        }
        held.sort_by_key(|h| h.mint);
        let mut pending: Vec<HeldPending> = Vec::new();
        for (m, o) in &self.model_orders {
            pending.push(HeldPending {
                kind: "entry".into(),
                id: o.id,
                mint: *m,
                intended: o.clip_lamports,
                filled: 0,
                max_spend: 0,
                spent: 0,
                fee_bps: 0,
                amm: o.amm,
                created_ms: o.created_ms,
                created_slot: o.created_slot,
                version: 0,
                attempt: o.attempt,
                snap_t_dec_ms: o.snap_t_dec_ms,
                price_limit_bits: o.price_limit.map(f64::to_bits),
                snap_price_bits: o.snap_price.to_bits(),
                lane_index: lane_index(o.lane),
                discovery_lane_index: disc_index(o.discovery_lane),
            });
        }
        for (m, o) in &self.model_mgmt.orders {
            pending.push(HeldPending {
                kind: match o.kind {
                    MgmtKind::Reduce => "reduce",
                    MgmtKind::Exit => "exit",
                    MgmtKind::Add => "add",
                }
                .into(),
                id: o.id,
                mint: *m,
                intended: o.intended,
                filled: o.filled,
                max_spend: o.max_spend,
                spent: o.spent,
                fee_bps: o.fee_bps,
                amm: o.amm,
                created_ms: o.created_ms,
                created_slot: o.created_slot,
                version: o.version,
                attempt: 0,
                snap_t_dec_ms: 0,
                price_limit_bits: None,
                snap_price_bits: 0,
                lane_index: 0,
                discovery_lane_index: 0,
            });
        }
        pending.sort_by(|a, b| (a.mint, a.id, &a.kind).cmp(&(b.mint, b.id, &b.kind)));
        HeldLedger {
            seed_lamports: self.bankroll_origin.seed_lamports(),
            realized_lamports: self.bankroll_realized,
            model_order_seq: self.model_order_seq,
            mgmt_seq: self.model_mgmt.seq,
            written_wall_ms: 0,
            held,
            pending,
        }
    }

    fn model_held_digest(l: &HeldLedger) -> String {
        let mut c = l.clone();
        c.written_wall_ms = 0;
        let s = c.to_json().to_string();
        pump_quant_protocol::sha256::to_hex(&pump_quant_protocol::sha256::sha256(s.as_bytes()))
    }

    /// Write the ledger if exposure changed (or the refresh interval elapsed). Called every tick while the
    /// model lane is armed; a no-op without an attached path. A FAILED write with exposure present trips
    /// SAFETY_OFF: state that cannot be recovered must not keep growing.
    pub(super) fn model_held_persist_if_changed(&mut self) {
        let Some(path) = self.model_held.path.clone() else {
            return;
        };
        let ledger = self.model_held_ledger();
        let digest = Self::model_held_digest(&ledger);
        let due = digest != self.model_held.last_digest
            || self.model_clock_ms - self.model_held.last_write_ms >= HELD_REFRESH_MS;
        if !due {
            return;
        }
        let exposure = !ledger.held.is_empty() || !ledger.pending.is_empty();
        // Nothing to protect and nothing changed since an empty write: skip.
        if !exposure && digest == self.model_held.last_digest {
            self.model_held.last_write_ms = self.model_clock_ms;
            return;
        }
        match ledger.write(&path) {
            Ok(()) => {
                self.model_held.last_digest = digest;
                self.model_held.last_write_ms = self.model_clock_ms;
                self.model_held.writes += 1;
            }
            Err(_) => {
                self.model_held.failures += 1;
                self.mrep("held_state:persist_failed");
                if exposure && !self.model_safety.blocked {
                    self.model_safety_trip("held_state_unpersistable");
                }
            }
        }
    }

    /// Persist failures so far.
    #[must_use]
    pub fn model_held_persist_failures(&self) -> u64 {
        self.model_held.failures
    }

    /// Force a write now (shutdown, tests). Returns whether it is durable.
    pub fn model_held_persist_now(&mut self) -> bool {
        let Some(path) = self.model_held.path.clone() else {
            return false;
        };
        let ledger = self.model_held_ledger();
        let digest = Self::model_held_digest(&ledger);
        if ledger.write(&path).is_ok() {
            self.model_held.last_digest = digest;
            self.model_held.last_write_ms = self.model_clock_ms;
            self.model_held.writes += 1;
            true
        } else {
            self.model_held.failures += 1;
            false
        }
    }

    /// Restore from the attached path. `Ok(None)` when there is no ledger (a clean start).
    ///
    /// # Errors
    /// [`RestoreOutcomeError`]: the file is untrusted, or the restore was refused (nothing applied).
    pub fn model_held_restore(&mut self) -> Result<Option<RestoreReport>, RestoreOutcomeError> {
        let Some(path) = self.model_held.path.clone() else {
            return Ok(None);
        };
        let ledger = match HeldLedger::read(&path) {
            Ok(l) => l,
            Err(LedgerReadError::Absent) => return Ok(None),
            Err(LedgerReadError::Untrusted(w)) => {
                self.mrep("held_state:restore_untrusted_file");
                return Err(RestoreOutcomeError::Untrusted(w));
            }
        };
        if ledger.held.is_empty() && ledger.pending.is_empty() {
            // Nothing held. Books (realized, sequences) still restore so ids never repeat.
            if let Err(r) = self.model_held_validate(&ledger) {
                self.mrep("held_state:restore_refused");
                return Err(RestoreOutcomeError::Refused(r));
            }
            self.model_held_apply(&ledger);
            return Ok(Some(RestoreReport {
                realized_lamports: ledger.realized_lamports,
                ..RestoreReport::default()
            }));
        }
        match self.model_held_validate(&ledger) {
            Ok(()) => {
                let rep = self.model_held_apply(&ledger);
                self.mrep("held_state:restored");
                Ok(Some(rep))
            }
            Err(r) => {
                self.mrep("held_state:restore_refused");
                Err(RestoreOutcomeError::Refused(r))
            }
        }
    }

    fn model_held_validate(&self, l: &HeldLedger) -> Result<(), RestoreRefusal> {
        if !self.positions.export_held().is_empty()
            || !self.model_orders.is_empty()
            || !self.model_mgmt.orders.is_empty()
            || self.bankroll_realized != 0
            || self.bankroll_committed != 0
        {
            return Err(RestoreRefusal::EngineNotFresh);
        }
        let seed = self.bankroll_origin.seed_lamports();
        if l.seed_lamports != seed {
            return Err(RestoreRefusal::SeedMismatch {
                file: l.seed_lamports,
                engine: seed,
            });
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut committed: u128 = 0;
        for h in &l.held {
            if !seen.insert(h.mint) {
                return Err(RestoreRefusal::DuplicateHeld);
            }
            if h.entry_price_fp == 0 {
                return Err(RestoreRefusal::ZeroEntryPrice);
            }
            if wl_lane(h.lane_index).is_none() || disc_lane(h.discovery_lane_index).is_none() {
                return Err(RestoreRefusal::UnknownLane);
            }
            if !h.model_managed {
                return Err(RestoreRefusal::NotModelManaged);
            }
            committed += u128::from(h.entry_spend);
        }
        if l.held.len() > self.cfg.max_concurrent_positions {
            return Err(RestoreRefusal::OverCapacity);
        }
        let balance = i128::from(seed) + l.realized_lamports;
        if i128::try_from(committed).unwrap_or(i128::MAX) > balance.max(0) {
            return Err(RestoreRefusal::CommittedExceedsBalance);
        }
        for p in &l.pending {
            match p.kind.as_str() {
                "entry" => {
                    if wl_lane(p.lane_index).is_none()
                        || disc_lane(p.discovery_lane_index).is_none()
                    {
                        return Err(RestoreRefusal::UnknownLane);
                    }
                }
                "reduce" | "exit" | "add" => {
                    if !seen.contains(&p.mint) {
                        return Err(RestoreRefusal::OrphanPendingOrder);
                    }
                }
                _ => return Err(RestoreRefusal::UnknownOrderKind),
            }
        }
        Ok(())
    }

    fn model_held_apply(&mut self, l: &HeldLedger) -> RestoreReport {
        let mut rep = RestoreReport::default();
        self.bankroll_realized = l.realized_lamports;
        self.model_order_seq = self.model_order_seq.max(l.model_order_seq);
        self.model_mgmt.seq = self.model_mgmt.seq.max(l.mgmt_seq);
        rep.realized_lamports = l.realized_lamports;
        for h in &l.held {
            let x = crate::position::HeldExport {
                mint: h.mint,
                entry_price_fp: h.entry_price_fp,
                size_lamports: h.size_lamports,
                cost_lamports: h.cost_lamports,
                remaining_bps: h.remaining_bps,
                inventory_tokens: h.inventory_tokens,
                model_managed: h.model_managed,
            };
            // Capacity was validated; a refusal here would be a logic error, surfaced loudly.
            let ok = self.positions.restore_held(&x, self.now);
            debug_assert!(ok, "validated restore must open");
            self.bankroll_committed = self
                .bankroll_committed
                .saturating_add(u128::from(h.entry_spend));
            rep.committed_lamports = rep.committed_lamports.saturating_add(h.entry_spend);
            if h.inventory_tokens.is_none() {
                rep.inventory_unknown += 1;
            }
            self.open_lane.insert(
                h.mint,
                OpenAttribution {
                    lane: wl_lane(h.lane_index).unwrap_or(WlLane::ActiveMarketScalp),
                    discovery_lane: disc_lane(h.discovery_lane_index)
                        .unwrap_or(DiscoveryLane::ActiveMarket),
                    archetype: 0,
                    realized_acc: h.realized_acc,
                    entry_spend: h.entry_spend,
                    scale_add: 0,
                    scale_cost: 0,
                    entry_price: h.entry_price_fp,
                    brain: None,
                    entry_tick: self.now,
                    entry_vsol: 0,
                    entry_obs: SignalObs::none(),
                    latency: Default::default(),
                },
            );
            self.model_position_order.insert(h.mint, h.position_order);
            self.model_mgmt.pos.insert(
                h.mint,
                MgmtPos {
                    fill_ms: h.fill_ms,
                    entry_px_fp: h.entry_price_fp,
                    peak_fp: h.peak_fp,
                    trough_fp: h.trough_fp,
                    step: h.step,
                    last_ask_ms: None,
                    last_try_ms: i64::MIN / 2,
                    version: h.version,
                    position_order: h.position_order,
                },
            );
            rep.positions += 1;
        }
        for p in &l.pending {
            match p.kind.as_str() {
                "entry" => {
                    self.model_orders.insert(
                        p.mint,
                        super::model_admit::ModelOrder {
                            id: p.id,
                            attempt: p.attempt,
                            clip_lamports: p.intended,
                            price_limit: p.price_limit_bits.map(f64::from_bits),
                            created_ms: p.created_ms,
                            created_slot: p.created_slot,
                            snap_t_dec_ms: p.snap_t_dec_ms,
                            uncertain: true,
                            confirmed: None,
                            lane: wl_lane(p.lane_index).unwrap_or(WlLane::ActiveMarketScalp),
                            discovery_lane: disc_lane(p.discovery_lane_index)
                                .unwrap_or(DiscoveryLane::ActiveMarket),
                            snap_price: f64::from_bits(p.snap_price_bits),
                            amm: p.amm,
                        },
                    );
                }
                kind => {
                    self.model_mgmt.orders.insert(
                        p.mint,
                        MgmtOrder {
                            id: p.id,
                            kind: match kind {
                                "reduce" => MgmtKind::Reduce,
                                "exit" => MgmtKind::Exit,
                                _ => MgmtKind::Add,
                            },
                            intended: p.intended,
                            filled: p.filled,
                            created_ms: p.created_ms,
                            created_slot: p.created_slot,
                            version: p.version,
                            amm: p.amm,
                            max_spend: p.max_spend,
                            spent: p.spent,
                            fee_bps: p.fee_bps,
                            uncertain: true,
                        },
                    );
                }
            }
            rep.pending_uncertain += 1;
        }
        // Restored entry orders need a log record so a later report resolves them by id.
        for p in l.pending.iter().filter(|p| p.kind == "entry") {
            self.model_order_log.insert(
                p.id,
                super::model_admit::OrderRec {
                    id: p.id,
                    mint: p.mint,
                    attempt: p.attempt,
                    clip_lamports: p.intended,
                    filled_clip_lamports: 0,
                    state: super::model_admit::OrderState::PendingUncertain,
                    terminal: None,
                },
            );
        }
        for h in &l.held {
            self.model_order_log
                .entry(h.position_order)
                .or_insert(super::model_admit::OrderRec {
                    id: h.position_order,
                    mint: h.mint,
                    attempt: 1,
                    clip_lamports: h.size_lamports,
                    filled_clip_lamports: h.size_lamports,
                    state: super::model_admit::OrderState::Filled,
                    terminal: None,
                });
        }
        self.model_held.last_digest = Self::model_held_digest(l);
        self.mrep_add("held_state:restored_positions", rep.positions as u64);
        self.mrep_add(
            "held_state:restored_pending_uncertain",
            rep.pending_uncertain as u64,
        );
        rep
    }
}

/// Why a restore did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreOutcomeError {
    /// The ledger file could not be trusted (named field).
    Untrusted(&'static str),
    /// Valid file, inconsistent with this engine: nothing applied.
    Refused(RestoreRefusal),
}

fn lane_index(l: WlLane) -> u8 {
    l.index() as u8
}

fn disc_index(l: DiscoveryLane) -> u8 {
    l.index() as u8
}
