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
const MODEL_FILL_LANDING_MS: i64 = 400;
/// A pending order with no landing state by then expires unfilled.
const MODEL_ORDER_TTL_MS: i64 = 5_000;
const FIRST_CAND_CAP: usize = 100_000;
/// Bound on stream-registered markets.
const REGISTRY_CAP: usize = 100_000;
/// Maximum asks started per tick: a coalescing bound, not a strategy filter.
const SCHEDULE_PER_TICK: usize = 8;

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

/// One paper-model fill and what is (not) established about it. Assessment, evaluation and
/// promotion consumers MUST read [`Engine::model_assessable_fills`], never positions directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelFillRecord {
    pub mint: [u8; 32],
    pub amm: bool,
    /// Quote arithmetic validated for this instruction/fee case (protocol vectors).
    pub quote_validated: bool,
    /// Landing/ordering realism validated (Track A). Always false until demonstrated.
    pub landing_validated: bool,
    /// Came from `model_reconcile` (execution truth) rather than the paper simulator.
    pub from_reconcile: bool,
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

impl Engine {
    fn mrep(&mut self, key: impl Into<String>) {
        *self.model_report.entry(key.into()).or_insert(0) += 1;
    }

    /// Block or release NEW model entries (an emergency / SAFETY_OFF supervisor hook). Held-position
    /// handling, the feed and every tick are untouched; only new asks stop, and in-flight answers
    /// are discarded rather than acted on.
    pub fn set_model_entries_blocked(&mut self, blocked: bool) {
        self.model_table.set_entries_blocked(blocked);
    }

    /// The model lane's coverage / refusal / lifecycle counters (candidate-ticks, not unique mints).
    #[must_use]
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

    /// The legacy-promoted admit-site branch (kept as a SECOND source; the stream registry below
    /// is the one that does not depend on legacy admission).
    pub(super) fn model_admit_candidate(&mut self, cand: Candidate) {
        self.model_admit_mint(cand.mint.bytes(), cand.lane, cand.discovery_lane);
    }

    /// Register a stream-discovered market. Bounded; idempotent; never consults legacy state.
    pub(super) fn model_register(&mut self, mint: [u8; 32]) {
        self.model_dirty.insert(mint);
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

    /// Coalesced, bounded scheduling of stream-discovered markets: at most `SCHEDULE_PER_TICK`
    /// asks per tick, each only for a market with a NEW observation since its last ask and outside
    /// its re-ask window. A market the model SKIPped stays registered and is re-offered when new
    /// observations arrive; nothing is dropped for having been skipped.
    pub(super) fn model_stream_schedule(&mut self) {
        let clock = self.model_clock_ms;
        let dirty: Vec<[u8; 32]> = self.model_dirty.iter().copied().collect();
        let mut budget = SCHEDULE_PER_TICK;
        for mint in dirty {
            if budget == 0 {
                self.mrep("sched_deferred_budget");
                let (v, _, _) = self.model_cache.describe(&mint, clock);
                self.mrep(format!("sched_deferred|venue={v}"));
                continue;
            }
            if self
                .model_last_ask
                .get(&mint)
                .is_some_and(|t| clock - *t < MODEL_REASK_MS)
            {
                continue; // stays dirty; re-offered after the window
            }
            self.model_dirty.remove(&mint);
            budget -= 1;
            self.model_admit_mint(mint, WlLane::ActiveMarketScalp, DiscoveryLane::ActiveMarket);
        }
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
            let signed = i64::try_from(a.token_amount).unwrap_or(i64::MAX);
            let entity =
                pump_quant_wallet_graph::tracked_wallet_matcher::wallet_entity_id(&a.trader);
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
                });
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
        self.model_register(mint);
        // Event-driven: the FIRST eligible landing state fills the order, not whatever is newest at
        // the next tick.
        let c = self.model_clock_ms;
        self.model_swap_ctx = Some((ts_ms, a.slot));
        self.model_try_fills(c);
        self.model_swap_ctx = None;
    }

    fn model_admit_mint(&mut self, mint: [u8; 32], cand_lane: WlLane, cand_dlane: DiscoveryLane) {
        if self.mode == RunMode::Live || self.outbound_sink.is_some() {
            self.mrep("refuse:live_forbidden");
            return;
        }
        if let Err(fault) = self.model_source() {
            self.mrep(format!("fault:{fault:?}"));
            return;
        }
        if self.model_pool.is_none() {
            self.mrep("fault:missing_pool");
            return;
        }
        let clock = self.model_clock_ms;
        if clock == 0 {
            self.mrep("refuse:no_feed_clock");
            return;
        }
        if self.model_recon_faults.contains_key(&mint) {
            self.mrep("refuse:recon_fault_blocks_exposure");
            return;
        }
        if self.open_lane.contains_key(&mint)
            || self.model_orders.contains_key(&mint)
            || self.model_table.has_live_for(&mint)
        {
            self.mrep("skip:held_or_pending");
            return;
        }
        if self
            .model_last_ask
            .get(&mint)
            .is_some_and(|t| clock - *t < MODEL_REASK_MS)
        {
            self.mrep("skip:reask_window");
            return;
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
                return;
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
                return;
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
            self.model_table.release(id);
            self.mrep(match e {
                DispatchRefusal::QueueFull => "refuse:dispatch_queue_full",
                DispatchRefusal::PoolClosed => "refuse:dispatch_pool_closed",
            });
            return;
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
        self.model_uniq("dispatched", &mint, venue);
        self.mrep(format!("dispatched|venue={venue}"));
        self.mrep("dispatched");
    }

    /// Non-blocking: collect finished verdicts, expire deadlines, then try pending fills. Runs at the
    /// top of every evaluation tick, so it can never delay the feed or held-position handling.
    pub(super) fn model_poll(&mut self) {
        let clock = self.model_clock_ms;
        let mut done = Vec::new();
        if let Some(p) = &self.model_pool {
            while let Some(v) = p.try_recv() {
                done.push(v);
            }
        }
        for v in done {
            self.model_accept(v, clock);
        }
        for _id in self.model_table.expire(clock) {
            self.mrep("request_abandoned_deadline");
        }
        self.model_try_fills(clock);
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
                self.model_orders.insert(
                    entry.mint,
                    ModelOrder {
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
                self.model_orders.remove(&mint);
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
                        self.model_orders.remove(&mint);
                        self.mrep("fill_none:no_landing_state");
                    }
                    continue;
                };
                self.model_orders.remove(&mint);
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
                        self.model_orders.remove(&mint);
                        self.mrep("fill_none:no_landing_state");
                    }
                    continue;
                };
                self.model_orders.remove(&mint);
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
    fn model_open_filled(
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
            self.model_fills.push(ModelFillRecord {
                mint,
                amm: order.amm,
                quote_validated: order.amm,
                landing_validated: false,
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

    /// The execution acknowledgement for a pending order is unknown. The order stays pending: it
    /// is not inventory, and it is NOT cleared by the TTL. Returns false if there is no order.
    pub fn model_mark_ack_uncertain(&mut self, mint: &[u8; 32]) -> bool {
        match self.model_orders.get_mut(mint) {
            Some(o) => {
                o.uncertain = true;
                self.mrep("ack:uncertain_marked");
                true
            }
            None => {
                self.mrep("ack:no_pending_order");
                false
            }
        }
    }

    /// Resolve an uncertain order from execution truth. `NotFilled` clears the intent; `Filled`
    /// applies the reported fill exactly once. With no pending order (already applied / cleared /
    /// never existed) it is ignored and counted: a duplicate report cannot create a second position.
    pub fn model_reconcile(&mut self, mint: &[u8; 32], outcome: ReconcileOutcome) -> bool {
        // A report that CONFLICTS with the first terminal one is never ignored and never applied:
        // the evidence is kept, a named fault is raised and the mint is blocked from new exposure
        // until a human/authority resolves it (`model_resolve_recon_fault`).
        if let Some(first) = self.model_terminal.get(mint).copied() {
            if first == outcome {
                self.mrep("reconcile:duplicate_same_terminal");
            } else {
                self.model_recon_faults
                    .entry(*mint)
                    .or_default()
                    .push(outcome);
                self.mrep("reconcile:FAULT_conflicting_terminal");
            }
            return false;
        }
        let Some(o) = self.model_orders.get_mut(mint) else {
            self.mrep("reconcile:no_pending_order");
            return false;
        };
        match outcome {
            ReconcileOutcome::NotFilled => {
                self.model_orders.remove(mint);
                self.model_terminal.insert(*mint, outcome);
                self.mrep("reconcile:not_filled_cleared");
            }
            ReconcileOutcome::Filled(fr) => {
                o.confirmed = Some(fr);
                self.model_terminal.insert(*mint, outcome);
                let c = self.model_clock_ms;
                self.model_try_fills(c);
            }
        }
        true
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

    /// Every model fill with its validation flags (routing simulation included, labelled).
    #[must_use]
    pub fn model_all_fills(&self) -> &[ModelFillRecord] {
        &self.model_fills
    }

    /// Mints with an unresolved reconciliation fault, with the conflicting evidence preserved.
    #[must_use]
    pub fn model_recon_faults(&self) -> &BTreeMap<[u8; 32], Vec<ReconcileOutcome>> {
        &self.model_recon_faults
    }

    /// Explicit, auditable resolution of a fault (authority decision); counted.
    pub fn model_resolve_recon_fault(&mut self, mint: &[u8; 32]) -> bool {
        let had = self.model_recon_faults.remove(mint).is_some();
        if had {
            self.model_terminal.remove(mint);
            self.mrep("reconcile:fault_resolved_by_authority");
        }
        had
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
