// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'exit_ladder' component (leaf 'el_partial_ladder').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports)]
use pump_quant_strategy::exit_ladder::*;

#[test]
fn prop_rungs_conserve_bound_and_cost_priced() {
    let c = ImpactCurve::linear_test(1_000);        // 1_000 lamports per bps
    // fixed cost 100 lamports, require each rung margin >= 200 bps -> rung >= 5_000
    let rungs = ladder_rungs(20_000, 4, 100, 200, &c);
    assert_eq!(rungs.iter().sum::<u64>(), 20_000);
    for r in &rungs {
        assert!(*r >= 5_000, "every rung clears the fixed-cost floor");
        assert!(c.impact_bps(*r) <= 4 || rungs.len() == MAX_RUNGS);
    }
    // a position only twice the cost floor cannot profitably split -> one clip
    let tiny = ladder_rungs(6_000, 4, 100, 200, &c);
    assert_eq!(tiny.len(), 1);
    assert_eq!(tiny[0], 6_000);
    // zero-margin sentinel also collapses to a single clip (no free splitting)
    let z = ladder_rungs(20_000, 4, 100, 0, &c);
    assert_eq!(z.len(), 1);
}
