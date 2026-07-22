// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'risk_type' component (leaf 'rt_treatments').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_strategy::risk_type::*;

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

#[test]
fn recommend_treatments_maps_each_grade_exactly() {
    let th = RiskThresholds::test();
    let m = clean();

    // Untradeable and ResearchOnly both collapse to exactly {Reject}.
    for rt in [RiskType::Untradeable, RiskType::ResearchOnly] {
        let set = recommend_treatments(rt, &m, &th);
        assert_eq!(set.treatments(), vec![Treatment::Reject]);
        assert_eq!(set.len(), 1);
        assert!(set.contains(Treatment::Reject));
    }

    // Unknown -> gather-more-evidence pair, canonical order, no Reject.
    let unk = recommend_treatments(RiskType::Unknown, &m, &th);
    assert_eq!(
        unk.treatments(),
        vec![
            Treatment::DelayedConfirmation,
            Treatment::HigherConfidenceRequirement
        ]
    );
    assert!(!unk.contains(Treatment::Reject));

    // AvoidUnlessProven -> full pricing menu of 8, never Reject.
    let avoid = recommend_treatments(RiskType::AvoidUnlessProven, &m, &th);
    assert_eq!(avoid.len(), 8);
    assert!(!avoid.contains(Treatment::Reject));
    assert!(avoid.contains(Treatment::DifferentEntryMode));
    assert!(avoid.contains(Treatment::NoMoonbag));
    // Canonical ordering: first recommended treatment is ReducedSize.
    assert_eq!(avoid.treatments().first(), Some(&Treatment::ReducedSize));

    // TradableButFragile with low score -> empty set (trade normally).
    let low = clean();
    assert!(risk_score_bps(&low) < th.elevated_score_bps);
    let low_set = recommend_treatments(RiskType::TradableButFragile, &low, &th);
    assert!(low_set.is_empty());
    assert_eq!(low_set.len(), 0);

    // TradableButFragile with elevated score -> exactly the 4-treatment pricing set.
    let mut hi = clean();
    hi.creator_ownership_bps = 5_000;
    hi.buyer_independence_bps = 4_000; // inv 6_000
    hi.cluster_adjusted_breadth_bps = 5_000; // inv 5_000
    hi.wallet_concentration_bps = 4_000;
    assert_eq!(risk_score_bps(&hi), 5_000);
    assert!(risk_score_bps(&hi) >= th.elevated_score_bps);
    let hi_set = recommend_treatments(RiskType::TradableButFragile, &hi, &th);
    assert_eq!(hi_set.len(), 4);
    assert!(hi_set.contains(Treatment::ReducedSize));
    assert!(hi_set.contains(Treatment::StricterExitPressure));
    assert!(hi_set.contains(Treatment::ShorterMaximumHold));
    assert!(hi_set.contains(Treatment::FasterDeRisk));
    assert!(!hi_set.contains(Treatment::Reject));

    // Reject appears iff the grade is a no-live-participation grade.
    for rt in [
        RiskType::Untradeable,
        RiskType::TradableButFragile,
        RiskType::AvoidUnlessProven,
        RiskType::ResearchOnly,
        RiskType::Unknown,
    ] {
        let has_reject = recommend_treatments(rt, &hi, &th).contains(Treatment::Reject);
        let should = matches!(rt, RiskType::Untradeable | RiskType::ResearchOnly);
        assert_eq!(has_reject, should, "reject presence wrong for {rt:?}");
    }
}
