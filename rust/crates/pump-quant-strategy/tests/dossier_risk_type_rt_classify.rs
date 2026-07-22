// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'risk_type' component (leaf 'rt_classify').
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
fn classify_risk_type_priority_cascade_is_exact() {
    let th = RiskThresholds::test();

    // Baseline clean, complete, low-score candidate -> TradableButFragile.
    assert_eq!(
        classify_risk_type(&clean(), &th),
        RiskType::TradableButFragile
    );

    // (1) Mechanical vetoes each force Untradeable, dominating everything else.
    let mut a = clean();
    a.mechanically_sellable = false;
    assert_eq!(classify_risk_type(&a, &th), RiskType::Untradeable);
    let mut b = clean();
    b.protocol_safe = false;
    assert_eq!(classify_risk_type(&b, &th), RiskType::Untradeable);
    let mut c = clean();
    c.active_creator_dump = true;
    assert_eq!(classify_risk_type(&c, &th), RiskType::Untradeable);
    let mut d = clean();
    d.exit_capacity_bps = th.min_exit_capacity_bps - 1;
    assert_eq!(classify_risk_type(&d, &th), RiskType::Untradeable);
    // Veto dominates even when completeness is below research floor.
    let mut veto_dom = clean();
    veto_dom.protocol_safe = false;
    veto_dom.measure_completeness_bps = 0;
    assert_eq!(classify_risk_type(&veto_dom, &th), RiskType::Untradeable);

    // Boundary: exactly at min_exit_capacity is NOT vetoed.
    let mut at = clean();
    at.exit_capacity_bps = th.min_exit_capacity_bps;
    assert_ne!(classify_risk_type(&at, &th), RiskType::Untradeable);

    // (2) Completeness below research floor -> ResearchOnly.
    let mut r = clean();
    r.measure_completeness_bps = th.research_completeness_bps - 1;
    assert_eq!(classify_risk_type(&r, &th), RiskType::ResearchOnly);

    // (3) research <= completeness < gradeable -> Unknown.
    let mut u = clean();
    u.measure_completeness_bps = th.research_completeness_bps;
    assert_eq!(classify_risk_type(&u, &th), RiskType::Unknown);
    let mut u2 = clean();
    u2.measure_completeness_bps = th.gradeable_completeness_bps - 1;
    assert_eq!(classify_risk_type(&u2, &th), RiskType::Unknown);

    // (4) At gradeable floor with high score -> AvoidUnlessProven.
    let mut avoid = clean();
    avoid.measure_completeness_bps = th.gradeable_completeness_bps;
    avoid.creator_ownership_bps = 10_000;
    avoid.buyer_independence_bps = 0;
    avoid.cluster_adjusted_breadth_bps = 0;
    avoid.wallet_concentration_bps = 10_000;
    assert_eq!(risk_score_bps(&avoid), 10_000);
    assert_eq!(classify_risk_type(&avoid, &th), RiskType::AvoidUnlessProven);

    // Score exactly at avoid threshold -> AvoidUnlessProven (>= is inclusive).
    let mut at_avoid = clean();
    at_avoid.creator_ownership_bps = 7_000;
    at_avoid.buyer_independence_bps = 3_000; // inv 7_000
    at_avoid.cluster_adjusted_breadth_bps = 3_000; // inv 7_000
    at_avoid.wallet_concentration_bps = 7_000;
    assert_eq!(risk_score_bps(&at_avoid), 7_000);
    assert_eq!(
        classify_risk_type(&at_avoid, &th),
        RiskType::AvoidUnlessProven
    );
}
