//! **PRICED MOVE — the expected favourable move, its estimator, and the ONE place a
//! candidate's benefit term is computed.**
//!
//! # The defect this module exists to close
//!
//! `Engine::gate_evaluate` makes two consecutive money decisions about one candidate:
//! it prices the size band (**admission**, `gate::decide` → `economic_gate::
//! size_band`), and it ranks the candidate for one of the scarce position slots
//! (**arbitration**, §23, `expected_net = size · (move − round_trip_cost)`).
//!
//! Both need "the expected favourable move", and both used to fetch it independently
//! as a bare integer. The concrete defect: when the stratified estimator
//! ([`crate::expected_move::MoveTable`]) produced a per-candidate estimate, admission
//! used it and **arbitration threw it away** and substituted the per-lane figure — six
//! numbers for the whole universe. Nothing recorded which estimator had priced a given
//! trade, so a replay could not answer *"what did we think this was worth, and who
//! told us?"* — the question every post-mortem starts with.
//!
//! # THE ASYMMETRY IS DELIBERATE, AND IT IS NOT THE DEFECT
//!
//! It is tempting — and the original plan for this module proposed it — to collapse
//! the two into a single number and let the lane's realized expectancy price admission
//! as well as ranking. **That is a constitutional over-reach and it was measured to be
//! expensive.** Two independent reasons:
//!
//! 1. **§24/§38 scope.** The lane expectancy is built from PAPER-realized fills.
//!    `engine.rs` states the rule directly: *"Paper-realized returns rank slots; they
//!    are never promotion evidence (§38 — the fill model is graded separately)."*
//!    §24 grants that quantity exactly one job, conditioning §23 arbitration. Making
//!    it an admission veto hands a paper quantity authority over capital that the
//!    constitution does not give it.
//! 2. **It was measured, and it costs.** Wiring lane expectancy into admission was
//!    built and run. On LAW B3's own hazard tape the admitted count collapses 48 → 13,
//!    the armed arm's net falls from **+276_922_370 to −27_981_846**, and — worse —
//!    the law's measured effect becomes exactly **0**, because the expectancy gate
//!    shuts trading down before the brain law can act. A shipped, default-ON law would
//!    have been silently deprived of its own evidence.
//!
//! The two call sites are asking **different questions**, and a type that pretends
//! otherwise is not removing a silo, it is erasing a distinction:
//!
//! | question | §  | authority |
//! |---|---|---|
//! | does this trade beat its OWN costs? | §18 | the population estimate: the calibrated model, else the cold-start prior |
//! | which admissible candidate gets the scarce slot? | §23/§24 | the lane's realized expectancy, shrunk toward that same prior |
//!
//! This is exactly the shape of [`crate::curve_depth::CurveDepth`], whose
//! `price_reserve()` and `payout_reserve()` are two accessors because the market
//! genuinely has two reserves. **One type, computed once, from one construction, with
//! both views and both provenances travelling together and journalled — that is the
//! removal of the silo. Merging the numbers would have been a strategy change wearing
//! a refactor's clothes.**
//!
//! When the model IS armed and above its sample floor, the two views are the SAME
//! number with the SAME source, because the per-candidate estimate is strictly better
//! evidence than a per-lane one for both questions. That is the case the original
//! defect actually broke, and it is closed.
//!
//! # The pattern, copied rather than invented
//!
//! `BankrollOrigin` solved this defect class first: the sizing base is either
//! `PaperSeed(cfg.bankroll_initial_lamports)` or a live reconciled balance, and the
//! distinction rides in the TYPE. **When one quantity can come from more than one
//! place, the value and its provenance travel together, and consumers receive the type
//! — never a bare integer.**
//!
//! [`PricedMove`] has a private body and exactly **one** public constructor, which
//! takes *evidence* (a `MoveEstimate` the estimator alone can produce, plus the lane's
//! realized `(Σ bps, n)`) and never a ready-made answer. There is no `new(bps)`, no
//! `From<u32>`, and no public field: without that, the type is a bare integer with
//! extra steps.
//!
//! # Signed on the ranking side, floored on the admission side
//!
//! The ranking view is `i128` and may be negative: a lane whose realized fills have
//! lost money must rank below one that is merely thin. The admission view is `u32`,
//! floored at zero, because a negative expected move is a refusal for the gate to
//! express as a refusal rather than as a size.
//!
//! Integer only, `Copy`, no allocation: free on the hot path (§22, §24, §99).

use crate::expected_move::MoveEstimate;
use pump_quant_watchlist::candidate::Lane as WlLane;

/// Which estimator produced a [`PricedMove`]. Journalled on every admit, so a replay
/// can attribute a size to the thing that priced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveSource {
    /// The stratified per-candidate model ([`crate::expected_move::MoveTable`]), armed
    /// and above its sample floor. Carries the curve-progress stratum, the episodes
    /// behind it, and how many conditioning signals were calibrated enough to
    /// contribute — the thinness of the evidence, inseparable from the number.
    Model {
        /// Curve-progress stratum index.
        band: usize,
        /// Episodes behind the base stratum.
        n: u32,
        /// Conditioning signals that cleared their own sample floor.
        signals_applied: u32,
    },
    /// The §24 EXPECTANCY_V1 lane estimate: the lane's realized mean shrunk toward the
    /// configured prior, once the lane has cleared `expectancy_min_lane_trades`.
    LanePrior {
        /// The lane whose realized fills produced it.
        lane: WlLane,
        /// Fills behind the estimate.
        n: u32,
    },
    /// The configured cold-start constant `gate_expected_move_bps`. No evidence at
    /// all — recorded as such rather than dressed as a lane estimate that happens to
    /// equal the prior.
    ColdStart,
}

impl MoveSource {
    /// A stable small code for the journal. Never reordered — it is part of the replay
    /// identity (§19/§22).
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::ColdStart => 0,
            Self::LanePrior { .. } => 1,
            Self::Model { .. } => 2,
        }
    }
}

/// An expected favourable move, inseparable from the estimator that produced it.
///
/// Two views, because the admission and arbitration questions are different (see the
/// module note): [`Self::admission_bps`] / [`Self::admission_source`] answer §18, and
/// [`Self::ranking_bps`] / [`Self::ranking_source`] answer §23/§24. They coincide
/// exactly when the calibrated model has spoken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub struct PricedMove {
    /// §18 admission benefit term, bps. Floored at zero by construction.
    admission_bps: u32,
    admission_source: MoveSource,
    /// §23/§24 ranking term, signed bps.
    ranking_bps: i128,
    ranking_source: MoveSource,
}

impl PricedMove {
    /// **THE ONE CONSTRUCTOR.** It takes evidence, never an answer.
    ///
    /// * `model` — the stratified per-candidate estimate, `Some` only when the
    ///   estimator is armed AND its stratum cleared the sample floor. A
    ///   [`MoveEstimate`] can only be produced by [`crate::expected_move::MoveTable`],
    ///   so this argument cannot be forged from a number.
    /// * `lane`, `realized_sum_bps`, `realized_n` — the lane's §24 EXPECTANCY_V1
    ///   evidence, straight from `Engine::lane_edge`.
    /// * `prior_bps` — `Config::gate_expected_move_bps`, the cold-start constant, used
    ///   both as the fallback and as the shrinkage prior.
    /// * `min_lane_trades` — `Config::expectancy_min_lane_trades`, the sample gate and
    ///   the pooling pseudo-count.
    ///
    /// Resolution:
    ///
    /// ```text
    ///   model present  -> BOTH views are the model estimate (one number, one source)
    ///   model absent   -> admission = ColdStart(prior)                        (§18)
    ///                     ranking   = n >= k ? LanePrior(pooled) : ColdStart  (§24)
    /// ```
    ///
    /// The pooled lane estimate is `(Σ realized + prior · k) / (n + k)` with
    /// `k = max(min_lane_trades, 1)` — the same hierarchical partial pooling the
    /// retired `Engine::conditional_edge_bps` performed, relocated here so there is no
    /// standalone `i128` for a second call site to reach for.
    pub fn for_candidate(
        model: Option<&MoveEstimate>,
        lane: WlLane,
        realized_sum_bps: i128,
        realized_n: u32,
        prior_bps: u32,
        min_lane_trades: u32,
    ) -> Self {
        if let Some(e) = model {
            // The per-candidate estimate is strictly better evidence than a per-lane
            // one for BOTH questions, so it answers both. This is the case the silo
            // broke: arbitration used to discard it.
            let src = MoveSource::Model {
                band: e.band,
                n: e.n,
                signals_applied: e.signals_applied,
            };
            return Self {
                admission_bps: e.bps,
                admission_source: src,
                ranking_bps: i128::from(e.bps),
                ranking_source: src,
            };
        }
        let prior = i128::from(prior_bps);
        let k = i128::from(min_lane_trades.max(1));
        let (ranking_bps, ranking_source) = if i128::from(realized_n) < k {
            (prior, MoveSource::ColdStart)
        } else {
            (
                (realized_sum_bps + prior * k) / (i128::from(realized_n) + k),
                MoveSource::LanePrior {
                    lane,
                    n: realized_n,
                },
            )
        };
        Self {
            // §18/§38: admission is priced on the POPULATION estimate. The lane's
            // paper-realized history ranks slots; it does not authorise or veto
            // capital. See the module note for the measurement behind that line.
            admission_bps: prior_bps,
            admission_source: MoveSource::ColdStart,
            ranking_bps,
            ranking_source,
        }
    }

    /// §18: the benefit term the economic gate compares against this market's measured
    /// round-trip cost. Never negative — a model estimate is floored at zero by the
    /// estimator, and the cold-start prior is a `u32`.
    #[must_use]
    pub const fn admission_bps(&self) -> u32 {
        self.admission_bps
    }

    /// Which estimator justified the admitted size. Journalled on every admit.
    #[must_use]
    pub const fn admission_source(&self) -> MoveSource {
        self.admission_source
    }

    /// §23/§24: the signed term that ranks this candidate against the others competing
    /// for a position slot.
    #[must_use]
    pub const fn ranking_bps(&self) -> i128 {
        self.ranking_bps
    }

    /// Which estimator produced the ranking term.
    #[must_use]
    pub const fn ranking_source(&self) -> MoveSource {
        self.ranking_source
    }

    /// Whether both questions were answered by the same estimator — true exactly when
    /// the calibrated model spoke.
    #[must_use]
    pub fn is_single_sourced(&self) -> bool {
        self.admission_source == self.ranking_source
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expected_move::{MoveParams, MoveTable, MoveVerdict, SignalObs};

    const LANE: WlLane = WlLane::CreationSniper;
    const PRIOR: u32 = 1_800;
    const K: u32 = 8;
    const IN_BAND_VSOL: u64 = 61_740_908_643;

    fn priced(sum: i128, n: u32) -> PricedMove {
        PricedMove::for_candidate(None, LANE, sum, n, PRIOR, K)
    }

    /// **§18/§38 — THE LANE'S PAPER HISTORY MAY RANK, BUT NEVER ADMIT.**
    ///
    /// This is the load-bearing property of the whole module. A lane that has bled
    /// catastrophically must still be priced for admission on the POPULATION estimate,
    /// because paper-realized returns are not promotion evidence and must not acquire a
    /// capital veto by the back door. Merging the two views was built, measured, and
    /// rejected: it collapsed LAW B3's hazard tape from 48 admits to 13 and took the
    /// law's measured effect to exactly zero (module note).
    #[test]
    fn a_bleeding_lane_ranks_last_but_still_prices_admission_on_the_prior() {
        let bled = priced(-400_000, 100);
        assert!(bled.ranking_bps() < 0, "the lane must be able to rank last");
        assert_eq!(
            bled.admission_bps(),
            PRIOR,
            "a paper-realized loss must not become an admission veto (§24/§38)"
        );
        assert_eq!(bled.admission_source(), MoveSource::ColdStart);
        assert_eq!(
            bled.ranking_source(),
            MoveSource::LanePrior { lane: LANE, n: 100 }
        );
        assert!(!bled.is_single_sourced());
        // …and a winning lane ranks ahead of it on exactly the same rule.
        assert!(priced(400_000, 100).ranking_bps() > bled.ranking_bps());
    }

    /// **NO EVIDENCE, NO OPINION.** A lane under its sample gate reports the cold-start
    /// constant AND says that is where it came from, rather than reporting a lane
    /// estimate that happens to equal the prior — a distinction the journal needs and a
    /// bare integer erases.
    #[test]
    fn a_lane_under_its_sample_gate_reports_cold_start_not_a_lane_estimate() {
        for n in 0..K {
            let p = priced(999_999, n);
            assert_eq!(p.ranking_bps(), i128::from(PRIOR), "evidence leaked in early");
            assert_eq!(p.ranking_source(), MoveSource::ColdStart);
            assert_eq!(p.ranking_source().code(), 0);
            assert!(p.is_single_sourced(), "both views are the prior here");
        }
        assert_eq!(
            priced(0, K).ranking_source(),
            MoveSource::LanePrior { lane: LANE, n: K }
        );
        assert_eq!(MoveSource::LanePrior { lane: LANE, n: K }.code(), 1);
    }

    /// Shrinkage is hierarchical and hand-auditable: at `n == k` the ranking term sits
    /// exactly halfway between the lane's realized mean and the prior. This is the
    /// retired `conditional_edge_bps`, unchanged, relocated into the constructor.
    #[test]
    fn shrinkage_is_exactly_halfway_at_n_equals_k() {
        let realized = 200i128;
        let p = priced(realized * i128::from(K), K);
        assert_eq!(p.ranking_bps(), (realized + i128::from(PRIOR)) / 2);
    }

    /// **THE SILO THE MODULE ACTUALLY CLOSES.** With the model armed and calibrated,
    /// admission and arbitration price the SAME trade with the SAME number from the
    /// SAME source. Arbitration used to discard the per-candidate estimate and rank on
    /// six per-lane numbers instead.
    #[test]
    fn a_calibrated_model_prices_both_questions_identically() {
        let mut t = MoveTable::empty();
        let obs = SignalObs::from_features(6_500, 45, 300);
        for _ in 0..60 {
            t.record(IN_BAND_VSOL, obs, 900);
        }
        let params = MoveParams {
            min_sample: 30,
            prior_weight: 30,
            prior_bps: PRIOR,
        };
        let MoveVerdict::Known(e) = t.estimate(IN_BAND_VSOL, obs, params) else {
            panic!("the table is calibrated");
        };
        // A lane with a wildly different realized history must NOT be able to pull the
        // two views apart once the model has spoken.
        let p = PricedMove::for_candidate(Some(&e), LANE, -900_000, 500, PRIOR, K);
        assert!(p.is_single_sourced());
        assert_eq!(p.admission_bps(), e.bps);
        assert_eq!(p.ranking_bps(), i128::from(e.bps));
        assert_eq!(
            p.admission_source(),
            MoveSource::Model {
                band: e.band,
                n: e.n,
                signals_applied: e.signals_applied,
            }
        );
        assert_eq!(p.admission_source().code(), 2);
    }

    /// Totality at the extremes §22 requires be handled rather than assumed away.
    #[test]
    fn the_views_saturate_rather_than_wrapping() {
        let huge = PricedMove::for_candidate(None, LANE, i128::MAX / 2, 1, u32::MAX, 1);
        assert!(huge.ranking_bps() > i128::from(u32::MAX));
        assert_eq!(huge.admission_bps(), u32::MAX);
        let tiny = PricedMove::for_candidate(None, LANE, i128::MIN / 2, 1, 0, 1);
        assert!(tiny.ranking_bps() < 0);
        assert_eq!(tiny.admission_bps(), 0);
    }
}
