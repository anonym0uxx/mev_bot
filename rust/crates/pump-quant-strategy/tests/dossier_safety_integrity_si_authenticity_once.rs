// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'safety_integrity' component (leaf 'si_authenticity_once').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(
    unused_imports,
    dead_code,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_strategy::safety_integrity::*;

#[test]
fn each_mode_touches_only_its_channel() {
    let s = SizeInputs::fresh(200);
    let edge = apply_authenticity(
        s.clone(),
        AuthConfidence { bps: 5000 },
        AuthMode::ThroughEdge,
    );
    assert_eq!(edge.edge_bps, 100); // 200 * 0.5
    assert_eq!(edge.haircut_bps, 10_000); // haircut untouched
    assert!(edge.edge_auth_adjusted && !edge.haircut_applied);

    let hc = apply_authenticity(s.clone(), AuthConfidence { bps: 5000 }, AuthMode::Haircut);
    assert_eq!(hc.edge_bps, 200); // edge untouched
    assert_eq!(hc.haircut_bps, 5000);
    assert!(hc.haircut_applied && !hc.edge_auth_adjusted);
}
#[test]
fn double_application_rejected() {
    let s = SizeInputs::fresh(200);
    let once = apply_authenticity(s, AuthConfidence { bps: 5000 }, AuthMode::ThroughEdge);
    // now try to also apply a haircut -> must be rejected (unchanged)
    let twice = apply_authenticity(
        once.clone(),
        AuthConfidence { bps: 5000 },
        AuthMode::Haircut,
    );
    assert_eq!(twice, once);
}
