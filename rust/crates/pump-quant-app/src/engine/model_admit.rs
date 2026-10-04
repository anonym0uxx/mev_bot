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
    pub lane: WlLane,
    pub discovery_lane: DiscoveryLane,
    pub snap_price: f64,
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

    /// The admit-site branch. Called INSTEAD of `gate_evaluate` when the lane is armed.
    pub(super) fn model_admit_candidate(&mut self, cand: Candidate) {
        let mint = cand.mint.bytes();
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
                self.mrep(format!("refuse:{}|{dims}", r.as_str()));
                return;
            }
        };
        self.mrep(format!("snapshot_ok|{dims}"));
        if let Some(first) = self.model_first_cand.get(&mint).copied() {
            *self
                .model_report
                .entry("ready_delay_ms_sum".into())
                .or_insert(0) += (clock - first).max(0) as u64;
            *self.model_report.entry("ready_delay_n".into()).or_insert(0) += 1;
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
                lane: cand.lane,
                discovery_lane: cand.discovery_lane,
                dims,
            },
        );
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
                        lane: meta.lane,
                        discovery_lane: meta.discovery_lane,
                        snap_price: meta.snap.price_lamports_per_raw_token,
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
    fn model_try_fills(&mut self, clock: i64) {
        let mints: Vec<[u8; 32]> = self.model_orders.keys().copied().collect();
        for mint in mints {
            let Some(order) = self.model_orders.get(&mint).copied() else {
                continue;
            };
            let landing = order.created_ms + MODEL_FILL_LANDING_MS;
            let obs = self
                .model_cache
                .curve_obs(&mint)
                .filter(|o| o.ts_ms >= landing && o.ts_ms <= clock);
            let Some(obs) = obs else {
                if clock - order.created_ms > MODEL_ORDER_TTL_MS {
                    self.model_orders.remove(&mint);
                    self.mrep("fill_none:no_landing_state");
                }
                continue;
            };
            self.model_orders.remove(&mint);
            let size = order.clip_lamports;
            let Some(tokens_out) =
                crate::curve_fill::buy_tokens_out(obs.v_sol_lamports, obs.v_tokens, size)
            else {
                self.mrep("fill_none:unpriceable");
                continue;
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
            let Some(entry_price) =
                crate::curve_fill::buy_avg_price_fp(obs.v_sol_lamports, obs.v_tokens, size)
            else {
                self.mrep("fill_none:unpriceable");
                continue;
            };
            let Some(rt_bps) = self.unified_rt_bps(&mint, size, obs.v_sol_lamports) else {
                self.mrep("fill_none:undecoded_quote");
                continue;
            };
            let entry_fee = (u128::from(size)
                * u128::from(crate::cost_model::venue_fee_bps_per_leg(obs.v_sol_lamports))
                / 10_000) as u64;
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
                continue;
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
                entry_vsol: obs.v_sol_lamports,
                entry_obs: crate::expected_move::SignalObs::none(),
                x_min: 0,
                x_cost: 0,
                x_max: 0,
                priced_move: self.priced_move(order.lane, None),
                // Depth was decoded from the curve account observation (basis code 2).
                depth_basis: 2,
                brain: None,
                t_dec: None,
                price_limit: order.price_limit,
                full_clip: true,
            };
            self.open_pending(&pe);
            if self.open_lane.contains_key(&mint) {
                self.mrep("fill:position_opened");
                self.journal.record(Decision::Promoted {
                    mint,
                    lane: order.lane as u8,
                    rank: 0,
                });
            } else {
                self.mrep("fill_none:position_cap");
            }
        }
    }
}
