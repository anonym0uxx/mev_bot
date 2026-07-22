// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'catalyst_classifier' component (leaf 'thresholds_standard_values').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_narrative::catalyst_classifier::*;

#[test]
fn thresholds_standard_values() {
    let t = CatalystThresholds::standard();
    assert_eq!(t.echo_bps, 6_000);
    assert_eq!(t.coordination_bps, 5_000);
    assert_eq!(t.creator_bps, 4_000);
    assert_eq!(t.platform_bps, 5_000);
    assert_eq!(t.exit_flow, 0);
    assert_eq!(t.flow_floor, 0);
    assert_eq!(t.genuine_sources, 8);
    assert_eq!(t.genuine_breadth, 5);
    for g in [
        t.echo_bps,
        t.coordination_bps,
        t.creator_bps,
        t.platform_bps,
    ] {
        assert!(g <= 10_000);
        assert!(g > 0);
    }
    assert_eq!(t, CatalystThresholds::standard());
}
