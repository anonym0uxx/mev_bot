// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'hazard' component (leaf 'hz_estimate_all_sorted').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_simulator::hazard::*;

#[test]
fn hz_estimate_all_sorted_and_matches_per_phase() {
    let mut h = PartialPooledHazard::new(1_000, 4, 8);
    h.observe(5, 2, 4).unwrap();
    h.observe(1, 0, 4).unwrap();
    h.observe(3, 4, 4).unwrap();
    let all = h.estimate_all();
    let ids: Vec<u16> = all.iter().map(|e| e.phase_id).collect();
    assert_eq!(ids, vec![1, 3, 5], "deterministic ascending phase order");
    assert_eq!(all.len(), 3);
    // Each aggregated entry matches the single-phase estimate.
    for e in &all {
        assert_eq!(e.hazard_bps, h.estimate(e.phase_id).hazard_bps);
    }
    // global = (2+0+4)*10000/(4+4+4) = 60000/12 = 5000
    assert_eq!(h.global_bps(), 5_000);
}
