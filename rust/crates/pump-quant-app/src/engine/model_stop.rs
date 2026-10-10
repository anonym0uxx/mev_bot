//! Engine side of the stop trigger -> action table (`crate::stop_policy`).
//!
//! `model_stop_evaluate` is the ONE place the engine turns measured conditions into the table's actions:
//! * latching rows trip the durable SAFETY_OFF with the row's reason (no auto re-arm);
//! * while-condition rows restrict NEW entries and ADDs while the condition holds and clear when it clears;
//! * the run deadline stops entries for the rest of the run, drains for a bounded time, then reports
//!   `handoff_due` so the daemon issues the existing protective-handoff stop request.
//!
//! No row disables reconciliation, REDUCE/EXIT management or protection, and nothing here closes a book.

use super::*;
use crate::stop_policy::{
    action_for, run_phase, valuation_trigger, value_book, Latch, LiquidationEstimate, OpsInputs,
    RiskValuation, RunPhase, StopAction, StopTrigger, DRAIN_BOUND_MS, ESTIMATOR_EXEC_QUOTE,
    ESTIMATOR_SHADOW, RUN_DEADLINE_MS,
};

/// Stop-table state carried by the engine.
#[derive(Debug, Clone)]
pub struct StopState {
    /// Non-latching triggers currently restricting new risk (while-condition rows + the deadline phase).
    pub active: BTreeSet<StopTrigger>,
    /// Every trigger raised at the last evaluation.
    pub raised: BTreeSet<StopTrigger>,
    /// Run deadline, ms after start.
    pub deadline_ms: i64,
    /// Drain bound after the deadline, ms.
    pub drain_ms: i64,
    /// Once the deadline passes it stays passed for this run.
    pub deadline_passed: bool,
    held_failures_seen: u64,
    safety_failures_seen: u64,
    /// The valuation METRIC ID the loss stop reads, pinned at run start (or by the first evaluation) and immutable
    /// for the run. Today [`ESTIMATOR_EXEC_QUOTE`] (PROVISIONAL, labelled); the shadow slice's estimate is a
    /// different metric id and therefore a different run.
    valuation_metric: Option<&'static str>,
    /// Evaluations that offered a different metric than the pinned one (refused, counted).
    metric_switch_refused: u64,
}

impl Default for StopState {
    fn default() -> Self {
        Self {
            active: BTreeSet::new(),
            raised: BTreeSet::new(),
            deadline_ms: RUN_DEADLINE_MS,
            drain_ms: DRAIN_BOUND_MS,
            deadline_passed: false,
            held_failures_seen: 0,
            safety_failures_seen: 0,
            valuation_metric: None,
            metric_switch_refused: 0,
        }
    }
}

/// Result of one evaluation.
#[derive(Debug, Clone)]
pub struct StopEvaluation {
    /// Every trigger raised now, with its table row.
    pub raised: Vec<(StopTrigger, StopAction)>,
    /// Triggers raised now that were not raised at the previous evaluation (alert on these).
    pub newly_raised: Vec<StopTrigger>,
    /// Book valuation under the MODEL liquidation estimate incl. costs (the one the loss stop reads).
    pub model_valuation: RiskValuation,
    /// Book valuation under external market state (zero-size spot, no costs). Reported, never used to decide.
    pub external_valuation: RiskValuation,
    /// Label of the model estimator in use.
    pub estimator: &'static str,
    /// Run phase.
    pub phase: RunPhase,
    /// The drain bound passed: the daemon must request the protective handoff (never a force close).
    pub handoff_due: bool,
    /// New BUY entries and ADDs are refused now.
    pub new_risk_blocked: bool,
}

impl StopState {
    /// The pinned valuation metric id (None before run start / the first evaluation).
    #[must_use]
    pub fn valuation_metric(&self) -> Option<&'static str> {
        self.valuation_metric
    }

    /// How many evaluations offered a metric other than the pinned one (each refused).
    #[must_use]
    pub fn metric_switch_refused(&self) -> u64 {
        self.metric_switch_refused
    }
}

impl Engine {
    /// Pin the loss-stop valuation metric for this run (call once at run start). Returns the metric in force: a
    /// second call naming a different metric does NOT switch it (refused, counted, reported by name).
    pub fn model_stop_pin_valuation_metric(&mut self, metric: &'static str) -> &'static str {
        match self.model_stop.valuation_metric {
            None => {
                self.model_stop.valuation_metric = Some(metric);
                self.mrep(format!("stop:valuation_metric_pinned:{metric}"));
                metric
            }
            Some(m) => {
                if m != metric {
                    self.model_stop.metric_switch_refused += 1;
                    self.mrep("stop:valuation_metric_switch_refused");
                }
                m
            }
        }
    }

    /// Override the run deadline and drain bound (daemon configuration / tests).
    pub fn model_stop_set_deadline(&mut self, deadline_ms: i64, drain_ms: i64) {
        self.model_stop.deadline_ms = deadline_ms;
        self.model_stop.drain_ms = drain_ms;
    }

    /// Stop-table state.
    #[must_use]
    pub fn model_stop_state(&self) -> &StopState {
        &self.model_stop
    }

    /// Whether NEW risk (BUY entries, ADD) is refused right now: SAFETY_OFF or any active restriction.
    #[must_use]
    pub fn model_new_risk_blocked(&self) -> bool {
        self.model_safety.blocked || !self.model_stop.active.is_empty()
    }

    /// Whether the ENTRY request table refuses new asks / discards entry answers right now.
    #[must_use]
    pub fn model_entries_blocked(&self) -> bool {
        self.model_table.entries_blocked()
    }

    /// Whether the MANAGEMENT request table is blocked. No stop-table row ever sets this: management asks and
    /// verdicts continue under every trigger (REDUCE/EXIT; ADD is refused at placement).
    #[must_use]
    pub fn model_mgmt_asks_blocked(&self) -> bool {
        self.model_mgmt.table.entries_blocked()
    }

    /// The first active non-latching restriction's action name (for refusal labels).
    #[must_use]
    pub(super) fn model_stop_block_label(&self) -> &'static str {
        if self.model_safety.blocked {
            return "safety_off";
        }
        self.model_stop
            .active
            .iter()
            .next()
            .map_or("none", |t| action_for(*t).name)
    }

    /// Re-derive the entry table's block from SAFETY_OFF + active restrictions.
    pub(super) fn model_stop_sync_entry_block(&mut self) {
        let b = self.model_new_risk_blocked();
        self.model_table.set_entries_blocked(b);
    }

    /// Invalidate queued risk-increasing intents (unfilled, certain entry orders and ADDs). Uncertain ones stay
    /// pending for reconciliation. Shared by SAFETY_OFF and the non-latching restrictions.
    pub(super) fn model_invalidate_new_risk(&mut self) -> usize {
        let victims: Vec<[u8; 32]> = self
            .model_orders
            .iter()
            .filter(|(_, o)| !o.uncertain && o.confirmed.is_none())
            .map(|(m, _)| *m)
            .collect();
        for m in &victims {
            self.model_retire_order(m);
            self.mrep("stop:entry_order_invalidated");
        }
        victims.len() + self.model_mgmt_cancel_adds()
    }

    /// Net liquidation value of the WHOLE held inventory on `mint` under the size-specific executable quote at
    /// the latest reserve state, minus one landed exit leg (network fee p50 + exit tip). Label:
    /// [`ESTIMATOR_EXEC_QUOTE`] - the placeholder for the shadow model's estimate. A missing, stale or
    /// unpriceable input is a NAMED unavailable value, never zero and never cost.
    #[must_use]
    pub fn model_liquidation_estimate(&self, mint: &[u8; 32]) -> Result<i128, &'static str> {
        let inv = self
            .positions
            .inventory_tokens(mint)
            .ok_or("inventory_unknown")?;
        let clock = self.model_clock_ms;
        let budget = crate::curve_annotation::PRICING_BUDGET_MS;
        let q = if self.model_cache.snapshot_venue_is_amm(mint) {
            if self.model_cache.pool_conflicting(mint) {
                return Err("pool_binding_conflict");
            }
            let o = self
                .model_cache
                .amm_obs(mint)
                .ok_or("no_reserve_observation")?;
            if clock.saturating_sub(o.ts_ms) > budget {
                return Err("mark_stale");
            }
            match self.model_amm_econ.get(mint).copied() {
                Some((parts, vq, t, cashback)) if t == o.ts_ms => crate::exec_quote::amm_sell(
                    o.base_reserves_raw,
                    o.quote_reserves_lamports,
                    vq,
                    parts,
                    cashback,
                    inv,
                ),
                _ => Err(crate::exec_quote::QuoteRefusal::AmmEconomicsMissing),
            }
        } else {
            if let Some(x) = self.model_curve_mode_exclusion(mint) {
                return Err(x.quote_refusal());
            }
            let o = self
                .model_cache
                .curve_obs(mint)
                .ok_or("no_reserve_observation")?;
            if clock.saturating_sub(o.ts_ms) > budget {
                return Err("mark_stale");
            }
            crate::exec_quote::curve_sell(o.v_sol_lamports, o.v_tokens, o.real_sol_lamports, inv)
        }
        .map_err(crate::exec_quote::QuoteRefusal::label)?;
        let leg = crate::exec_quote::landed_leg_cost(self.cfg.exit_tip_lamports);
        Ok(i128::from(q.net) - i128::from(leg))
    }

    /// Cash on hand for valuation: seed + realized - committed entry spend (pending intent is not spent).
    fn model_stop_cash(&self) -> i128 {
        i128::from(self.bankroll_origin.seed_lamports()) + self.books_realized()
            - i128::try_from(self.books_committed()).unwrap_or(i128::MAX)
    }

    /// Both valuations of the book with the default (exec-quote) estimator.
    #[must_use]
    pub fn model_stop_valuations(&self) -> (RiskValuation, RiskValuation) {
        self.model_stop_valuations_with(None)
    }

    /// Both valuations of the book: (model estimate incl. costs, external spot state). Separate by design.
    /// `shadow`: the shadow model's net liquidation estimates (the slice that produces them is separate). When
    /// given, a held position it does not cover is UNKNOWN (`shadow_estimate_missing`), never filled in.
    #[must_use]
    pub fn model_stop_valuations_with(
        &self,
        shadow: Option<&[LiquidationEstimate]>,
    ) -> (RiskValuation, RiskValuation) {
        let seed = self.bankroll_origin.seed_lamports();
        let cash = self.model_stop_cash();
        let held: Vec<[u8; 32]> = self
            .positions
            .held_records()
            .iter()
            .filter(|h| h.model_managed)
            .map(|h| h.mint)
            .collect();
        let model: Vec<LiquidationEstimate> = held
            .iter()
            .map(|m| LiquidationEstimate {
                mint: *m,
                value: match shadow {
                    None => self.model_liquidation_estimate(m),
                    Some(xs) => xs
                        .iter()
                        .find(|x| x.mint == *m)
                        .map_or(Err("shadow_estimate_missing"), |x| x.value),
                },
            })
            .collect();
        let external: Vec<LiquidationEstimate> = self
            .model_open_exposure()
            .into_iter()
            .map(|x| LiquidationEstimate {
                mint: x.mint,
                value: match x.spot_estimate_lamports {
                    Some(v) => Ok(i128::from(v)),
                    None => Err(x.valuation_unavailable.unwrap_or("unavailable")),
                },
            })
            .collect();
        (
            value_book(seed, cash, &model),
            value_book(seed, cash, &external),
        )
    }

    /// Apply the stop table once. Under `paper_fill_v1_observed`: the exec-quote estimator
    /// ([`ESTIMATOR_EXEC_QUOTE`]) and the caller's `shadow_divergence`. Under `paper_fill_v2_shadow`: the SHADOW net
    /// liquidation estimates ([`ESTIMATOR_SHADOW`]) and the shadow book's divergence on any held market is OR-ed
    /// into `shadow_divergence` (it can raise the row, never clear one the caller raised).
    pub fn model_stop_evaluate(
        &mut self,
        elapsed_run_ms: i64,
        mut ops: OpsInputs,
    ) -> StopEvaluation {
        if !self.model_v2() {
            return self.model_stop_evaluate_with(elapsed_run_ms, ops, None);
        }
        if let Some((m, d)) = self.model_shadow_held_divergence() {
            ops.shadow_divergence = true;
            let hx: String = m[..4].iter().map(|b| format!("{b:02x}")).collect();
            self.mrep(format!("stop:shadow_divergence_input:{hx}:{}", d.label()));
        }
        let est = self.model_shadow_liquidation_estimates();
        self.model_stop_evaluate_with(elapsed_run_ms, ops, Some(&est))
    }

    /// Apply the stop table once. `elapsed_run_ms` is the run's age; `ops` the measured operational conditions;
    /// `shadow` the shadow model's liquidation estimates (None = the labelled exec-quote placeholder).
    pub fn model_stop_evaluate_with(
        &mut self,
        elapsed_run_ms: i64,
        ops: OpsInputs,
        shadow: Option<&[LiquidationEstimate]>,
    ) -> StopEvaluation {
        let mut raised: BTreeSet<StopTrigger> = BTreeSet::new();
        // Latching rows.
        if !self.model_recon_faults.is_empty() || !self.model_sell_faults.is_empty() {
            raised.insert(StopTrigger::ReconciliationFault);
        }
        let (hf, sf) = (self.model_held.failures, self.model_safety.persist_failures);
        if hf > self.model_stop.held_failures_seen || sf > self.model_stop.safety_failures_seen {
            raised.insert(StopTrigger::DurableWriteFailure);
        }
        self.model_stop.held_failures_seen = hf;
        self.model_stop.safety_failures_seen = sf;
        if ops.shadow_divergence {
            raised.insert(StopTrigger::ShadowDivergence);
        }
        // The metric is immutable for the run: the first evaluation pins it if run start did not; an evaluation
        // offering the other metric is refused and valued under the PINNED one (never the better of the two).
        let offered = if shadow.is_some() {
            ESTIMATOR_SHADOW
        } else {
            ESTIMATOR_EXEC_QUOTE
        };
        let metric = self.model_stop_pin_valuation_metric(offered);
        let (model_val, external_val) = if metric == offered {
            self.model_stop_valuations_with(shadow)
        } else if metric == ESTIMATOR_EXEC_QUOTE {
            self.model_stop_valuations_with(None)
        } else {
            // Pinned to the shadow metric but no shadow estimates were supplied: risk is UNKNOWN, not exec-quote.
            self.model_stop_valuations_with(Some(&[]))
        };
        if let Some(t) = valuation_trigger(&model_val) {
            raised.insert(t);
        }
        if self.model_safety.blocked
            && self.model_safety.reason == crate::safety_off::REASON_ENDPOINT_HUNG
        {
            raised.insert(StopTrigger::EndpointHung);
        }
        // While-condition rows. An unmeasurable reading restricts like a low one.
        let mut active: BTreeSet<StopTrigger> = BTreeSet::new();
        if ops.disk_ok != Some(true) {
            active.insert(StopTrigger::DiskHeadroomLow);
        }
        // Hard floor: only a MEASURED reading below it latches (risk-off); evidence keeps being written.
        if ops.disk_hard_ok == Some(false) {
            raised.insert(StopTrigger::DiskHardFloor);
        }
        if ops.ram_ok != Some(true) {
            active.insert(StopTrigger::RamHeadroomLow);
        }
        if !ops.feed_ok {
            active.insert(StopTrigger::FeedGap);
        }
        if !ops.rpc_budget_ok {
            active.insert(StopTrigger::RpcBudgetLow);
        }
        // Run deadline (sticky for the run).
        let phase = run_phase(
            elapsed_run_ms,
            self.model_stop.deadline_ms,
            self.model_stop.drain_ms,
        );
        if phase != RunPhase::Running || ops.deadline_passed {
            self.model_stop.deadline_passed = true;
        }
        let phase = if self.model_stop.deadline_passed && phase == RunPhase::Running {
            RunPhase::Drain {
                ends_at_ms: self
                    .model_stop
                    .deadline_ms
                    .saturating_add(self.model_stop.drain_ms),
            }
        } else {
            phase
        };
        if self.model_stop.deadline_passed {
            active.insert(StopTrigger::RunDeadline);
        }
        raised.extend(active.iter().copied());
        // Apply.
        for t in &raised {
            if let Latch::SafetyOff(reason) = action_for(*t).latch {
                if !self.model_safety.blocked {
                    self.model_safety_trip(reason);
                }
            }
        }
        let newly_active = active.iter().any(|t| !self.model_stop.active.contains(t));
        self.model_stop.active = active;
        if newly_active {
            self.model_invalidate_new_risk();
        }
        self.model_stop_sync_entry_block();
        let newly_raised: Vec<StopTrigger> = raised
            .iter()
            .filter(|t| !self.model_stop.raised.contains(t))
            .copied()
            .collect();
        for t in &newly_raised {
            let a = action_for(*t);
            self.mrep(format!("stop:{}:{}", a.alert, a.name));
        }
        self.model_stop.raised = raised.clone();
        StopEvaluation {
            raised: raised.iter().map(|t| (*t, action_for(*t))).collect(),
            newly_raised,
            model_valuation: model_val,
            external_valuation: external_val,
            estimator: metric,
            phase,
            handoff_due: phase == RunPhase::HandoffDue,
            new_risk_blocked: self.model_new_risk_blocked(),
        }
    }
}
