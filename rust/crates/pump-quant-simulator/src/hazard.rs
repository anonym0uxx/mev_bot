//! Partial-pooled, phase-separated hazard estimator.
//!
//! Responsibility: estimate the probability of an adverse terminal / near-terminal
//! event over a short forward interval, *separately per lifecycle phase* but
//! *partially pooled* toward a global rate so thin phases borrow strength from the
//! whole (constitution §48 hazard-model family; §47 terminal-state base rates as
//! re-measured priors). Pure integer / fixed-point arithmetic (§22); the estimator
//! is memory-bounded by a hard cap on the number of phases.
//!
//! Estimator (all in basis points, `u128` intermediates):
//! * global rate `g = total_events * BPS_ONE / total_trials` (or the prior when no
//!   trials have been observed);
//! * per-phase shrinkage toward `g` with pseudo-count `k` (the pooling strength):
//!   `hazard_p = (events_p * BPS_ONE + k * g) / (trials_p + k)`.
//!
//! With `trials_p == 0` the phase estimate collapses to `g` (fully pooled); as
//! `trials_p` grows it converges to the phase's raw rate (phase-separated).

use crate::fixed::BPS_ONE;
use std::collections::BTreeMap;

/// Observation counts for a single phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhaseObservation {
    /// Phase identifier (e.g. a lifecycle / hold-time bucket).
    pub phase_id: u16,
    /// Number of adverse terminal events observed in this phase.
    pub events: u64,
    /// Number of trials (opportunities for the event) observed in this phase.
    pub trials: u64,
}

/// A per-phase hazard estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HazardEstimate {
    /// Phase this estimate is for.
    pub phase_id: u16,
    /// Trials observed in this phase.
    pub trials: u64,
    /// Events observed in this phase.
    pub events: u64,
    /// Partial-pooled hazard estimate for this phase, in bps.
    pub hazard_bps: u32,
}

/// Error returned when an observation would violate a memory bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HazardError {
    /// Adding a new phase would exceed the configured phase capacity.
    PhaseCapacityExceeded,
}

/// Partial-pooled, phase-separated hazard estimator with a bounded phase set.
#[derive(Debug, Clone)]
pub struct PartialPooledHazard {
    prior_bps: u32,
    pooling_strength: u64,
    max_phases: usize,
    /// `phase_id -> (events, trials)`, ordered for deterministic iteration.
    obs: BTreeMap<u16, (u64, u64)>,
}

impl PartialPooledHazard {
    /// Create an estimator.
    ///
    /// * `prior_bps` — the global rate used before any trials exist (§47 published
    ///   base rate, clamped to `BPS_ONE`).
    /// * `pooling_strength` — pseudo-count `k`; larger means stronger shrinkage of
    ///   thin phases toward the global rate.
    /// * `max_phases` — hard cap on distinct phases (memory bound), clamped to `1`.
    #[must_use]
    pub fn new(prior_bps: u32, pooling_strength: u64, max_phases: usize) -> Self {
        PartialPooledHazard {
            prior_bps: prior_bps.min(BPS_ONE),
            pooling_strength,
            max_phases: max_phases.max(1),
            obs: BTreeMap::new(),
        }
    }

    /// Accumulate an observation for `phase_id` (`events` out of `trials`).
    ///
    /// Counts are added to any existing counts for the phase. Introducing a *new*
    /// phase when the phase set is full returns
    /// [`HazardError::PhaseCapacityExceeded`] (explicit, never silent drop).
    pub fn observe(&mut self, phase_id: u16, events: u64, trials: u64) -> Result<(), HazardError> {
        if !self.obs.contains_key(&phase_id) && self.obs.len() >= self.max_phases {
            return Err(HazardError::PhaseCapacityExceeded);
        }
        let entry = self.obs.entry(phase_id).or_insert((0, 0));
        entry.0 = entry.0.saturating_add(events);
        entry.1 = entry.1.saturating_add(trials);
        Ok(())
    }

    /// The pooled global hazard rate across all phases, in bps.
    ///
    /// `total_events * BPS_ONE / total_trials`, or `prior_bps` when no trials exist.
    #[must_use]
    pub fn global_bps(&self) -> u32 {
        let mut total_events: u128 = 0;
        let mut total_trials: u128 = 0;
        for &(e, n) in self.obs.values() {
            total_events += e as u128;
            total_trials += n as u128;
        }
        if total_trials == 0 {
            return self.prior_bps;
        }
        let g = total_events * (BPS_ONE as u128) / total_trials;
        if g > BPS_ONE as u128 {
            BPS_ONE
        } else {
            g as u32
        }
    }

    /// Partial-pooled hazard estimate (bps) for a single phase.
    ///
    /// Uses the phase's accumulated counts (treated as `0/0` if the phase is
    /// unseen, giving the fully-pooled global rate). Shrinks toward
    /// [`Self::global_bps`] with pseudo-count `pooling_strength`.
    #[must_use]
    pub fn estimate(&self, phase_id: u16) -> HazardEstimate {
        let (events, trials) = self.obs.get(&phase_id).copied().unwrap_or((0, 0));
        let g = self.global_bps() as u128;
        let k = self.pooling_strength as u128;
        let numerator = (events as u128) * (BPS_ONE as u128) + k * g;
        let denominator = trials as u128 + k;
        // `checked_div` yields `None` only when denominator == 0 (no data and zero
        // pooling); fall back to the global rate in that case.
        let hazard = numerator.checked_div(denominator).unwrap_or(g);
        let hazard_bps = if hazard > BPS_ONE as u128 {
            BPS_ONE
        } else {
            hazard as u32
        };
        HazardEstimate {
            phase_id,
            trials,
            events,
            hazard_bps,
        }
    }

    /// Estimates for every observed phase, in ascending `phase_id` order
    /// (deterministic iteration, §22).
    #[must_use]
    pub fn estimate_all(&self) -> Vec<HazardEstimate> {
        self.obs.keys().map(|&p| self.estimate(p)).collect()
    }

    /// Number of distinct phases currently held.
    #[must_use]
    pub fn phase_count(&self) -> usize {
        self.obs.len()
    }
}
