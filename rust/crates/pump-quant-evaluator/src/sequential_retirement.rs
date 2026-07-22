//! `sequential_retirement` — sequential edge-decay / auto-retirement detector
//! (constitution §49 sizing-vs-edge, §54 edge-decay trend, §56.11 learning
//! horizon).
//!
//! Responsibility: watch an ordered stream of reconciled per-trade net-SOL
//! outcomes for a single lane and decide, sequentially, whether the lane's edge
//! has decayed below a null well enough to RETIRE it, or whether to CONTINUE.
//! Retirement is lane-scoped — the search never retires (constitution §2 as
//! rescoped in the revision log); a decayed lane is retired, the mandate is not.
//!
//! Method: a one-sided CUSUM sequential change detector, which for a two-point
//! mean hypothesis is the running Wald log-likelihood ratio up to a positive
//! scale constant (the LLR increment for a Gaussian mean shift is linear in the
//! observation, so `Σ(reference − xᵢ)` is the sufficient statistic). The
//! detector accumulates how far each outcome falls *below* the null reference,
//! net of a slack `k` that absorbs benign noise, reflecting at zero so a healthy
//! run cannot bank credit against a later slump. When the accumulated deficit
//! reaches `threshold_h`, the edge has decayed. This is deterministic and
//! integer-only (constitution §22): all money is `i128` lamports, no floats, no
//! logs, no RNG.
//!
//! Learning-horizon gate (constitution §56.11): paper/shadow lanes get a
//! several-days-minimum evidence horizon before a negative verdict can bind.
//! Here that is a hard integer `min_samples` guard — RETIRE is impossible until
//! at least `min_samples` reconciled outcomes have been observed, no matter how
//! bad the early sample looks.

use crate::evaluator_stats::Lane;

/// Configuration for the sequential retirement detector.
///
/// All fields are integer lamport-scale quantities (constitution §22). Cloneable
/// so a supervisor can hold one config per lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetirementConfig {
    /// Lane this detector is scoped to (constitution §48 — lanes never blend).
    pub lane: Lane,
    /// Null per-trade net-SOL reference (lamports). Outcomes below this erode
    /// the lane's standing; a decayed edge produces sustained shortfalls.
    pub reference_lamports: i128,
    /// Slack `k` (lamports, non-negative by contract): per-trade noise the
    /// detector tolerates before counting a shortfall. A larger `k` makes the
    /// detector less twitchy.
    pub slack_lamports: i128,
    /// Decision threshold `h` (lamports, positive by contract): accumulated
    /// below-null deficit at which the lane is declared decayed.
    pub threshold_lamports: i128,
    /// Minimum reconciled sample before RETIRE may bind (constitution §56.11
    /// learning horizon). RETIRE is impossible strictly before this count.
    pub min_samples: u32,
}

/// Sequential verdict for the lane (constitution §49). Two-valued as required:
/// keep trading/testing, or retire the lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetirementVerdict {
    /// Edge has not decayed decisively (or the learning horizon is unmet).
    Continue,
    /// Edge has decayed past the boundary — retire this lane.
    Retire,
}

/// Full decision record: the verdict plus the evidence that produced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetirementDecision {
    /// The sequential verdict.
    pub verdict: RetirementVerdict,
    /// Number of outcomes consumed.
    pub n: u32,
    /// Peak accumulated below-null deficit reached (the CUSUM statistic's max).
    pub peak_deficit: i128,
    /// 1-based index of the outcome at which RETIRE bound, if it did.
    pub decided_at_sample: Option<u32>,
}

/// Run the sequential edge-decay / auto-retirement detector over an ordered
/// stream of reconciled per-trade net-SOL outcomes.
///
/// Responsibility (constitution §49, §54, §56.11): fold the one-sided CUSUM
/// statistic `g` where `g₀ = 0` and
/// `gᵢ = max(0, gᵢ₋₁ + (reference − xᵢ) − slack)`, tracking its peak. The first
/// sample at which `g ≥ threshold` **and** at least `min_samples` outcomes have
/// been seen yields [`RetirementVerdict::Retire`]; otherwise the lane keeps
/// running with [`RetirementVerdict::Continue`]. Deterministic and integer-only;
/// accumulation uses saturating arithmetic by contract so a pathological input
/// cannot panic the frozen evaluator (the statistic is monotone-clamped anyway).
///
/// `outcomes` must be in chronological order — this is a sequential test and
/// order is load-bearing.
pub fn sequential_retirement(outcomes: &[i128], cfg: &RetirementConfig) -> RetirementDecision {
    let mut g: i128 = 0;
    let mut peak: i128 = 0;
    let mut decided_at: Option<u32> = None;

    for (idx, &x) in outcomes.iter().enumerate() {
        // Below-null shortfall for this outcome, net of tolerated noise.
        let shortfall = cfg
            .reference_lamports
            .saturating_sub(x)
            .saturating_sub(cfg.slack_lamports);
        // Reflect at zero: a healthy stretch cannot bank credit.
        g = g.saturating_add(shortfall).max(0);
        if g > peak {
            peak = g;
        }
        let sample_no = (idx as u32).saturating_add(1);
        // Learning-horizon guard: cannot retire before min_samples (§56.11).
        if decided_at.is_none() && sample_no >= cfg.min_samples && g >= cfg.threshold_lamports {
            decided_at = Some(sample_no);
        }
    }

    let verdict = if decided_at.is_some() {
        RetirementVerdict::Retire
    } else {
        RetirementVerdict::Continue
    };

    RetirementDecision {
        verdict,
        n: outcomes.len() as u32,
        peak_deficit: peak,
        decided_at_sample: decided_at,
    }
}
