//! # risk_type — RiskTypeClassifier + §26 risk-priced participation
//!
//! §25/§26 forbid collapsing behavioral risk into a binary accept/reject. Instead
//! a candidate is graded into one of five risk types, and non-mechanical risk is
//! *priced* by selecting from an enumerated set of competing treatments rather
//! than auto-rejected:
//!
//! * [`RiskType`] — the five-way taxonomy: `UNTRADEABLE_RISK`,
//!   `TRADABLE_BUT_FRAGILE_RISK`, `AVOID_UNLESS_PROVEN_RISK`,
//!   `RESEARCH_ONLY_RISK`, `UNKNOWN_RISK`.
//! * [`Treatment`] / [`ALL_TREATMENTS`] — the §26 competing-treatment set (reject,
//!   reduced size, delayed confirmation, different EntryMode, shorter maximum
//!   hold, stricter thesis invalidation, stricter exit pressure, higher confidence
//!   requirement, no moonbag, faster de-risk).
//! * [`classify_risk_type`] — deterministic grader over already-computed market
//!   measures; [`recommend_treatments`] — the deterministic §26 treatment set for
//!   a graded candidate.
//!
//! Hard mechanical vetoes are preserved: a mechanically untradeable / unsafe /
//! actively-dumping state is `UNTRADEABLE_RISK` and its only treatment is
//! `Reject`. Behavioral risk never becomes a hard veto here — it is graded and
//! priced. The *which-treatment-wins-out-of-sample* decision is server-side; this
//! leaf produces the candidate set and a deterministic default recommendation.
//!
//! ## Constitution
//! §22: integer-only, no floats, deterministic. Thresholds are supplied via
//! [`RiskThresholds`] (operator-tunable), never hardcoded in the decision path.

// ---------------------------------------------------------------------------
// Risk taxonomy
// ---------------------------------------------------------------------------

/// The graded risk-type taxonomy of §25.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RiskType {
    /// Mechanically untradeable / unsafe — a hard veto, not a behavioral concern.
    Untradeable = 0,
    /// Tradeable but fragile — participate with risk-pricing treatments.
    TradableButFragile = 1,
    /// Avoid unless the edge is out-of-sample proven — highest tradeable caution.
    AvoidUnlessProven = 2,
    /// Insufficient evidence to trade live — research-only.
    ResearchOnly = 3,
    /// Ambiguous / not enough signal to grade confidently.
    Unknown = 4,
}

impl RiskType {
    /// Stable `u8` label.
    #[inline]
    pub fn label(self) -> u8 {
        self as u8
    }
}

/// One §26 competing treatment. Bit index equals the discriminant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum Treatment {
    /// Do not trade.
    Reject = 0,
    /// Trade at reduced size.
    ReducedSize = 1,
    /// Require additional confirmation before entry.
    DelayedConfirmation = 2,
    /// Use a different, safer EntryMode.
    DifferentEntryMode = 3,
    /// Cap the maximum hold shorter than default.
    ShorterMaximumHold = 4,
    /// Apply stricter thesis-invalidation conditions.
    StricterThesisInvalidation = 5,
    /// Apply stricter exit pressure.
    StricterExitPressure = 6,
    /// Require higher entry confidence.
    HigherConfidenceRequirement = 7,
    /// Do not carry a moonbag.
    NoMoonbag = 8,
    /// De-risk faster than default.
    FasterDeRisk = 9,
}

impl Treatment {
    /// This treatment's bit within a [`TreatmentSet`].
    #[inline]
    pub fn bit(self) -> u16 {
        1u16 << (self as u16)
    }
}

/// The full §26 competing-treatment set, in canonical order.
pub const ALL_TREATMENTS: [Treatment; 10] = [
    Treatment::Reject,
    Treatment::ReducedSize,
    Treatment::DelayedConfirmation,
    Treatment::DifferentEntryMode,
    Treatment::ShorterMaximumHold,
    Treatment::StricterThesisInvalidation,
    Treatment::StricterExitPressure,
    Treatment::HigherConfidenceRequirement,
    Treatment::NoMoonbag,
    Treatment::FasterDeRisk,
];

/// A deterministic bitset of recommended treatments.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct TreatmentSet(pub u16);

impl TreatmentSet {
    /// The empty set.
    pub const EMPTY: TreatmentSet = TreatmentSet(0);

    /// Build a set from a treatment slice.
    pub fn from_slice(ts: &[Treatment]) -> Self {
        let mut m = 0u16;
        for t in ts {
            m |= t.bit();
        }
        TreatmentSet(m)
    }

    /// Whether `t` is recommended.
    #[inline]
    pub fn contains(&self, t: Treatment) -> bool {
        self.0 & t.bit() != 0
    }

    /// The recommended treatments in canonical [`ALL_TREATMENTS`] order.
    pub fn treatments(&self) -> Vec<Treatment> {
        ALL_TREATMENTS
            .iter()
            .copied()
            .filter(|t| self.contains(*t))
            .collect()
    }

    /// Count of recommended treatments.
    #[inline]
    pub fn len(&self) -> u32 {
        self.0.count_ones()
    }

    /// Whether no treatment is recommended.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }
}

// ---------------------------------------------------------------------------
// Measures + thresholds
// ---------------------------------------------------------------------------

/// Already-computed market-state measures a candidate is risk-graded from.
///
/// The mechanical flags encode §26's reserved hard-veto states; the bps measures
/// are the behavioral inputs §26 says to *price*, not auto-reject.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RiskMeasures {
    /// A sell route mechanically exists.
    pub mechanically_sellable: bool,
    /// Protocol/mint/pool identity is valid and chain state trusted.
    pub protocol_safe: bool,
    /// Confirmed active creator dump in progress.
    pub active_creator_dump: bool,
    /// Fraction of position that can be exited at acceptable impact (bps).
    pub exit_capacity_bps: u32,
    /// Creator ownership of supply (bps) — higher is riskier.
    pub creator_ownership_bps: u32,
    /// Buyer independence (bps) — higher is safer.
    pub buyer_independence_bps: u32,
    /// Cluster-adjusted breadth quality (bps) — higher is safer.
    pub cluster_adjusted_breadth_bps: u32,
    /// Wallet concentration (bps) — higher is riskier.
    pub wallet_concentration_bps: u32,
    /// Evidence completeness across the measures above (bps).
    pub measure_completeness_bps: u32,
}

/// Operator-tunable risk-grading thresholds (versioned, not hardcoded in path).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RiskThresholds {
    /// Below this exit capacity a candidate is mechanically untradeable (bps).
    pub min_exit_capacity_bps: u32,
    /// Below this completeness there is too little evidence to trade (research only).
    pub research_completeness_bps: u32,
    /// Below this completeness (but above research) the grade is `Unknown`.
    pub gradeable_completeness_bps: u32,
    /// At/above this composite risk score ⇒ `AvoidUnlessProven` (bps).
    pub avoid_score_bps: u32,
    /// At/above this composite risk score ⇒ apply the elevated treatment set (bps).
    pub elevated_score_bps: u32,
}

impl RiskThresholds {
    /// A deterministic fixture used by the tests.
    pub fn test() -> Self {
        RiskThresholds {
            min_exit_capacity_bps: 1_000,
            research_completeness_bps: 3_000,
            gradeable_completeness_bps: 6_000,
            avoid_score_bps: 7_000,
            elevated_score_bps: 4_000,
        }
    }
}

/// The composite behavioral-risk score in bps (`0` = clean, `10_000` = maximal).
///
/// The unweighted mean of four risk axes, each expressed so that higher = riskier:
/// creator ownership, `(10_000 − buyer independence)`, `(10_000 − breadth)`, and
/// wallet concentration. Pure integer, saturating.
pub fn risk_score_bps(m: &RiskMeasures) -> u32 {
    let inv = |x: u32| 10_000u32.saturating_sub(x.min(10_000));
    let sum = (m.creator_ownership_bps.min(10_000) as u64)
        + inv(m.buyer_independence_bps) as u64
        + inv(m.cluster_adjusted_breadth_bps) as u64
        + (m.wallet_concentration_bps.min(10_000) as u64);
    (sum / 4) as u32
}

// ---------------------------------------------------------------------------
// Classifier (leaf: rt_classify)
// ---------------------------------------------------------------------------

/// Grade a candidate into a [`RiskType`] (leaf **rt_classify**).
///
/// Priority cascade:
/// 1. mechanically unsafe/untradeable/actively-dumping/exit-starved ⇒
///    `Untradeable` (a preserved hard veto);
/// 2. evidence completeness below the research floor ⇒ `ResearchOnly`;
/// 3. completeness below the gradeable floor ⇒ `Unknown`;
/// 4. composite risk score at/above the avoid threshold ⇒ `AvoidUnlessProven`;
/// 5. otherwise ⇒ `TradableButFragile`.
///
/// Pure integer, deterministic.
pub fn classify_risk_type(m: &RiskMeasures, th: &RiskThresholds) -> RiskType {
    if !m.protocol_safe
        || !m.mechanically_sellable
        || m.active_creator_dump
        || m.exit_capacity_bps < th.min_exit_capacity_bps
    {
        return RiskType::Untradeable;
    }
    if m.measure_completeness_bps < th.research_completeness_bps {
        return RiskType::ResearchOnly;
    }
    if m.measure_completeness_bps < th.gradeable_completeness_bps {
        return RiskType::Unknown;
    }
    if risk_score_bps(m) >= th.avoid_score_bps {
        return RiskType::AvoidUnlessProven;
    }
    RiskType::TradableButFragile
}

/// The deterministic §26 competing-treatment recommendation for a graded candidate
/// (leaf **rt_treatments**).
///
/// * `Untradeable` / `ResearchOnly` ⇒ `{Reject}` (no live participation).
/// * `Unknown` ⇒ `{DelayedConfirmation, HigherConfidenceRequirement}` — gather
///   more evidence before committing.
/// * `AvoidUnlessProven` ⇒ the full risk-pricing set: reduced size, delayed
///   confirmation, safer EntryMode, higher confidence, stricter invalidation,
///   stricter exit pressure, no moonbag, faster de-risk.
/// * `TradableButFragile` ⇒ reduced size, stricter exit pressure, shorter hold,
///   faster de-risk when the composite score is elevated; the empty set (trade
///   normally) when the score is below the elevated threshold.
///
/// This is the deterministic default candidate set; the out-of-sample winner is
/// selected server-side. Pure, deterministic.
pub fn recommend_treatments(
    risk_type: RiskType,
    m: &RiskMeasures,
    th: &RiskThresholds,
) -> TreatmentSet {
    use Treatment::*;
    match risk_type {
        RiskType::Untradeable | RiskType::ResearchOnly => TreatmentSet::from_slice(&[Reject]),
        RiskType::Unknown => {
            TreatmentSet::from_slice(&[DelayedConfirmation, HigherConfidenceRequirement])
        }
        RiskType::AvoidUnlessProven => TreatmentSet::from_slice(&[
            ReducedSize,
            DelayedConfirmation,
            DifferentEntryMode,
            HigherConfidenceRequirement,
            StricterThesisInvalidation,
            StricterExitPressure,
            NoMoonbag,
            FasterDeRisk,
        ]),
        RiskType::TradableButFragile => {
            if risk_score_bps(m) >= th.elevated_score_bps {
                TreatmentSet::from_slice(&[
                    ReducedSize,
                    StricterExitPressure,
                    ShorterMaximumHold,
                    FasterDeRisk,
                ])
            } else {
                TreatmentSet::EMPTY
            }
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// A clean, complete, low-risk baseline.
    fn clean() -> RiskMeasures {
        RiskMeasures {
            mechanically_sellable: true,
            protocol_safe: true,
            active_creator_dump: false,
            exit_capacity_bps: 9_000,
            creator_ownership_bps: 500,
            buyer_independence_bps: 9_000,
            cluster_adjusted_breadth_bps: 9_000,
            wallet_concentration_bps: 500,
            measure_completeness_bps: 9_000,
        }
    }

    fn classify(m: RiskMeasures) -> RiskType {
        classify_risk_type(&m, &RiskThresholds::test())
    }

    #[test]
    fn score_is_mean_of_four_axes() {
        // creator 1000, independence 6000 -> inv 4000, breadth 5000 -> inv 5000,
        // concentration 2000. mean = (1000+4000+5000+2000)/4 = 3000.
        let m = RiskMeasures {
            creator_ownership_bps: 1_000,
            buyer_independence_bps: 6_000,
            cluster_adjusted_breadth_bps: 5_000,
            wallet_concentration_bps: 2_000,
            ..clean()
        };
        assert_eq!(risk_score_bps(&m), 3_000);
    }

    #[test]
    fn mechanical_states_are_untradeable_hard_veto() {
        let mut a = clean();
        a.mechanically_sellable = false;
        assert_eq!(classify(a), RiskType::Untradeable);

        let mut b = clean();
        b.protocol_safe = false;
        assert_eq!(classify(b), RiskType::Untradeable);

        let mut c = clean();
        c.active_creator_dump = true;
        assert_eq!(classify(c), RiskType::Untradeable);

        let mut d = clean();
        d.exit_capacity_bps = 500;
        assert_eq!(classify(d), RiskType::Untradeable);

        // Its only treatment is Reject.
        let set = recommend_treatments(RiskType::Untradeable, &a, &RiskThresholds::test());
        assert_eq!(set.treatments(), vec![Treatment::Reject]);
    }

    #[test]
    fn completeness_bands_research_then_unknown() {
        let mut research = clean();
        research.measure_completeness_bps = 2_000; // below research floor 3_000
        assert_eq!(classify(research), RiskType::ResearchOnly);

        let mut unknown = clean();
        unknown.measure_completeness_bps = 5_000; // between 3_000 and 6_000
        assert_eq!(classify(unknown), RiskType::Unknown);
    }

    #[test]
    fn high_score_avoids_unless_proven() {
        // Maximal risk on every axis -> score 10_000 >= avoid 7_000.
        let m = RiskMeasures {
            creator_ownership_bps: 10_000,
            buyer_independence_bps: 0,
            cluster_adjusted_breadth_bps: 0,
            wallet_concentration_bps: 10_000,
            ..clean()
        };
        assert_eq!(risk_score_bps(&m), 10_000);
        assert_eq!(classify(m), RiskType::AvoidUnlessProven);
    }

    #[test]
    fn clean_candidate_is_tradable_but_fragile() {
        // clean() score = (500 + 1000 + 1000 + 500)/4 = 750 < elevated 4_000.
        assert_eq!(risk_score_bps(&clean()), 750);
        assert_eq!(classify(clean()), RiskType::TradableButFragile);
        // Low score -> no special treatment (trade normally).
        let set = recommend_treatments(
            RiskType::TradableButFragile,
            &clean(),
            &RiskThresholds::test(),
        );
        assert!(set.is_empty());
    }

    #[test]
    fn elevated_fragile_gets_risk_pricing_treatments() {
        // Push the score above elevated (4_000) but below avoid (7_000).
        let m = RiskMeasures {
            creator_ownership_bps: 5_000,
            buyer_independence_bps: 4_000,       // inv 6000
            cluster_adjusted_breadth_bps: 5_000, // inv 5000
            wallet_concentration_bps: 4_000,
            ..clean()
        };
        // score = (5000+6000+5000+4000)/4 = 5000.
        assert_eq!(risk_score_bps(&m), 5_000);
        assert_eq!(classify(m), RiskType::TradableButFragile);
        let set = recommend_treatments(RiskType::TradableButFragile, &m, &RiskThresholds::test());
        assert!(set.contains(Treatment::ReducedSize));
        assert!(set.contains(Treatment::StricterExitPressure));
        assert!(set.contains(Treatment::ShorterMaximumHold));
        assert!(set.contains(Treatment::FasterDeRisk));
        assert_eq!(set.len(), 4);
        assert!(!set.contains(Treatment::Reject));
    }

    #[test]
    fn avoid_treatment_set_is_the_full_pricing_menu_without_reject() {
        let m = clean();
        let set = recommend_treatments(RiskType::AvoidUnlessProven, &m, &RiskThresholds::test());
        assert_eq!(set.len(), 8);
        assert!(!set.contains(Treatment::Reject));
        assert!(set.contains(Treatment::DifferentEntryMode));
        assert!(set.contains(Treatment::NoMoonbag));
        // Canonical ordering preserved.
        assert_eq!(set.treatments().first(), Some(&Treatment::ReducedSize));
    }

    #[test]
    fn unknown_gathers_more_evidence() {
        let m = clean();
        let set = recommend_treatments(RiskType::Unknown, &m, &RiskThresholds::test());
        assert_eq!(
            set.treatments(),
            vec![
                Treatment::DelayedConfirmation,
                Treatment::HigherConfidenceRequirement
            ]
        );
    }

    #[test]
    fn all_treatments_set_is_complete_and_bits_unique() {
        let full = TreatmentSet::from_slice(&ALL_TREATMENTS);
        assert_eq!(full.len(), 10);
        assert_eq!(full.treatments().len(), 10);
    }

    #[test]
    fn labels_stable() {
        assert_eq!(RiskType::Untradeable.label(), 0);
        assert_eq!(RiskType::Unknown.label(), 4);
    }
}
