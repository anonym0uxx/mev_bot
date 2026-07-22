// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'risk_type' component (leaf 'rt_risk_score_bps').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_strategy::risk_type::*;

fn measures(co: u32, bi: u32, br: u32, wc: u32) -> RiskMeasures {
    RiskMeasures {
        mechanically_sellable: true,
        protocol_safe: true,
        active_creator_dump: false,
        exit_capacity_bps: 9_000,
        creator_ownership_bps: co,
        buyer_independence_bps: bi,
        cluster_adjusted_breadth_bps: br,
        wallet_concentration_bps: wc,
        measure_completeness_bps: 9_000,
    }
}

#[test]
fn risk_score_bps_is_saturating_mean_of_four_axes() {
    // Concrete anchor from the module's own doctest math.
    assert_eq!(risk_score_bps(&measures(1_000, 6_000, 5_000, 2_000)), 3_000);
    // Clean baseline: (500 + 1000 + 1000 + 500)/4 = 750.
    assert_eq!(risk_score_bps(&measures(500, 9_000, 9_000, 500)), 750);
    // Maximal risk on every axis -> 10_000.
    assert_eq!(risk_score_bps(&measures(10_000, 0, 0, 10_000)), 10_000);
    // Minimal risk on every axis -> 0.
    assert_eq!(risk_score_bps(&measures(0, 10_000, 10_000, 0)), 0);

    // Saturation: out-of-range inputs are clamped to 10_000 before averaging.
    let sat = risk_score_bps(&measures(50_000, 50_000, 50_000, 50_000));
    // (10_000 + 0 + 0 + 10_000)/4 = 5_000.
    assert_eq!(sat, 5_000);

    // Exhaustive invariant sweep: formula match + bounds over a deterministic grid.
    let inv = |x: u32| 10_000u32.saturating_sub(x.min(10_000));
    for &co in &[0u32, 2_500, 7_500, 10_000, 40_000] {
        for &bi in &[0u32, 3_000, 10_000, 40_000] {
            for &br in &[0u32, 6_000, 10_000] {
                for &wc in &[0u32, 4_000, 10_000, 40_000] {
                    let s = risk_score_bps(&measures(co, bi, br, wc));
                    let expect = (co.min(10_000) + inv(bi) + inv(br) + wc.min(10_000)) / 4;
                    assert_eq!(
                        s, expect,
                        "formula mismatch co={co} bi={bi} br={br} wc={wc}"
                    );
                    assert!(s <= 10_000, "out of range: {s}");
                }
            }
        }
    }
}
