//! The social-source **earn loop** — closes the dormant D1–D10 quality feedback.
//!
//! Before this, the engine held a `SourceQualityLedger` but never folded anything
//! into it: every source resolved to the PUBLIC_BURNED baseline forever (a dead
//! loop). This reducer wires the missing half — it attributes each source's calls
//! to the realized net-SOL of the markets they named, runs the §82 reconciliation
//! ([`reconcile_social_quality`], which enforces the D3 state-at-call time-safety
//! control), and produces an **earned** favorable-rate per source that
//! [`crate::social_ingest::ledger_quality`]'s caller can prefer over the baseline.
//!
//! # Discipline (binding)
//! * **Deterministic, integer (§22).** Attribution keys, timestamps (measured
//!   `observed_at_ns`), and outcomes (`i128` lamports) only; no wall-clock, no
//!   float. The same call/outcome stream always yields the same scorecards.
//! * **Earned, never assumed (§29.8, §82).** Quality comes only from reconciled
//!   realized outcomes; a source with no reconciled evidence stays undefined (the
//!   caller falls back to the PUBLIC_BURNED baseline). Look-ahead grading is
//!   rejected outright by the underlying reducer, never down-weighted.
//! * **Bounded (§99).** Tracked mints, callers per mint, and accumulated
//!   reconciled calls are all capped with oldest-eviction.
//! * **Golden-safe.** Fed only by the source-attributed `ingest_social` path; a
//!   run that never ingests attributed social calls records nothing and changes no
//!   decision.

use pump_quant_evaluator::social_ledger::{
    reconcile_social_quality, QualityBps, SocialCall, SourceId,
};
use std::collections::BTreeMap;

/// Named bounds for the earn reducer (§99/§102).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SocialEarnParams {
    /// Max distinct mints whose callers are tracked before the weakest is evicted.
    pub track_cap: usize,
    /// Max distinct callers retained per mint.
    pub callers_per_mint: usize,
    /// Max accumulated reconciled calls before the oldest is dropped.
    pub reconciled_cap: usize,
}

impl SocialEarnParams {
    /// Shipped defaults: 4096 mints (matches the lane track cap), 16 callers per
    /// mint (a market called by more than 16 distinct sources is already crowded),
    /// and 8192 reconciled calls (a bounded rolling window of graded outcomes).
    #[must_use]
    pub const fn standard() -> Self {
        Self {
            track_cap: 4_096,
            callers_per_mint: 16,
            reconciled_cap: 8_192,
        }
    }
}

impl Default for SocialEarnParams {
    fn default() -> Self {
        Self::standard()
    }
}

/// One recorded call: which source named a mint, and when (measured ns).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Caller {
    source_id: u64,
    call_ts_ns: u64,
}

/// The bounded social earn reducer.
#[derive(Clone, Debug)]
pub struct SocialEarn {
    /// mint → distinct callers (bounded), each with the call instant.
    callers: BTreeMap<[u8; 32], Vec<Caller>>,
    /// Accumulated reconciled calls (bounded rolling window).
    reconciled: Vec<SocialCall>,
    /// Earned favorable-rate (bps) per source from the last [`Self::reconcile`].
    earned: BTreeMap<u64, u32>,
    params: SocialEarnParams,
}

impl SocialEarn {
    /// A fresh reducer under the given bounds.
    #[must_use]
    pub fn new(params: SocialEarnParams) -> Self {
        Self {
            callers: BTreeMap::new(),
            reconciled: Vec::new(),
            earned: BTreeMap::new(),
            params,
        }
    }

    /// Record that `source_id` named `mint` at `call_ts_ns` (from `ingest_social`).
    /// Bounded (§99): a new mint beyond `track_cap` evicts the mint with the fewest
    /// callers; a mint keeps at most `callers_per_mint` distinct callers; a repeat
    /// caller keeps its earliest call instant (first sighting).
    pub fn record_call(&mut self, source_id: u64, mint: [u8; 32], call_ts_ns: u64) {
        if !self.callers.contains_key(&mint) && self.callers.len() >= self.params.track_cap {
            if let Some((&weakest, _)) = self.callers.iter().min_by_key(|(_, v)| v.len()) {
                self.callers.remove(&weakest);
            }
        }
        let cap = self.params.callers_per_mint;
        let v = self.callers.entry(mint).or_default();
        if let Some(existing) = v.iter_mut().find(|c| c.source_id == source_id) {
            existing.call_ts_ns = existing.call_ts_ns.min(call_ts_ns);
            return;
        }
        if v.len() < cap {
            v.push(Caller {
                source_id,
                call_ts_ns,
            });
        }
    }

    /// Attribute a market's realized net-SOL back to every source that called it,
    /// emitting a reconciled [`SocialCall`] per caller. Called when a scalp on
    /// `mint` realizes `net_lamports` (favorable iff strictly positive). The grading
    /// feature timestamp equals the call timestamp — the outcome is ground truth, no
    /// look-ahead *feature* is used, so the call is D3-admissible by construction.
    /// Bounded: the reconciled window drops its oldest entries past `reconciled_cap`.
    pub fn record_outcome(&mut self, mint: &[u8; 32], net_lamports: i128) {
        let Some(callers) = self.callers.get(mint) else {
            return;
        };
        let favorable = net_lamports > 0;
        for c in callers {
            self.reconciled.push(SocialCall {
                source_id: SourceId(c.source_id),
                call_ts_ns: c.call_ts_ns,
                feature_ts_ns: c.call_ts_ns,
                realized_net_lamports: net_lamports,
                realized_favorable: favorable,
            });
        }
        let overflow = self
            .reconciled
            .len()
            .saturating_sub(self.params.reconciled_cap);
        if overflow > 0 {
            self.reconciled.drain(0..overflow);
        }
    }

    /// Re-derive earned per-source quality from all accumulated reconciled calls
    /// (§82 reconciliation with the D3 time-safety control). Run at the reflection
    /// cadence — off the hot path. Deterministic.
    pub fn reconcile(&mut self) {
        self.earned.clear();
        for card in reconcile_social_quality(&self.reconciled) {
            if let QualityBps::Bps(bps) = card.quality_bps {
                self.earned.insert(card.source_id.0, bps);
            }
        }
    }

    /// The earned favorable-rate (bps, 0..=10_000) for a source, if it has any
    /// admissible reconciled evidence. `None` ⇒ unproven ⇒ caller uses the
    /// PUBLIC_BURNED baseline (never trust without evidence).
    #[must_use]
    pub fn quality_bps_for(&self, source_id: u64) -> Option<u32> {
        self.earned.get(&source_id).copied()
    }

    /// Number of sources with earned evidence.
    #[must_use]
    pub fn earned_len(&self) -> usize {
        self.earned.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unproven_source_stays_undefined() {
        let e = SocialEarn::new(SocialEarnParams::standard());
        assert_eq!(
            e.quality_bps_for(42),
            None,
            "no evidence -> baseline fallback"
        );
    }

    #[test]
    fn favorable_outcomes_earn_positive_quality() {
        let mut e = SocialEarn::new(SocialEarnParams::standard());
        let mint = [1u8; 32];
        // Source 7 called this mint; it later realized a positive outcome.
        e.record_call(7, mint, 1_000);
        e.record_outcome(&mint, 5_000_000);
        e.reconcile();
        // One favorable admissible call => 100% favorable rate (10_000 bps).
        assert_eq!(e.quality_bps_for(7), Some(10_000));
    }

    #[test]
    fn losing_calls_earn_low_quality() {
        let mut e = SocialEarn::new(SocialEarnParams::standard());
        let mint = [2u8; 32];
        e.record_call(9, mint, 1_000);
        e.record_outcome(&mint, -3_000_000); // a loss
        e.reconcile();
        assert_eq!(e.quality_bps_for(9), Some(0), "an unfavorable call earns 0");
    }

    #[test]
    fn outcome_for_uncalled_mint_records_nothing() {
        let mut e = SocialEarn::new(SocialEarnParams::standard());
        e.record_outcome(&[3u8; 32], 9_999); // no caller recorded for this mint
        e.reconcile();
        assert_eq!(e.earned_len(), 0);
    }

    #[test]
    fn bounded_tracking_evicts_weakest_mint() {
        let params = SocialEarnParams {
            track_cap: 2,
            ..SocialEarnParams::standard()
        };
        let mut e = SocialEarn::new(params);
        e.record_call(1, [1u8; 32], 1);
        e.record_call(2, [1u8; 32], 2); // mint 1 has 2 callers
        e.record_call(3, [2u8; 32], 3); // mint 2 has 1
        e.record_call(4, [3u8; 32], 4); // evicts weakest (mint 2)
        assert!(e.callers.contains_key(&[1u8; 32]));
        assert!(!e.callers.contains_key(&[2u8; 32]));
        assert_eq!(e.callers.len(), 2);
    }

    #[test]
    fn same_stream_is_deterministic() {
        let build = || {
            let mut e = SocialEarn::new(SocialEarnParams::standard());
            for (s, m, ts, net) in [
                (1u64, [1u8; 32], 100u64, 5_000i128),
                (2, [1u8; 32], 110, 5_000),
                (1, [2u8; 32], 120, -2_000),
            ] {
                e.record_call(s, m, ts);
                e.record_outcome(&m, net);
            }
            e.reconcile();
            (e.quality_bps_for(1), e.quality_bps_for(2))
        };
        assert_eq!(build(), build());
    }
}
