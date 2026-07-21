// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'economic_gate' component (leaf 'eg_size_band').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports)]
use pump_quant_strategy::economic_gate::*;

#[test]
fn prop_band_ordering_and_refusal() {
    let c = ImpactCurve::linear_test(1_000);
    let b = size_band(400, 160, 2680, 200, 50, 85_000_000, &c, 50_000_000);
    if matches!(b.verdict, Verdict::Admit) {
        assert!(b.x_min <= b.x_cost && b.x_cost <= b.x_max);
    }
    // impossibly thin edge -> Refuse, empty band
    let r = size_band(205, 160, 2680, 200, 50, 85_000_000, &c, 50_000_000);
    assert!(matches!(r.verdict, Verdict::Refuse));
    // profit-max sanity: for a 400bps move on depth 85e6, R*(move-proto)/4 is huge;
    // x_cost must be far below it (band is a constraint, not the maximizer)
    let admit = size_band(400, 160, 0, 200, 50, 85_000_000, &c, 50_000_000);
    if matches!(admit.verdict, Verdict::Admit) {
        let profit_max = 85_000_000u64 * (400 - 200) as u64 / 4 / 10_000;
        assert!(admit.x_cost < profit_max);
    }
}
