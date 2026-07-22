//! `social_ledger` — SocialSourceQualityLedger reconciliation reducer
//! (constitution §82, §29.8).
//!
//! Responsibility: reconcile every attributable social call to chain truth,
//! per source, producing a deterministic quality scorecard. The mandatory
//! control is the **D3 state-at-call selection**: only inputs knowable at the
//! call's timestamp may inform any quality claim. A call whose feature evidence
//! was observed *after* the call is look-ahead — it is rejected from the quality
//! computation entirely, never merely down-weighted, because a scorecard built
//! on future information is fraud. The cadence / experiment-registration side is
//! supervisor governance; this is the pure, fixture-testable reducer.
//!
//! Integer-only (constitution §22): realized outcomes are `i128` lamports, the
//! quality ratio is basis points, timestamps are `u64` ns; no floats.

use std::collections::BTreeMap;

/// Opaque social source identifier (a Telegram channel, X account, …). Ordering
/// drives deterministic output order only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceId(pub u64);

/// One attributable social call reconciled to chain truth (constitution §82).
///
/// Carries the D3 timestamps needed to prove state-at-call time-safety: the
/// moment the call was made (`call_ts_ns`) and the moment the feature evidence
/// used to grade it was observed (`feature_ts_ns`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SocialCall {
    /// Which source made the call.
    pub source_id: SourceId,
    /// Timestamp of the call (nanoseconds).
    pub call_ts_ns: u64,
    /// Timestamp at which the grading feature evidence was observed
    /// (nanoseconds). Must be `≤ call_ts_ns` to be admissible (D3).
    pub feature_ts_ns: u64,
    /// Reconciled realized net outcome attributable to the call (lamports).
    pub realized_net_lamports: i128,
    /// True iff the reconciled outcome was favorable (a follower who acted on
    /// the call at supportable size netted positive).
    pub realized_favorable: bool,
}

impl SocialCall {
    /// True iff this call is D3-admissible: its grading evidence was knowable at
    /// call time (constitution §82 state-at-call selection control).
    pub fn is_time_safe(&self) -> bool {
        self.feature_ts_ns <= self.call_ts_ns
    }
}

/// Quality basis-points, or `Missing` when no admissible calls exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QualityBps {
    /// Favorable-rate in basis points over admissible calls.
    Bps(u32),
    /// Undefined — no admissible (time-safe) calls for this source.
    Missing,
}

impl QualityBps {
    /// True iff undefined.
    pub fn is_missing(&self) -> bool {
        matches!(self, QualityBps::Missing)
    }
}

/// Per-source reconciled quality scorecard (constitution §82).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceScorecard {
    /// The source this scorecard describes.
    pub source_id: SourceId,
    /// Total calls attributed to the source (admissible + rejected).
    pub n_total: u32,
    /// Calls admitted to the quality claim (D3 time-safe).
    pub n_admissible: u32,
    /// Calls rejected as look-ahead (feature observed after the call).
    pub n_lookahead_rejected: u32,
    /// Favorable admissible calls.
    pub n_favorable: u32,
    /// Net lamports summed over admissible calls only.
    pub net_lamports: i128,
    /// Favorable-rate over admissible calls, bps, or `Missing`.
    pub quality_bps: QualityBps,
}

/// Reconcile a batch of social calls into per-source quality scorecards.
///
/// Responsibility (constitution §82, §29.8): fold every call into its source's
/// scorecard, but admit **only D3 time-safe calls** ([`SocialCall::is_time_safe`])
/// into the net-lamports sum, favorable count, and quality ratio. Look-ahead
/// calls are counted solely in `n_total` and `n_lookahead_rejected` so the
/// contamination is visible but can never inflate a quality claim.
/// `quality_bps = n_favorable · 10_000 / n_admissible` (integer), or
/// [`QualityBps::Missing`] when a source has no admissible calls. Output is one
/// scorecard per source in deterministic ascending [`SourceId`] order (backed by
/// a `BTreeMap`). Net accumulation is checked `i128` — reconciled lamport books
/// cannot overflow it in normal operation.
pub fn reconcile_social_quality(calls: &[SocialCall]) -> Vec<SourceScorecard> {
    let mut out: BTreeMap<SourceId, SourceScorecard> = BTreeMap::new();

    for c in calls {
        let sc = out.entry(c.source_id).or_insert(SourceScorecard {
            source_id: c.source_id,
            n_total: 0,
            n_admissible: 0,
            n_lookahead_rejected: 0,
            n_favorable: 0,
            net_lamports: 0,
            quality_bps: QualityBps::Missing,
        });
        sc.n_total += 1;

        if !c.is_time_safe() {
            // D3 violation: exclude from every quality-bearing quantity.
            sc.n_lookahead_rejected += 1;
            continue;
        }

        sc.n_admissible += 1;
        if c.realized_favorable {
            sc.n_favorable += 1;
        }
        sc.net_lamports = sc
            .net_lamports
            .checked_add(c.realized_net_lamports)
            .expect("reconcile_social_quality: net i128 overflow");
    }

    // Finalize the quality ratio for each source over admissible calls only.
    for sc in out.values_mut() {
        sc.quality_bps = if sc.n_admissible == 0 {
            QualityBps::Missing
        } else {
            let bps = (sc.n_favorable as u64)
                .checked_mul(10_000)
                .expect("reconcile_social_quality: bps overflow")
                / sc.n_admissible as u64;
            QualityBps::Bps(bps as u32)
        };
    }

    out.into_values().collect()
}
